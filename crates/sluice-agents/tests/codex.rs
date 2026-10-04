use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sluice_agents::engines::{
    DeliveryOutcome, EngineAdapter, EngineCommand, EngineContext, EngineErrorKind, EngineStatus,
    InputId,
    codex::{
        Codex, CodexOptions, profile,
        protocol::{Rpc, redact},
    },
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    net::UnixListener,
    time::{Instant, sleep},
};

#[path = "fixtures/executable.rs"]
mod executable;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "sluice-test-codex-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Running the same test executable as a child exercises process launch and library WebSocket framing.
#[test]
#[ignore]
fn fake_codex_executable() {
    let Ok(socket) = std::env::var("SLUICE_CODEX_TEST_SOCKET") else {
        return;
    };
    let scenario = std::env::var("SLUICE_CODEX_TEST_SCENARIO").unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(sluice_agents::engines::codex::protocol::fixture_server(
            Path::new(&socket),
            &scenario,
        ))
        .unwrap();
}
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
fn setup(scratch: &Scratch, scenario: &str) -> (Codex, EngineContext) {
    let source = scratch.path().join("owner");
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("config.toml"),
        "[mcp_servers.x]\ncommand='x'\n[mcp_servers.x.env]\nTOKEN='secret'\n",
    )
    .unwrap();
    fs::write(source.join("auth.json"), "private credential fixture").unwrap();
    let binary = scratch.path().join("fake-codex");
    let executable = std::env::current_exe().unwrap();
    executable::write(
        &binary,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'codex-cli 0.160.0'; exit; fi\nexport SLUICE_CODEX_TEST_SOCKET=\"${{3#unix://}}\"\nexport SLUICE_CODEX_TEST_SCENARIO='{scenario}'\nexec {} --ignored --exact fake_codex_executable --nocapture\n",
            shell_quote(&executable)
        ),
    );
    let cwd = scratch.path().join("work");
    fs::create_dir(&cwd).unwrap();
    let context = EngineContext {
        run_dir: scratch.path().join("run"),
        cwd,
        model: None,
        effort: None,
        tmux_binary: None,
    };
    let options = CodexOptions {
        request_timeout: Duration::from_secs(2),
        ..CodexOptions::new(binary, source, scratch.path().join("home"))
    };
    (Codex::new(options), context)
}
async fn start(adapter: &mut Codex, context: &EngineContext) {
    assert!(adapter.prepare(context, None).await.unwrap().is_none());
    adapter
        .execute(context, EngineCommand::StartFresh)
        .await
        .unwrap();
}
async fn text(
    adapter: &mut Codex,
    context: &EngineContext,
    id: InputId,
    text: &str,
) -> DeliveryOutcome {
    adapter
        .execute(
            context,
            EngineCommand::DeliverText {
                id,
                text: text.into(),
            },
        )
        .await
        .unwrap()
}
async fn completed(
    adapter: &mut Codex,
    context: &EngineContext,
    turns: u64,
) -> sluice_agents::engines::EngineObservation {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let observation = adapter.observe(context).await.unwrap();
        if observation.turns_completed >= turns {
            return observation;
        }
        assert!(Instant::now() < deadline, "{observation:?}");
        sleep(Duration::from_millis(10)).await;
    }
}
#[test]
fn tui_disables_startup_updates_for_fresh_and_resume() {
    for session in [None, Some("thread-1")] {
        let argv = profile::tui_argv(
            Path::new("/tools/codex"),
            Path::new("/tmp/app.sock"),
            session,
        );
        assert_eq!(
            &argv[..3],
            ["/tools/codex", "-c", "check_for_update_on_startup=false"]
        );
        assert!(argv.contains(&"--remote".into()));
        assert_eq!(
            argv.last().unwrap(),
            session.unwrap_or("unix:///tmp/app.sock")
        );
    }
}
#[test]
fn private_config_disables_all_mcp_forms_and_preserves_toml_types() {
    for servers in [
        "mcp_servers = { 'a.b' = {command='x',env={TOKEN='secret'},http_headers={Authorization='secret'}} }",
        "mcp_servers.'a.b'.command='x'\nmcp_servers.'a.b'.env.TOKEN='secret'",
        "[mcp_servers.'a.b']\ncommand='x'\n[mcp_servers.'a.b'.http_headers]\nAuthorization='secret'",
    ] {
        let source = format!(
            "model='old'\nweb_search='cached'\ncount=23\nratio=1.25\nflag=true\nwhen=2026-09-29T12:34:56Z\nitems=[1,'two',{{ nested=[true,false] }}]\n{servers}\n[profiles.work]\nmodel='profile'\n[[profiles.work.tools]]\nname='first'\n[[profiles.work.tools]]\nname='second'\n"
        );
        let output = profile::private_config(&source, "gpt-6-astra", "max", false).unwrap();
        let doc = output.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(doc["model"].as_str(), Some("gpt-6-astra"));
        assert_eq!(doc["mcp_servers"]["a.b"]["enabled"].as_bool(), Some(false));
        assert!(doc["mcp_servers"]["a.b"].get("env").is_none());
        assert!(doc["mcp_servers"]["a.b"].get("http_headers").is_none());
        assert!(doc.get("web_search").is_none());
        assert_eq!(doc["profiles"]["work"]["model"].as_str(), Some("profile"));
        assert_eq!(doc["count"].as_integer(), Some(23));
        assert!(doc["when"].as_datetime().is_some());
        assert_eq!(
            doc["profiles"]["work"]["tools"]
                .as_array_of_tables()
                .unwrap()
                .len(),
            2
        );
        assert!(!output.contains("secret"));
    }
}
#[test]
fn private_config_keeps_difficult_values() {
    let source = "'key with space'=1\n'é'='ünï'\nbig=1e300\nneg=-inf\nodd=nan\nlocal=2026-09-29T12:34:56.5\noff=2026-09-29T12:34:56+02:00\nempty={}\nnone=[]\ndeep=[[{a={'b.c'=[1.5,'x']}}]]\n";
    let output = profile::private_config(source, "sol", "max", true).unwrap();
    assert!(output.starts_with(source));
    assert!(
        output.parse::<toml_edit::DocumentMut>().unwrap()["odd"]
            .as_float()
            .unwrap()
            .is_nan()
    );
}
#[test]
fn malformed_config_and_invalid_mcp_tables_fail() {
    for source in [
        "model=[",
        "model='a'\nmodel='b'",
        "mcp_servers=1",
        "mcp_servers.x='server'",
    ] {
        assert!(profile::private_config(source, "sol", "high", false).is_err());
    }
}
#[tokio::test]
async fn malformed_owner_config_leaves_private_preimage_and_launch_untouched() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    let private = scratch.path().join("home/codex-native-homes/pending-run");
    fs::create_dir_all(&private).unwrap();
    fs::write(private.join("config.toml"), "saved").unwrap();
    fs::write(scratch.path().join("owner/config.toml"), "model=[").unwrap();
    assert!(adapter.prepare(&context, None).await.is_err());
    assert_eq!(
        fs::read_to_string(private.join("config.toml")).unwrap(),
        "saved"
    );
    assert!(adapter.server_pid().is_none());
}
#[tokio::test]
async fn fresh_turn_and_completed_message_and_compaction_reprime() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    start(&mut adapter, &context).await;
    assert_eq!(
        text(&mut adapter, &context, InputId::Task, "required submit").await,
        DeliveryOutcome::Acknowledged
    );
    let first = completed(&mut adapter, &context, 1).await;
    assert_eq!(
        (
            first.status,
            first.turns_started,
            first.final_text.as_str(),
            first.compactions
        ),
        (EngineStatus::Idle, 1, "[redacted]", 1)
    );
    text(
        &mut adapter,
        &context,
        InputId::Reprime { ordinal: 1 },
        "step context",
    )
    .await;
    let next = completed(&mut adapter, &context, 2).await;
    assert_eq!(next.turns_started, 2);
    assert!(next.acknowledged.contains(&InputId::Task));
    let pid = adapter.server_pid().unwrap();
    adapter.close().await.unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}
