//! `status` (SPEC §8) through the coordinator socket on a scratch home: done-unit folding,
//! selection, `brief`, and the units view's rows, state filter and refusals, over the MCP
//! flat arguments and `sluice tool status`.
use crate::support;

use serde_json::{Value, json};
use sluice_model::{
    RuntimeApi,
    commands::{CommandRequest, StatusView, UnitState},
    error::PublicError,
    rpc::{FnInvocation, JsonMap},
};
use sluice_process::guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence};
use sluice_runtime::{
    client::CoordinatorClient,
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use sluice_web::mcp::{decode_tool, reply_value};
use std::time::Duration;
use support::home::ScratchHome;
use tokio_util::sync::CancellationToken;

// The model a unit's agent step binds: the retired string for most, a model object for two,
// so the units view shows both.
fn model(unit: &str) -> Value {
    match unit {
        "later" => json!({"type":"normal","model":"gpt-6-luna","effort":"max"}),
        "held" => {
            json!({"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high"},"sidekick":{"model":"swe-2"}})
        }
        _ => json!("sol"),
    }
}
// Accepts every launch and never completes it: a launched step stays running.
#[derive(Clone)]
struct Fake;
impl FnHost for Fake {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
impl AdoptionHost for Fake {
    async fn reconcile(&self, _: &AdoptionAttempt) -> std::io::Result<GuardianPresence> {
        Ok(GuardianPresence::Ambiguous("fixture".into()))
    }
}
impl ExecutionHost for Fake {
    async fn terminal_gone(
        &self,
        _: sluice_model::ids::RunId,
        _: bool,
    ) -> Result<bool, sluice_model::error::PublicError> {
        Ok(true)
    }

