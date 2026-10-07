use serde_json::{Value, json};
use sluice_agents::engines::{
    claude::{
        Claude,
        protocol::{self, Tail},
        state::ClaudeState,
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

#[path = "../../../tests/support/executable.rs"]
mod executable;

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
/// A workspace binary from the profile directory cargo built this test into (the
/// shared or configured target dir), never a stale `<repo>/target/debug` copy.
fn built(name: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    exe.parent().and_then(Path::parent).unwrap().join(name)
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

/// Claude Code writes a transcript line over 1 MiB for a large tool result or file read:
/// the reader skips it and goes on with the records after it, and the run's state keeps
/// following the transcript.
#[test]
fn a_two_mib_transcript_record_is_skipped_and_the_records_around_it_are_read() {
    let scratch = Scratch::new();
    let path = scratch.0.join("transcript.jsonl");
    let assistant = |text: &str| {
        json!({"type":"assistant","message":{"content":[{"type":"text","text":text}]}}).to_string()
    };
    let huge = json!({"type":"user","message":{"content":[{"type":"tool_result","content":"x".repeat(2 * protocol::MAX_EVENT_BYTES)}]}}).to_string();
    fs::write(
        &path,
        format!("{}\n{huge}\n{}\n", assistant("before"), assistant("after")),
    )
    .unwrap();
    let mut tail = Tail::new(path.clone(), 0);
    let mut records = vec![];
    loop {
        let offset = tail.offset();
        records.extend(tail.read().unwrap());
        if tail.offset() == offset {
            break;
        }
    }
    assert_eq!(
        records.len(),
        2,
        "{:?}",
        records
            .iter()
            .map(|r| r["type"].clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(tail.skipped(), 1);
    let mut state = ClaudeState::default();
    for record in &records {
        state.transcript(record, true);
    }
    assert_eq!(snapshot(&mut state, "busy", 1).final_text, "after");
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "{}", assistant("later")).unwrap();
    for record in tail.read().unwrap() {
        state.transcript(&record, true);
    }
    assert_eq!(snapshot(&mut state, "busy", 2).final_text, "later");
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
    // A record still growing past the limit is dropped up to its newline, not fatal.
    fs::write(&path, vec![b'x'; protocol::MAX_EVENT_BYTES + 1]).unwrap();
    let mut tail = Tail::new(path.clone(), 0);
    assert!(tail.read().unwrap().is_empty());
    assert_eq!(tail.skipped(), 1);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"xxxx\n{\"type\":\"user\"}\n").unwrap();
    assert_eq!(tail.read().unwrap(), vec![json!({"type":"user"})]);
    assert_eq!(tail.skipped(), 1);
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
        let fixture = built("fixture")
            .canonicalize()
            .expect("build workspace binaries first");
        let wrapper = scratch.0.join("fake-claude");
        executable::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexport SLUICE_HOME={}\nexec {} claude \"$@\"\n",
                protocol::shell_quote(&scratch.0.to_string_lossy()),
                protocol::shell_quote(&fixture.to_string_lossy())
            ),
        );
        fs::write(
            scratch.0.join("config.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
        let context = EngineContext {
            run_dir: scratch.0.join("run"),
            cwd: scratch.0.join("work"),
            model: None,
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
        built("fixture").canonicalize().unwrap(),
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
        &argv[..7],
        [
            "--model",
            "opus",
            "--effort",
            "high",
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
/// A fake Claude printing `version` and `help`, the rest delegated to the fixture.
fn fake_claude(h: &Harness, name: &str, version: &str, help: &str) -> PathBuf {
    let bin = h.scratch.0.join(name);
    executable::write(
        &bin,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then printf '%s\\n' '{version}'; exit; fi\nif [ \"$1\" = --help ]; then printf '%s' '{help}'; exit; fi\nexit 1\n"
        ),
    );
    bin
}
#[tokio::test]
async fn version_policy_refuses_older_or_unreadable_versions_before_task_or_launch() {
    for (version, message) in [
        (
            "2.1.282 (Claude Code)",
            "claude 2.1.282 is older than 2.1.283, the oldest version with every capability sluice needs (tested 2.1.283, 2.1.284); update claude on this host, then step_retry",
        ),
        (
            "2.1.284 (Other Tool)",
            "claude: `claude --version` printed `2.1.284 (Other Tool)`, not a Claude Code version; sluice needs Claude Code 2.1.283 or newer (tested 2.1.283, 2.1.284)",
        ),
        (
            "latest (Claude Code)",
            "claude: cannot read a version from `claude --version` (got `latest`); sluice needs claude 2.1.283 or newer (tested 2.1.283, 2.1.284)",
        ),
    ] {
        let mut h = Harness::new(json!({})).await;
        let bin = fake_claude(&h, "old", version, claude::fixture::FIXTURE_HELP);
        h.adapter = Claude::new(
            bin,
            h.scratch.0.join("claude"),
            "/usr/bin/true".into(),
            RunId::new(),
        );
        let error = h.adapter.prepare(&h.context, None).await.unwrap_err();
        assert_eq!(error.kind, EngineErrorKind::CapabilityMismatch);
        assert_eq!(error.message, message);
        assert!(!h.context.run_dir.join("claude-settings.json").exists());
    }
}
#[tokio::test]
async fn a_newer_version_runs_untested_unless_a_flag_sluice_needs_is_gone() {
    let h = Harness::new(json!({})).await;
    let bin = fake_claude(
        &h,
        "newer",
        "2.1.293 (Claude Code)",
        claude::fixture::FIXTURE_HELP,
    );
    let mut adapter = Claude::new(
        bin.clone(),
        h.scratch.0.join("claude"),
        "/usr/bin/true".into(),
        RunId::new(),
    )
    .with_probe_cache(h.scratch.0.join("probes"));
    // Past the version and the probe, the launch goes on (here to the tmux it lacks).
    let _ = adapter.prepare(&h.context, None).await;
    let verdict = adapter.version().unwrap();
    assert_eq!(
        verdict.note().unwrap(),
        "claude 2.1.293 is newer than the tested 2.1.283, 2.1.284; accepted untested (floor 2.1.283)"
    );
    assert!(h.scratch.0.join("probes/claude.json").is_file());

    let mut h = Harness::new(json!({})).await;
    let without = claude::fixture::FIXTURE_HELP.replace("  --effort <level>\n", "");
    let bin = fake_claude(
        &h,
        "newer-without-effort",
        "2.1.293 (Claude Code)",
        &without,
    );
    h.adapter = Claude::new(
        bin,
        h.scratch.0.join("claude"),
        "/usr/bin/true".into(),
        RunId::new(),
    );
    let error = h.adapter.prepare(&h.context, None).await.unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::CapabilityMismatch);
    assert_eq!(
        error.message,
        "claude 2.1.293 lacks `--effort` that sluice needs (tested 2.1.283, 2.1.284; this version is untested)"
    );
    assert!(!h.context.run_dir.join("claude-settings.json").exists());
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
    let account = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let owner = &account.join(".claude/.credentials.json");
    if !owner.is_file() || owner.symlink_metadata().unwrap().file_type().is_symlink() {
        eprintln!("g3_claude PENDING: no privately copyable Claude credential file");
        return;
    }
    let credential = h.scratch.0.join("claude/.credentials.json");
    fs::copy(owner, &credential).unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    let owner_config: Value =
        serde_json::from_slice(&fs::read(account.join(".claude.json")).unwrap()).unwrap();
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
        .unwrap_or_else(|| account.join(".local/bin/claude"));
    let fixture = built("fixture").canonicalize().unwrap();
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
async fn supervisor_claude_hooks_submit_live_compact_and_resume() {
    // The agent submits only after the live message: submitting ends the session.
    let mut h = Harness::new(json!({"turns":[{"reply":"done","compact":true,"delay_ms":100},{"reply":"feedback","submit":{"word":"blue"}}]})).await;
    let hook = h.scratch.0.join("journal-hook");
    executable::write(&hook, include_str!("acceptance/hook.py"));
    let fixture = built("fixture").canonicalize().unwrap();
    let wrapper = h.scratch.0.join("supervised-claude");
    executable::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport SLUICE_HOME={}\nexport HOME={}\nexport SLUICE_FAKE_CLAUDE={}\nexec {} claude \"$@\"\n",
            protocol::shell_quote(&h.scratch.0.to_string_lossy()),
            protocol::shell_quote(&h.scratch.0.join("home").to_string_lossy()),
            protocol::shell_quote(&h.scratch.0.join("config.json").to_string_lossy()),
            protocol::shell_quote(&fixture.to_string_lossy())
        ),
    );
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
        // The turn cursor carries over from the first session (two turns): the resumed
        // session's task and message turns submit nothing, a later one submits.
        serde_json::to_vec(&json!({"turns":[
            {}, {}, {"reply":"resumed"}, {"reply":"resumed"},
            {"reply":"resumed","submit":{"word":"blue"}},
            {"reply":"resumed","submit":{"word":"blue"}}
        ]}))
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

/// The redacted real API error entries 2.1.284 wrote for usage and rate limits.
fn real_limits() -> Vec<Value> {
    include_str!("fixtures/claude/real-usage-limits.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
/// The real API error entries 2.1.284 wrote when it could not authenticate.
fn real_auth() -> Vec<Value> {
    include_str!("fixtures/claude/real-auth-errors.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn prompted() -> ClaudeState {
    let mut state = ClaudeState::default();
    state
        .hook(&hook("UserPromptSubmit", json!({"prompt":"p"})))
        .unwrap();
    state
}
fn transcript_error(entry: &Value) -> Option<EngineError> {
    let mut state = prompted();
    state.transcript(entry, true);
    snapshot(&mut state, "idle", 0).error
}
fn text_entry(category: &str, text: &str) -> Value {
    json!({"type":"assistant","isApiErrorMessage":true,"error":category,"message":{"content":[{"type":"text","text":text}]}})
}
#[test]
fn claude_usage_limits_fail_as_quota_exhausted_and_short_or_plain_rate_limits_stay_transient() {
    let real = real_limits();
    let now = account::now();
    // The weekly limit, resetting in three days (the capture's own reset has passed).
    let mut weekly = real[0].clone();
    weekly["quotaLimits"]["resetsAt"] = json!(now + 3 * 86400);
    let error = transcript_error(&weekly).unwrap();
    assert_eq!(error.kind, EngineErrorKind::QuotaExhausted, "{error:?}");
    assert!(
        error
            .message
            .starts_with("claude: weekly limit reached (seven_day); resets 20"),
        "{}",
        error.message
    );
    assert!(
        error.message.ends_with(
            " — wait for the reset or buy usage credits at https://claude.ai/settings/usage, then step_retry (or run the step on another engine). Claude said: You've hit your weekly limit \u{b7} resets Sep 22, 1am (Europe/Berlin)"
        ),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("(in 2d 23h)") || error.message.contains("(in 3d)"),
        "{}",
        error.message
    );
    assert_eq!(error.retry_at, None);
    // The 5-hour session limit an hour from its reset is a hard cap too.
    let mut session = real[1].clone();
    session["quotaLimits"]["resetsAt"] = json!(now + 3600);
    assert_eq!(
        transcript_error(&session).unwrap().kind,
        EngineErrorKind::QuotaExhausted
    );
    // Ten minutes from its reset it is a short limit that retries just after the reset.
    session["quotaLimits"]["resetsAt"] = json!(now + 600);
    let error = transcript_error(&session).unwrap();
    assert_eq!(error.kind, EngineErrorKind::Transient, "{error:?}");
    assert!(
        error
            .message
            .starts_with("claude: 5-hour session limit reached (five_hour); resets 20"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains(
            " — retrying just after the reset. Claude said: You've hit your session limit"
        ),
        "{}",
        error.message
    );
    let retry_at = error.retry_at.unwrap();
    assert!((now + 600..=now + 605).contains(&retry_at), "{retry_at}");
    // A smaller threshold makes the same limit a hard cap.
    let mut state = prompted();
    state.set_quota_threshold(Duration::from_secs(300));
    state.transcript(&session, true);
    assert_eq!(
        snapshot(&mut state, "idle", 0).error.unwrap().kind,
        EngineErrorKind::QuotaExhausted
    );
    // Out of usage credits, or a model's limit with no reset: hard caps.
    for entry in [&real[2], &real[3], &real[5]] {
        let error = transcript_error(entry).unwrap();
        assert_eq!(error.kind, EngineErrorKind::QuotaExhausted, "{error:?}");
        assert!(
            error.message.starts_with(
                "claude: usage limit reached — buy usage credits at https://claude.ai/settings/usage, or wait for it to reset, then step_retry"
            ),
            "{}",
            error.message
        );
    }
    // An ordinary 429 stays transient with the fixed backoff.
    let error = transcript_error(&real[4]).unwrap();
    assert_eq!(error.kind, EngineErrorKind::Transient, "{error:?}");
    assert_eq!(error.retry_at, None);
    // A billing error is a hard cap whatever its text.
    let billing = text_entry("billing_error", "API Error: credit balance is too low");
    assert_eq!(
        transcript_error(&billing).unwrap().kind,
        EngineErrorKind::QuotaExhausted
    );
}

#[test]
fn claude_auth_failures_fail_as_auth_failed_from_their_category_or_login_wording() {
    let real = real_auth();
    let expected = [
        "claude: not logged in (token expired)",
        "claude: not logged in (token expired)",
        "claude: not logged in (no credentials)",
        "claude: not logged in (unauthorized)",
    ];
    for (entry, head) in real.iter().zip(expected) {
        let error = transcript_error(entry).unwrap();
        assert_eq!(error.kind, EngineErrorKind::AuthFailed, "{error:?}");
        assert_eq!(
            error.message,
            format!(
                "{head} — run `claude auth login` on this host (or /login inside `claude`), then step_retry. Claude said: {}",
                entry["message"]["content"][0]["text"].as_str().unwrap()
            )
        );
        assert_eq!(error.retry_at, None);
    }
    let error = transcript_error(&real[4]).unwrap();
    assert_eq!(error.kind, EngineErrorKind::AuthFailed);
    assert!(error.message.starts_with("claude: the organization disabled subscription access for Claude Code (oauth_org_not_allowed) — use an Anthropic API key or ask the org admin to enable access, then step_retry. Claude said: Your organization"), "{}", error.message);
    let error = transcript_error(&real[5]).unwrap();
    assert_eq!(error.kind, EngineErrorKind::AuthFailed);
    assert!(
        error.message.starts_with(
            "claude: account on hold (account_on_hold) — resolve the hold at https://claude.ai/restricted, then step_retry."
        ),
        "{}",
        error.message
    );
    // The StopFailure hook says the same when it comes first.
    let mut state = prompted();
    state
        .hook(&hook(
            "StopFailure",
            json!({"error":"authentication_failed","last_assistant_message":"OAuth token revoked \u{b7} Please run /login"}),
        ))
        .unwrap();
    let error = snapshot(&mut state, "idle", 0).error.unwrap();
    assert_eq!(error.kind, EngineErrorKind::AuthFailed);
    assert!(
        error
            .message
            .starts_with("claude: not logged in (token revoked) — run `claude auth login`")
    );
    // Without a category, Claude's login wording at the start of the error is enough.
    let mut state = prompted();
    state.transcript(&json!({"type":"assistant","isApiErrorMessage":true,"message":{"content":[{"type":"text","text":"Login expired \u{b7} Please run /login"}]}}), true);
    assert_eq!(
        snapshot(&mut state, "idle", 0).error.unwrap().kind,
        EngineErrorKind::AuthFailed
    );
    // Claude's own temporary authentication error stays transient.
    let flaky = text_entry(
        "authentication_failed",
        "Authentication error \u{b7} This may be a temporary network issue, please try again",
    );
    assert_eq!(
        transcript_error(&flaky).unwrap().kind,
        EngineErrorKind::Transient
    );
}

#[tokio::test]
async fn fake_usage_limit_is_quota_exhausted_a_plain_429_transient_and_quoted_words_nothing() {
    let weekly = "You've hit your weekly limit \u{b7} resets Oct 9, 1am (Europe/Berlin)";
    let mut h = Harness::new(json!({"turns":[{"error":weekly,"error_type":"rate_limit","quota":{"status":"rejected","rateLimitType":"seven_day","resets_in_s":259200,"overageStatus":"rejected"}}]})).await;
    h.launch(None).await;
    let error = h.turn(InputId::Task, "p", 1).await.error.unwrap();
    assert_eq!(error.kind, EngineErrorKind::QuotaExhausted, "{error:?}");
    assert!(
        error
            .message
            .starts_with("claude: weekly limit reached (seven_day); resets 20"),
        "{}",
        error.message
    );
    assert!(
        error.message.ends_with(&format!("Claude said: {weekly}")),
        "{}",
        error.message
    );
    h.cleanup().await;

    let mut h = Harness::new(json!({"turns":[{"error":"API Error: Request rejected (429) \u{b7} This request would exceed your account's rate limit. Please try again later.","error_type":"rate_limit"}]})).await;
    h.launch(None).await;
    let error = h.turn(InputId::Task, "p", 1).await.error.unwrap();
    assert_eq!(error.kind, EngineErrorKind::Transient, "{error:?}");
    assert_eq!(error.retry_at, None);
    h.cleanup().await;

    let mut h = Harness::new(json!({"turns":[{"tool":{"name":"Bash","input":{"command":"cat notes"}},"tool_error":weekly,"reply":weekly}]})).await;
    h.launch(None).await;
    let s = h.turn(InputId::Task, "p", 1).await;
    assert_eq!(s.error, None);
    assert_eq!(s.final_text, weekly);
    h.cleanup().await;
}
#[tokio::test]
async fn fake_auth_failure_is_auth_failed_with_any_key_masked() {
    let mut h = Harness::new(json!({"turns":[{"error":"Not logged in \u{b7} Please run /login","error_type":"authentication_failed"}]})).await;
    h.launch(None).await;
    let error = h.turn(InputId::Task, "p", 1).await.error.unwrap();
    assert_eq!(error.kind, EngineErrorKind::AuthFailed, "{error:?}");
    assert_eq!(
        error.message,
        "claude: not logged in (no credentials) — run `claude auth login` on this host (or /login inside `claude`), then step_retry. Claude said: Not logged in \u{b7} Please run /login"
    );
    h.cleanup().await;

    // A user-supplied key Claude echoes back is masked in the message.
    let key = "sk-ant-api03-Zq8xV4mN2pL7kR9tY3wE6uI1oP5aS0dF";
    let mut h = Harness::new(json!({"turns":[{"error":format!("Invalid API key \u{b7} Fix external API key \u{b7} x-api-key {key} was rejected"),"error_type":"invalid_request"}]})).await;
    h.launch(None).await;
    let error = h.turn(InputId::Task, "p", 1).await.error.unwrap();
    assert_eq!(error.kind, EngineErrorKind::AuthFailed, "{error:?}");
    assert!(!error.message.contains("Zq8xV4mN2pL7"), "{}", error.message);
    assert!(
        error.message.ends_with(
            "Claude said: Invalid API key \u{b7} Fix external API key \u{b7} x-api-key [redacted] was rejected"
        ),
        "{}",
        error.message
    );
    h.cleanup().await;
}
#[tokio::test]
async fn fake_login_screen_fails_as_auth_failed_before_any_input_is_taken() {
    let mut h = Harness::new(json!({"login":true})).await;
    h.launch(None).await;
    let started = Instant::now();
    let outcome = h
        .adapter
        .execute(
            &h.context,
            EngineCommand::DeliverText {
                id: InputId::Task,
                text: "p".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, DeliveryOutcome::Pending);
    let error = loop {
        let s = h.poll().await;
        if let Some(error) = s.error {
            assert!(s.acknowledged.is_empty());
            break error;
        }
        assert!(started.elapsed() < Duration::from_secs(10), "{s:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert_eq!(error.kind, EngineErrorKind::AuthFailed, "{error:?}");
    assert!(
        error.message.starts_with(
            "claude: not logged in (Claude shows its login screen) — run `claude auth login` on this host (or /login inside `claude`), then step_retry. Claude said: Select login method:"
        ),
        "{}",
        error.message
    );
    h.cleanup().await;
}

/// With no `CLAUDE_CONFIG_DIR` of the host's own, Claude is not told one: told the default
/// `~/.claude`, it reads `~/.claude/.claude.json` rather than the owner's `~/.claude.json`, opens
/// on its first-run setup and never starts a turn. A host that chose a directory still passes it.
#[tokio::test]
async fn claude_is_told_its_config_dir_only_when_the_host_chose_one() {
    let mut h = Harness::new(json!({})).await;
    let chosen = h.adapter.prepare(&h.context, None).await.unwrap().unwrap();
    assert!(chosen.env.contains_key("CLAUDE_CONFIG_DIR"));
    let mut h = Harness::new(json!({})).await;
    h.adapter = std::mem::replace(
        &mut h.adapter,
        Claude::new(
            "claude".into(),
            "/nowhere".into(),
            "/nowhere".into(),
            RunId::new(),
        ),
    )
    .with_config_dir_passed(false);
    let default = h.adapter.prepare(&h.context, None).await.unwrap().unwrap();
    assert!(
        !default.env.contains_key("CLAUDE_CONFIG_DIR"),
        "Claude must find the owner's ~/.claude.json: {:?}",
        default.env.keys().collect::<Vec<_>>()
    );
}

/// Claude Code 2.1.284's first-run theme picker, as a run whose config dir was wrong showed it.
const THEME_PICKER: &str = " Let's get started.\n\n Choose the text style that looks best with your terminal\n To change this later, run /theme\n\n \u{276f} 1. Auto (match terminal)\n   2. Dark mode \u{2714}\n   3. Light mode\n   4. Dark mode (colorblind-friendly)\n   5. Light mode (colorblind-friendly)\n   6. Dark mode (ANSI colors only)\n   7. Light mode (ANSI colors only)\n\n  1  function greet() {\n  2 -  console.log(\"Hello, World!\");\n  2 +  console.log(\"Hello, Claude!\");\n  3  }\n\n  Syntax highlighting enabled (ctrl+t to disable)\n";

#[test]
fn pane_text_kept_at_a_failure_is_masked_and_its_tail_trimmed() {
    use sluice_agents::engines::screen;
    let key = "sk-ant-api03-Zq8xV4mN2pL7kR9tY3wE6uI1oP5aS0dF";
    let pane = format!(
        "  Welcome back   \n\n  export ANTHROPIC_API_KEY={key}\n  Authorization: Bearer abc123def456ghi789\n\n a\n b\n c\n d\n   e   \n\n\n"
    );
    let kept = screen::evidence(&pane);
    assert!(!kept.contains("Zq8xV4mN2pL7"), "{kept}");
    assert!(!kept.contains("abc123def456"), "{kept}");
    assert!(
        kept.contains("export ANTHROPIC_API_KEY=[redacted]"),
        "{kept}"
    );
    assert!(kept.contains("Authorization: Bearer [redacted]"), "{kept}");
    assert!(kept.ends_with("   e\n"), "{kept:?}");
    assert_eq!(
        screen::tail(&kept),
        ["Authorization: Bearer [redacted]", "a", "b", "c", "d", "e"]
    );
    assert!(!screen::quote(&pane).contains("Zq8xV4mN2pL7"));
    assert!(screen::tail("\n  \n").is_empty());
}

async fn first_error(h: &mut Harness, within: Duration) -> (EngineError, Duration) {
    let started = Instant::now();
    let outcome = h
        .adapter
        .execute(
            &h.context,
            EngineCommand::DeliverText {
                id: InputId::Task,
                text: "p".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, DeliveryOutcome::Pending);
    loop {
        let s = h.poll().await;
        if let Some(error) = s.error {
            assert!(s.acknowledged.is_empty());
            return (error, started.elapsed());
        }
        assert!(started.elapsed() < within, "{s:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn fake_first_run_setup_fails_as_blocked_screen_at_once_with_its_text() {
    let mut h = Harness::new(json!({"screen":THEME_PICKER})).await;
    h.launch(None).await;
    let (error, took) = first_error(&mut h, Duration::from_secs(10)).await;
    assert_eq!(error.kind, EngineErrorKind::BlockedScreen, "{error:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert!(
        error.message.starts_with(
            "claude: blocked on its first-run setup (theme picker) — Claude Code's first-run setup is unfinished for the account it runs as: run `claude` once on this host as that user and finish it (and if the host sets CLAUDE_CONFIG_DIR, check that it is the configured one), then step_retry. Claude showed: Let's get started. | Choose the text style that looks best with your terminal |"
        ),
        "{}",
        error.message
    );
    assert!(!h.context.run_dir.join("fixture-prompts.jsonl").exists());
    h.cleanup().await;
}

#[tokio::test]
async fn fake_trust_and_bypass_dialogs_are_still_answered_before_the_turn() {
    let mut h = Harness::new(json!({"trust":true,"bypass":true,"turns":[{"reply":"ok"}]})).await;
    h.launch(None).await;
    let s = h.turn(InputId::Task, "p", 1).await;
    assert_eq!(s.error, None);
    assert_eq!(s.final_text, "ok");
    assert_eq!(h.prompts(), vec!["p"]);
    assert_eq!(
        fs::read_to_string(h.context.run_dir.join("fixture-dialogs.jsonl")).unwrap(),
        "\"trust\"\n\"bypass\"\n"
    );
    h.cleanup().await;
}

#[tokio::test]
async fn fake_unrecognized_screen_fails_as_blocked_screen_after_the_grace() {
    let mut h = Harness::new(
        json!({"screen":" Something new is here.\n\n Press Enter to continue\u{2026}"}),
    )
    .await;
    h.adapter = std::mem::replace(
        &mut h.adapter,
        Claude::new(
            "claude".into(),
            "/nowhere".into(),
            "/nowhere".into(),
            RunId::new(),
        ),
    )
    .with_screen_grace(Duration::from_secs(2));
    h.launch(None).await;
    let (error, took) = first_error(&mut h, Duration::from_secs(10)).await;
    assert_eq!(error.kind, EngineErrorKind::BlockedScreen, "{error:?}");
    assert!(took >= Duration::from_secs(2), "{took:?}");
    assert_eq!(
        error.message,
        "claude: blocked on a screen sluice does not recognize, unchanged for 2s before any turn — run `claude` once in the step's cwd on this host and answer it (or attach to the run's private pane while it waits), then step_retry. Claude showed: Something new is here. | Press Enter to continue\u{2026}"
    );
    h.cleanup().await;
}

/// A supervised Claude on the fixture's `config`, its limits short but a dialog nudge never due.
async fn supervise_claude(
    h: &Harness,
    configure: impl FnOnce(&mut sluice_agents::supervisor::SupervisorConfig),
) -> (
    Result<sluice_agents::supervisor::AgentResult, sluice_agents::supervisor::AgentFailure>,
    PathBuf,
) {
    let hook = h.scratch.0.join("journal-hook");
    executable::write(&hook, include_str!("acceptance/hook.py"));
    let fixture = built("fixture").canonicalize().unwrap();
    let wrapper = h.scratch.0.join("supervised-claude");
    executable::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexport SLUICE_HOME={}\nexport HOME={}\nexport SLUICE_FAKE_CLAUDE={}\nexec {} claude \"$@\"\n",
            protocol::shell_quote(&h.scratch.0.to_string_lossy()),
            protocol::shell_quote(&h.scratch.0.join("home").to_string_lossy()),
            protocol::shell_quote(&h.scratch.0.join("config.json").to_string_lossy()),
            protocol::shell_quote(&fixture.to_string_lossy())
        ),
    );
    let mut cfg = acceptance::config(&h.scratch.0, "claude");
    cfg.limits.wall = Duration::from_secs(15);
    cfg.limits.ready = Duration::from_secs(5);
    cfg.limits.turn_start = Duration::from_secs(2);
    cfg.limits.stall = Duration::from_secs(10);
    cfg.limits.dialog = Duration::from_secs(60);
    configure(&mut cfg);
    let mut adapter = Claude::new(wrapper, h.scratch.0.join("claude"), hook, cfg.run);
    let directory = cfg.run_dir.clone();
    let mut host = acceptance::Host {
        directory: Some(directory.clone()),
        ..Default::default()
    };
    let result = sluice_agents::supervisor::supervise(
        cfg,
        &mut adapter,
        &mut host,
        &mut Default::default(),
        Some(&h.tmux),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await;
    (result, directory)
}

/// The original failure: Claude sat on a screen the adapter did not know and the run failed
/// `TurnStartTimeout` with nothing to say what was on it. The pane is kept before teardown.
#[tokio::test]
async fn supervisor_claude_turn_start_timeout_keeps_the_pane_at_failure() {
    let key = "sk-ant-api03-Zq8xV4mN2pL7kR9tY3wE6uI1oP5aS0dF";
    let h = Harness::new(json!({"screen":format!(" Welcome back!\n\n Your key: {key}\n\n Press Enter to continue\u{2026}")})).await;
    let (result, directory) = supervise_claude(&h, |_| {}).await;
    let error = result.unwrap_err();
    assert_eq!(
        error.kind,
        sluice_agents::supervisor::FailureKind::TurnStartTimeout,
        "{error}"
    );
    let path = fs::canonicalize(&directory)
        .unwrap()
        .join("pane-at-failure.txt");
    assert!(
        error.message.ends_with(&format!(
            "refusing blind replay\npane at failure (last rows; whole screen: {}):\n  Welcome back!\n  Your key: [redacted]\n  Press Enter to continue\u{2026}",
            path.display()
        )),
        "{}",
        error.message
    );
    let kept = fs::read_to_string(&path).unwrap();
    assert!(kept.contains(" Your key: [redacted]\n"), "{kept}");
    assert!(!kept.contains("Zq8xV4mN2pL7"), "{kept}");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!h.client(&["list-sessions"]).status.success());
}

/// Claude prints a required update as it exits; the dead pane stays until teardown, so the run
/// fails typed, quoting it, and keeps the pane.
#[tokio::test]
async fn supervisor_claude_required_update_at_exit_fails_as_blocked_screen() {
    let h = Harness::new(json!({"exit_at_start":1,"stderr":"It looks like your version of Claude Code (2.1.284) needs an update.\r\nA newer version (2.2.0 or higher) is required to continue."})).await;
    let (result, directory) = supervise_claude(&h, |_| {}).await;
    let error = result.unwrap_err();
    assert_eq!(
        error.kind,
        sluice_agents::supervisor::FailureKind::BlockedScreen,
        "{error}"
    );
    assert!(
        error.message.starts_with(
            "claude: blocked on a required update (this Claude Code is older than the version it now requires) — update Claude Code on this host (`claude update`; sluice runs the newer version untested), then step_retry. Claude showed: It looks like your version of Claude Code (2.1.284) needs an update. | A newer version (2.2.0 or higher) is required to continue."
        ),
        "{}",
        error.message
    );
    let path = fs::canonicalize(&directory)
        .unwrap()
        .join("pane-at-failure.txt");
    assert!(
        error
            .message
            .ends_with(&format!("\npane at failure: {}", path.display())),
        "{}",
        error.message
    );
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("needs an update.")
    );
}
