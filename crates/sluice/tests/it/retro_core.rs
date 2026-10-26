//! Regressions from the agent-use retrospective, observed by a peer through the CLI.
use crate::support::home::ScratchHome;
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    rpc::{FnInvocation, JsonMap},
};
use sluice_process::{
    guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence},
    journal::{CleanupEvidence, CompletionJournal, PayloadResult},
};
use sluice_runtime::{
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
    run_health,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct Host(Arc<Mutex<Vec<Launch>>>, Arc<tokio::sync::Notify>);
impl FnHost for Host {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        unreachable!()
    }
}
impl AdoptionHost for Host {
    async fn reconcile(&self, _: &AdoptionAttempt) -> std::io::Result<GuardianPresence> {
        Ok(GuardianPresence::Ambiguous("fixture".into()))
    }
}
impl ExecutionHost for Host {
    async fn terminal_gone(
        &self,
        _: sluice_model::ids::RunId,
        _: bool,
    ) -> Result<bool, PublicError> {
        Ok(true)
    }
    async fn cleanup_valid(&self, _: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(true)
    }
    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, PublicError> {
        self.0.lock().unwrap().push(launch);
        self.1.notify_waiters();
        Ok(LaunchOutcome::Accepted)
    }
}
struct Fixture {
    home: ScratchHome,
    broker: Coordinator<Host>,
    host: Host,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
impl Fixture {
    async fn new() -> Self {
        let home = ScratchHome::new().unwrap();
        let host = Host::default();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host.clone())
            .await
            .unwrap();
        broker.acquire_scheduler("retro-test".into()).await.unwrap();
        let stop = CancellationToken::new();
        let (serving, token) = (broker.clone(), stop.clone());
        let server = tokio::spawn(async move { serving.serve(token).await });
        while !home.path().join("coordinator.sock").exists() {
            tokio::task::yield_now().await;
        }
        let f = Self {
            home,
            broker,
            host,
            stop,
            server,
        };
        f.call("project_create", json!({"name":"p","resources":{"land":1}}))
            .await;
        f
    }
    async fn call(&self, name: &str, args: Value) -> Value {
        let mut args = args.as_object().unwrap().clone();
        args.entry("me").or_insert(json!("@peer"));
        let request = sluice_web::mcp::decode_tool(name, args, Some("retro-test")).unwrap();
        sluice_web::mcp::reply_value(self.broker.command(request).await.unwrap()).unwrap()
    }
    async fn tool(&self, name: &str, args: Value) -> Value {
        let path = self.home.path().to_owned();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new(env!("CARGO_BIN_EXE_sluice"))
                .env("SLUICE_HOME", path)
                .env("SLUICE_ME", "@peer")
                .env("TOKIO_WORKER_THREADS", "2")
                .env_remove("SLUICE_RUN_ID")
                .env_remove("SLUICE_PROJECT_ID")
                .env_remove("SLUICE_STEP")
                .env_remove("SLUICE_AUTHOR")
                .args(["tool", &name, &args.to_string()])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            serde_json::from_slice(&out.stdout).unwrap()
        })
        .await
        .unwrap()
    }
    async fn step(&self, name: &str) -> Launch {
        self.call("step_add",json!({"project":"p","step":name,"spec":{"run":"fixture.submit","tags":[format!("unit:{name}")],"in":{"value":{"default":1},"engine":{"default":"codex"},"model":{"default":{"type":"normal","model":"sol","effort":"high"}}},"outputs":{"summary":"string"}}})).await;
        loop {
            let notified = self.host.1.notified();
            let launch = self
                .host
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|l| l.identity.step.as_ref().is_some_and(|s| s.as_str() == name))
                .cloned();
            if let Some(launch) = launch {
                let dir = self
                    .home
                    .path()
                    .join("runs")
                    .join(launch.identity.run.to_string());
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("agent-observation.json"),json!({"engine":"codex","model":"gpt-6.1-sol@high","last_activity_ms":(run_health::now_seconds()-1800)*1000,"last_message":"Working on the parser"}).to_string()).unwrap();
                self.start(&launch).await;
                return launch;
            }
            notified.await;
        }
    }
    async fn start(&self, l: &Launch) {
        use sluice_process::{
            identity::ProcessIdentity,
            socket::{CoordinatorCommand as C, GuardianIdentity},
        };
        let process = ProcessIdentity {
            pid: 123,
            start_time: 42,
            boot_id: "fixture".into(),
            cgroup: format!("/fake/sluice-test-{}.service/control", l.identity.run),
        };
        self.broker
            .guardian(
                C::Claim(GuardianIdentity {
                    identity: l.identity.clone(),
                    process: process.clone(),
                    unit: format!("sluice-test-{}.service", l.identity.run),
                    socket_challenge: "fixture".into(),
                }),
                Some(&l.capability),
            )
            .await
            .unwrap();
        self.broker
            .guardian(
                C::Started {
                    identity: l.identity.clone(),
                    invocation: l.invocation.invocation,
                    executor: ProcessIdentity {
                        pid: 124,
                        cgroup: "/fake/payload".into(),
                        ..process
                    },
                },
                Some(&l.capability),
            )
            .await
            .unwrap();
    }
    async fn finish(&self, launch: &Launch, result: PayloadResult) {
        let submission = self.broker.submissions(launch.identity.run).await.unwrap();
        self.broker
            .complete(CompletionJournal {
                protocol: 1,
                limits: launch.limits,
                guardian_memory_bytes: launch.policy.guardian_memory_bytes,
                usage: None,
                identity: launch.identity.clone(),
                completion_id: format!("done-{}", launch.identity.run),
                result,
                starts: vec![],
                exits: vec![],
                cleanup: vec![CleanupEvidence {
                    cgroup: "fixture".into(),
                    empty: true,
                    escalated: false,
                }],
                submissions: submission.fields,
                submission_version: submission.version,
                delivery_acks: vec![],
            })
            .await
            .unwrap();
    }
    async fn next(&self, since: i64, include: bool) -> Value {
        self.call("next",json!({"projects":["p"],"since_seq":since,"timeout":0,"settle":1,"settle_max":0,"include_routine":include})).await
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn check_notes_repeat_without_stopping_an_agent_and_follow_its_owner() {
    let f = Fixture::new().await;
    let launch = f.step("work").await;
    let status = f
        .call("status", json!({"project":"p","view":"units"}))
        .await;
    let at = status["units"][0]["check_due"][0]["at"].as_i64().unwrap();
    f.call(
        "subscription_set",
        json!({"name":"checks","projects":["p"],"wake":["ask"],"me":"@peer"}),
    )
    .await;
    run_health::check_at(&f.broker, at - 1).await.unwrap();
    assert!(
        f.next(0, false).await["records"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    run_health::check_at(&f.broker, at).await.unwrap();
    let first = f
        .tool("next", json!({"subscription":"checks","timeout":0}))
        .await;
    let records = first["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    let check = &records[0]["data"]["run_check"];
    assert_eq!(check["unit"], "work");
    assert_eq!(check["step"], "work");
    assert_eq!(check["engine"], "codex");
    assert_eq!(check["model"], "gpt-6.1-sol@high");
    assert_eq!(check["elapsed_seconds"], 7200);
    assert_eq!(check["last_message"], "Working on the parser");
    assert!(check["last_activity_at"].as_str().unwrap().ends_with('Z'));
    assert!(check["last_activity_ago_seconds"].as_i64().unwrap() >= 1800);
    assert_eq!(check["parked"], false);
    assert_eq!(check["waiting"], json!([]));
    run_health::check_at(&f.broker, at + 1).await.unwrap();
    assert!(
        f.call("next", json!({"subscription":"checks","timeout":0}))
            .await["records"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    run_health::check_at(&f.broker, at + 7200).await.unwrap();
    let second = f.next(first["last_seq"].as_i64().unwrap(), false).await;
    assert_eq!(second["records"].as_array().unwrap().len(), 1);
    assert_eq!(
        second["records"][0]["data"]["run_check"]["elapsed_seconds"],
        14400
    );
    f.call(
        "next",
        json!({"projects":["p"],"me":"@successor","timeout":0,"settle":0}),
    )
    .await;
    f.call(
        "unit_release",
        json!({"project":"p","unit":"work","to":"@successor","reason":"handoff"}),
    )
    .await;
    f.call("step_add",json!({"project":"p","step":"blocker","spec":{"run":"fixture.wait","in":{"value":{"default":1}}}})).await;
    let blocker = loop {
        let ready = f.host.1.notified();
        if let Some(l) = f
            .host
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|l| {
                l.identity
                    .step
                    .as_ref()
                    .is_some_and(|s| s.as_str() == "blocker")
            })
            .cloned()
        {
            break l;
        }
        ready.await;
    };
    f.start(&blocker).await;
    f.call(
        "lease_acquire",
        json!({"project":"p","run":blocker.identity.run,"resource":"land","wait":false}),
    )
    .await;
    let lease = f
        .call(
            "lease_acquire",
            json!({"project":"p","run":launch.identity.run,"resource":"land","wait":false}),
        )
        .await;
    assert_eq!(lease["state"], "waiting");
    run_health::check_at(&f.broker, at + 14400).await.unwrap();
    let handed=f.call("next",json!({"projects":["p"],"me":"@successor","since_seq":second["last_seq"],"timeout":0,"settle":1,"settle_max":0})).await;
    let latest = handed["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| !r["data"]["run_check"].is_null())
        .unwrap();
    assert_eq!(latest["to"], "@successor");
    assert_eq!(latest["data"]["run_check"]["waiting"], json!(["land"]));
    f.call(
        "project_update",
        json!({"project":"p","check_age_seconds":0}),
    )
    .await;
    run_health::check_at(&f.broker, at + 21600).await.unwrap();
    assert!(
        f.next(second["last_seq"].as_i64().unwrap(), false).await["records"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let running = f.call("status", json!({"project":"p","all":true})).await;
    assert_eq!(running["steps"]["work"]["status"], "running");
    assert_eq!(running["steps"]["work"]["check_due"], json!([]));
    let log = f
        .call("log_read", json!({"project":"p","kinds":["message"]}))
        .await;
    assert_eq!(
        log["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| !r["data"]["run_check"].is_null())
            .count(),
        3
    );
    // Completion keeps the live run's identity and the next check cannot resurrect it.
    f.finish(
        &launch,
        PayloadResult::Succeeded(
            serde_json::from_value(json!({"value":1,"summary":"done"})).unwrap(),
        ),
    )
    .await;
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_failures_report_operational_causes_to_a_peer() {
    let f = Fixture::new().await;
    let agent = |kind: &str, message: &str| {
        PayloadResult::Failed(PublicError::AgentFailure {
            kind: kind.into(),
            message: message.into(),
            session: None,
            retry_at: None,
        })
    };
    let cases = vec![
        (
            "task-text",
            "harness",
            PayloadResult::Failed(PublicError::FnFailure {
                message: "expected 503 outputs".into(),
            }),
        ),
        ("quota", "provider_quota", agent("QuotaExhausted", "quota")),
        (
            "provider",
            "provider_unavailable",
            agent("Transient", "HTTP 429"),
        ),
        ("auth", "provider_unavailable", agent("AuthFailed", "login")),
        (
            "stall",
            "stall",
            agent("StallCap", "HTTP 429 appeared in task output"),
        ),
        ("wall", "wall_cap", agent("WallCap", "expired")),
        (
            "memory",
            "resource_kill",
            PayloadResult::Failed(PublicError::ResourceExhausted {
                resource: sluice_model::limits::ExhaustedResource::Memory,
                limit: 1024,
                peak: None,
                message: "OOM".into(),
            }),
        ),
        (
            "disk",
            "resource_kill",
            PayloadResult::Failed(PublicError::FnFailure {
                message: "No space left on device".into(),
            }),
        ),
        (
            "missing",
            "no_submission",
            PayloadResult::Succeeded(serde_json::from_value(json!({"value":1})).unwrap()),
        ),
        (
            "rejected",
            "task_rejected",
            PayloadResult::Rejected("work rejected".into()),
        ),
        (
            "harness",
            "harness",
            agent("Invalid", "429 quoted in invalid input"),
        ),
    ];
    for (step, _, result) in &cases {
        let launch = f.step(step).await;
        f.finish(&launch, result.clone()).await;
    }
    let success = f.step("success").await;
    f.finish(
        &success,
        PayloadResult::Succeeded(
            serde_json::from_value(json!({"value":1,"summary":"done"})).unwrap(),
        ),
    )
    .await;
    f.step("running").await;
    let steps = f.call("status", json!({"project":"p","all":true})).await;
    let units = f
        .call("status", json!({"project":"p","view":"units","all":true}))
        .await;
    let log = f
        .tool(
            "log_read",
            json!({"project":"p","kinds":["step.status"],"limit":200}),
        )
        .await;
    for (step, class, _) in &cases {
        assert_eq!(steps["steps"][step]["failure_class"], *class);
        assert_eq!(steps["steps"][step]["error"]["failure_class"], *class);
        let row = units["units"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["unit"] == *step)
            .unwrap();
        assert_eq!(row["failure_class"][step], *class);
        assert!(
            log["records"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["step"] == *step && r["failure_class"] == *class)
        );
    }
    let report = f
        .tool(
            "engine_report",
            json!({"project":"p","since":"2026-01-01T00:00:00Z"}),
        )
        .await;
    let row = &report["engines"][0];
    assert_eq!(row["engine"], "codex");
    assert_eq!(row["model"], "gpt-6.1-sol@high");
    assert_eq!(row["attempts"], cases.len() + 2);
    assert_eq!(row["succeeded"], 1);
    for class in [
        "provider_quota",
        "provider_unavailable",
        "stall",
        "wall_cap",
        "resource_kill",
        "no_submission",
        "task_rejected",
        "harness",
    ] {
        let count = cases.iter().filter(|(_, c, _)| *c == class).count();
        assert_eq!(row["failed"][class], count);
    }
    assert!(
        f.call(
            "engine_report",
            json!({"project":"p","since":"2099-01-01T00:00:00Z"})
        )
        .await["engines"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    f.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_filters_routine_notices_for_sessions_and_subscriptions() {
    let f = Fixture::new().await;
    f.call("subscription_set",json!({"name":"lanes","projects":["p"],"wake":["ask","reply","failed","settled","limit","say-to-me"],"me":"@peer"})).await;
    // Typed notices use the same coordinator posting path as capacity and engine observations.
    let project = f.broker.projects().await.unwrap()[0];
    f.broker
        .writer()
        .write(sluice_store::RetrySafety::NonIdempotent, move |tx| {
            for (body, data) in [
                ("bypassed", json!({"step_bypass":{"by":"@peer"}})),
                (
                    "held",
                    json!({"execution_limit":{"limit":"host_memory","outcome":"held"}}),
                ),
                (
                    "cpu",
                    json!({"execution_limit":{"limit":"host_cpu","outcome":"held"}}),
                ),
                (
                    "capacity",
                    json!({"execution_limit":{"limit":"capacity","outcome":"held"}}),
                ),
                (
                    "memory warning",
                    json!({"execution_limit":{"limit":"memory","outcome":"running"}}),
                ),
                (
                    "engine probe observation unavailable; fact is unknown",
                    json!({}),
                ),
                ("needs attention", json!({})),
                (
                    "killed",
                    json!({"execution_limit":{"limit":"memory","outcome":"killed"}}),
                ),
                ("check", json!({"run_check":{"step":"work"}})),
            ] {
                sluice_store::messages::post(
                    tx,
                    sluice_store::messages::Post {
                        project: sluice_model::ids::ProjectSelector::Id(project),
                        speaker: sluice_store::messages::Speaker::Sluice,
                        body: body.into(),
                        verb: sluice_store::messages::Verb::Say {
                            to: "@peer".into(),
                            data: Some(data.try_into()?),
                        },
                    },
                    &sluice_store::messages::NoPlanInputs,
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let question = f
        .call(
            "ask",
            json!({"project":"p","to":"@peer","body":"Choose a crate?","me":"@other"}),
        )
        .await;
    f.call(
        "reply",
        json!({"project":"p","to_message":question["id"],"body":"Parser"}),
    )
    .await;
    let visible = f
        .tool(
            "next",
            json!({"projects":["p"],"since_seq":0,"timeout":0,"settle":1,"settle_max":0}),
        )
        .await;
    let bodies = |v: &Value| {
        v["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["body"].as_str())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        bodies(&visible),
        ["needs attention", "killed", "check", "Choose a crate?"]
    );
    let subscribed = f
        .call("next", json!({"subscription":"lanes","timeout":0}))
        .await;
    assert_eq!(bodies(&subscribed), bodies(&visible));
    assert_eq!(subscribed["suppressed"]["say"], 6);
    let complete = f.next(0, true).await;
    assert_eq!(bodies(&complete).len(), 10);
    // A failed land is routine only after its completion action really retried work.
    for (prefix, reason) in [
        ("conflict", "rebase onto origin/main conflicts in parser.rs"),
        (
            "slice",
            "not ready: the work step submitted ready=false. Unresolved: awaiting slice next",
        ),
    ] {
        let work = f.step(&format!("{prefix}-work")).await;
        f.finish(
            &work,
            PayloadResult::Succeeded(
                serde_json::from_value(json!({"value":1,"summary":"ready"})).unwrap(),
            ),
        )
        .await;
        let land = f.step(&format!("{prefix}-land")).await;
        let target = f
            .broker
            .writer()
            .write(sluice_store::RetrySafety::Idempotent, move |tx| {
                sluice_store::attempts::completion_target(
                    tx,
                    project,
                    work.identity.step.as_ref().unwrap(),
                )
            })
            .await
            .unwrap()
            .unwrap();
        f.broker
            .command(
                sluice_model::commands::CommandRequest::RegisterCompletionAction(
                    sluice_model::commands::RegisterCompletionAction {
                        project,
                        run: land.identity.run,
                        target,
                        message: reason.into(),
                        author: format!("step:{prefix}-land"),
                    },
                ),
            )
            .await
            .unwrap();
        f.finish(&land, PayloadResult::Rejected(reason.into()))
            .await;
    }
    let failed = f.step("unresolved-land").await;
    f.finish(
        &failed,
        PayloadResult::Rejected("rebase onto origin/main conflicts in parser.rs".into()),
    )
    .await;
    let handbacks = f.next(complete["last_seq"].as_i64().unwrap(), false).await;
    assert!(
        !handbacks["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "step.status"
                && (r["step"] == "conflict-land" || r["step"] == "slice-land"))
    );
    assert!(
        !bodies(&handbacks)
            .iter()
            .any(|b| b.starts_with("rebase onto origin/main") || b.starts_with("not ready:"))
    );
    assert!(
        handbacks["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["step"] == "unresolved-land" && r["to"] == "failed")
    );
    let restored = f.next(complete["last_seq"].as_i64().unwrap(), true).await;
    assert!(
        restored["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["step"] == "conflict-land")
    );
    // The option also applies to named subscriptions without changing their stored policy.
    f.call("subscription_set",json!({"name":"raw","projects":["p"],"wake":["say-to-me"],"include_routine":true,"me":"@peer"})).await;
    f.call("step_add",json!({"project":"p","step":"bypass","spec":{"run":"core.external","outputs":{"ok":"boolean"}}})).await;
    f.call(
        "step_bypass",
        json!({"project":"p","step":"bypass","outputs":{"ok":true},"reason":"approved"}),
    )
    .await;
    let raw_sub = f
        .call("next", json!({"subscription":"raw","timeout":0}))
        .await;
    assert!(
        raw_sub["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["data"]["step_bypass"]["by"] == "@peer")
    );
    let launch = f.step("work").await;
    f.call(
        "step_cancel",
        json!({"project":"p","steps":["work"],"reason":"peer requested"}),
    )
    .await;
    let filtered = f.next(complete["last_seq"].as_i64().unwrap(), false).await;
    assert!(
        !filtered["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "step.cancel")
    );
    // all=true is required only to select nonwaking records such as cancel requests.
    let all=f.call("next",json!({"projects":["p"],"since_seq":complete["last_seq"],"all":true,"include_routine":true,"timeout":0,"settle":1,"settle_max":0})).await;
    assert!(
        all["records"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "step.cancel")
    );
    f.finish(&launch, PayloadResult::Cancelled("peer requested".into()))
        .await;
    f.close().await;
}

/// A departed pipe consumer used to leave a quiet CLI waiting indefinitely, holding
/// its caller's inherited descriptors. Killing it must not obstruct a replacement.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_cli_exits_when_output_closes_and_a_killed_waiter_can_be_replaced() {
    use std::process::Stdio;
    let f = Fixture::new().await;
    f.call(
        "subscription_set",
        json!({"name":"all","wake":["ask"],"me":"@peer"}),
    )
    .await;
    let spawn = || {
        tokio::process::Command::new(env!("CARGO_BIN_EXE_sluice"))
            .env("SLUICE_HOME", f.home.path())
            .env("SLUICE_ME", "@peer")
            .env("TOKIO_WORKER_THREADS", "2")
            .env_remove("SLUICE_RUN_ID")
            .env_remove("SLUICE_PROJECT_ID")
            .env_remove("SLUICE_STEP")
            .args(["next", "--subscription", "all", "--timeout", "3600"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    let mut broken_pipe = spawn();
    drop(broken_pipe.stdout.take());
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        broken_pipe.wait_with_output(),
    )
    .await
    .expect("CLI retained a closed stdout pipe")
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    f.call(
        "say",
        json!({"project":"p","me":"@parent","to":"owner","body":"register parent fixture"}),
    )
    .await;
    f.call(
        "project_update",
        json!({"project":"p","me":"@parent","fallback":"@parent"}),
    )
    .await;
    // Keep the output reader alive while the shell parent exits: the parent pidfd,
    // rather than a broken pipe, must end this wait.
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut parent = tokio::process::Command::new("bash")
        .env("SLUICE_HOME", f.home.path())
        .env("SLUICE_ME", "@parent")
        .env("TOKIO_WORKER_THREADS", "2")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT_ID")
        .env_remove("SLUICE_STEP")
        .args([
            "-c",
            "\"$1\" next --all --timeout 3600 & child=$!; printf '%s\\n' \"$child\"; read -r line",
            "wait-parent",
            env!("CARGO_BIN_EXE_sluice"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(parent.stdout.take().unwrap()).lines();
    let pid: u32 = lines.next_line().await.unwrap().unwrap().parse().unwrap();
    // A live heartbeat proves that the child initialized before its parent leaves.
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let query = f.call("query", json!({"sql":"SELECT count(*) FROM readers WHERE identity='@parent' AND heartbeat_at IS NOT NULL"})).await;
            if query["rows"][0][0].as_i64().unwrap() > 0 { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let child = sluice_process::identity::OwnedProcess::capture(pid).unwrap();
    let exited = tokio::io::unix::AsyncFd::new(child.pidfd()).unwrap();
    drop(parent.stdin.take());
    assert!(!parent.wait().await.unwrap().success()); // read sees EOF
    let gone = tokio::time::timeout(std::time::Duration::from_secs(1), exited.readable()).await;
    if gone.is_err() {
        child
            .signal(sluice_process::identity::Signal::KILL)
            .unwrap();
    }
    let _ = gone.expect("CLI outlived its parent").unwrap();
    drop(lines);
    f.call(
        "say",
        json!({"project":"p","me":"@peer","to":"owner","body":"register replacement fixture"}),
    )
    .await;
    f.call("project_update", json!({"project":"p","fallback":"@peer"}))
        .await;
    let mut killed = spawn();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let query = f.call("query", json!({"sql":"SELECT count(*) FROM readers WHERE identity='@peer' AND heartbeat_at IS NOT NULL"})).await;
            if query["rows"][0][0].as_i64().unwrap() > 0 { break; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    killed.kill().await.unwrap();
    f.call("project_create", json!({"name":"q"})).await;
    f.call("next", json!({"projects":["q"],"me":"@peer","timeout":0}))
        .await;
    f.call(
        "ask",
        json!({"project":"q","me":"@sender","to":"@peer","body":"after kill"}),
    )
    .await;
    let batch = f
        .tool("next", json!({"subscription":"all","timeout":0}))
        .await;
    assert_eq!(batch["records"].as_array().unwrap().len(), 1);
    assert_eq!(batch["records"][0]["body"], "after kill");
    let empty = f
        .tool("next", json!({"subscription":"all","timeout":0}))
        .await;
    assert!(empty["records"].as_array().unwrap().is_empty());
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", f.home.path())
        .env("SLUICE_ME", "@peer")
        .env("TOKIO_WORKER_THREADS", "2")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT_ID")
        .env_remove("SLUICE_STEP")
        .args(["next", "--all", "--timeout", "0"])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("[id:") && text.contains("after kill"),
        "{text}"
    );
    f.close().await;
}
