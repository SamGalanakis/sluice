use serde_json::{Value, json};
use sluice_agents::engines::{
    DeliveryOutcome, EngineAdapter, EngineCommand, EngineContext, EngineErrorKind, EngineLaunch,
    EngineObservation, EngineStatus, HookEvent, InputId,
    devin::{Devin, DevinOptions, profile, protocol},
};
use sluice_agents::supervisor::{
    Checkpoint, HostSnapshot, Limits, RetryPolicy, State, SupervisorConfig, SupervisorHost,
};
use sluice_model::ids::{AttemptId, InvocationId, MessageId, RunId};
use sluice_process::socket::{AssignedRange, DeliveryMessage};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[path = "../../../tests/support/executable.rs"]
mod executable;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "sluice-test-devin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
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
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn options(root: &Scratch, script: Value) -> DevinOptions {
    let fixture = built("fixture");
    assert!(
        fixture.is_file(),
        "build the workspace binaries before the Devin executable tests"
    );
    let wrapper = root.join("devin");
    executable::write(
        &wrapper,
        format!(
            "#!/bin/sh\nexec {} devin \"$@\"\n",
            protocol::shell_quote(&fixture.to_string_lossy())
        ),
    );
    let mut script = script;
    script["prompts"] = root
        .join("prompts.jsonl")
        .to_string_lossy()
        .into_owned()
        .into();
    protocol::private_write(
        &root.join("fixture.json"),
        &serde_json::to_vec(&script).unwrap(),
    )
    .unwrap();
    DevinOptions {
        binary: wrapper,
        hook_binary: fixture,
        config: root.join("owner-config.json"),
        data_home: root.join("data"),
        environment: BTreeMap::from([
            (
                "SLUICE_HOME".into(),
                root.join("home").to_string_lossy().into_owned(),
            ),
            (
                "XDG_DATA_HOME".into(),
                root.join("data").to_string_lossy().into_owned(),
            ),
            (
                "FAKE_DEVIN".into(),
                root.join("fixture.json").to_string_lossy().into_owned(),
            ),
        ]),
        ready_timeout: Duration::from_secs(5),
        delivery_timeout: Duration::from_secs(4),
    }
}
fn context(root: &Scratch, name: &str, tmux: bool) -> EngineContext {
    let run = root.join(name);
    fs::create_dir(&run).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    EngineContext {
        run_dir: run,
        cwd,
        model: None,
        effort: None,
        tmux_binary: tmux.then(|| workspace().join("target/private-tmux/bin/tmux")),
    }
}
fn event(adapter: &mut Devin, payload: Value) {
    let event = payload["hook_event_name"].as_str().unwrap().into();
    assert_eq!(
        adapter
            .on_hook(HookEvent { event, payload })
            .unwrap()
            .exit_code,
        0
    );
}

