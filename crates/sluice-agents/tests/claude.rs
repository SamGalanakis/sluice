use serde_json::{Value, json};
use sluice_agents::engines::{
    claude::{
        Claude,
        protocol::{self, Tail},
        state::{ClaudeState, STABLE_IDLE},
    },
    *,
};
use sluice_model::ids::{MessageId, RunId};
use sluice_process::{
    systemd::{ServiceCommand, StartOutcome, TransientService},
    tmux::ApprovedTmux,
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sluice-test-claude-{}", RunId::new()));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn hook(event: &str, extra: Value) -> protocol::Hook {
    let mut value = json!({"hook_event_name":event,"session_id":"redacted-session","cwd":"/scratch/repo","transcript_path":"/scratch/claude/projects/repo/redacted-session.jsonl"});
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    protocol::decode_hook(event, &value).unwrap()
}
fn snapshot(state: &mut ClaudeState, status: &str, now: u64) -> EngineObservation {
    state.snapshot(
        Some(status),
        true,
        false,
        Duration::from_secs(now),
        1900000000.0,
    )
}
#[test]
fn background_shell_agent_wakeup_and_only_own_one_shot_crons_wait() {
    let mut state = ClaudeState::default();
    state
        .hook(&hook("UserPromptSubmit", json!({"prompt":"p"})))
        .unwrap();
    state.hook(&hook("Stop",json!({"last_assistant_message":"Started.","background_tasks":[{"type":"shell","status":"running"},{"type":"local_agent","status":"running","description":"review"},{"type":"shell","status":"completed"}]}))).unwrap();
    let s = snapshot(&mut state, "shell", 0);
    assert_eq!(s.status, EngineStatus::Idle);
    assert_eq!(s.turns_completed, 1);
    assert_eq!(s.background_work.len(), 2);
    assert_eq!(
        snapshot(&mut state, "idle", 1).waiting.as_deref(),
        Some("background task: review")
    );
    let entries: Vec<Value> = include_str!("fixtures/claude/transcript.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for entry in &entries[..5] {
        state.transcript(entry, true);
    }
    state
        .hook(&hook("UserPromptSubmit", json!({"prompt":"p2"})))
        .unwrap();
    state.hook(&hook("Stop",json!({"last_assistant_message":"Scheduled.","session_crons":[{"id":"cron-redacted","recurring":false},{"id":"loop-owned"}]}))).unwrap();
    let s = snapshot(&mut state, "idle", 2);
    assert!(
        s.background_work
            .iter()
            .any(|s| s.starts_with("a wakeup at"))
    );
    assert!(s.background_work.contains(&"a scheduled job".into()));
    state.transcript(&entries[5], true);
    state
        .hook(&hook("UserPromptSubmit", json!({"prompt":"awake"})))
        .unwrap();
    state
        .hook(&hook(
            "Stop",
            json!({"last_assistant_message":"AWAKE","session_crons":[{"id":"loop-owned"}]}),
        ))
        .unwrap();
    let s = snapshot(&mut state, "idle", 3);
    assert_eq!(s.turns_completed, 3);
    assert!(s.waiting.is_none());
    assert_eq!(s.final_text, "AWAKE");
}
#[test]
fn interrupt_requires_ten_seconds_of_continuous_verified_idle_in_current_prompt() {
    assert_eq!(STABLE_IDLE, Duration::from_secs(10));
    for interruption in ["busy", "waiting", "new-prompt", "progress", "overlay"] {
        let mut state = ClaudeState::default();
        state
            .hook(&hook("UserPromptSubmit", json!({"prompt":"first"})))
            .unwrap();
        assert_eq!(snapshot(&mut state, "idle", 10).status, EngineStatus::Busy);
        match interruption {
            "new-prompt"=>state.hook(&hook("UserPromptSubmit",json!({"prompt":"second"}))).unwrap(),
            "progress"=>state.transcript(&json!({"type":"assistant","message":{"content":[{"type":"text","text":"working"}]}}),true),
            "overlay"=>{state.snapshot(Some("idle"),false,false,Duration::from_secs(12),0.0);},
            other=>{snapshot(&mut state,other,12);}
        }
        assert_eq!(snapshot(&mut state, "idle", 16).turns_completed, 0);
        assert_eq!(snapshot(&mut state, "idle", 25).turns_completed, 0);
        assert_eq!(snapshot(&mut state, "idle", 26).turns_completed, 1);
        assert_eq!(snapshot(&mut state, "idle", 50).turns_completed, 1);
    }
}
#[test]
fn hook_acknowledgements_errors_and_subagent_text_are_facts_only() {
    let mut state = ClaudeState::default();
    state.pending(InputId::Task, "the task".into());
    state
        .hook(&hook("UserPromptSubmit", json!({"prompt":"different"})))
        .unwrap();
    assert!(snapshot(&mut state, "idle", 0).acknowledged.is_empty());
    state
        .hook(&hook("UserPromptSubmit", json!({"prompt":"\nthe task\n"})))
        .unwrap();
    state
        .hook(&hook(
            "StopFailure",
            json!({"error":"rate_limit","last_assistant_message":"API Error: 429"}),
        ))
        .unwrap();
    let s = snapshot(&mut state, "idle", 1);
    assert_eq!(s.acknowledged, vec![InputId::Task]);
    assert_eq!(s.error.unwrap().kind, EngineErrorKind::Transient);
    state
        .hook(&hook("SubagentStart", json!({"agent_id":"a"})))
        .unwrap();
    state.transcript(&json!({"type":"assistant","message":{"content":[{"type":"text","text":"subagent result"}]}}),false);
    let s = snapshot(&mut state, "idle", 2);
    assert_eq!(s.final_text, "API Error: 429");
    assert_eq!(s.turns_completed, 1);
    assert!(s.waiting.is_some());
    state
        .hook(&hook(
            "SubagentStop",
            json!({"agent_id":"a","last_assistant_message":"child done"}),
        ))
        .unwrap();
    assert!(snapshot(&mut state, "idle", 3).waiting.is_none());
    state.hook(&hook("SessionEnd", json!({}))).unwrap();
    assert_eq!(snapshot(&mut state, "idle", 4).status, EngineStatus::Exited);
}
#[test]
fn protocol_bounds_identity_and_partial_transcript_offsets() {
    let scratch = Scratch::new();
    let path = scratch.0.join("transcript");
    fs::write(&path, b"old history\n").unwrap();
    let mut tail = Tail::new(path.clone(), fs::metadata(&path).unwrap().len());
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"type\":\"assistant\"").unwrap();
    assert!(tail.read().unwrap().is_empty());
    file.write_all(b"}\nnot json\n").unwrap();
    assert_eq!(tail.read().unwrap(), vec![json!({"type":"assistant"})]);
    fs::write(&path, b"{}\n").unwrap();
    assert_eq!(tail.read().unwrap(), vec![json!({})]);
    fs::write(&path, vec![b'x'; protocol::MAX_EVENT_BYTES + 1]).unwrap();
    assert!(Tail::new(path.clone(), 0).read().is_err());
    for id in ["../bad", "", "a/b", "--session"] {
        assert!(protocol::session_id(id).is_err());
    }
    let mut value: Value = serde_json::from_str(
        include_str!("fixtures/claude/hooks.jsonl")
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert!(protocol::decode_hook("Stop", &value).is_err());
    value["session_id"] = json!("../escape");
    assert!(protocol::decode_hook("SessionStart", &value).is_err());
}
#[test]
fn composer_rules_history_shell_overlay_wrapped_drafts_and_control_bytes() {
    let rule = "─".repeat(40);
    let pane = format!("x\n{rule}\n❯ \n{rule}\n footer");
    assert!(protocol::composer_ready(&pane));
    assert!(!protocol::composer_ready(&format!("{rule}\n! ls\n{rule}")));
    assert!(protocol::occupied("a dialog\n❯ 1. Yes"));
    assert!(!protocol::occupied(""));
    assert!(!protocol::composer_ready(&format!(
        "{rule}\n❯ \n{rule}\nsearch prompts: old"
    )));
    assert!(protocol::draft_visible(
        &format!("{rule}\n❯ \n  [Pasted text #1 +3 lines]\n{rule}"),
        "any"
    ));
    assert!(protocol::draft_visible(
        &format!("{rule}\n❯ \n Your task is in\n /a/very/long/path\n{rule}"),
        "Your task is in /a/very/"
    ));
    assert!(!protocol::draft_visible(
        &format!("❯ old task\nout\n{rule}\n❯ \n{rule}"),
        "old task"
    ));
    assert_eq!(protocol::paste_payload("x\r\ny\t\x1b\\\n"), b"x\ry\t\\\r");
}