#[tokio::test]
async fn busy_turn_steer_addresses_live_message_once() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "busy");
    start(&mut adapter, &context).await;
    text(&mut adapter, &context, InputId::Task, "task").await;
    let id = InputId::Message {
        id: sluice_model::ids::MessageId(42),
    };
    text(&mut adapter, &context, id.clone(), "addressed live message").await;
    text(&mut adapter, &context, id.clone(), "must not repeat").await;
    let observation = adapter.observe(&context).await.unwrap();
    assert!(observation.acknowledged.contains(&id));
    assert_eq!(observation.turns_started, 1);
    let wire = fs::read_to_string(context.run_dir.join("codex-wire.jsonl")).unwrap();
    assert_eq!(wire.matches("turn/steer").count(), 1);
    assert!(wire.contains("expectedTurnId"));
    assert!(!wire.contains("addressed live message"));
    adapter
        .execute(&context, EngineCommand::RequestExit)
        .await
        .unwrap();
    adapter.close().await.unwrap();
}
#[tokio::test]
async fn steer_race_starts_only_after_one_verified_idle_refresh() {
    for scenario in ["race-idle", "race-busy"] {
        let scratch = Scratch::new();
        let (mut adapter, context) = setup(&scratch, scenario);
        start(&mut adapter, &context).await;
        text(&mut adapter, &context, InputId::Task, "task").await;
        // Queue the next request immediately, before turn/completed is drained.
        let result = text(
            &mut adapter,
            &context,
            InputId::Nudge { ordinal: 1 },
            "feedback",
        )
        .await;
        assert_eq!(
            result,
            if scenario == "race-busy" {
                DeliveryOutcome::NotAccepted
            } else {
                DeliveryOutcome::Acknowledged
            }
        );
        adapter.close().await.unwrap();
    }
}
#[tokio::test]
async fn lost_turn_start_reply_is_never_replayed() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "uncertain");
    start(&mut adapter, &context).await;
    for _ in 0..2 {
        let e = adapter
            .execute(
                &context,
                EngineCommand::DeliverText {
                    id: InputId::Task,
                    text: "task".into(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(e.kind, EngineErrorKind::UnknownAcceptance);
    }
    assert_eq!(
        fs::read_to_string(context.run_dir.join("codex-wire.jsonl"))
            .unwrap()
            .matches("turn/start")
            .count(),
        1
    );
    adapter.close().await.unwrap();
}
#[tokio::test]
async fn resume_ignores_history_and_internal_transient_keeps_session() {
    let scratch = Scratch::new();
    let (mut adapter, mut context) = setup(&scratch, "normal");
    start(&mut adapter, &context).await;
    text(&mut adapter, &context, InputId::Task, "first").await;
    let first = completed(&mut adapter, &context, 1).await;
    adapter.inject_transient();
    assert_eq!(
        adapter.observe(&context).await.unwrap().error.unwrap().kind,
        EngineErrorKind::Transient
    );
    let session = first.session_id.unwrap();
    adapter.close().await.unwrap();
    context.run_dir = scratch.path().join("resume");
    adapter.prepare(&context, Some(&session)).await.unwrap();
    adapter
        .execute(
            &context,
            EngineCommand::Resume {
                session: session.clone(),
            },
        )
        .await
        .unwrap();
    let resumed = adapter.observe(&context).await.unwrap();
    assert_eq!(resumed.turns_completed, 0);
    text(
        &mut adapter,
        &context,
        InputId::Continue { attempt: 1 },
        "continue without new messages",
    )
    .await;
    assert_eq!(
        completed(&mut adapter, &context, 1)
            .await
            .session_id
            .as_deref(),
        Some(session.as_str())
    );
    adapter.close().await.unwrap();
}
#[tokio::test]
async fn private_home_permissions_credentials_and_mapping_are_isolated() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    start(&mut adapter, &context).await;
    let private = adapter.private_home().unwrap();
    assert_eq!(
        fs::metadata(private.join("config.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!private.join("auth.json").is_symlink());
    assert_eq!(
        fs::metadata(private.join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let doc = fs::read_to_string(private.join("config.toml"))
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    assert_eq!(doc["mcp_servers"]["x"]["enabled"].as_bool(), Some(false));
    assert!(
        fs::read_to_string(scratch.path().join("owner/config.toml"))
            .unwrap()
            .contains("secret")
    );
    let saved: Value = serde_json::from_slice(
        &fs::read(
            scratch
                .path()
                .join("home/codex-native-sessions/fixture-thread.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(saved["home"], private.to_str().unwrap());
    assert_eq!(
        adapter
            .session("fixture-thread")
            .await
            .unwrap()
            .unwrap()
            .cwd,
        context.cwd
    );
    adapter.close().await.unwrap();
}
#[tokio::test]
async fn imported_home_is_cloned_and_config_rebuilt() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    let old = scratch.path().join("imported");
    fs::create_dir_all(old.join("sessions")).unwrap();
    fs::write(old.join("sessions/old.jsonl"), "saved rollout").unwrap();
    fs::write(old.join("config.toml"), "TOKEN='old'").unwrap();
    let registry = scratch.path().join("home/codex-native-sessions");
    fs::create_dir_all(&registry).unwrap();
    fs::write(
        registry.join("legacy.json"),
        json!({"home":old,"cwd":context.cwd}).to_string(),
    )
    .unwrap();
    adapter.prepare(&context, Some("legacy")).await.unwrap();
    adapter
        .execute(
            &context,
            EngineCommand::Resume {
                session: "legacy".into(),
            },
        )
        .await
        .unwrap();
    let private = adapter.private_home().unwrap();
    assert_eq!(
        fs::read_to_string(private.join("sessions/old.jsonl")).unwrap(),
        "saved rollout"
    );
    assert!(
        !fs::read_to_string(private.join("config.toml"))
            .unwrap()
            .contains("old")
    );
    assert!(
        fs::read_to_string(old.join("config.toml"))
            .unwrap()
            .contains("old")
    );
    adapter.close().await.unwrap();
}
#[tokio::test]
async fn missing_private_home_and_cwd_mismatch_are_clear() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    let registry = scratch.path().join("home/codex-native-sessions");
    fs::create_dir_all(&registry).unwrap();
    fs::write(
        registry.join("t.json"),
        json!({"home":scratch.path().join("gone"),"cwd":context.cwd}).to_string(),
    )
    .unwrap();
    assert_eq!(
        adapter.prepare(&context, Some("t")).await.unwrap_err().kind,
        EngineErrorKind::MissingSession
    );
    fs::create_dir(scratch.path().join("gone")).unwrap();
    fs::write(
        registry.join("t.json"),
        json!({"home":scratch.path().join("gone"),"cwd":scratch.path()}).to_string(),
    )
    .unwrap();
    assert!(
        adapter
            .prepare(&context, Some("t"))
            .await
            .unwrap_err()
            .message
            .contains("cwd")
    );
    assert!(adapter.server_pid().is_none());
}
#[tokio::test]
async fn corrupt_rollout_line_does_not_hide_session_cwd() {
    let scratch = Scratch::new();
    let (mut adapter, _) = setup(&scratch, "normal");
    let sessions = scratch.path().join("owner/sessions/2026/09/28");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-a-t.jsonl"),
        "bad json\n{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/work\"}}\n",
    )
    .unwrap();
    assert_eq!(
        adapter.session("t").await.unwrap().unwrap().cwd,
        PathBuf::from("/work")
    );
    assert!(adapter.session("../owner").await.is_err());
}
#[tokio::test]
async fn unknown_version_and_model_effort_fail_before_launch() {
    let scratch = Scratch::new();
    let (mut adapter, mut context) = setup(&scratch, "normal");
    context.model = Some("unknown".into());
    assert_eq!(
        adapter.prepare(&context, None).await.unwrap_err().kind,
        EngineErrorKind::CapabilityMismatch
    );
    context.model = None;
    context.effort = Some("ultra".into());
    assert_eq!(
        adapter.prepare(&context, None).await.unwrap_err().kind,
        EngineErrorKind::CapabilityMismatch
    );
    context.effort = None;
    fs::write(
        scratch.path().join("fake-codex"),
        "#!/bin/sh\necho 'codex-cli 999.0.0'\n",
    )
    .unwrap();
    assert_eq!(
        adapter.prepare(&context, None).await.unwrap_err().kind,
        EngineErrorKind::CapabilityMismatch
    );
    assert!(adapter.server_pid().is_none());
    assert!(!context.run_dir.exists());
}
#[test]
fn transcripts_redact_prose_credentials_paths_and_identities() {
    let redacted = redact(&json!({"method":"turn/start","params":{"text":"secret","token":"credential","cwd":"/private","threadId":"identity","model":"gpt-6.1-sol"}})).to_string();
    assert!(redacted.contains("turn/start"));
    for value in ["secret", "credential", "/private", "identity"] {
        assert!(!redacted.contains(value));
    }
}
#[tokio::test]
async fn rpc_handshake_notifications_large_partial_frames_and_continuations() {
    use tokio::io::AsyncWriteExt;
    use tokio_tungstenite::tungstenite::{
        Message,
        protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        },
    };
    for continuation in [false, true] {
        let scratch = Scratch::new();
        let socket = scratch.path().join("rpc.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request: Value =
                serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
            ws.send(Message::Text(
                json!({"method":"thread/started","params":{"thread":{"id":"t"}}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({"id":request["id"],"result":{"ok":true}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let body = json!({"method":"item/completed","params":{"text":"x".repeat(300000)}})
                .to_string()
                .into_bytes();
            let split = if continuation {
                body.len() / 2
            } else {
                body.len()
            };
            let mut first = Vec::new();
            Frame::message(
                body[..split].to_vec(),
                OpCode::Data(Data::Text),
                !continuation,
            )
            .format(&mut first)
            .unwrap();
            ws.get_mut()
                .write_all(&first[..first.len() / 2])
                .await
                .unwrap();
            sleep(Duration::from_millis(100)).await;
            ws.get_mut()
                .write_all(&first[first.len() / 2..])
                .await
                .unwrap();
            if continuation {
                let mut second = Vec::new();
                Frame::message(body[split..].to_vec(), OpCode::Data(Data::Continue), true)
                    .format(&mut second)
                    .unwrap();
                ws.get_mut().write_all(&second).await.unwrap();
            }
            sleep(Duration::from_millis(100)).await;
        });
        let mut rpc = Rpc::connect(
            &socket,
            &scratch.path().join("wire.jsonl"),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(
            rpc.request("initialize", json!({})).await.unwrap()["ok"],
            true
        );
        assert_eq!(rpc.drain().unwrap()[0]["method"], "thread/started");
        sleep(Duration::from_millis(30)).await;
        let now = Instant::now();
        assert!(rpc.drain().unwrap().is_empty());
        assert!(now.elapsed() < Duration::from_millis(20));
        sleep(Duration::from_millis(120)).await;
        let events = rpc.drain().unwrap();
        assert_eq!(events[0]["params"]["text"].as_str().unwrap().len(), 300000);
        rpc.close().await.unwrap();
        server.await.unwrap();
    }
}
#[tokio::test]
async fn empty_rollout_subscription_retries_without_repeating_the_task() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "empty-rollout");
    start(&mut adapter, &context).await;
    text(&mut adapter, &context, InputId::Task, "one task").await;
    assert_eq!(completed(&mut adapter, &context, 1).await.turns_started, 1);
    let wire = fs::read_to_string(context.run_dir.join("codex-wire.jsonl")).unwrap();
    assert_eq!(wire.matches("thread/resume").count(), 3);
    assert_eq!(wire.matches("turn/start\"").count(), 1);
    adapter.close().await.unwrap();
}
#[tokio::test]
async fn a_resumed_long_thread_larger_than_four_mebibytes_is_received() {
    let scratch = Scratch::new();
    let socket = scratch.path().join("rpc.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let history = "x".repeat(6 * 1024 * 1024);
    let reply = history.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let request: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        let response = json!({"id": request["id"], "result": {"thread": {"history": reply}}});
        ws.send(tokio_tungstenite::tungstenite::Message::text(
            response.to_string(),
        ))
        .await
        .unwrap();
        sleep(Duration::from_secs(60)).await;
    });
    let mut rpc = Rpc::connect(
        &socket,
        &scratch.path().join("wire.jsonl"),
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    let result = rpc.request("thread/resume", json!({})).await.unwrap();
    assert_eq!(
        result["thread"]["history"].as_str().unwrap().len(),
        history.len()
    );
    rpc.close().await.unwrap();
    server.abort();
}
#[tokio::test]
async fn cancelled_request_keeps_unknown_acceptance_and_cleanup_reaps_server() {
    let scratch = Scratch::new();
    let socket = scratch.path().join("rpc.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.next().await.unwrap().unwrap();
        sleep(Duration::from_secs(60)).await;
    });
    let mut rpc = Rpc::connect(
        &socket,
        &scratch.path().join("wire.jsonl"),
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            rpc.request("turn/start", json!({}))
        )
        .await
        .is_err()
    );
    assert_eq!(
        rpc.request("turn/start", json!({})).await.unwrap_err().kind,
        EngineErrorKind::UnknownAcceptance
    );
    rpc.close().await.unwrap();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
#[tokio::test]
async fn missing_engine_session_is_typed_and_fresh_fallback_is_left_to_supervisor() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "missing");
    start(&mut adapter, &context).await;
    assert_eq!(
        adapter
            .execute(
                &context,
                EngineCommand::Resume {
                    session: "missing".into()
                }
            )
            .await
            .unwrap_err()
            .kind,
        EngineErrorKind::MissingSession
    );
    adapter.close().await.unwrap();
}

async fn g3_completed(adapter: &mut Codex, context: &EngineContext) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut reported_session = None;
    loop {
        let observation = adapter.observe(context).await.map_err(|e| e.to_string())?;
        if observation.session_id.is_some() && observation.session_id != reported_session {
            println!(
                "g3_codex session={:?} run_dir={}",
                observation.session_id,
                context.run_dir.display()
            );
            reported_session = observation.session_id.clone();
        }
        if let Some(e) = observation.error {
            return Err(e.to_string());
        }
        if observation.turns_completed >= 1 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("g3_codex turn deadline exceeded".into());
        }
        sleep(Duration::from_millis(100)).await;
    }
}
fn git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "git failed");
    String::from_utf8(output.stdout).unwrap().trim().into()
}
#[tokio::test]
#[ignore = "g3_codex uses copied credentials and one labelled scratch session"]
async fn g3_codex() {
    let scratch = Scratch::new();
    let owner = std::env::var_os("SLUICE_CODEX_G3_SOURCE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".codex"));
    if !owner.join("auth.json").is_file() {
        println!("g3_codex PENDING: no privately copyable auth.json");
        return;
    }
    let source = scratch.path().join("credentials");
    fs::create_dir(&source).unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    let auth = fs::read(owner.join("auth.json")).unwrap();
    fs::write(source.join("auth.json"), auth).unwrap();
    fs::set_permissions(source.join("auth.json"), fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = toml_edit::DocumentMut::new();
    if let Ok(text) = fs::read_to_string(owner.join("config.toml")) {
        let doc = text.parse::<toml_edit::DocumentMut>().unwrap();
        for key in ["model_provider", "model_providers", "service_tier"] {
            if let Some(value) = doc.get(key) {
                config[key] = value.clone();
            }
        }
    }
    fs::write(source.join("config.toml"), config.to_string()).unwrap();
    fs::set_permissions(
        source.join("config.toml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let cwd = scratch.path().join("g3_codex");
    fs::create_dir(&cwd).unwrap();
    git(&cwd, &["init", "-q"]);
    git(&cwd, &["config", "user.email", "g3@example.invalid"]);
    git(&cwd, &["config", "user.name", "G3 fixture"]);
    fs::write(cwd.join("baseline"), "g3_codex\n").unwrap();
    git(&cwd, &["add", "."]);
    git(&cwd, &["commit", "-qm", "Create the gate baseline."]);
    let baseline = git(&cwd, &["rev-parse", "HEAD"]);
    executable::write(
        cwd.join("submit.sh"),
        "#!/bin/sh\nset -eu\nprintf '%s\\n' '{\"word\":\"blue\"}' > submitted.json\n",
    );
    let mut context = EngineContext {
        run_dir: scratch.path().join("fresh"),
        cwd: cwd.clone(),
        model: Some("sol".into()),
        effort: Some("low".into()),
        tmux_binary: None,
    };
    let options = CodexOptions::new(
        std::env::var_os("SLUICE_CODEX_G3_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("codex")),
        source,
        scratch.path().join("home"),
    );
    let mut adapter = Codex::new(options);
    let result: Result<String, String> = async {
        adapter.prepare(&context, None).await.map_err(|e| e.to_string())?;
        adapter.execute(&context, EngineCommand::StartFresh).await.map_err(|e| e.to_string())?;
        adapter.execute(&context, EngineCommand::DeliverText { id: InputId::Task, text: "This is the labelled g3_codex scratch gate. Declared output: word must be the string blue. Run ./submit.sh to submit it, then git add submitted.json submit.sh and commit with message 'Submit the gate word.' Do no other work. An addressed live message may arrive during this turn. Finish after the commit.".into() }).await.map_err(|e| e.to_string())?;
        adapter.execute(&context, EngineCommand::Steer { id: InputId::Message { id: sluice_model::ids::MessageId(1) }, text: "Addressed live message for g3_codex: also write received.txt containing live, and include it in your commit.".into() }).await.map_err(|e| e.to_string())?;
        g3_completed(&mut adapter, &context).await?;
        if serde_json::from_slice::<Value>(&fs::read(cwd.join("submitted.json")).map_err(|e| e.to_string())?).map_err(|e| e.to_string())? != json!({"word":"blue"}) { return Err("declared output mismatch".into()); }
        if fs::read_to_string(cwd.join("received.txt")).map_err(|e| e.to_string())?.trim() != "live" { return Err("live message not handled".into()); }
        let session = adapter.observe(&context).await.map_err(|e| e.to_string())?.session_id.ok_or("no session")?;
        let first_pid = adapter.server_pid().ok_or("no server pid")?;
        adapter.close().await.map_err(|e| e.to_string())?;
        if Path::new(&format!("/proc/{first_pid}")).exists() { return Err("fresh server leaked".into()); }
        context.run_dir = scratch.path().join("feedback");
        adapter.prepare(&context, Some(&session)).await.map_err(|e| e.to_string())?;
        adapter.execute(&context, EngineCommand::Resume { session: session.clone() }).await.map_err(|e| e.to_string())?;
        adapter.execute(&context, EngineCommand::DeliverText { id: InputId::Message { id: sluice_model::ids::MessageId(2) }, text: "Feedback resume for g3_codex. Write feedback.txt containing resumed, git add feedback.txt, and commit with message 'Record gate feedback.' Finish.".into() }).await.map_err(|e| e.to_string())?;
        g3_completed(&mut adapter, &context).await?;
        adapter.inject_transient();
        if adapter.observe(&context).await.map_err(|e| e.to_string())?.error.map(|e| e.kind) != Some(EngineErrorKind::Transient) { return Err("transient injection missing".into()); }
        adapter.close().await.map_err(|e| e.to_string())?;
        context.run_dir = scratch.path().join("transient");
        adapter.prepare(&context, Some(&session)).await.map_err(|e| e.to_string())?;
        adapter.execute(&context, EngineCommand::Resume { session: session.clone() }).await.map_err(|e| e.to_string())?;
        adapter.execute(&context, EngineCommand::DeliverText { id: InputId::Continue { attempt: 1 }, text: "Internal transient continuation of the same g3_codex run, with no new feedback. Write continued.txt containing recovered, git add continued.txt, and commit with message 'Recover the gate turn.' Finish.".into() }).await.map_err(|e| e.to_string())?;
        g3_completed(&mut adapter, &context).await?;
        if git(&cwd, &["rev-list", "--count", &format!("{baseline}..HEAD")]) != "3" { return Err("original git baseline did not count three commits".into()); }
        if adapter.observe(&context).await.map_err(|e| e.to_string())?.session_id.as_deref() != Some(&session) { return Err("session changed during continuation".into()); }
        Ok(session)
    }.await;
    let pid = adapter.server_pid();
    adapter.close().await.unwrap();
    if let Some(pid) = pid {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    let evidence = std::env::var_os("SLUICE_CODEX_G3_EVIDENCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/sluice-p5-02-g3-evidence"));
    fs::create_dir_all(&evidence).unwrap();
    fs::set_permissions(&evidence, fs::Permissions::from_mode(0o700)).unwrap();
    for run in ["fresh", "feedback", "transient"] {
        if let Ok(bytes) = fs::read(scratch.path().join(run).join("codex-wire.jsonl")) {
            fs::write(evidence.join(format!("{run}-wire.jsonl")), bytes).unwrap();
        }
        if let Ok(bytes) = fs::read(scratch.path().join(run).join("app-server.log")) {
            let summary = format!("app-server log bytes: {}", bytes.len());
            fs::write(evidence.join(format!("{run}-log-size.txt")), summary).unwrap();
        }
    }
    match &result {
        Ok(session) => println!(
            "g3_codex PASS session={} commits=3 submission=blue live=received feedback=resumed transient=recovered cleanup=reaped",
            session
        ),
        Err(e) => println!("g3_codex FAILED: {e}"),
    }
    assert!(result.is_ok(), "{}", result.unwrap_err());
}

#[tokio::test]
async fn remote_tui_launch_reports_thread_and_subscribes_after_first_turn() {
    let scratch = Scratch::new();
    let (mut adapter, mut context) = setup(&scratch, "tui");
    context.tmux_binary = Some(PathBuf::from("/private/tmux"));
    let launch = adapter.prepare(&context, None).await.unwrap().unwrap();
    assert!(launch.argv.contains(&"--remote".into()));
    assert_eq!(
        launch.env["SLUICE_HOME"],
        scratch.path().join("home").to_str().unwrap()
    );
    assert!(launch.env.keys().all(|key| !key.starts_with("CARGO_")));
    adapter
        .execute(&context, EngineCommand::StartFresh)
        .await
        .unwrap();
    text(
        &mut adapter,
        &context,
        InputId::Task,
        "task for remote TUI thread",
    )
    .await;
    assert_eq!(
        completed(&mut adapter, &context, 1)
            .await
            .session_id
            .as_deref(),
        Some("fixture-thread")
    );
    adapter.close().await.unwrap();
}
#[tokio::test]
async fn failed_turn_classifies_transient_and_capability_failure_cleans_startup() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "transient");
    start(&mut adapter, &context).await;
    text(&mut adapter, &context, InputId::Task, "task").await;
    assert_eq!(
        completed(&mut adapter, &context, 1)
            .await
            .error
            .unwrap()
            .kind,
        EngineErrorKind::Transient
    );
    adapter.close().await.unwrap();
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "capability-mismatch");
    assert_eq!(
        adapter.prepare(&context, None).await.unwrap_err().kind,
        EngineErrorKind::CapabilityMismatch
    );
    assert!(adapter.server_pid().is_none());
}
#[tokio::test]
async fn cleanup_after_cancelled_prepare_reaps_auxiliary_child() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    assert!(
        tokio::time::timeout(Duration::from_millis(1), adapter.prepare(&context, None))
            .await
            .is_err()
    );
    let pid = adapter.server_pid();
    adapter.close().await.unwrap();
    if let Some(pid) = pid {
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }
    adapter.close().await.unwrap();
}

#[path = "acceptance/support.rs"]
mod acceptance;

#[tokio::test]
async fn supervisor_fresh_required_submit() {
    acceptance::scenario("fresh_required_submit", "codex").await;
}

#[tokio::test]
async fn supervisor_busy_submitted() {
    acceptance::scenario("busy_submitted", "codex").await;
}

#[tokio::test]
async fn supervisor_background() {
    acceptance::scenario("background", "codex").await;
}

#[tokio::test]
async fn supervisor_quiet() {
    acceptance::scenario("quiet", "codex").await;
}

#[tokio::test]
async fn supervisor_compaction() {
    acceptance::scenario("compaction", "codex").await;
}

#[tokio::test]
async fn supervisor_addressed_live_message() {
    acceptance::scenario("addressed_live_message", "codex").await;
}

#[tokio::test]
async fn supervisor_feedback_resume() {
    acceptance::scenario("feedback_resume", "codex").await;
}

#[tokio::test]
async fn supervisor_missing_outputs() {
    acceptance::scenario("missing_outputs", "codex").await;
}

#[tokio::test]
async fn supervisor_nudge() {
    acceptance::scenario("nudge", "codex").await;
}

#[tokio::test]
async fn supervisor_unknown_acceptance() {
    acceptance::scenario("unknown_acceptance", "codex").await;
}

#[tokio::test]
async fn supervisor_cancel_backoff() {
    acceptance::scenario("cancel_backoff", "codex").await;
}

#[tokio::test]
async fn supervisor_retry_exhaustion() {
    acceptance::scenario("retry_exhaustion", "codex").await;
}

#[tokio::test]
async fn supervisor_session_cwd_mismatch() {
    acceptance::scenario("session_cwd_mismatch", "codex").await;
}

#[tokio::test]
async fn supervisor_engine_mismatch() {
    acceptance::scenario("engine_mismatch", "codex").await;
}

#[tokio::test]
async fn supervisor_codex_wire_compaction_required_submit_and_cleanup() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    let mut cfg = acceptance::config(scratch.path(), "codex");
    cfg.cwd = context.cwd;
    cfg.run_dir = context.run_dir;
    cfg.limits.wall = Duration::from_millis(300);
    let mut host = acceptance::Host::submitted();
    let directory = cfg.run_dir.clone();
    let result = acceptance::run(cfg, &mut adapter, &mut host).await.unwrap();
    assert_eq!(result.session, "fixture-thread");
    let checkpoint = sluice_agents::supervisor::Checkpoint::read(&directory)
        .unwrap()
        .unwrap();
    assert!(checkpoint.compactions > 0);
    assert!(
        checkpoint
            .delivery
            .entries
            .iter()
            .any(|e| matches!(e.id, InputId::Reprime { .. }))
    );
    assert!(adapter.server_pid().is_none());
    assert_eq!(host.cleanups, 1);
}

#[tokio::test]
async fn supervisor_same_run_transient_commits_without_feedback() {
    acceptance::transient_commits("codex").await;
}

#[tokio::test]
async fn supervisor_missing_session_lock_and_cwd() {
    acceptance::session_policy("codex").await;
}

#[tokio::test]
async fn supervisor_predecessor_cwd_mismatch_starts_fresh() {
    acceptance::scenario("predecessor_cwd_mismatch", "codex").await;
}

// g3-fix owns native generation cloning. Real Codex creates these links in CODEX_HOME/tmp.
#[tokio::test]
async fn supervisor_codex_resume_with_native_temporary_executable_links() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    let mut cfg = acceptance::config(scratch.path(), "codex");
    cfg.cwd = context.cwd.clone();
    cfg.run_dir = context.run_dir;
    cfg.limits.wall = Duration::from_millis(300);
    let first = acceptance::run(cfg, &mut adapter, &mut acceptance::Host::submitted())
        .await
        .unwrap();
    let home = adapter.session_home(&first.session).unwrap().unwrap();
    let temporary = home.join("tmp/arg0/codex-arg0-fixture");
    fs::create_dir_all(&temporary).unwrap();
    std::os::unix::fs::symlink("/usr/bin/true", temporary.join("apply_patch")).unwrap();
    let mut cfg = acceptance::config(scratch.path(), "codex");
    cfg.cwd = context.cwd;
    cfg.run_dir = scratch.path().join("resumed");
    cfg.session = Some(first.session.clone());
    cfg.limits.wall = Duration::from_millis(300);
    let resumed = acceptance::run(cfg, &mut adapter, &mut acceptance::Host::submitted())
        .await
        .unwrap();
    assert_eq!(resumed.session, first.session);
    assert!(adapter.server_pid().is_none());
}

// Ported from p5-05's native temporary-link regression.
#[tokio::test]
async fn codex_resume_with_native_temporary_executable_links() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    start(&mut adapter, &context).await;
    text(&mut adapter, &context, InputId::Task, "original task").await;
    let first = completed(&mut adapter, &context, 1).await;
    let session = first.session_id.unwrap();
    let home = adapter.session_home(&session).unwrap().unwrap();
    let temporary = home.join("tmp/arg0/codex-arg0-fixture");
    fs::create_dir_all(&temporary).unwrap();
    std::os::unix::fs::symlink("/usr/bin/true", temporary.join("apply_patch")).unwrap();
    adapter.close().await.unwrap();
    adapter.prepare(&context, Some(&session)).await.unwrap();
    adapter
        .execute(
            &context,
            EngineCommand::Resume {
                session: session.clone(),
            },
        )
        .await
        .unwrap();
    text(
        &mut adapter,
        &context,
        InputId::Continue { attempt: 2 },
        "continue",
    )
    .await;
    assert_eq!(
        completed(&mut adapter, &context, 1)
            .await
            .session_id
            .as_deref(),
        Some(session.as_str())
    );
    assert_eq!(adapter.private_home(), Some(home.as_path()));
    assert!(temporary.join("apply_patch").is_symlink());
    adapter.close().await.unwrap();
    assert!(adapter.server_pid().is_none());
}

#[tokio::test]
async fn imported_home_with_executable_symlink_is_still_rejected() {
    let scratch = Scratch::new();
    let (mut adapter, context) = setup(&scratch, "normal");
    let imported = scratch.path().join("imported");
    fs::create_dir_all(imported.join("tmp/arg0")).unwrap();
    std::os::unix::fs::symlink("/usr/bin/true", imported.join("tmp/arg0/apply_patch")).unwrap();
    let registry = scratch.path().join("home/codex-native-sessions");
    fs::create_dir_all(&registry).unwrap();
    fs::write(
        registry.join("imported.json"),
        json!({"home":imported,"cwd":context.cwd}).to_string(),
    )
    .unwrap();
    let error = adapter
        .prepare(&context, Some("imported"))
        .await
        .unwrap_err();
    assert_eq!(error.kind, EngineErrorKind::Fatal);
    assert!(error.message.contains("symlink"), "{error}");
    assert!(
        !scratch
            .path()
            .join("home/codex-native-homes/imported")
            .exists()
    );
    assert!(imported.join("tmp/arg0/apply_patch").is_symlink());
    assert!(adapter.server_pid().is_none());
}