struct Pane {
    run: PathBuf,
    binary: PathBuf,
    server: Child,
}
impl Pane {
    async fn start(context: &EngineContext, launch: EngineLaunch) -> Self {
        let artifact =
            sluice_process::tmux::ApprovedTmux::load(&workspace().join("target/private-tmux"))
                .await
                .unwrap();
        assert_eq!(Some(artifact.binary()), context.tmux_binary.as_deref());
        let server = artifact
            .server_command(&context.run_dir, None)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut pane = Self {
            run: context.run_dir.clone(),
            binary: artifact.binary().into(),
            server,
        };
        let start = Instant::now();
        while !context.run_dir.join("tmux.sock").exists() {
            assert!(pane.server.try_wait().unwrap().is_none());
            assert!(start.elapsed() < Duration::from_secs(5));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            pane.cmd(&["set-option", "-g", "remain-on-exit", "on"])
                .success()
        );
        let mut shell = format!(
            "cd {} && exec /usr/bin/env",
            protocol::shell_quote(&context.cwd.to_string_lossy())
        );
        for (k, v) in launch.env {
            shell.push_str(&format!(" {}", protocol::shell_quote(&format!("{k}={v}"))));
        }
        for arg in launch.argv {
            shell.push_str(&format!(" {}", protocol::shell_quote(&arg)));
        }
        assert!(
            pane.cmd(&[
                "new-session",
                "-d",
                "-s",
                "sluice-test-devin",
                "-x",
                "140",
                "-y",
                "40",
                &shell
            ])
            .success()
        );
        pane
    }
    fn cmd(&self, args: &[&str]) -> std::process::ExitStatus {
        Command::new(&self.binary)
            .current_dir(&self.run)
            .args(["-S", "tmux.sock", "-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
    }
    fn pid(&self) -> u32 {
        let out = Command::new(&self.binary)
            .current_dir(&self.run)
            .args([
                "-S",
                "tmux.sock",
                "display-message",
                "-p",
                "-t",
                "%0",
                "#{pane_pid}",
            ])
            .output()
            .unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    fn close(&mut self) {
        self.cmd(&["kill-server"]);
        let _ = self.server.wait();
    }
}
impl Drop for Pane {
    fn drop(&mut self) {
        self.close();
    }
}
async fn poll(
    adapter: &mut Devin,
    ctx: &EngineContext,
    done: impl Fn(&EngineObservation) -> bool,
) -> EngineObservation {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let obs = adapter.observe(ctx).await.unwrap();
        if done(&obs) {
            return obs;
        }
        assert!(Instant::now() < deadline, "timed out: {obs:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
async fn deliver(adapter: &mut Devin, ctx: &EngineContext, id: InputId, text: &str) {
    assert_eq!(
        adapter
            .execute(
                ctx,
                EngineCommand::DeliverText {
                    id: id.clone(),
                    text: text.into()
                }
            )
            .await
            .unwrap(),
        DeliveryOutcome::Pending
    );
    poll(adapter, ctx, |o| o.acknowledged.contains(&id)).await;
}

#[tokio::test]
async fn devin_session_start_reports_readiness_before_any_input() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(&root, json!({})));
    let ctx = context(&root, "run", false);
    adapter.prepare(&ctx, None).await.unwrap();
    adapter
        .execute(&ctx, EngineCommand::StartFresh)
        .await
        .unwrap();
    assert_eq!(
        adapter.observe(&ctx).await.unwrap().status,
        EngineStatus::Starting
    );

    let start = json!({"hook_event_name":"SessionStart","session_id":"ready-session"});
    event(&mut adapter, start.clone());
    let ready = adapter.observe(&ctx).await.unwrap();
    assert_eq!(ready.status, EngineStatus::Idle);
    assert_eq!(ready.session_id.as_deref(), Some("ready-session"));
    assert_eq!((ready.turns_started, ready.turns_completed), (0, 0));
    assert!(ready.acknowledged.is_empty());
    assert!(ready.not_accepted.is_empty());

    event(
        &mut adapter,
        json!({"hook_event_name":"UserPromptSubmit","prompt_id":"0"}),
    );
    event(&mut adapter, start.clone());
    let busy = adapter.observe(&ctx).await.unwrap();
    assert_eq!(busy.status, EngineStatus::Busy);
    assert_eq!((busy.turns_started, busy.turns_completed), (1, 0));
    event(&mut adapter, json!({"hook_event_name":"SessionEnd"}));
    event(&mut adapter, start);
    assert_eq!(
        adapter.observe(&ctx).await.unwrap().status,
        EngineStatus::Exited
    );
    adapter.close().await.unwrap();
}

#[tokio::test]
async fn devin_visible_composer_reports_readiness_without_session_start() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(
        &root,
        json!({"omit_session_start":true,"boot_ms":100}),
    ));
    let ctx = context(&root, "run", true);
    let launch = adapter.prepare(&ctx, None).await.unwrap().unwrap();
    let mut pane = Pane::start(&ctx, launch).await;
    let pid = pane.pid();
    adapter
        .execute(&ctx, EngineCommand::StartFresh)
        .await
        .unwrap();
    let ready = poll(&mut adapter, &ctx, |o| o.status == EngineStatus::Idle).await;
    assert_eq!((ready.turns_started, ready.turns_completed), (0, 0));
    assert!(ready.acknowledged.is_empty());
    assert!(ready.session_id.is_none());
    assert!(!root.join("prompts.jsonl").exists());
    assert!(
        fs::read(ctx.run_dir.join("devin-hooks.jsonl"))
            .unwrap()
            .is_empty()
    );

    event(
        &mut adapter,
        json!({"hook_event_name":"UserPromptSubmit","prompt_id":"0"}),
    );
    assert_eq!(
        adapter.observe(&ctx).await.unwrap().status,
        EngineStatus::Busy
    );
    event(&mut adapter, json!({"hook_event_name":"SessionEnd"}));
    assert_eq!(
        adapter.observe(&ctx).await.unwrap().status,
        EngineStatus::Exited
    );
    adapter
        .execute(&ctx, EngineCommand::RequestExit)
        .await
        .unwrap();
    poll(&mut adapter, &ctx, |o| {
        o.status == EngineStatus::Exited && !Path::new(&format!("/proc/{pid}")).exists()
    })
    .await;
    adapter.close().await.unwrap();
    pane.close();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}

#[derive(Default)]
struct DevinHost {
    directory: PathBuf,
    submissions: BTreeMap<String, Value>,
    messages: Vec<DeliveryMessage>,
    acks: Vec<MessageId>,
    cleanups: u32,
}
impl SupervisorHost for DevinHost {
    async fn snapshot(&mut self, after: MessageId) -> io::Result<HostSnapshot> {
        match fs::read(self.directory.join("submission.json")) {
            Ok(bytes) => self.submissions = serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(HostSnapshot {
            submissions: self.submissions.clone(),
            messages: self
                .messages
                .iter()
                .filter(|m| m.id > after)
                .cloned()
                .collect(),
            ..HostSnapshot::default()
        })
    }
    async fn acknowledge(&mut self, ids: &[MessageId]) -> io::Result<()> {
        for id in ids {
            if !self.acks.contains(id) {
                self.acks.push(*id);
            }
        }
        Ok(())
    }
    async fn me(&mut self) -> io::Result<String> {
        Ok("current scratch run context".into())
    }
    async fn note(&mut self, _body: &str) -> io::Result<()> {
        Ok(())
    }
    async fn checkpoint(&mut self, _checkpoint: &Checkpoint) -> io::Result<()> {
        Ok(())
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        self.cleanups += 1;
        Ok(())
    }
}
fn supervisor_config(root: &Scratch) -> SupervisorConfig {
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    SupervisorConfig {
        run: RunId::new(),
        attempt: AttemptId::new(),
        invocation: InvocationId::new(),
        project: "readiness".into(),
        home: root.0.clone(),
        run_dir: root.join("run"),
        cwd,
        engine: "devin".into(),
        task: "Submit the declared word blue and finish.".into(),
        required: vec!["word".into()],
        session: None,
        previous: None,
        assigned: AssignedRange {
            after: MessageId(0),
            through: MessageId(0),
        },
        messages: vec![],
        model: None,
        effort: None,
        limits: Limits {
            wall: Duration::from_secs(15),
            ready: Duration::from_secs(5),
            turn_start: Duration::from_secs(5),
            stall: Duration::from_secs(10),
            settle: Duration::from_millis(100),
            ..Limits::test_profile()
        },
        retry: RetryPolicy::agent(),
        internal_attempt: 1,
    }
}
fn supervisor_message(id: i64) -> DeliveryMessage {
    DeliveryMessage {
        id: MessageId(id),
        body: sluice_model::rpc::JsonValue::try_from(
            json!({"body":"Keep word blue and finish after feedback."}),
        )
        .unwrap(),
    }
}

#[tokio::test]
async fn supervisor_devin_fresh_task_delivered_once_submission_and_finish() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(
        &root,
        json!({"boot_ms":100,"turns":[{"reply":"done","submit":{"word":"blue"}}]}),
    ));
    let tmux = sluice_process::tmux::ApprovedTmux::load(&workspace().join("target/private-tmux"))
        .await
        .unwrap();
    let cfg = supervisor_config(&root);
    let dir = cfg.run_dir.clone();
    let mut host = DevinHost {
        directory: cfg.cwd.clone(),
        ..Default::default()
    };
    let result = sluice_agents::supervisor::supervise(
        cfg,
        &mut adapter,
        &mut host,
        &mut Default::default(),
        Some(&tmux),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.final_text, "done");
    assert_eq!(result.session, "fixture-devin-session");
    assert_eq!(host.submissions["word"], json!("blue"));
    assert_eq!(host.cleanups, 1);
    let checkpoint = Checkpoint::read(&dir).unwrap().unwrap();
    assert_eq!(checkpoint.state, State::Done);
    assert_eq!(checkpoint.delivery.entries.len(), 1);
    let task = &checkpoint.delivery.entries[0];
    assert_eq!(task.id, InputId::Task);
    assert_eq!(task.tries, 1);
    assert_eq!(
        task.state,
        sluice_agents::delivery::DeliveryState::Acknowledged
    );
    let prompts: Vec<Value> = fs::read_to_string(root.join("prompts.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["text"].as_str().unwrap().trim_end(), task.text);
    assert!(
        !tmux
            .client_command(&dir)
            .unwrap()
            .arg("list-sessions")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[test]
fn devin_model_profile_and_composer_match_the_python_contract() {
    let profile = profile::profile();
    for name in ["high", "swe-2-high"] {
        assert_eq!(profile.models[name], "swe-2-high");
    }
    for name in ["fusion", profile::FUSION] {
        assert_eq!(profile.models[name], profile::FUSION);
    }
    assert!(
        profile
            .validate_selection(Some("swe-2-medium"), None)
            .is_err()
    );
    assert!(profile.validate_selection(None, Some("high")).is_err());
    assert!(!profile.reports_waiting);
    let pane = include_str!("fixtures/devin/composer.txt");
    assert!(protocol::composer_ready(pane));
    assert!(protocol::draft_visible(pane, "Your task is in"));
    assert!(!protocol::draft_visible(pane, "old answer"));
    assert_eq!(
        protocol::paste_payload("a\r\nb\t\x1b[201~\\"),
        b"a\rb\t[201~\\\r"
    );
}

#[tokio::test]
async fn devin_private_jsonc_config_preserves_settings_and_pins_fusion() {
    let root = Scratch::new();
    let opts = options(&root, json!({}));
    let raw = "// owner\n{\"theme_mode\":\"dark\",\"agent\":{\"model\":\"old\"},\"permissions\":{\"allow\":[\"read\"]},\"link\":\"https://example.org/a//b\",\"hooks\":{\"SessionStart\":[{\"hooks\":[{\"type\":\"command\",\"command\":\"true\"}]}]}} /* end */";
    fs::write(&opts.config, raw).unwrap();
    let mut adapter = Devin::new(opts.clone());
    let mut ctx = context(&root, "run", false);
    ctx.model = Some("fusion".into());
    let launch = adapter.prepare(&ctx, None).await.unwrap().unwrap();
    let cfg: Value =
        serde_json::from_slice(&fs::read(ctx.run_dir.join("devin-config.json")).unwrap()).unwrap();
    assert_eq!(cfg["agent"]["model"], profile::FUSION);
    assert_eq!(cfg["theme_mode"], "dark");
    assert_eq!(cfg["permissions"], json!({"allow":["read"]}));
    assert_eq!(cfg["link"], "https://example.org/a//b");
    assert_eq!(cfg["hooks"]["SessionStart"].as_array().unwrap().len(), 2);
    assert_eq!(fs::read_to_string(&opts.config).unwrap(), raw);
    assert_eq!(
        fs::metadata(ctx.run_dir.join("devin-config.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        launch
            .argv
            .windows(2)
            .any(|w| w == ["--model", profile::FUSION])
    );
    assert_eq!(
        &launch.argv[launch.argv.len() - 4..],
        [
            "--permission-mode",
            "dangerous",
            "--respect-workspace-trust",
            "false"
        ]
    );
    assert!(launch.argv.windows(2).any(|w| w == ["-u", "CLAUDECODE"]));
    assert!(launch.argv.windows(2).any(|w| w == ["-u", "PYTHONPATH"]));
    assert!(launch.argv.windows(2).any(|w| w == ["-u", "VIRTUAL_ENV"]));
    let command = cfg["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains("agent-hook devin Stop"));
    assert!(!command.contains("cat >>"));
    let children: Vec<_> = (0..12)
        .map(|_| {
            let mut child = Command::new("/bin/sh")
                .args(["-c", command])
                .envs(&launch.env)
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(b"{\"hook_event_name\":\"Stop\"}")
                .unwrap();
            child
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let journal = fs::read_to_string(ctx.run_dir.join("devin-hooks.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 12);
    for line in journal.lines() {
        serde_json::from_str::<protocol::JournalEntry>(line).unwrap();
    }
}

#[tokio::test]
async fn devin_hook_stream_tracks_turns_errors_compaction_and_artifacts() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(&root, json!({})));
    let ctx = context(&root, "run", false);
    adapter.prepare(&ctx, None).await.unwrap();
    for line in include_str!("fixtures/devin/hooks.jsonl").lines().take(5) {
        event(&mut adapter, serde_json::from_str(line).unwrap());
    }
    let busy = adapter.observe(&ctx).await.unwrap();
    assert_eq!(busy.status, EngineStatus::Busy);
    assert_eq!(busy.turns_completed, 0);
    assert_eq!(busy.compactions, 1);
    let stop = json!({"hook_event_name":"Stop","session_id":"captured-redacted","prompt_id":"0","last_assistant_message":"Landed a529f1c; 1529 tests pass"});
    event(&mut adapter, stop.clone());
    event(&mut adapter, stop);
    let idle = adapter.observe(&ctx).await.unwrap();
    assert_eq!(idle.turns_completed, 1);
    assert!(idle.error.is_none());
    assert_eq!(idle.status, EngineStatus::Idle);
    event(
        &mut adapter,
        json!({"hook_event_name":"UserPromptSubmit","prompt_id":"1"}),
    );
    event(
        &mut adapter,
        json!({"hook_event_name":"Stop","prompt_id":"1","error":"HTTP status 529"}),
    );
    assert_eq!(
        adapter.observe(&ctx).await.unwrap().error.unwrap().kind,
        EngineErrorKind::Transient
    );
    event(
        &mut adapter,
        json!({"hook_event_name":"UserPromptSubmit","prompt_id":"2"}),
    );
    assert!(adapter.observe(&ctx).await.unwrap().error.is_none());
    // A late stop for an older prompt cannot mark the new turn idle.
    event(
        &mut adapter,
        json!({"hook_event_name":"Stop","prompt_id":"1"}),
    );
    assert_eq!(
        adapter.observe(&ctx).await.unwrap().status,
        EngineStatus::Busy
    );
    adapter.close().await.unwrap();
    adapter.close().await.unwrap();
    assert_eq!(
        fs::read_to_string(ctx.run_dir.join("devin.log.session")).unwrap(),
        "captured-redacted\n"
    );
    assert!(
        fs::read_to_string(ctx.run_dir.join("devin.log"))
            .unwrap()
            .contains("tool exec echo done")
    );
    Devin::default().close().await.unwrap();
}

#[tokio::test]
async fn devin_export_and_session_end_are_not_completed_turns() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(&root, json!({})));
    let ctx = context(&root, "run", false);
    adapter.prepare(&ctx, None).await.unwrap();
    fs::write(ctx.run_dir.join("devin.json"), "{partial").unwrap();
    assert_eq!(adapter.observe(&ctx).await.unwrap().turns_completed, 0);
    fs::write(
        ctx.run_dir.join("devin.json"),
        r#"{"session_id":"export-session","steps":[{"source":"assistant","message":"history"}]}"#,
    )
    .unwrap();
    event(
        &mut adapter,
        json!({"hook_event_name":"SessionEnd","session_id":"export-session"}),
    );
    let obs = adapter.observe(&ctx).await.unwrap();
    assert_eq!(obs.status, EngineStatus::Exited);
    assert_eq!(obs.turns_completed, 0);
    assert_eq!(obs.final_text, "history");
    adapter.close().await.unwrap();
    assert!(ctx.run_dir.join("devin.log.json").exists());
}

#[tokio::test]
async fn devin_resume_validates_read_only_metadata_cwd_and_session() {
    let root = Scratch::new();
    let opts = options(&root, json!({}));
    let ctx = context(&root, "run", false);
    fs::create_dir_all(opts.data_home.join("devin/cli")).unwrap();
    let db = opts.data_home.join("devin/cli/sessions.db");
    let sql = format!(
        "CREATE TABLE sessions(id TEXT, working_directory TEXT); INSERT INTO sessions VALUES('s1','{}');",
        ctx.cwd.display()
    );
    assert!(
        Command::new("sqlite3")
            .arg(&db)
            .arg(sql)
            .status()
            .unwrap()
            .success()
    );
    let before = fs::read(&db).unwrap();
    let mut adapter = Devin::new(opts);
    assert_eq!(adapter.session("s1").await.unwrap().unwrap().cwd, ctx.cwd);
    assert!(adapter.session("missing").await.unwrap().is_none());
    assert!(adapter.session("s1' OR 1=1 --").await.unwrap().is_none());
    let alias = root.join("prior.log");
    fs::write(root.join("prior.log.session"), "s1\n").unwrap();
    assert_eq!(
        adapter
            .session(alias.to_str().unwrap())
            .await
            .unwrap()
            .unwrap()
            .id,
        "s1"
    );
    let launch = adapter.prepare(&ctx, Some("s1")).await.unwrap().unwrap();
    assert_eq!(&launch.argv[launch.argv.len() - 2..], ["--resume", "s1"]);
    assert_eq!(adapter.observe(&ctx).await.unwrap().turns_completed, 0);
    let mut wrong = ctx.clone();
    wrong.cwd = root.0.clone();
    assert_eq!(
        adapter.prepare(&wrong, Some("s1")).await.unwrap_err().kind,
        EngineErrorKind::CapabilityMismatch
    );
    assert_eq!(fs::read(&db).unwrap(), before);
    assert!(
        Command::new("sqlite3")
            .arg(&db)
            .arg("PRAGMA user_version=1")
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        adapter.session("s1").await.unwrap_err().kind,
        EngineErrorKind::CapabilityMismatch
    );
}

#[tokio::test]
async fn devin_unknown_version_and_bad_config_fail_before_delivery() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(&root, json!({"version":"3001.0.0"})));
    let ctx = context(&root, "run", false);
    assert_eq!(
        adapter.prepare(&ctx, None).await.unwrap_err().kind,
        EngineErrorKind::CapabilityMismatch
    );
    assert!(!ctx.run_dir.join("devin-config.json").exists());
    assert!(!root.join("prompts.jsonl").exists());
    for raw in [
        "[]",
        "{\"agent\":[]}",
        "{\"hooks\":[]}",
        "{\"hooks\":{\"Stop\":{}}}",
        "/* unterminated",
    ] {
        assert!(
            protocol::config(
                raw,
                "swe-2-high",
                Path::new("sluice"),
                Path::new("journal"),
                "inv"
            )
            .is_err()
        );
    }
    assert!(protocol::decode_hook(b"{}", "Stop").is_err());
    assert!(protocol::decode_hook(br#"{"hook_event_name":"Stop"}"#, "SessionEnd").is_err());
}

#[tokio::test]
async fn devin_fake_tui_fresh_required_submit_live_message_compaction_and_cleanup() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(
        &root,
        json!({"boot_ms":100,"drop_enters":1,"drop_exit_enter":true,"wrap":14,"turns":[{"reply":"blue","submit":{"word":"blue"},"tool":true,"compact":true},{"reply":"addressed"},{"reply":"reprimed"}]}),
    ));
    let ctx = context(&root, "run", true);
    let launch = adapter.prepare(&ctx, None).await.unwrap().unwrap();
    let mut pane = Pane::start(&ctx, launch).await;
    let engine_pid = pane.pid();
    adapter
        .execute(&ctx, EngineCommand::StartFresh)
        .await
        .unwrap();
    deliver(
        &mut adapter,
        &ctx,
        InputId::Task,
        "Submit word blue.\nMultiline task ending with slash\\",
    )
    .await;
    let first = poll(&mut adapter, &ctx, |o| o.turns_completed == 1).await;
    assert_eq!(first.compactions, 1);
    assert_eq!(first.final_text, "blue");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(ctx.cwd.join("submission.json")).unwrap())
            .unwrap(),
        json!({"word":"blue"})
    );
    let id = InputId::Message {
        id: sluice_model::ids::MessageId(1),
    };
    adapter
        .execute(
            &ctx,
            EngineCommand::Steer {
                id: id.clone(),
                text: "Addressed live message".into(),
            },
        )
        .await
        .unwrap();
    poll(&mut adapter, &ctx, |o| {
        o.acknowledged.contains(&id) && o.turns_completed == 2
    })
    .await;
    deliver(
        &mut adapter,
        &ctx,
        InputId::Reprime { ordinal: 1 },
        "Reprime current me snapshot",
    )
    .await;
    poll(&mut adapter, &ctx, |o| o.turns_completed == 3).await;
    assert_eq!(
        adapter
            .execute(
                &ctx,
                EngineCommand::DeliverText {
                    id: InputId::Task,
                    text: "never replay".into()
                }
            )
            .await
            .unwrap(),
        DeliveryOutcome::Acknowledged
    );
    assert_eq!(
        fs::read_to_string(root.join("prompts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    adapter
        .execute(&ctx, EngineCommand::RequestExit)
        .await
        .unwrap();
    poll(&mut adapter, &ctx, |o| o.status == EngineStatus::Exited).await;
    adapter.close().await.unwrap();
    pane.close();
    assert!(!Path::new(&format!("/proc/{engine_pid}")).exists());
}

#[tokio::test]
async fn devin_collapsed_large_paste_and_missing_hook_do_not_duplicate_input() {
    let root = Scratch::new();
    let mut opts = options(&root, json!({"turns":[{"omit_ack":true}]}));
    opts.delivery_timeout = Duration::from_millis(600);
    let mut adapter = Devin::new(opts);
    let ctx = context(&root, "run", true);
    let launch = adapter.prepare(&ctx, None).await.unwrap().unwrap();
    let _pane = Pane::start(&ctx, launch).await;
    let text = format!("Task {}\nline2\nline3\nline4", "x".repeat(18000));
    adapter
        .execute(
            &ctx,
            EngineCommand::DeliverText {
                id: InputId::Task,
                text: text.clone(),
            },
        )
        .await
        .unwrap();
    let obs = poll(&mut adapter, &ctx, |o| {
        o.error
            .as_ref()
            .is_some_and(|e| e.kind == EngineErrorKind::UnknownAcceptance)
    })
    .await;
    assert!(obs.acknowledged.is_empty());
    assert!(obs.not_accepted.is_empty());
    assert_eq!(
        adapter
            .execute(
                &ctx,
                EngineCommand::DeliverText {
                    id: InputId::Task,
                    text
                }
            )
            .await
            .unwrap(),
        DeliveryOutcome::Pending
    );
    let prompts = fs::read_to_string(root.join("prompts.jsonl")).unwrap();
    assert_eq!(prompts.lines().count(), 1);
}

#[tokio::test]
async fn devin_transient_resume_keeps_session_and_new_invocation_counters() {
    let root = Scratch::new();
    let opts = options(
        &root,
        json!({"turns":[{"reply":"committed","commit":"first","error":"HTTP status 529"}]}),
    );
    let ctx = context(&root, "run", true);
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["config", "user.name", "Fixture"],
        vec!["commit", "--allow-empty", "-qm", "baseline"],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&ctx.cwd)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let baseline = Command::new("git")
        .current_dir(&ctx.cwd)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap()
        .stdout;
    let mut adapter = Devin::new(opts.clone());
    let launch = adapter.prepare(&ctx, None).await.unwrap().unwrap();
    let mut pane = Pane::start(&ctx, launch).await;
    deliver(&mut adapter, &ctx, InputId::Task, "Commit first").await;
    let obs = poll(&mut adapter, &ctx, |o| o.error.is_some()).await;
    let session = obs.session_id.unwrap();
    assert_eq!(obs.error.unwrap().kind, EngineErrorKind::Transient);
    pane.close();
    adapter.close().await.unwrap();
    let mut script: Value =
        serde_json::from_slice(&fs::read(root.join("fixture.json")).unwrap()).unwrap();
    script["turns"] = json!([{"reply":"continued","commit":"second"}]);
    fs::write(
        root.join("fixture.json"),
        serde_json::to_vec(&script).unwrap(),
    )
    .unwrap();
    let resumed = context(&root, "resume", true);
    let launch = adapter
        .prepare(&resumed, Some(&session))
        .await
        .unwrap()
        .unwrap();
    let _pane = Pane::start(&resumed, launch).await;
    adapter
        .execute(
            &resumed,
            EngineCommand::Resume {
                session: session.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(adapter.observe(&resumed).await.unwrap().turns_completed, 0);
    deliver(
        &mut adapter,
        &resumed,
        InputId::Continue { attempt: 1 },
        "Continue after transient",
    )
    .await;
    let obs = poll(&mut adapter, &resumed, |o| o.turns_completed == 1).await;
    assert_eq!(obs.session_id.as_deref(), Some(session.as_str()));
    let range = format!("{}..HEAD", String::from_utf8(baseline).unwrap().trim());
    let count = Command::new("git")
        .current_dir(&ctx.cwd)
        .args(["rev-list", "--count", &range])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(count.stdout).unwrap().trim(), "2");
    assert_eq!(
        fs::read_to_string(root.join("prompts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    adapter.close().await.unwrap();
}

#[tokio::test]
async fn devin_exit_probe_drains_terminal_hooks_before_reporting_exit() {
    for (stop, on_client) in [(false, false), (true, false), (false, true), (true, true)] {
        let root = Scratch::new();
        let opts = options(&root, json!({}));
        let mut ctx = context(&root, "exit-probe", false);
        let tmux = root.join("probe-tmux");
        executable::write(&tmux, include_str!("fixtures/devin/exit-during-probe.py"));
        ctx.tmux_binary = Some(tmux);
        let mut adapter = Devin::new(opts);
        adapter.prepare(&ctx, None).await.unwrap();
        adapter
            .execute(
                &ctx,
                EngineCommand::DeliverText {
                    id: InputId::Task,
                    text: "finish the task".into(),
                },
            )
            .await
            .unwrap();
        adapter.observe(&ctx).await.unwrap();
        let mut hooks = vec![
            json!({"hook_event_name":"UserPromptSubmit", "session_id":"fixture-devin-session", "prompt_id":"last", "prompt":"finish the task"}),
        ];
        if stop {
            hooks.push(json!({"hook_event_name":"Stop", "session_id":"fixture-devin-session", "prompt_id":"last", "last_assistant_message":"done"}));
        }
        if on_client {
            fs::write(ctx.run_dir.join("exit-on-client"), "").unwrap();
        }
        fs::write(
            ctx.run_dir.join("exit-hooks.json"),
            serde_json::to_vec(&hooks).unwrap(),
        )
        .unwrap();
        let observation = adapter.observe(&ctx).await.unwrap();
        assert_eq!(observation.status, EngineStatus::Exited);
        assert_eq!(observation.acknowledged, vec![InputId::Task]);
        assert_eq!(observation.turns_started, 1);
        assert_eq!(observation.turns_completed, u64::from(stop));
        adapter.close().await.unwrap();
    }
}

#[test]
fn devin_permission_mode_reads_the_release_indicator_near_the_composer() {
    use protocol::{PermissionMode::*, permission_mode};
    // Captured from labelled G3 session amusing-learning: the indicator sits in the top rule.
    for captured in [
        include_str!("fixtures/devin/real-fresh-bypass-pane.txt"),
        include_str!("fixtures/devin/real-resume-bypass-pane.txt"),
    ] {
        assert_eq!(permission_mode(captured), Bypass);
        let normal = captured.replace(" (bypass permissions on) ", &"\u{2500}".repeat(25));
        assert_eq!(permission_mode(&normal), NotBypass);
    }
    let real = include_str!("fixtures/devin/resume-footer-excerpt.txt");
    assert_eq!(permission_mode(real), Bypass);
    let coloured = real.replace(
        "(bypass permissions on)",
        "\x1b[38;5;214m(bypass\x1b[0m permissions on)\x1b]8;;\x07",
    );
    assert_eq!(permission_mode(&coloured), Bypass);
    let normal: String = real.lines().skip(1).map(|l| format!("{l}\n")).collect();
    assert_eq!(permission_mode(&normal), NotBypass);
    let history = format!(
        "(bypass permissions on)\n{}{normal}",
        "earlier output\n".repeat(8)
    );
    assert_eq!(permission_mode(&history), NotBypass);
    assert_eq!(
        permission_mode(&real.replace("bypass permissions on", "accept edits on")),
        NotBypass
    );
    // A draft hides the placeholder; an undrawn footer leaves the mode unknown.
    assert_eq!(
        permission_mode(&real.replace(
            "Ask Devin to build features, fix bugs, or work on your code",
            "/bypass"
        )),
        Unknown
    );
    assert_eq!(
        permission_mode("❭ Ask Devin to build features, fix bugs, or work on your code\n"),
        Unknown
    );
}

#[tokio::test]
async fn devin_resume_accepts_reported_real_footer_without_toggling() {
    resumed_bypass(json!({"ready_pane":include_str!("fixtures/devin/resume-footer-excerpt.txt")}))
        .await;
}

#[tokio::test]
async fn devin_resume_accepts_captured_real_pane_without_toggling() {
    resumed_bypass(
        json!({"ready_pane":include_str!("fixtures/devin/real-resume-bypass-pane.txt")}),
    )
    .await;
}

#[tokio::test]
async fn devin_resume_preserves_already_restored_bypass_mode() {
    resumed_bypass(json!({})).await;
}

#[tokio::test]
async fn devin_resume_waits_for_late_mode_restore_without_toggling() {
    resumed_bypass(json!({"restore_ms":400})).await;
}

async fn resumed_bypass(settings: Value) {
    let root = Scratch::new();
    let mut script = json!({"resume_bypass":true,"turns":[{"reply":"continued"}]});
    script
        .as_object_mut()
        .unwrap()
        .extend(settings.as_object().unwrap().clone());
    let opts = options(&root, script);
    let ctx = context(&root, "resume", true);
    fs::create_dir_all(opts.data_home.join("devin/cli")).unwrap();
    assert!(Command::new("/usr/bin/sqlite3")
        .arg(opts.data_home.join("devin/cli/sessions.db"))
        .arg(format!("CREATE TABLE sessions(id TEXT PRIMARY KEY, working_directory TEXT); INSERT INTO sessions VALUES('fixture-devin-session','{}');", ctx.cwd.display()))
        .status().unwrap().success());
    let mut adapter = Devin::new(opts);
    let launch = adapter
        .prepare(&ctx, Some("fixture-devin-session"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(launch.env["DEVIN_PERMISSION_MODE"], "dangerous");
    assert!(
        launch
            .argv
            .windows(2)
            .any(|args| args == ["--permission-mode", "dangerous"])
    );
    let _pane = Pane::start(&ctx, launch).await;
    adapter
        .execute(
            &ctx,
            EngineCommand::Resume {
                session: "fixture-devin-session".into(),
            },
        )
        .await
        .unwrap();
    deliver(
        &mut adapter,
        &ctx,
        InputId::Continue { attempt: 2 },
        "Continue without toggling permission mode",
    )
    .await;
    let observation = poll(&mut adapter, &ctx, |o| o.turns_completed == 1).await;
    assert_eq!(
        observation.session_id.as_deref(),
        Some("fixture-devin-session")
    );
    assert_eq!(
        fs::read_to_string(root.join("prompts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    adapter.close().await.unwrap();
}

#[tokio::test]
async fn devin_queued_inputs_restore_a_verified_menu_and_acknowledge_each_id() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(
        &root,
        json!({"dialog":true,"turns":[{"reply":"first"},{"reply":"second"}]}),
    ));
    let ctx = context(&root, "run", true);
    let launch = adapter.prepare(&ctx, None).await.unwrap().unwrap();
    let _pane = Pane::start(&ctx, launch).await;
    let message = InputId::Message {
        id: sluice_model::ids::MessageId(2),
    };
    for (id, text) in [
        (InputId::Task, "First queued prompt"),
        (message.clone(), "Second queued prompt"),
    ] {
        assert_eq!(
            adapter
                .execute(
                    &ctx,
                    EngineCommand::DeliverText {
                        id,
                        text: text.into()
                    }
                )
                .await
                .unwrap(),
            DeliveryOutcome::Pending
        );
    }
    let obs = poll(&mut adapter, &ctx, |o| {
        o.turns_completed == 2 && o.acknowledged.contains(&message)
    })
    .await;
    assert!(obs.acknowledged.contains(&InputId::Task));
    assert_eq!(
        fs::read_to_string(root.join("prompts.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[tokio::test]
async fn devin_late_previous_invocation_hooks_and_cancelled_queue_do_not_deliver() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(&root, json!({})));
    let ctx = context(&root, "run", false);
    adapter.prepare(&ctx, None).await.unwrap();
    let cfg: Value =
        serde_json::from_slice(&fs::read(ctx.run_dir.join("devin-config.json")).unwrap()).unwrap();
    let old_command = cfg["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    adapter
        .execute(
            &ctx,
            EngineCommand::DeliverText {
                id: InputId::Task,
                text: "Cancelled before any paste".into(),
            },
        )
        .await
        .unwrap();
    adapter.close().await.unwrap();
    let launch = adapter.prepare(&ctx, None).await.unwrap().unwrap();
    let mut child = Command::new("/bin/sh")
        .args(["-c", old_command])
        .envs(launch.env)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            br#"{"hook_event_name":"UserPromptSubmit","session_id":"old-session","prompt_id":"0"}"#,
        )
        .unwrap();
    assert!(child.wait().unwrap().success());
    let obs = adapter.observe(&ctx).await.unwrap();
    assert_eq!(obs.turns_started, 0);
    assert!(obs.session_id.is_none());
    assert!(obs.acknowledged.is_empty());
    assert!(!root.join("prompts.jsonl").exists());
}

#[tokio::test]
async fn devin_captured_live_steering_has_two_starts_and_one_completion() {
    let root = Scratch::new();
    let mut adapter = Devin::new(options(&root, json!({})));
    let ctx = context(&root, "run", false);
    adapter.prepare(&ctx, None).await.unwrap();
    for line in include_str!("fixtures/devin/real-captured.jsonl").lines() {
        event(&mut adapter, serde_json::from_str(line).unwrap());
    }
    let obs = adapter.observe(&ctx).await.unwrap();
    assert_eq!(obs.turns_started, 2);
    assert_eq!(obs.turns_completed, 1);
    assert_eq!(obs.status, EngineStatus::Idle);
}

/// With SLUICE_G3_DEVIN_EVIDENCE set, saves the latest real pane and the pane behind each
/// permission-mode verdict change, for redacted replay fixtures.
fn g3_capture(ctx: &EngineContext, verdict: &mut Option<protocol::PermissionMode>) {
    let Some(dir) = std::env::var_os("SLUICE_G3_DEVIN_EVIDENCE").map(PathBuf::from) else {
        return;
    };
    let Ok(out) = Command::new(ctx.tmux_binary.as_ref().unwrap())
        .current_dir(&ctx.run_dir)
        .args([
            "-S",
            "tmux.sock",
            "-f",
            "/dev/null",
            "capture-pane",
            "-p",
            "-t",
            "%0",
        ])
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let pane = String::from_utf8_lossy(&out.stdout);
    let run = ctx.run_dir.file_name().unwrap().to_string_lossy();
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join(format!("{run}-latest.txt")), pane.as_bytes());
    let mode = protocol::permission_mode(&pane);
    if *verdict != Some(mode) {
        *verdict = Some(mode);
        println!("g3_devin {run} permission verdict {mode:?}");
        let _ = fs::write(dir.join(format!("{run}-{mode:?}.txt")), pane.as_bytes());
    }
}
async fn real_poll(
    adapter: &mut Devin,
    ctx: &EngineContext,
    done: impl Fn(&EngineObservation) -> bool,
) -> io::Result<EngineObservation> {
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut reported_session = None;
    let mut verdict = None;
    loop {
        g3_capture(ctx, &mut verdict);
        let obs = adapter.observe(ctx).await.map_err(io::Error::other)?;
        if obs.session_id.is_some() && obs.session_id != reported_session {
            println!(
                "g3_devin session={:?} run_dir={}",
                obs.session_id,
                ctx.run_dir.display()
            );
            reported_session = obs.session_id.clone();
        }
        if done(&obs) {
            return Ok(obs);
        }
        if let Some(error) = obs.error {
            return Err(io::Error::other(error));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "g3_devin timed out in {:?}, starts={}, completed={}",
                obs.status, obs.turns_started, obs.turns_completed
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
fn git(cwd: &Path, args: &[&str]) -> io::Result<String> {
    let out = Command::new("git").current_dir(cwd).args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other("scratch git command failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().into())
}

async fn g3_wait_reaped(identity: &sluice_process::identity::ProcessIdentity) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while identity.matches_current()? {
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "g3_devin pane survived shutdown: {identity:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    println!(
        "g3_devin cleanup reaped pid={} start={}",
        identity.pid, identity.start_time
    );
    Ok(())
}

#[tokio::test]
#[ignore = "labelled real engine gate; run separately in a private home"]
async fn g3_devin() -> io::Result<()> {
    let root = Scratch::new();
    let owner = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let owner_data = std::env::var_os("SLUICE_G3_DEVIN_OWNER_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| owner.join(".local/share"));
    let credentials = owner_data.join("devin/credentials.toml");
    if !credentials.is_file() {
        return Err(io::Error::other(
            "PENDING: no privately copyable Devin credentials.toml",
        ));
    }
    let home = root.join("engine-home");
    for path in [
        &home,
        &home.join(".config/devin"),
        &home.join(".local/share/devin"),
        &home.join(".cache"),
        &home.join(".local/state"),
    ] {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    protocol::private_write(
        &home.join(".local/share/devin/credentials.toml"),
        &fs::read(credentials)?,
    )?;
    // Only authentication/provider configuration is needed. Owner callbacks are not run in G3.
    let owner_config = std::env::var_os("SLUICE_G3_DEVIN_OWNER_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| owner.join(".config/devin/config.json"));
    let original: Value =
        serde_json::from_str(&protocol::strip_jsonc(&fs::read_to_string(owner_config)?)?)?;
    let mut cfg = json!({});
    for key in ["version", "devin"] {
        if let Some(value) = original.get(key) {
            cfg[key] = value.clone();
        }
    }
    protocol::private_write(
        &home.join(".config/devin/config.json"),
        &serde_json::to_vec(&cfg)?,
    )?;
    let fixture = built("fixture");
    let mut adapter = Devin::new(DevinOptions {
        binary: std::env::var_os("SLUICE_G3_DEVIN_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| owner.join(".local/bin/devin")),
        hook_binary: fixture.clone(),
        config: home.join(".config/devin/config.json"),
        data_home: home.join(".local/share"),
        environment: BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME".into(),
                home.join(".config").to_string_lossy().into_owned(),
            ),
            (
                "XDG_DATA_HOME".into(),
                home.join(".local/share").to_string_lossy().into_owned(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                home.join(".cache").to_string_lossy().into_owned(),
            ),
            (
                "XDG_STATE_HOME".into(),
                home.join(".local/state").to_string_lossy().into_owned(),
            ),
            (
                "SLUICE_HOME".into(),
                root.join("scratch-sluice").to_string_lossy().into_owned(),
            ),
        ]),
        ready_timeout: Duration::from_secs(60),
        delivery_timeout: Duration::from_secs(30),
    });
    let ctx = context(&root, "g3_devin-fresh", true);
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
        vec!["commit", "--allow-empty", "-qm", "baseline"],
    ] {
        git(&ctx.cwd, &args)?;
    }
    let baseline = git(&ctx.cwd, &["rev-parse", "HEAD"])?;
    let launch = adapter
        .prepare(&ctx, None)
        .await
        .map_err(io::Error::other)?
        .unwrap();
    let mut pane = Pane::start(&ctx, launch).await;
    let pid = pane.pid();
    let identity = sluice_process::identity::ProcessIdentity::read(pid)?;
    println!(
        "g3_devin owned pane pid={pid} start={}",
        identity.start_time
    );
    adapter
        .execute(&ctx, EngineCommand::StartFresh)
        .await
        .map_err(io::Error::other)?;
    let prompt = format!(
        "g3_devin labelled scratch acceptance task. Work only in this scratch cwd. Do not spawn subagents. Declared output is word:string. Run {} devin-submit {} '{{\"word\":\"blue\"}}' to submit it. Then write work.txt containing first and git add work.txt && git commit -m 'First scratch change'. Finish briefly. Do not touch any other directories, account settings, or services.",
        protocol::shell_quote(&fixture.to_string_lossy()),
        protocol::shell_quote(&ctx.cwd.join("submission.json").to_string_lossy())
    );
    adapter
        .execute(
            &ctx,
            EngineCommand::DeliverText {
                id: InputId::Task,
                text: prompt,
            },
        )
        .await
        .map_err(io::Error::other)?;
    real_poll(&mut adapter, &ctx, |o| {
        o.acknowledged.contains(&InputId::Task)
    })
    .await?;
    let live = InputId::Message {
        id: sluice_model::ids::MessageId(1),
    };
    adapter.execute(&ctx,EngineCommand::Steer {id:live.clone(),text:"Addressed live message for g3_devin: also write live.txt containing addressed. Keep the declared word blue. Finish after the first scratch commit.".into()}).await.map_err(io::Error::other)?;
    let obs = real_poll(&mut adapter, &ctx, |o| {
        o.acknowledged.contains(&live) && o.turns_completed >= 1 && o.status == EngineStatus::Idle
    })
    .await?;
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(ctx.cwd.join("submission.json"))?)?,
        json!({"word":"blue"})
    );
    assert_eq!(
        fs::read_to_string(ctx.cwd.join("live.txt"))?.trim(),
        "addressed"
    );
    let session = obs
        .session_id
        .ok_or_else(|| io::Error::other("g3_devin did not report a session"))?;
    adapter.inject_transient_once();
    assert_eq!(
        adapter
            .observe(&ctx)
            .await
            .map_err(io::Error::other)?
            .error
            .unwrap()
            .kind,
        EngineErrorKind::Transient
    );
    adapter
        .execute(&ctx, EngineCommand::RequestExit)
        .await
        .map_err(io::Error::other)?;
    real_poll(&mut adapter, &ctx, |o| o.status == EngineStatus::Exited).await?;
    adapter.close().await?;
    pane.close();
    g3_wait_reaped(&identity).await?;
    let resumed = context(&root, "g3_devin-resume", true);
    let launch = adapter
        .prepare(&resumed, Some(&session))
        .await
        .map_err(io::Error::other)?
        .unwrap();
    let mut pane = Pane::start(&resumed, launch).await;
    let pid = pane.pid();
    let identity = sluice_process::identity::ProcessIdentity::read(pid)?;
    println!(
        "g3_devin owned pane pid={pid} start={}",
        identity.start_time
    );
    adapter
        .execute(
            &resumed,
            EngineCommand::Resume {
                session: session.clone(),
            },
        )
        .await
        .map_err(io::Error::other)?;
    assert_eq!(
        adapter
            .observe(&resumed)
            .await
            .map_err(io::Error::other)?
            .turns_completed,
        0
    );
    let continuation = InputId::Continue { attempt: 1 };
    adapter.execute(&resumed,EngineCommand::DeliverText {id:continuation.clone(),text:"g3_devin same-run continuation after an injected transient, with feedback: keep word blue, replace work.txt with second, git add work.txt && git commit -m 'Second scratch change', and finish briefly. Do not submit the original task again.".into()}).await.map_err(io::Error::other)?;
    let obs = real_poll(&mut adapter, &resumed, |o| {
        o.acknowledged.contains(&continuation)
            && o.turns_completed >= 1
            && o.status == EngineStatus::Idle
    })
    .await?;
    assert_eq!(obs.session_id.as_deref(), Some(session.as_str()));
    assert_eq!(
        git(
            &ctx.cwd,
            &["rev-list", "--count", &format!("{baseline}..HEAD")]
        )?,
        "2"
    );
    adapter
        .execute(&resumed, EngineCommand::RequestExit)
        .await
        .map_err(io::Error::other)?;
    real_poll(&mut adapter, &resumed, |o| o.status == EngineStatus::Exited).await?;
    adapter.close().await?;
    pane.close();
    g3_wait_reaped(&identity).await?;
    println!(
        "g3_devin PASS: session {}, fresh declared word=blue, addressed live message, feedback/transient resume, original baseline has two commits, both pane processes reaped",
        session
    );
    Ok(())
}

#[tokio::test]
async fn supervisor_devin_inline_hooks_submit_live_compact_and_resume() {
    let root = Scratch::new();
    let opts = options(
        &root,
        json!({"turns":[{"reply":"done","submit":{"word":"blue"},"compact":true,"busy_ms":100},{"reply":"feedback","submit":{"word":"blue"}}]}),
    );
    let mut adapter = Devin::new(opts);
    let tmux = sluice_process::tmux::ApprovedTmux::load(&workspace().join("target/private-tmux"))
        .await
        .unwrap();
    let mut cfg = supervisor_config(&root);
    let dir = cfg.run_dir.clone();
    let mut host = DevinHost {
        directory: cfg.cwd.clone(),
        messages: vec![supervisor_message(1)],
        ..Default::default()
    };
    let result = sluice_agents::supervisor::supervise(
        cfg.clone(),
        &mut adapter,
        &mut host,
        &mut Default::default(),
        Some(&tmux),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(host.submissions["word"], json!("blue"));
    assert_eq!(host.acks, vec![sluice_model::ids::MessageId(1)]);
    let checkpoint = Checkpoint::read(&dir).unwrap().unwrap();
    assert_eq!(checkpoint.state, State::Done);
    assert_eq!(checkpoint.compactions, 1);
    assert!(checkpoint.delivery.all_acknowledged());
    assert!(
        checkpoint
            .delivery
            .entries
            .iter()
            .any(|entry| entry.id == InputId::Reprime { ordinal: 1 })
    );
    assert!(
        !tmux
            .client_command(&dir)
            .unwrap()
            .arg("list-sessions")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    fs::remove_file(cfg.cwd.join("submission.json")).unwrap();
    cfg.run = sluice_model::ids::RunId::new();
    cfg.attempt = sluice_model::ids::AttemptId::new();
    cfg.invocation = sluice_model::ids::InvocationId::new();
    cfg.run_dir = root.join("resumed");
    cfg.previous = Some(sluice_agents::supervisor::PreviousSession {
        engine: "devin".into(),
        cwd: cfg.cwd.clone(),
        session: Some(result.session.clone()),
    });
    cfg.assigned.through = sluice_model::ids::MessageId(2);
    cfg.messages = vec![supervisor_message(2)];
    let mut host = DevinHost {
        directory: cfg.cwd.clone(),
        ..Default::default()
    };
    let next = sluice_agents::supervisor::supervise(
        cfg,
        &mut adapter,
        &mut host,
        &mut Default::default(),
        Some(&tmux),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(next.session, result.session);
    assert_eq!(host.acks, vec![sluice_model::ids::MessageId(2)]);
}

#[path = "acceptance/support.rs"]
mod acceptance;

#[tokio::test]
async fn supervisor_fresh_required_submit() {
    acceptance::scenario("fresh_required_submit", "devin").await;
}

#[tokio::test]
async fn supervisor_busy_submitted() {
    acceptance::scenario("busy_submitted", "devin").await;
}

#[tokio::test]
async fn supervisor_background() {
    acceptance::scenario("background", "devin").await;
}

#[tokio::test]
async fn supervisor_quiet() {
    acceptance::scenario("quiet", "devin").await;
}

#[tokio::test]
async fn supervisor_compaction() {
    acceptance::scenario("compaction", "devin").await;
}

#[tokio::test]
async fn supervisor_addressed_live_message() {
    acceptance::scenario("addressed_live_message", "devin").await;
}

#[tokio::test]
async fn supervisor_feedback_resume() {
    acceptance::scenario("feedback_resume", "devin").await;
}

#[tokio::test]
async fn supervisor_missing_outputs() {
    acceptance::scenario("missing_outputs", "devin").await;
}

#[tokio::test]
async fn supervisor_nudge() {
    acceptance::scenario("nudge", "devin").await;
}

#[tokio::test]
async fn supervisor_unknown_acceptance() {
    acceptance::scenario("unknown_acceptance", "devin").await;
}

#[tokio::test]
async fn supervisor_cancel_backoff() {
    acceptance::scenario("cancel_backoff", "devin").await;
}

#[tokio::test]
async fn supervisor_retry_exhaustion() {
    acceptance::scenario("retry_exhaustion", "devin").await;
}

#[tokio::test]
async fn supervisor_session_cwd_mismatch() {
    acceptance::scenario("session_cwd_mismatch", "devin").await;
}

#[tokio::test]
async fn supervisor_engine_mismatch() {
    acceptance::scenario("engine_mismatch", "devin").await;
}

#[tokio::test]
async fn supervisor_same_run_transient_commits_without_feedback() {
    acceptance::transient_commits("devin").await;
}

#[tokio::test]
async fn supervisor_missing_session_lock_and_cwd() {
    acceptance::session_policy("devin").await;
}

#[tokio::test]
async fn supervisor_predecessor_cwd_mismatch_starts_fresh() {
    acceptance::scenario("predecessor_cwd_mismatch", "devin").await;
}