struct Harness {
    scratch: Scratch,
    context: EngineContext,
    adapter: Claude,
    tmux: ApprovedTmux,
    service: TransientService,
    hook_listener: std::os::unix::net::UnixListener,
}
impl Harness {
    async fn new(config: Value) -> Self {
        let scratch = Scratch::new();
        for dir in ["run", "work", "claude", "home"] {
            fs::create_dir(scratch.0.join(dir)).unwrap();
            fs::set_permissions(scratch.0.join(dir), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let prefix = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/private-tmux");
        let tmux = ApprovedTmux::load(&prefix)
            .await
            .expect("build scripts/build-private-tmux before Claude integration tests");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/fixture")
            .canonicalize()
            .expect("build workspace binaries first");
        let wrapper = scratch.0.join("fake-claude");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport SLUICE_HOME={}\nexec {} claude \"$@\"\n",
                protocol::shell_quote(&scratch.0.to_string_lossy()),
                protocol::shell_quote(&fixture.to_string_lossy())
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            scratch.0.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let context = EngineContext {
            run_dir: scratch.0.join("run"),
            cwd: scratch.0.join("work"),
            model: None,
            effort: None,
            tmux_binary: Some(tmux.binary().into()),
        };
        let run = RunId::new();
        let adapter = Claude::new(wrapper, scratch.0.join("claude"), fixture.clone(), run);
        let hook_listener =
            std::os::unix::net::UnixListener::bind(scratch.0.join("hook.sock")).unwrap();
        hook_listener.set_nonblocking(true).unwrap();
        Self {
            scratch,
            context,
            adapter,
            tmux,
            service: TransientService::for_test(run),
            hook_listener,
        }
    }
    fn client(&self, args: &[&str]) -> std::process::Output {
        self.tmux
            .client_command(&self.context.run_dir)
            .unwrap()
            .args(args)
            .output()
            .unwrap()
    }
    async fn launch(&mut self, session: Option<&str>) {
        let launch = self
            .adapter
            .prepare(&self.context, session)
            .await
            .unwrap()
            .unwrap();
        fs::write(
            self.context.run_dir.join("task.md"),
            "original task context",
        )
        .unwrap();
        let server = self
            .tmux
            .server_command(&self.context.run_dir, None)
            .unwrap();
        let mut spec = ServiceCommand::new(server.get_program());
        spec.args = server.get_args().map(|s| s.into()).collect();
        spec.cwd = Some(self.context.run_dir.clone());
        spec.env = launch
            .env
            .iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        spec.env.extend(BTreeMap::from([
            (
                "SLUICE_HOME".into(),
                self.scratch.0.clone().into_os_string(),
            ),
            (
                "SLUICE_CLAUDE_HOOK_SOCKET".into(),
                self.scratch.0.join("hook.sock").into_os_string(),
            ),
            ("HOME".into(), self.scratch.0.join("home").into_os_string()),
            (
                "SLUICE_FAKE_CLAUDE".into(),
                self.scratch.0.join("config.json").into_os_string(),
            ),
        ]));
        let start = self.service.start_once(&spec).await.unwrap();
        assert!(matches!(start, StartOutcome::Confirmed { .. }), "{start:?}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.context.run_dir.join("tmux.sock").exists() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let mut command = self.tmux.client_command(&self.context.run_dir).unwrap();
        command.args(["new-session", "-d", "-s", "engine", "-x", "120", "-y", "50"]);
        // Multiple command arguments use tmux's direct exec path, not a shell.
        command.arg("-c").arg(&self.context.cwd);
        command.args(&launch.argv);
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            self.client(&["set-option", "-g", "remain-on-exit", "on"])
                .status
                .success()
        );
        self.adapter
            .execute(
                &self.context,
                session
                    .map(|s| EngineCommand::Resume { session: s.into() })
                    .unwrap_or(EngineCommand::StartFresh),
            )
            .await
            .unwrap();
    }
    async fn poll(&mut self) -> EngineObservation {
        use std::io::{BufRead, BufReader, Read};
        while let Ok((mut stream, _)) = self.hook_listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut bytes = vec![];
            BufReader::new(stream.try_clone().unwrap())
                .take((protocol::MAX_EVENT_BYTES + 1) as u64)
                .read_until(b'\n', &mut bytes)
                .unwrap();
            let hook: HookEvent = serde_json::from_slice(&bytes).unwrap();
            let reply = self.adapter.on_hook(hook).unwrap();
            assert_eq!(reply.exit_code, 0);
            let mut bytes = serde_json::to_vec(&reply).unwrap();
            bytes.push(b'\n');
            stream.write_all(&bytes).unwrap();
        }
        self.adapter.observe(&self.context).await.unwrap()
    }
    async fn turn(&mut self, id: InputId, text: &str, turns: u64) -> EngineObservation {
        let outcome = self
            .adapter
            .execute(
                &self.context,
                EngineCommand::DeliverText {
                    id,
                    text: text.into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            outcome,
            DeliveryOutcome::Pending | DeliveryOutcome::Acknowledged
        ));
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let s = self.poll().await;
            if s.turns_completed >= turns {
                return s;
            }
            assert!(Instant::now() < deadline, "timed out: {s:?}");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }
    async fn cleanup(&mut self) {
        self.adapter.close().await.unwrap();
        self.service.stop().await.unwrap();
        // The owned subtree is gone. Discard callbacks queued during its shutdown
        // before admitting the next invocation on this direct-test relay.
        while self.hook_listener.accept().is_ok() {}
        assert!(
            !self.context.run_dir.join("tmux.sock").exists()
                || !self.client(&["list-sessions"]).status.success()
        );
    }
    fn prompts(&self) -> Vec<String> {
        fs::read_to_string(self.context.run_dir.join("fixture-prompts.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = Command::new("/usr/bin/systemctl")
            .args(["--user", "stop", self.service.name()])
            .output();
    }
}
#[tokio::test]
async fn fake_fresh_required_submit_live_message_compaction_and_opus_mcp_profile() {
    let mut h=Harness::new(json!({"turns":[{"reply":"did it","tool":{"name":"Bash","input":{"command":"git status"}},"compact":true},{"reply":"noted","submit":{"word":"blue"}}]})).await;
    h.adapter = Claude::new(
        h.scratch.0.join("fake-claude"),
        h.scratch.0.join("claude"),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug/fixture")
            .canonicalize()
            .unwrap(),
        RunId::new(),
    )
    .with_mcp(Some("{\"mcpServers\":{}}".into()));
    h.launch(None).await;
    let s = h.turn(InputId::Task, "the-secret-prompt", 1).await;
    assert_eq!(s.final_text, "did it");
    assert_eq!(s.compactions, 1);
    assert!(s.acknowledged.contains(&InputId::Task));
    // Stop is turn evidence, and does not manufacture a submission.
    assert!(!h.context.run_dir.join("fixture-submission.json").exists());
    let s = h
        .turn(
            InputId::Message { id: MessageId(7) },
            "Message from orchestrator: please submit blue",
            2,
        )
        .await;
    assert_eq!(s.final_text, "noted");
    assert!(
        s.acknowledged
            .contains(&InputId::Message { id: MessageId(7) })
    );
    let submitted: Value = serde_json::from_slice(
        &fs::read(h.context.run_dir.join("fixture-submission.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(submitted, json!({"word":"blue"}));
    let argv: Vec<String> =
        serde_json::from_slice(&fs::read(h.context.run_dir.join("fixture-argv.json")).unwrap())
            .unwrap();
    assert!(!argv.contains(&"the-secret-prompt".into()));
    assert_eq!(
        &argv[..5],
        [
            "--model",
            "opus",
            "--dangerously-skip-permissions",
            "--disallowedTools",
            "AskUserQuestion"
        ]
    );
    assert!(argv.contains(&"--strict-mcp-config".into()));
    assert_eq!(
        fs::metadata(h.context.run_dir.join("claude-settings.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        h.adapter
            .progress_lines()
            .iter()
            .any(|line| line == "tool Bash git status")
    );
    h.cleanup().await;
}
#[tokio::test]
async fn fake_paste_boot_trust_wrapped_draft_dropped_enter_and_large_literal_input() {
    let mut h = Harness::new(
        json!({"boot_ms":200,"trust":true,"wrap":40,"drop_enters":1,"turns":[{"reply":"trusted"}]}),
    )
    .await;
    h.launch(None).await;
    let text = "run `ls $HOME` and \"$(date)\"\nthen a line ending in a backslash \\".to_owned()
        + &"\nlong literal line".repeat(1400);
    let s = h.turn(InputId::Task, &text, 1).await;
    assert_eq!(s.final_text, "trusted");
    assert_eq!(h.prompts(), vec![text]);
    h.cleanup().await;
}
#[tokio::test]
async fn fake_background_and_wakeup_continue_without_additional_delivery() {
    for field in ["background_s", "wakeup_s"] {
        let mut turn = json!({"reply":"started"});
        turn[field] = json!(0.4);
        let mut h =
            Harness::new(json!({"turns":[turn,{"reply":"finished","submit":{"word":"built"}}]}))
                .await;
        h.launch(None).await;
        let s = h.turn(InputId::Task, "build", 1).await;
        assert!(s.waiting.is_some(), "{field}: {s:?}");
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let s = h.poll().await;
            if s.turns_completed == 2 {
                assert_eq!(s.final_text, "finished");
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert_eq!(h.prompts(), vec!["build"]);
        h.cleanup().await;
    }
}
#[tokio::test]
async fn fake_resume_excludes_history_and_refuses_other_cwd_and_missing_session() {
    let mut h = Harness::new(json!({"turns":[{"reply":"old news"},{"reply":"new work"}]})).await;
    h.launch(None).await;
    let first = h.turn(InputId::Task, "first", 1).await;
    let sid = first.session_id.unwrap();
    // A shutdown callback must not be routed to a subsequent invocation.
    let mut stale = std::os::unix::net::UnixStream::connect(h.scratch.0.join("hook.sock")).unwrap();
    stale.write_all(b"{}\n").unwrap();
    h.cleanup().await;
    let meta = h.adapter.session(&sid).await.unwrap().unwrap();
    assert_eq!(meta.cwd, h.context.cwd);
    let mut wrong = h.context.clone();
    wrong.cwd = h.scratch.0.join("home");
    assert_eq!(
        h.adapter
            .prepare(&wrong, Some(&sid))
            .await
            .unwrap_err()
            .kind,
        EngineErrorKind::Fatal
    );
    assert_eq!(
        h.adapter
            .prepare(&h.context, Some("missing-session"))
            .await
            .unwrap_err()
            .kind,
        EngineErrorKind::MissingSession
    );
    fs::write(h.context.run_dir.join("fixture-events.jsonl"), "").unwrap();

    h.service = TransientService::for_test(RunId::new());
    h.launch(Some(&sid)).await;
    let s = h
        .turn(InputId::Message { id: MessageId(8) }, "feedback", 1)
        .await;
    assert_eq!(s.session_id.as_deref(), Some(sid.as_str()));
    assert_eq!(s.turns_started, 1);
    assert_eq!(s.final_text, "new work");
    assert!(
        !h.adapter
            .progress_lines()
            .iter()
            .any(|s| s.contains("old news"))
    );
    h.cleanup().await;
}
#[tokio::test]
async fn private_compaction_context_refresh_and_ask_user_denial() {
    let mut h = Harness::new(json!({})).await;
    h.adapter.prepare(&h.context, None).await.unwrap();
    let path = h.scratch.0.join("claude/projects/x/session.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "").unwrap();
    let payload = |event: &str, extra: Value| {
        let mut v = json!({"hook_event_name":event,"session_id":"session","transcript_path":path,"cwd":h.context.cwd});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    fs::write(h.context.run_dir.join("task.md"), "fallback task").unwrap();
    fs::write(
        h.context.run_dir.join("me.md"),
        "current callback and feedback",
    )
    .unwrap();
    let reply = h
        .adapter
        .on_hook(HookEvent {
            event: "SessionStart".into(),
            payload: payload("SessionStart", json!({"source":"compact"})),
        })
        .unwrap();
    assert_eq!(
        reply.stdout.unwrap()["hookSpecificOutput"]["additionalContext"],
        "current callback and feedback"
    );
    let reply = h
        .adapter
        .on_hook(HookEvent {
            event: "PreToolUse".into(),
            payload: payload("PreToolUse", json!({"tool_name":"AskUserQuestion"})),
        })
        .unwrap();
    assert_eq!(
        reply.stdout.unwrap()["hookSpecificOutput"]["permissionDecision"],
        "deny"
    );
}
#[tokio::test]
async fn unknown_version_and_model_rejected_before_task_or_launch() {
    let mut h = Harness::new(json!({})).await;
    let bin = h.scratch.0.join("unknown");
    fs::write(&bin, "#!/bin/sh\nprintf '9.0.0 (Claude Code)\\n'\n").unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
    h.adapter = Claude::new(
        bin,
        h.scratch.0.join("claude"),
        "/usr/bin/true".into(),
        RunId::new(),
    );
    assert_eq!(
        h.adapter.prepare(&h.context, None).await.unwrap_err().kind,
        EngineErrorKind::CapabilityMismatch
    );
    assert!(!h.context.run_dir.join("claude-settings.json").exists());
    h.context.model = Some("sonnet".into());
    assert!(
        h.adapter
            .prepare(&h.context, None)
            .await
            .unwrap_err()
            .message
            .contains("always runs Opus")
    );
}
#[tokio::test]
async fn fake_transient_and_fatal_error_classification() {
    let mut h = Harness::new(json!({"turns":[{"error":"API Error: 529 overloaded"}]})).await;
    h.launch(None).await;
    let s = h.turn(InputId::Task, "p", 1).await;
    assert_eq!(s.error.unwrap().kind, EngineErrorKind::Transient);
    h.cleanup().await;
    assert_eq!(
        sluice_agents::engines::claude::state::classify("Error: auth failed").kind,
        EngineErrorKind::Fatal
    );
}
#[tokio::test]
async fn cancellation_reaps_private_server_engine_and_background_shell() {
    let mut h = Harness::new(json!({"turns":[{"reply":"building","background_s":60}]})).await;
    h.launch(None).await;
    h.turn(InputId::Task, "p", 1).await;
    let state = h.service.query().await.unwrap();
    let group = sluice_process::cgroup::Cgroup::open_service(&state.cgroup.unwrap()).unwrap();
    let pids = group.member_pids().unwrap();
    assert!(pids.len() >= 3);
    h.cleanup().await;
    for pid in pids {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
}

#[tokio::test]
#[ignore = "g3_claude: one labelled real Opus session in private scratch homes"]
async fn g3_claude() {
    let mut h = Harness::new(json!({})).await;
    let owner = Path::new("/home/sam/.claude/.credentials.json");
    if !owner.is_file() || owner.symlink_metadata().unwrap().file_type().is_symlink() {
        eprintln!("g3_claude PENDING: no privately copyable Claude credential file");
        return;
    }
    let credential = h.scratch.0.join("claude/.credentials.json");
    fs::copy(owner, &credential).unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    let owner_config: Value =
        serde_json::from_slice(&fs::read("/home/sam/.claude.json").unwrap()).unwrap();
    let mut private_config = json!({"hasCompletedOnboarding":true,"numStartups":1,"theme":"dark","autoUpdates":false,"bypassPermissionsModeAccepted":true});
    for key in [
        "oauthAccount",
        "lastOnboardingVersion",
        "hasAcknowledgedCostThreshold",
    ] {
        if let Some(value) = owner_config.get(key) {
            private_config[key] = value.clone();
        }
    }
    let private_config_path = h.scratch.0.join("claude/.claude.json");
    fs::write(
        &private_config_path,
        serde_json::to_vec(&private_config).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&private_config_path, fs::Permissions::from_mode(0o600)).unwrap();
    let binary = std::env::var_os("SLUICE_G3_CLAUDE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/home/sam/.local/bin/claude".into());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/fixture")
        .canonicalize()
        .unwrap();
    h.adapter = Claude::new(
        binary,
        h.scratch.0.join("claude"),
        fixture.clone(),
        RunId::new(),
    )
    .with_mcp(Some("{\"mcpServers\":{}}".into()));
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&h.context.cwd)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "-q"]);
    git(&["config", "user.name", "Scratch gate"]);
    git(&["config", "user.email", "scratch@example.invalid"]);
    git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "Initialize scratch gate",
    ]);
    let baseline = git(&["rev-parse", "HEAD"]);
    h.launch(None).await;
    let submit = format!(
        "{} claude-submit",
        protocol::shell_quote(&fixture.to_string_lossy())
    );
    let task = format!(
        "This is the labelled g3_claude acceptance test. Work only in this scratch repo. Create first.txt containing hi and commit with message 'Create first file'. Submit the declared output word=blue by running {submit} '{{\"word\":\"blue\"}}'. Then end your turn. Keep it brief."
    );
    h.adapter
        .execute(
            &h.context,
            EngineCommand::DeliverText {
                id: InputId::Task,
                text: task,
            },
        )
        .await
        .unwrap();
    let mut live_sent = false;
    let deadline = Instant::now() + Duration::from_secs(240);
    let first = loop {
        let s = h.poll().await;
        if s.acknowledged.contains(&InputId::Task) && !live_sent {
            h.adapter.execute(&h.context,EngineCommand::Steer{id:InputId::Message{id:MessageId(100)},text:"Message from orchestrator on your g3_claude thread: also create message.txt containing heron and include it in your commit. Keep the declared submission word=blue.".into()}).await.unwrap();
            live_sent = true;
        }
        if h.context.run_dir.join("fixture-submission.json").exists()
            && s.acknowledged
                .contains(&InputId::Message { id: MessageId(100) })
            && s.status == EngineStatus::Idle
            && s.turns_completed > 0
        {
            break s;
        }
        assert!(s.error.is_none(), "g3_claude engine error: {:?}", s.error);
        assert!(
            Instant::now() < deadline,
            "g3_claude fresh/live timed out: {s:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let sid = first.session_id.unwrap();
    eprintln!("g3_claude fresh and live message accepted, session {}", sid);
    assert_eq!(
        fs::read_to_string(h.context.cwd.join("message.txt"))
            .unwrap()
            .trim(),
        "heron"
    );
    let submitted: Value = serde_json::from_slice(
        &fs::read(h.context.run_dir.join("fixture-submission.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(submitted["word"], "blue");
    h.adapter.inject_transient();
    assert_eq!(
        h.poll().await.error.unwrap().kind,
        EngineErrorKind::Transient
    );
    h.cleanup().await;
    h.service = TransientService::for_test(RunId::new());
    h.launch(Some(&sid)).await;
    let task = format!(
        "Feedback resume in the same g3_claude session after an injected transient. Create second.txt containing resumed, commit it with message 'Create resumed file', and submit the declared word=green with {submit} '{{\"word\":\"green\"}}'. End your turn. Keep it brief."
    );
    h.adapter
        .execute(
            &h.context,
            EngineCommand::DeliverText {
                id: InputId::Continue { attempt: 1 },
                text: task,
            },
        )
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(180);
    let last = loop {
        let s = h.poll().await;
        let value = protocol::read_json(&h.context.run_dir.join("fixture-submission.json"));
        if value.is_some_and(|v| v["word"] == "green")
            && s.turns_completed > 0
            && s.status == EngineStatus::Idle
        {
            break s;
        }
        assert!(s.error.is_none(), "g3_claude resume error: {:?}", s.error);
        assert!(
            Instant::now() < deadline,
            "g3_claude resume timed out: {s:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(last.session_id.as_deref(), Some(sid.as_str()));
    assert!(
        last.acknowledged
            .contains(&InputId::Continue { attempt: 1 })
    );
    let count = Command::new("git")
        .args(["rev-list", "--count", &format!("{baseline}..HEAD")])
        .current_dir(&h.context.cwd)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&count.stdout)
            .trim()
            .parse::<u32>()
            .unwrap()
            >= 2
    );
    h.cleanup().await;
    eprintln!(
        "g3_claude PASS: fresh submit, addressed live message, same-session feedback/transient resume, original git baseline and cleanup"
    );
}

#[tokio::test]
async fn fake_missing_outputs_are_nudged_and_a_late_submission_is_separate_from_stop() {
    for submits in [false, true] {
        let last = if submits {
            json!({"reply":"submitted","submit":{"word":"late"}})
        } else {
            json!({"reply":"still not submitted"})
        };
        let mut h =
            Harness::new(json!({"turns":[{"reply":"done, I think"},{"reply":"still done"},last]}))
                .await;
        h.launch(None).await;
        h.turn(InputId::Task, "submit word", 1).await;
        assert!(!h.context.run_dir.join("fixture-submission.json").exists());
        h.turn(
            InputId::Nudge { ordinal: 1 },
            "Your turn ended but word is not submitted.",
            2,
        )
        .await;
        let s = h
            .turn(
                InputId::Nudge { ordinal: 2 },
                "Submit word before ending the turn.",
                3,
            )
            .await;
        assert_eq!(
            h.context.run_dir.join("fixture-submission.json").exists(),
            submits
        );
        assert_eq!(h.prompts().len(), 3);
        assert_eq!(
            s.final_text,
            if submits {
                "submitted"
            } else {
                "still not submitted"
            }
        );
        h.cleanup().await;
    }
}
#[tokio::test]
async fn fake_addressed_message_can_be_queued_during_an_active_turn() {
    let mut h =
        Harness::new(json!({"turns":[{"reply":"working","busy_s":1.5},{"reply":"noted"}]})).await;
    h.launch(None).await;
    h.adapter
        .execute(
            &h.context,
            EngineCommand::DeliverText {
                id: InputId::Task,
                text: "original task".into(),
            },
        )
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let s = h.poll().await;
        if s.acknowledged.contains(&InputId::Task) {
            assert_eq!(s.status, EngineStatus::Busy);
            assert_eq!(s.turns_completed, 0);
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let s = h
        .turn(
            InputId::Message { id: MessageId(9) },
            "Message from orchestrator: please also do X",
            2,
        )
        .await;
    assert!(
        s.acknowledged
            .contains(&InputId::Message { id: MessageId(9) })
    );
    assert_eq!(h.prompts().len(), 2);
    h.cleanup().await;
}
#[tokio::test]
async fn fake_same_run_transient_after_commit_preserves_session_and_original_baseline() {
    let mut h=Harness::new(json!({"turns":[{"run":"printf first > first; git add first; git commit -q -m 'Create first file'","error":"API Error: 529 overloaded"},{"run":"printf second > second; git add second; git commit -q -m 'Create second file'","reply":"recovered"}]})).await;
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Scratch"],
        vec!["config", "user.email", "scratch@example.invalid"],
        vec!["commit", "-q", "--allow-empty", "-m", "Initialize scratch"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&h.context.cwd)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let baseline = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&h.context.cwd)
        .output()
        .unwrap();
    let baseline = String::from_utf8_lossy(&baseline.stdout).trim().to_owned();
    h.launch(None).await;
    let first = h.turn(InputId::Task, "original task", 1).await;
    assert_eq!(first.error.unwrap().kind, EngineErrorKind::Transient);
    let sid = first.session_id.unwrap();
    h.cleanup().await;
    h.service = TransientService::for_test(RunId::new());
    h.launch(Some(&sid)).await;
    let last = h
        .turn(
            InputId::Continue { attempt: 1 },
            "Your session was interrupted by a rate limit; continue.",
            1,
        )
        .await;
    assert_eq!(last.session_id.as_deref(), Some(sid.as_str()));
    let count = Command::new("git")
        .args(["rev-list", "--count", &format!("{baseline}..HEAD")])
        .current_dir(&h.context.cwd)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2");
    assert_eq!(
        h.prompts().iter().filter(|p| *p == "original task").count(),
        1
    );
    h.cleanup().await;
}

#[path = "acceptance/support.rs"]
mod acceptance;

#[tokio::test]
async fn supervisor_fresh_required_submit() {
    acceptance::scenario("fresh_required_submit", "claude").await;
}

#[tokio::test]
async fn supervisor_busy_submitted() {
    acceptance::scenario("busy_submitted", "claude").await;
}

#[tokio::test]
async fn supervisor_background() {
    acceptance::scenario("background", "claude").await;
}

#[tokio::test]
async fn supervisor_quiet() {
    acceptance::scenario("quiet", "claude").await;
}

#[tokio::test]
async fn supervisor_compaction() {
    acceptance::scenario("compaction", "claude").await;
}

#[tokio::test]
async fn supervisor_addressed_live_message() {
    acceptance::scenario("addressed_live_message", "claude").await;
}

#[tokio::test]
async fn supervisor_feedback_resume() {
    acceptance::scenario("feedback_resume", "claude").await;
}

#[tokio::test]
async fn supervisor_missing_outputs() {
    acceptance::scenario("missing_outputs", "claude").await;
}

#[tokio::test]
async fn supervisor_nudge() {
    acceptance::scenario("nudge", "claude").await;
}

#[tokio::test]
async fn supervisor_unknown_acceptance() {
    acceptance::scenario("unknown_acceptance", "claude").await;
}

#[tokio::test]
async fn supervisor_cancel_backoff() {
    acceptance::scenario("cancel_backoff", "claude").await;
}

#[tokio::test]
async fn supervisor_retry_exhaustion() {
    acceptance::scenario("retry_exhaustion", "claude").await;
}

#[tokio::test]
async fn supervisor_session_cwd_mismatch() {
    acceptance::scenario("session_cwd_mismatch", "claude").await;
}

#[tokio::test]
async fn supervisor_engine_mismatch() {
    acceptance::scenario("engine_mismatch", "claude").await;
}

#[tokio::test]
async fn supervisor_claude_hooks_submit_live_compact_and_resume() {
    let mut h = Harness::new(json!({"turns":[{"reply":"done","submit":{"word":"blue"},"compact":true,"delay_ms":100},{"reply":"feedback","submit":{"word":"blue"}}]})).await;
    let hook = h.scratch.0.join("journal-hook");
    fs::write(&hook, include_str!("acceptance/hook.py")).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/fixture")
        .canonicalize()
        .unwrap();
    let wrapper = h.scratch.0.join("supervised-claude");
    fs::write(&wrapper, format!("#!/bin/sh\nexport SLUICE_HOME={}\nexport HOME={}\nexport SLUICE_FAKE_CLAUDE={}\nexec {} claude \"$@\"\n", protocol::shell_quote(&h.scratch.0.to_string_lossy()), protocol::shell_quote(&h.scratch.0.join("home").to_string_lossy()), protocol::shell_quote(&h.scratch.0.join("config.json").to_string_lossy()), protocol::shell_quote(&fixture.to_string_lossy()))).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut cfg = acceptance::config(&h.scratch.0, "claude");
    cfg.limits.wall = Duration::from_secs(15);
    cfg.limits.ready = Duration::from_secs(5);
    cfg.limits.turn_start = Duration::from_secs(5);
    cfg.limits.stall = Duration::from_secs(10);
    cfg.limits.settle = Duration::from_millis(100);
    let mut adapter = Claude::new(wrapper, h.scratch.0.join("claude"), hook, cfg.run);
    let directory = cfg.run_dir.clone();
    let mut host = acceptance::Host {
        directory: Some(directory.clone()),
        messages: vec![acceptance::message(1)],
        ..Default::default()
    };
    let result = sluice_agents::supervisor::supervise(
        cfg.clone(),
        &mut adapter,
        &mut host,
        &mut Default::default(),
        Some(&h.tmux),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(host.submissions["word"], json!("blue"));
    assert_eq!(host.acks, vec![sluice_model::ids::MessageId(1)]);
    assert!(
        fs::read_to_string(directory.join("me.md"))
            .unwrap()
            .contains("current acceptance")
    );
    assert!(!h.client(&["list-sessions"]).status.success());
    fs::write(
        h.scratch.0.join("config.json"),
        serde_json::to_vec(
            &json!({"turns":vec![json!({"reply":"resumed","submit":{"word":"blue"}});8]}),
        )
        .unwrap(),
    )
    .unwrap();
    cfg.run = RunId::new();
    cfg.attempt = sluice_model::ids::AttemptId::new();
    cfg.invocation = sluice_model::ids::InvocationId::new();
    cfg.run_dir = h.scratch.0.join("resumed");
    cfg.previous = Some(sluice_agents::supervisor::PreviousSession {
        engine: "claude".into(),
        cwd: cfg.cwd.clone(),
        session: Some(result.session.clone()),
    });
    cfg.assigned.through = sluice_model::ids::MessageId(2);
    cfg.messages = vec![acceptance::message(2)];
    let mut host = acceptance::Host {
        directory: Some(cfg.run_dir.clone()),
        ..Default::default()
    };
    let next = sluice_agents::supervisor::supervise(
        cfg,
        &mut adapter,
        &mut host,
        &mut Default::default(),
        Some(&h.tmux),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(next.session, result.session);
    assert_eq!(host.acks, vec![sluice_model::ids::MessageId(2)]);
    h.cleanup().await;
}

#[tokio::test]
async fn supervisor_same_run_transient_commits_without_feedback() {
    acceptance::transient_commits("claude").await;
}

#[tokio::test]
async fn supervisor_missing_session_lock_and_cwd() {
    acceptance::session_policy("claude").await;
}

#[tokio::test]
async fn supervisor_predecessor_cwd_mismatch_starts_fresh() {
    acceptance::scenario("predecessor_cwd_mismatch", "claude").await;
}
