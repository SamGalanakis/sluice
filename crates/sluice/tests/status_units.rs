//! `status` (SPEC §8) through the coordinator socket on a scratch home: done-unit folding,
//! selection, `brief`, and the units view's rows, state filter and refusals, over the MCP
//! flat arguments and `sluice tool status`.
#[allow(dead_code)]
#[path = "../../../tests/support/mod.rs"]
mod support;

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

/// The model a unit's agent step binds: the retired string for most, a model object for two,
/// so the units view shows both.
fn model(unit: &str) -> Value {
    match unit {
        "later" => json!({"type":"normal","model":"gpt-6-luna","effort":"max"}),
        "held" => {
            json!({"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high"},"sidekick":{"model":"swe-2"}})
        }
        _ => json!("sol"),
    }
}
/// Accepts every launch and never completes it: a launched step stays running.
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
            json!({"project":"p","to":"flow-work","body":"Which crate\n  owns   the parser?"}),
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
    assert_eq!(row("broke")["steps"], "work✓ land✗ rm·");
    assert_eq!(row("held")["state"], "blocked");
    assert_eq!(row("held")["steps"], "work‖ land‖ rm‖");
    assert_eq!(row("held")["blocked"], "paused");
    assert_eq!(row("wait")["state"], "blocked");
    assert_eq!(row("wait")["steps"], "work· land· rm·");
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
        row("wait")["line"].as_str().unwrap().contains(" ‖ "),
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