    async fn launch(&self, _: Launch) -> Result<LaunchOutcome, PublicError> {
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(
        &self,
        _: &sluice_process::journal::CompletionJournal,
    ) -> Result<bool, PublicError> {
        Ok(true)
    }
}

struct Fixture {
    home: ScratchHome,
    client: CoordinatorClient,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
impl Fixture {
    async fn new() -> Self {
        let home = ScratchHome::new().unwrap();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
            .await
            .unwrap();
        // The served scheduler launches ready steps once someone owns it.
        broker.acquire_scheduler("test".into()).await.unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let server = tokio::spawn(async move { broker.serve(token).await });
        let client = CoordinatorClient::new(home.path());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !client.path.exists() {
            assert!(tokio::time::Instant::now() < deadline, "socket never bound");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let dir = home.path().join("recipes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lane.json"),
            json!({"name":"lane","params":{"model":"Any"},"steps":{
                "{unit}-work":{"run":"fixture.submit","needs":{"lane":1},"in":{
                    "value":{"default":"{unit}"},"engine":{"default":"codex"},
                    "model":{"default":"{model}"},"effort":{"default":"xhigh"}}},
                "{unit}-land":{"run":"fixture.echo","in":{"value":{"source":"{unit}-work/value"}},"tags":["exit"]},
                "{unit}-rm":{"run":"core.external","outputs":{"done":"boolean"},"after":["{unit}-land?"]}
            }})
            .to_string(),
        )
        .unwrap();
        let f = Self {
            home,
            client,
            stop,
            server,
        };
        f.ok(
            "project_create",
            json!({"name":"p","description":"","resources":{"lane":1}}),
        )
        .await;
        f
    }
    async fn call(&self, name: &str, args: Value) -> Result<Value, PublicError> {
        let Value::Object(args) = args else {
            panic!("args")
        };
        let request = decode_tool(name, args, Some("test"))?;
        reply_value(self.client.command(request).await?)
    }
    async fn ok(&self, name: &str, args: Value) -> Value {
        self.call(name, args)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e:?}"))
    }
    async fn unit(&self, unit: &str, start: bool, after: Value) {
        self.ok(
            "unit_add",
            json!({"project":"p","recipe":"lane","unit":unit,"params":{"model":model(unit)},
                   "start":start,"after":after,"reason":"test"}),
        )
        .await;
    }
    /// Sets a step's outputs by hand; `force` reaches a paused step too.
    async fn set(&self, step: &str, outputs: Value) {
        self.ok(
            "step_set_output",
            json!({"project":"p","step":step,"outputs":outputs,"force":true,"reason":"test"}),
        )
        .await;
    }
    async fn status(&self, args: Value) -> Value {
        let mut args = args;
        args["project"] = json!("p");
        self.ok("status", args).await
    }
    /// Polls the steps view (all) until `step` has `status`.
    async fn wait_for(&self, step: &str, status: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let now = self.status(json!({"all":true})).await;
            if now["steps"][step]["status"] == status {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{step} never {status}: {now:#}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}

fn units_by_name(view: &Value) -> serde_json::Map<String, Value> {
    view["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["unit"].as_str().unwrap().to_owned(), row.clone()))
        .collect()
}

fn names(view: &Value) -> Vec<String> {
    let mut names: Vec<_> = units_by_name(view).keys().cloned().collect();
    names.sort();
    names
}

fn cli(home: &std::path::Path, args: &str) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home)
        .env("TOKIO_WORKER_THREADS", "2")
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_AUTHOR")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT")
        .args(["tool", "status", args])
        .output()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_folds_done_units_cuts_briefly_and_shows_unit_rows() {
    let f = Fixture::new().await;
    // flow: its work runs and holds the one lane; it asks a question.
    f.unit("flow", true, json!({})).await;
    f.wait_for("flow-work", "running").await;
    let question = f
        .ok(
            "ask",
            json!({"project":"p","to":"flow-work","body":"Which crate\n  owns   the parser?","me":"@orch"}),
        )
        .await;
    // later: ready, but the lane is held.
    f.unit("later", true, json!({})).await;
    // done: added paused, every step set by hand.
    f.unit("done", false, json!({})).await;
    let long = "x".repeat(300);
    f.set("done-work", json!({"value": long})).await;
    f.set("done-land", json!({"value": 1})).await;
    f.set("done-rm", json!({"done": true})).await;
    // held: added paused, nothing set.
    f.unit("held", false, json!({})).await;
    // broke: its land is gated on work's value, which is not a boolean, so it fails.
    f.unit("broke", true, json!({"land":["broke-work/value"]}))
        .await;
    f.set("broke-work", json!({"value": 5})).await;
    f.wait_for("broke-land", "failed").await;
    // wait: gated on unit broke, whose exit failed.
    f.unit("wait", true, json!({"*":["unit:broke"]})).await;

    // The steps view leaves the done unit out and counts it.
    let steps = f.status(json!({})).await;
    assert_eq!(steps["done_units"], json!({"units":1,"steps":3}));
    assert_eq!(steps["paused"], false);
    let ids: Vec<_> = steps["steps"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert!(!ids.iter().any(|id| id.starts_with("done-")), "{ids:?}");
    assert_eq!(ids.len(), 15);
    assert_eq!(
        steps["steps"]["later-work"]["waiting"],
        json!(["queued: needs lane 1 (1/1 held)"])
    );
    // all: every step, no count; brief cuts the long output.
    let every = f.status(json!({"all":true})).await;
    assert!(every.get("done_units").is_none());
    assert_eq!(every["steps"].as_object().unwrap().len(), 18);
    assert_eq!(every["steps"]["done-work"]["outputs"]["value"], json!(long));
    let brief = f.status(json!({"all":true,"brief":true})).await;
    assert_eq!(
        brief["steps"]["done-work"]["outputs"]["value"],
        json!(format!("{}… [100 more characters]", "x".repeat(200)))
    );
    assert_eq!(brief["steps"]["broke-work"]["outputs"]["value"], 5);
    // A selection returns what it selects, done or not, and counts nothing.
    let selected = f.status(json!({"steps":"done-work"})).await;
    assert_eq!(
        selected["steps"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["done-work"]
    );
    assert!(selected.get("done_units").is_none());
    let missing = f
        .call("status", json!({"project":"p","steps":["nope"]}))
        .await
        .unwrap_err();
    assert!(
        matches!(missing, PublicError::NotFound { .. }),
        "{missing:?}"
    );

    // The units view: one row per unit, the done one folded.
    let view = f.status(json!({"view":"units"})).await;
    assert_eq!(view["done_units"], json!({"units":1,"steps":3}));
    assert_eq!(view["resources"]["lane"]["held"], 1);
    assert!(view.get("steps").is_none());
    let rows = units_by_name(&view);
    assert_eq!(names(&view), ["broke", "flow", "held", "later", "wait"]);
    let row = |name: &str| rows[name].clone();
    assert_eq!(row("flow")["state"], "running");
    assert_eq!(row("flow")["steps"], "work▶ land· rm·");
    assert_eq!(row("flow")["engine"], "codex·sol·xhigh");
    assert_eq!(row("later")["engine"], "codex·gpt-6-luna·max");
    assert_eq!(row("held")["engine"], "codex·fusion·high");
    assert_eq!(row("flow")["blocked"], "");
    assert_eq!(
        row("flow")["last"],
        format!("#{} Q: Which crate owns the parser?", question["id"])
    );
    assert!(
        row("flow")["line"]
            .as_str()
            .unwrap()
            .contains(&format!("#{} Q:", question["id"]))
    );
    assert!(row("flow")["age"].as_i64().unwrap() >= 0);
    assert_eq!(row("later")["state"], "queued");
    assert_eq!(row("later")["steps"], "work≡ land· rm·");
    assert_eq!(row("later")["blocked"], "queued: needs lane 1 (1/1 held)");
    assert_eq!(row("broke")["state"], "failed");
    assert_eq!(row("broke")["steps"], "work✓ land✗ rm⊖");
    assert_eq!(row("held")["state"], "blocked");
    assert_eq!(row("held")["steps"], "work‖ land‖ rm‖");
    assert_eq!(row("held")["blocked"], "paused");
    assert_eq!(row("wait")["state"], "blocked");
    assert_eq!(row("wait")["steps"], "work⊖ land⊖ rm⊖");
    assert_eq!(
        row("wait")["blocked"],
        "after unit:broke (exit broke-land failed)"
    );
    for (name, row) in &rows {
        let line = row["line"].as_str().unwrap();
        assert!(line.chars().count() <= 80, "{line}");
        assert!(line.starts_with(name.as_str()), "{line}");
    }
    let flow_line = row("flow")["line"].as_str().unwrap().to_owned();
    assert!(flow_line.contains(" ▶ "), "{flow_line}");
    assert!(flow_line.contains("codex·sol·xhigh"), "{flow_line}");
    assert!(
        flow_line.contains(&format!("\"#{} Q: Which", question["id"])),
        "{flow_line}"
    );
    assert!(
        row("wait")["line"].as_str().unwrap().contains(" ⊖ "),
        "{}",
        row("wait")["line"]
    );
    // Oldest first, unknown ages last.
    let ages: Vec<Option<i64>> = view["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["age"].as_i64())
        .collect();
    let known: Vec<i64> = ages.iter().map_while(|a| *a).collect();
    assert!(known.windows(2).all(|w| w[0] >= w[1]), "{ages:?}");
    assert!(ages[known.len()..].iter().all(Option::is_none), "{ages:?}");

    // all shows the done unit settled; a selection shows the units it touches.
    let every = f.status(json!({"view":"units","all":true})).await;
    assert!(every.get("done_units").is_none());
    let done = units_by_name(&every)["done"].clone();
    assert_eq!(done["state"], "settled");
    assert_eq!(done["steps"], "work✓ land✓ rm✓");
    let tagged = f.status(json!({"view":"units","tags":["unit:held"]})).await;
    assert_eq!(names(&tagged), ["held"]);
    let by_step = f
        .status(json!({"view":"units","steps":["done-rm","flow-land"]}))
        .await;
    assert_eq!(names(&by_step), ["done", "flow"]);
    // state filters, one or a list.
    let blocked = f.status(json!({"view":"units","state":"blocked"})).await;
    assert_eq!(names(&blocked), ["held", "wait"]);
    let busy = f
        .status(json!({"view":"units","state":["running","queued"]}))
        .await;
    assert_eq!(names(&busy), ["flow", "later"]);

    // Refusals.
    for args in [
        json!({"project":"p","view":"units","brief":true}),
        json!({"project":"p","state":"blocked"}),
        json!({"project":"p","view":"units","state":"nope"}),
        json!({"project":"p","view":"rows"}),
    ] {
        let error = f.call("status", args.clone()).await.unwrap_err();
        assert!(
            matches!(error, PublicError::BadRequest { .. }),
            "{args}: {error:?}"
        );
    }
    let Ok(CommandRequest::Status(query)) = decode_tool(
        "status",
        json!({"project":"p","view":"units","state":"failed"})
            .as_object()
            .unwrap()
            .clone(),
        None,
    ) else {
        panic!("status decodes")
    };
    assert_eq!(query.view, StatusView::Units);
    assert_eq!(query.state, Some(vec![UnitState::Failed]));
    assert!(!query.all && !query.brief);

    // `sluice tool status` takes the same flat arguments over the same socket.
    let home = f.home.path().to_owned();
    let (units, filtered, refused) = tokio::task::spawn_blocking(move || {
        (
            cli(&home, r#"{"project":"p","view":"units"}"#),
            cli(&home, r#"{"project":"p","view":"units","state":"failed"}"#),
            cli(&home, r#"{"project":"p","view":"units","brief":true}"#),
        )
    })
    .await
    .unwrap();
    assert!(
        units.status.success(),
        "{}",
        String::from_utf8_lossy(&units.stderr)
    );
    let printed: Value = serde_json::from_slice(&units.stdout).unwrap();
    eprintln!(
        "sluice tool status '{{\"project\":\"p\",\"view\":\"units\"}}':\n{}",
        String::from_utf8_lossy(&units.stdout)
    );
    assert_eq!(names(&printed), names(&view));
    assert_eq!(printed["done_units"], json!({"units":1,"steps":3}));
    let filtered: Value = serde_json::from_slice(&filtered.stdout).unwrap();
    assert_eq!(names(&filtered), ["broke"]);
    assert_eq!(refused.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&refused.stderr).unwrap();
    assert_eq!(error["error"], "bad_request");
    f.close().await;
}

/// A coordinator used to expose run ids without any start or activity time, making a
/// long-running worker impossible to audit without decoding ids and opening transcripts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_running_and_step_get_report_stored_latest_run_times_and_transcript_costs() {
    let f = Fixture::new().await;
    f.unit("flow", true, json!({})).await;
    f.wait_for("flow-work", "running").await;
    f.unit("pending", false, json!({})).await;
    let initial = f.status(json!({"all":true})).await;
    let run = initial["steps"]["flow-work"]["run_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    // Fixture timestamps deliberately disagree with the run id. This pins the stored-time
    // rule and makes the elapsed assertion independent of scheduling and wall-clock sleeps.
    let sql = rusqlite::Connection::open(f.home.path().join("sluice.db")).unwrap();
    sql.execute("UPDATE runs SET created_at='2026-10-01T00:00:00Z',started_at='2026-10-01T00:00:00Z' WHERE run_id=?1",[&run]).unwrap();
    let dir = f.home.path().join("runs").join(&run);
    std::fs::create_dir_all(&dir).unwrap();
    let session = "01a11b9b-1eaf-7db2-9d93-c35331e86f78";
    std::fs::write(
        dir.join("native.json"),
        json!({"engine":"codex","session":session}).to_string(),
    )
    .unwrap();
    std::fs::write(dir.join("agent-observation.json"),json!({"engine":"codex","model":"gpt-6.1 xhigh","last_activity_ms":1790812890000u64,"last_message":"Working on the timing audit. ".repeat(12)}).to_string()).unwrap();
    let transcripts = f
        .home
        .path()
        .join("codex-native-homes")
        .join(session)
        .join("sessions/2026/10/01");
    std::fs::create_dir_all(&transcripts).unwrap();
    let transcript = transcripts.join(format!("rollout-fixture-{session}.jsonl"));
    std::fs::write(
        &transcript,
        include_str!("../../../sluice-agents/tests/fixtures/activity/timing-codex.jsonl"),
    )
    .unwrap();
    let step = f
        .ok("step_get", json!({"project":"p","step":"flow-work"}))
        .await["step"]
        .clone();
    assert_eq!(step["started_at"], "2026-10-01T00:00:00Z");
    assert!(step["elapsed_s"].as_f64().unwrap() > 100.0);
    assert!(step["ended_at"].is_null());
    assert_eq!(step["last_activity_at"], "2026-10-01T00:01:30Z");
    let status = f.status(json!({})).await;
    assert_eq!(
        status["steps"]["flow-work"]["started_at"],
        step["started_at"]
    );
    let units = f.status(json!({"view":"units"})).await;
    assert_eq!(
        units_by_name(&units)["flow"]["timing"]["flow-work"]["started_at"],
        step["started_at"]
    );
    let running = f.status(json!({"view":"running"})).await;
    assert_eq!(running["running"].as_array().unwrap().len(), 1);
    assert_eq!(running["running"][0][0], "flow");
    assert_eq!(running["running"][0][1], "flow-work");
    assert_eq!(running["running"][0][2], "codex gpt-6.1 xhigh");
    assert!(running["running"][0][6].as_str().unwrap().chars().count() <= 120);
    assert!(
        running["running"][0][7]
            .as_str()
            .unwrap()
            .contains("cargo build")
    );
    // Codex can publish an activity timestamp with empty final words while working.
    // Keep that timestamp authoritative and recover its latest commentary from the rollout.
    let observation = std::fs::read(dir.join("agent-observation.json")).unwrap();
    let mut empty_words: Value = serde_json::from_slice(&observation).unwrap();
    empty_words["last_message"] = json!("");
    std::fs::write(dir.join("agent-observation.json"), empty_words.to_string()).unwrap();
    let speaking = f.status(json!({"view":"running"})).await;
    assert_eq!(speaking["running"][0][6], "second done");
    let speaking = f
        .ok("step_get", json!({"project":"p","step":"flow-work"}))
        .await;
    assert_eq!(
        speaking["step"]["last_activity_at"],
        step["last_activity_at"]
    );
    std::fs::write(dir.join("agent-observation.json"), observation).unwrap();
    let home = f.home.path().to_owned();
    let cli = tokio::task::spawn_blocking(move || {
        std::process::Command::new(env!("CARGO_BIN_EXE_sluice"))
            .env("SLUICE_HOME", home)
            .env("TOKIO_WORKER_THREADS", "2")
            .args(["status", "--running", "--project", "p"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    let cli: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(cli["running"][0][1], "flow-work");
    // An existing park remains visible even though it has released its scheduling slot.
    sql.execute("UPDATE attempts SET request=json_set(request,'$.park',json(?2)) WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?1)",(&run,json!({"condition":{"kind":"lease","resource":"land","amount":1},"phase":"parked","requested_at":"2026-10-01T00:01:30Z","parked_at":"2026-10-01T00:01:30Z"}).to_string())).unwrap();
    let parked = f.status(json!({"view":"running"})).await;
    assert_eq!(parked["running"].as_array().unwrap().len(), 1);
    assert!(parked["running"][0][5].as_str().unwrap().contains("land"));
    // The 92 KB lash status incident: 25 workers must fit one compact reply even with
    // long worker messages. Add the rest in one authored edit, then lay their fake runs.
    let ops: Vec<_> = (1..25).map(|n| json!({"op":"step.add","step":format!("lane-{n}-work"),"spec":{"run":"fixture.submit","paused":true,"tags":[format!("unit:lane-{n}")],"in":{"value":{"default":"work"}}}})).collect();
    f.ok(
        "plan_edit",
        json!({"project":"p","ops":ops,"reason":"timing fixture"}),
    )
    .await;
    for n in 1..25 {
        let step = format!("lane-{n}-work");
        let attempt = sluice_model::ids::AttemptId::new().to_string();
        let extra_run = sluice_model::ids::RunId::new().to_string();
        sql.execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) SELECT ?1,project_id,?2,1,1,'executing',request,inputs_hash,created_at FROM attempts WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?3)",(&attempt,&step,&run)).unwrap();
        sql.execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at,started_at) SELECT ?1,project_id,?2,?3,created_at,started_at FROM runs WHERE run_id=?4",(&extra_run,&attempt,&step,&run)).unwrap();
        sql.execute(
            "UPDATE steps SET status='running' WHERE step_id=?1",
            [&step],
        )
        .unwrap();
        let extra_dir = f.home.path().join("runs").join(&extra_run);
        std::fs::create_dir_all(&extra_dir).unwrap();
        for file in ["native.json", "agent-observation.json"] {
            std::fs::copy(dir.join(file), extra_dir.join(file)).unwrap();
        }
    }
    let compact = f.status(json!({"view":"running"})).await;
    assert_eq!(compact["running"].as_array().unwrap().len(), 25);
    assert!(
        compact.to_string().len() <= 6100,
        "{} bytes",
        compact.to_string().len()
    );
    sql.execute(
        "UPDATE steps SET status='succeeded' WHERE step_id LIKE 'lane-%'",
        [],
    )
    .unwrap();
    // Finish the fixture run at 100s; ended durations must freeze, including the idle gap.
    sql.execute(
        "UPDATE runs SET finished_at='2026-10-01T00:01:40Z' WHERE run_id=?1",
        [&run],
    )
    .unwrap();
    sql.execute(
        "UPDATE steps SET status='succeeded' WHERE step_id='flow-work'",
        [],
    )
    .unwrap();
    let ended = f
        .ok("step_get", json!({"project":"p","step":"flow-work"}))
        .await["step"]
        .clone();
    assert_eq!(ended["ended_at"], "2026-10-01T00:01:40Z");
    assert!((ended["elapsed_s"].as_f64().unwrap() - 100.0).abs() < 0.001);
    assert_eq!(ended["time"]["model_s"], 40.0);
    assert_eq!(ended["time"]["commands_s"], 20.0);
    assert_eq!(ended["time"]["tools_s"], 10.0);
    assert_eq!(ended["time"]["idle_s"], 30.0);
    assert!(
        f.status(json!({"view":"running"})).await["running"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    std::fs::remove_file(transcript).unwrap();
    let unreadable = f
        .ok(
            "step_get",
            json!({"project":"p","step":"flow-work","compact":true}),
        )
        .await;
    assert!(unreadable["step"].as_object().unwrap().contains_key("time"));
    assert!(unreadable["step"]["time"].is_null());
    // The public query relation offers the same derived stamps, without parsing ids.
    let query = sluice_store::query::query(f.home.path(),"SELECT started_at,ended_at,elapsed_s,last_activity_at FROM steps WHERE step_id='flow-work'",None,None).unwrap();
    let query: Value = serde_json::from_slice(query.encoded()).unwrap();
    assert_eq!(query["rows"][0][0], step["started_at"]);
    assert!((query["rows"][0][2].as_f64().unwrap() - 100.0).abs() < 0.001);
    f.close().await;
}
