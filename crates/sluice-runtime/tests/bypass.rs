//! `step_bypass` and `unit_add(bypass=…)` (SPEC §7.5): a pending step with a preset settles as
//! succeeded (manual) with the preset's outputs once ready, is never launched, and the steps
//! after it run in order.
#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_process::{
    guardian::FnHost,
    journal::{CleanupEvidence, CompletionJournal, PayloadResult},
};
use sluice_runtime::{
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
    scheduler::reconcile_project,
};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Fake {
    launches: Arc<Mutex<Vec<Launch>>>,
}
impl FnHost for Fake {
    async fn invoke(&self, i: FnInvocation) -> Result<JsonMap, PublicError> {
        sluice_runtime::builtins::core::dispatch(
            &i.name,
            &i.inputs,
            &sluice_runtime::builtins::jev::BuiltinCtx::new([]),
        )
        .await
        .map_err(|e| PublicError::FnFailure {
            message: e.to_string(),
        })
    }
}
impl sluice_process::guardian::AdoptionHost for Fake {
    async fn reconcile(
        &self,
        _: &sluice_process::guardian::AdoptionAttempt,
    ) -> std::io::Result<sluice_process::guardian::GuardianPresence> {
        Ok(sluice_process::guardian::GuardianPresence::Ambiguous(
            "fake host owns active work".into(),
        ))
    }
}
impl ExecutionHost for Fake {
    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, PublicError> {
        self.launches.lock().unwrap().push(launch);
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(&self, j: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(j.cleanup.iter().all(|p| p.empty))
    }
}

struct Fixture {
    home: ScratchHome,
    broker: Coordinator<Fake>,
    host: Fake,
    project: ProjectId,
}
impl Fixture {
    async fn new(steps: Value) -> Self {
        let home = ScratchHome::new().unwrap();
        assert!(home.root().exists());
        ScratchHome::validate(home.path()).unwrap();
        let host = Fake::default();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host.clone())
            .await
            .unwrap();
        let CommandReply::Project(p) = command(&broker, json!({"command":"project_create","args":{"name":"p","description":"","icon":null,"resources":{},"author":"test"}})).await.unwrap() else {
            panic!("project reply")
        };
        let f = Self {
            home,
            broker,
            host,
            project: p.project_id,
        };
        let ops: Vec<Value> = steps
            .as_object()
            .unwrap()
            .iter()
            .map(|(step, spec)| json!({"op":"step.add","step":step,"spec":spec}))
            .collect();
        if !ops.is_empty() {
            f.edit(json!(ops)).await.unwrap();
        }
        f.broker.acquire_scheduler("test".into()).await.unwrap();
        f
    }
    fn project(&self) -> Value {
        json!({"kind":"id","value":self.project})
    }
    async fn call(&self, name: &str, mut args: Value) -> Result<CommandReply, PublicError> {
        args["project"] = self.project();
        command(&self.broker, json!({"command":name,"args":args})).await
    }
    async fn edit(&self, ops: Value) -> Result<CommandReply, PublicError> {
        self.call(
            "plan_edit",
            json!({"ops":ops,"start":true,"dry_run":false,"reason":"test","author":"test"}),
        )
        .await
    }
    async fn bypass(&self, step: &str, args: Value) -> Result<Value, PublicError> {
        let mut args = args;
        args["step"] = json!(step);
        if args.get("author").is_none() {
            args["author"] = json!("orchestrator");
        }
        match self.call("step_bypass", args).await? {
            CommandReply::Data(v) => Ok(v.into_value()),
            other => panic!("step_bypass reply {other:?}"),
        }
    }
    async fn tick(&self) {
        reconcile_project(&self.broker, self.project, "test")
            .await
            .unwrap();
    }
    fn launched(&self) -> Vec<String> {
        self.host
            .launches
            .lock()
            .unwrap()
            .iter()
            .map(|l| l.identity.step.as_ref().unwrap().to_string())
            .collect()
    }
    fn launch(&self, step: &str) -> Launch {
        self.host
            .launches
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| l.identity.step.as_ref().unwrap().as_str() == step)
            .cloned()
            .unwrap_or_else(|| panic!("{step} was not launched"))
    }
    /// Finish the step's latest run with `value`, then let the runner admit what follows.
    async fn succeed(&self, step: &str, value: Value) {
        let launch = self.launch(step);
        self.broker
            .complete(journal(
                &launch,
                PayloadResult::Succeeded(map(json!({ "value": value }))),
            ))
            .await
            .unwrap();
        self.tick().await;
    }
    async fn fail(&self, step: &str) {
        let launch = self.launch(step);
        self.broker
            .complete(journal(
                &launch,
                PayloadResult::Failed(PublicError::FnFailure {
                    message: "broke".into(),
                }),
            ))
            .await
            .unwrap();
        self.tick().await;
    }
    async fn status(&self) -> Value {
        let CommandReply::Data(v) = self
            .call(
                "status",
                json!({"selection":{"steps":null,"tags":null},"all":true}),
            )
            .await
            .unwrap()
        else {
            panic!("status")
        };
        v.into_value()["steps"].clone()
    }
    /// The project's records of these kinds, as the wire has them.
    async fn records(&self, kinds: &[&str]) -> Vec<Value> {
        let CommandReply::Records(page) = self
            .call(
                "log_read",
                json!({"since_seq":null,"kinds":kinds,"threads":null,"limit":200}),
            )
            .await
            .unwrap()
        else {
            panic!("records")
        };
        page.records
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect()
    }
    /// The bypass notes: each `say` from `sluice` carrying `data.step_bypass`, as that object
    /// with the record's `seq` and the note's `to`.
    async fn notes(&self) -> Vec<Value> {
        self.records(&["message"])
            .await
            .into_iter()
            .filter(|r| r["from"] == "sluice" && r["data"]["step_bypass"].is_object())
            .map(|r| {
                let mut note = r["data"]["step_bypass"].clone();
                note["seq"] = r["seq"].clone();
                note["to"] = r["to"].clone();
                note["verb"] = r["verb"].clone();
                note
            })
            .collect()
    }
    async fn reply(&self, name: &str, args: Value) -> Value {
        serde_json::to_value(self.call(name, args).await.unwrap()).unwrap()
    }
    fn recipe(&self, name: &str, steps: Value) {
        let dir = self.home.path().join("recipes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec(&json!({"name":name,"steps":steps})).unwrap(),
        )
        .unwrap();
    }
}
async fn command(b: &Coordinator<Fake>, v: Value) -> Result<CommandReply, PublicError> {
    b.command(decode_json(&serde_json::to_vec(&v).unwrap()).unwrap())
        .await
}
fn map(v: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&v).unwrap()).unwrap()
}
fn journal(l: &Launch, result: PayloadResult) -> CompletionJournal {
    CompletionJournal {
        protocol: 1,
        identity: l.identity.clone(),
        completion_id: format!("complete-{}", l.identity.run),
        result,
        starts: vec![],
        exits: vec![],
        cleanup: vec![CleanupEvidence {
            cgroup: format!("fake/{}", l.identity.run),
            empty: true,
            escalated: false,
        }],
        submissions: JsonMap::default(),
        submission_version: None,
        delivery_acks: vec![],
    }
}
/// These fields of a record.
fn pick(record: &Value, keys: &[&str]) -> Value {
    keys.iter()
        .map(|k| (k.to_string(), record[*k].clone()))
        .collect::<serde_json::Map<_, _>>()
        .into()
}
fn invalid(error: PublicError) -> (String, Vec<String>) {
    match error {
        PublicError::Invalid { message, errors } => (message, errors),
        other => panic!("expected invalid, got {other:?}"),
    }
}

/// A lane's tail: land, landed, then close reading landed, then rm gated on close.
fn lane(prefix: &str) -> Value {
    let id = |s: &str| format!("{prefix}{s}");
    json!({
        id("land"): {"run":"fixture.echo","in":{"value":{"default":1}}},
        id("landed"): {"run":"fixture.echo","in":{"value":{"source":format!("{}/value", id("land"))}}},
        id("close"): {"run":"fixture.echo","in":{"value":{"source":format!("{}/value", id("landed"))}}},
        id("rm"): {"run":"fixture.echo","in":{"value":{"default":2}},"after":[id("close")]},
    })
}
/// The recipe form of `lane`.
fn lane_recipe() -> Value {
    json!({
        "{unit}-land": {"run":"fixture.echo","in":{"value":{"default":1}}},
        "{unit}-landed": {"run":"fixture.echo","in":{"value":{"source":"{unit}-land/value"}}},
        "{unit}-close": {"run":"fixture.echo","in":{"value":{"source":"{unit}-landed/value"}}},
        "{unit}-rm": {"run":"fixture.echo","in":{"value":{"default":2}},"after":["{unit}-close"]},
    })
}

/// Drive a lane through land and landed; close must never launch and rm must run after it.
async fn run_lane(f: &Fixture, prefix: &str) {
    let id = |s: &str| format!("{prefix}{s}");
    f.tick().await;
    assert_eq!(f.launched(), [id("land")]);
    f.succeed(&id("land"), json!(1)).await;
    assert_eq!(f.launched(), [id("land"), id("landed")]);
    let status = f.status().await;
    assert_eq!(status[id("close")]["status"], "pending");
    assert_eq!(status[id("rm")]["status"], "pending");
    f.succeed(&id("landed"), json!(1)).await;
    assert_eq!(f.launched(), [id("land"), id("landed"), id("rm")]);
    let status = f.status().await;
    assert_eq!(status[id("close")]["status"], "succeeded");
    assert_eq!(status[id("close")]["manual"], 1);
    assert_eq!(status[id("close")]["outputs"], json!({"value":"left open"}));
    assert!(status[id("close")].get("bypass").is_none());
    assert_eq!(status[id("rm")]["status"], "running");
    f.succeed(&id("rm"), json!(2)).await;
    assert_eq!(f.status().await[id("rm")]["status"], "succeeded");
}

#[tokio::test]
async fn a_bypassed_pending_close_never_launches_and_rm_runs_after_landed() {
    let f = Fixture::new(lane("")).await;
    let reply = f
        .bypass(
            "close",
            json!({"outputs":{"value":"left open"},"reason":"red fix: the ticket stays open"}),
        )
        .await
        .unwrap();
    assert_eq!(reply["status"], "pending");
    assert_eq!(reply["applied"], false);
    assert_eq!(reply["bypass"]["outputs"], json!({"value":"left open"}));
    assert_eq!(reply["bypass"]["author"], "orchestrator");
    let waiting = &f.status().await["close"];
    assert_eq!(
        waiting["bypass"]["reason"],
        "red fix: the ticket stays open"
    );
    run_lane(&f, "").await;
    // The log: who bypassed it and why (a say from sluice, to owner as the author is no
    // session), then the output and status records of the settle.
    let notes = f.notes().await;
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(
        pick(
            &notes[0],
            &["step", "action", "by", "reason", "outputs", "to", "verb"]
        ),
        json!({"step":"close","action":"set","by":"orchestrator",
            "reason":"red fix: the ticket stays open","outputs":{"value":"left open"},
            "to":"owner","verb":"say"})
    );
    let outputs = f.records(&["step.output"]).await;
    let [output] = &outputs[..] else {
        panic!("{outputs:?}")
    };
    assert_eq!(
        pick(output, &["kind", "step", "author", "outputs", "force"]),
        json!({"kind":"step.output","step":"close","author":"orchestrator",
            "outputs":{"value":"left open"},"force":false})
    );
    assert!(notes[0]["seq"].as_i64() < output["seq"].as_i64());
    let settled = f.records(&["step.status"]).await;
    assert!(settled.iter().any(|r| r["step"] == "close"
        && r["from"] == "pending"
        && r["to"] == "succeeded"
        && r["run_ids"] == json!([])));
    assert!(
        !settled
            .iter()
            .any(|r| r["step"] == "close" && r["to"] == "running")
    );
}

#[tokio::test]
async fn unit_add_with_bypass_drops_the_step_atomically_with_the_add() {
    let f = Fixture::new(json!({})).await;
    f.recipe("lane", lane_recipe());
    let CommandReply::Edit(added) = f
        .call(
            "unit_add",
            json!({"recipe":"lane","unit":"x","params":{},"after":{},
                "bypass":{"close":{"value":"left open"}},
                "edit":{"expected":null,"reason":"red fix lane","author":"orchestrator"}}),
        )
        .await
        .unwrap()
    else {
        panic!("edit reply")
    };
    let records = f.records(&["plan.edit"]).await;
    let [.., edit] = &records[..] else {
        panic!("{records:?}")
    };
    assert_eq!(
        pick(edit, &["kind", "rev"]),
        json!({"kind":"plan.edit","rev":added.rev})
    );
    let notes = f.notes().await;
    let [note] = &notes[..] else {
        panic!("{notes:?}")
    };
    assert_eq!(
        pick(note, &["step", "action", "by", "reason", "rev"]),
        json!({"step":"x-close","action":"set","by":"orchestrator","reason":"red fix lane",
            "rev":added.rev})
    );
    assert!(note["seq"].as_i64() > edit["seq"].as_i64());
    assert_eq!(
        f.status().await["x-close"]["bypass"]["outputs"],
        json!({"value":"left open"})
    );
    run_lane(&f, "x-").await;
    // plan_edit's unit.add takes the same, by the step's id as well as its suffix.
    f.edit(json!([{"op":"unit.add","recipe":"lane","unit":"y","bypass":{"y-close":{"value":"left open"}}}]))
        .await
        .unwrap();
    assert_eq!(f.status().await["y-close"]["bypass"]["author"], "test");
    // A key no step of the unit has, or outputs that do not fit, refuse the whole add.
    let before = f.status().await.as_object().unwrap().len();
    let (message, _) = invalid(
        f.call(
            "unit_add",
            json!({"recipe":"lane","unit":"z","params":{},"after":{},"bypass":{"shut":{"value":1}},
                "edit":{"expected":null,"reason":"","author":"orchestrator"}}),
        )
        .await
        .unwrap_err(),
    );
    assert_eq!(message, "bypass.shut: unit z has no step shut");
    let (message, errors) = invalid(
        f.call(
            "unit_add",
            json!({"recipe":"lane","unit":"z","params":{},"after":{},"bypass":{"close":{"valu":1}},
                "edit":{"expected":null,"reason":"","author":"orchestrator"}}),
        )
        .await
        .unwrap_err(),
    );
    assert_eq!(
        message,
        "bypass.close: outputs do not match step z-close's outputs"
    );
    assert_eq!(
        errors,
        [
            "bypass.close.value: required",
            "bypass.close.valu: undeclared output"
        ]
    );
    assert_eq!(
        f.status().await.as_object().unwrap().len(),
        before,
        "nothing was added"
    );
}

#[tokio::test]
async fn a_step_already_ready_settles_at_once_even_paused() {
    let f = Fixture::new(json!({
        "solo": {"run":"fixture.echo","in":{"value":{"default":1}}},
        "held": {"run":"fixture.echo","in":{"value":{"default":1}},"paused":true},
        "next": {"run":"fixture.echo","in":{"value":{"source":"solo/value"}},"after":["held"]},
    }))
    .await;
    let reply = f
        .bypass(
            "solo",
            json!({"outputs":{"value":7},"reason":"done by hand"}),
        )
        .await
        .unwrap();
    assert_eq!(reply["status"], "succeeded");
    assert_eq!(reply["applied"], true);
    assert_eq!(reply["bypass"], Value::Null);
    let reply = f
        .bypass("held", json!({"outputs":{"value":8},"reason":"not needed"}))
        .await
        .unwrap();
    assert_eq!(reply["status"], "succeeded");
    f.tick().await;
    assert_eq!(f.launched(), ["next"]);
    assert_eq!(f.launch("next").invocation.inputs.0["value"].as_value(), 7);
    let status = f.status().await;
    assert_eq!(status["solo"]["manual"], 1);
    assert_eq!(status["held"]["manual"], 1);
}

#[tokio::test]
async fn clearing_the_preset_lets_the_step_run() {
    let f = Fixture::new(lane("")).await;
    f.bypass(
        "close",
        json!({"outputs":{"value":"left open"},"reason":"maybe"}),
    )
    .await
    .unwrap();
    let reply = f
        .bypass("close", json!({"off":true,"reason":"close it after all"}))
        .await
        .unwrap();
    assert_eq!(reply["bypass"], Value::Null);
    assert_eq!(reply["applied"], false);
    assert!(f.status().await["close"].get("bypass").is_none());
    let (message, _) = invalid(
        f.bypass("close", json!({"off":true,"reason":"again"}))
            .await
            .unwrap_err(),
    );
    assert_eq!(message, "step close has no bypass to clear");
    f.tick().await;
    f.succeed("land", json!(1)).await;
    f.succeed("landed", json!(1)).await;
    assert_eq!(f.launched(), ["land", "landed", "close"]);
    f.succeed("close", json!("closed")).await;
    assert_eq!(f.launched(), ["land", "landed", "close", "rm"]);
    let cleared: Vec<_> = f
        .notes()
        .await
        .iter()
        .map(|r| pick(r, &["action", "outputs", "reason"]))
        .collect();
    assert_eq!(
        cleared,
        [
            json!({"action":"set","outputs":{"value":"left open"},"reason":"maybe"}),
            json!({"action":"clear","outputs":null,"reason":"close it after all"})
        ]
    );
}

#[tokio::test]
async fn outputs_are_checked_and_only_a_pending_step_is_bypassed() {
    let f = Fixture::new(json!({
        "cap": {"run":"fixture.capacity","after":["land"]},
        "land": {"run":"fixture.echo","in":{"value":{"default":1}}},
    }))
    .await;
    let (message, errors) = invalid(
        f.bypass("cap", json!({"reason":"none given"}))
            .await
            .unwrap_err(),
    );
    assert_eq!(message, "outputs do not match declarations");
    assert_eq!(errors, ["outputs.capacity: required"]);
    let (_, errors) = invalid(
        f.bypass(
            "cap",
            json!({"outputs":{"capacity":"four"},"reason":"wrong type"}),
        )
        .await
        .unwrap_err(),
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].starts_with("outputs.capacity:"), "{errors:?}");
    let (message, _) = invalid(
        f.bypass("cap", json!({"outputs":{"capacity":4},"reason":" "}))
            .await
            .unwrap_err(),
    );
    assert_eq!(message, "reason: required");
    assert!(f.notes().await.is_empty(), "a refusal records nothing");
    f.tick().await;
    let (message, _) = invalid(
        f.bypass("land", json!({"outputs":{"value":1},"reason":"too late"}))
            .await
            .unwrap_err(),
    );
    assert_eq!(
        message,
        "step land is running: step_bypass bypasses a pending step only; step_cancel stops it (step_settle settles a finishing one), then step_set_output gives it outputs"
    );
    f.succeed("land", json!(1)).await;
    let (message, _) = invalid(
        f.bypass("land", json!({"outputs":{"value":1},"reason":"too late"}))
            .await
            .unwrap_err(),
    );
    assert_eq!(
        message,
        "step land is succeeded: step_bypass bypasses a pending step only; step_set_output replaces its outputs; step_retry runs it again"
    );
}

#[tokio::test]
async fn a_preset_outlives_an_upstream_retry_and_is_applied_once() {
    let f = Fixture::new(lane("")).await;
    f.bypass(
        "close",
        json!({"outputs":{"value":"left open"},"reason":"skip it"}),
    )
    .await
    .unwrap();
    f.tick().await;
    f.succeed("land", json!(1)).await;
    f.fail("landed").await;
    f.call("step_retry", json!({"selection":{"steps":["landed"],"tags":null},"message":null,"reason":"again","author":"test"}))
        .await
        .unwrap();
    assert_eq!(f.status().await["close"]["bypass"]["reason"], "skip it");
    f.tick().await;
    f.succeed("landed", json!(1)).await;
    assert_eq!(f.launched(), ["land", "landed", "landed", "rm"]);
    assert_eq!(f.status().await["close"]["manual"], 1);
    // Applied once: a retry of the bypassed step runs it for real.
    f.call("step_retry", json!({"selection":{"steps":["close"],"tags":null},"message":null,"reason":"close it now","author":"test"}))
        .await
        .unwrap();
    f.tick().await;
    assert_eq!(f.launched().last().unwrap(), "close");
}

#[tokio::test]
async fn a_preset_leaves_the_plan_and_every_typed_reply_unchanged() {
    let f = Fixture::new(lane("")).await;
    f.recipe("lane", lane_recipe());
    let plan = f.reply("plan_get", json!({})).await;
    let step = f.reply("step_get", json!({"step":"close"})).await;
    let rows = f.reply("plan_read", json!({"steps":["close"]})).await;
    f.bypass(
        "close",
        json!({"outputs":{"value":"left open"},"reason":"keep"}),
    )
    .await
    .unwrap();
    assert_eq!(
        f.reply("plan_get", json!({})).await,
        plan,
        "the plan export"
    );
    assert_eq!(f.reply("step_get", json!({"step":"close"})).await, step);
    assert_eq!(f.reply("plan_read", json!({"steps":["close"]})).await, rows);
    // A unit added with a preset exports exactly as one added without it.
    f.call("unit_add", json!({"recipe":"lane","unit":"a","params":{},"after":{},"edit":{"expected":null,"reason":"r","author":"o"}})).await.unwrap();
    f.call("unit_add", json!({"recipe":"lane","unit":"b","params":{},"after":{},"bypass":{"close":{"value":1}},"edit":{"expected":null,"reason":"r","author":"o"}})).await.unwrap();
    let CommandReply::Plan(plan) = f.call("plan_get", json!({})).await.unwrap() else {
        panic!("plan")
    };
    let steps = &serde_json::to_value(&plan).unwrap()["plan"]["steps"];
    let renamed = serde_json::to_string(&steps["a-close"])
        .unwrap()
        .replace("a-", "b-")
        .replace("unit:a", "unit:b");
    assert_eq!(renamed, serde_json::to_string(&steps["b-close"]).unwrap());
}

/// The record kinds a release pinned at d0cd18f (the last main before `step_bypass`) decodes:
/// `KINDS` in that release's `crates/sluice-store/src/records.rs`. Its `Event` decoder
/// (`deny_unknown_fields`, one variant per kind) is the one this branch decodes with:
/// `crates/sluice-model/src/events.rs` is unchanged since d0cd18f.
const PINNED_KINDS: &[&str] = &[
    "plan.edit",
    "plan.input",
    "step.output",
    "step.retry",
    "step.cancel",
    "step.submit",
    "step.settle",
    "step.status",
    "step.lease",
    "step.queued",
    "call",
    "message",
    "project.pause",
    "project.archive",
    "project.update",
    "project.board",
    "project.rename",
    "project.delete",
    "project.capacity",
    "project.notify",
    "run.adopt",
    "run.orphan",
    "run.completion_action",
    "run.completion_action.register",
    "unit.settled",
];

/// A session's bypass note goes to that session (registering it) as a say, which wakes
/// nobody; and a log written by every bypass path holds only kinds a release pinned before
/// `step_bypass` has, each record decoding with the strict decoder, so an unfiltered
/// `log_read` (compat-check's) still reads it there.
#[tokio::test]
async fn bypass_notes_are_says_from_sluice_that_pinned_releases_decode() {
    let f = Fixture::new(lane("")).await;
    f.recipe("lane", lane_recipe());
    let set = json!({"outputs":{"value":"left open"},"reason":"red fix","author":"@lead"});
    f.bypass("close", set.clone()).await.unwrap();
    f.bypass(
        "close",
        json!({"off":true,"reason":"close it after all","author":"@lead"}),
    )
    .await
    .unwrap();
    f.bypass("close", set).await.unwrap();
    run_lane(&f, "").await;
    f.call(
        "unit_add",
        json!({"recipe":"lane","unit":"x","params":{},"after":{},
        "bypass":{"close":{"value":"left open"}},
        "edit":{"expected":null,"reason":"red fix lane","author":"@lead"}}),
    )
    .await
    .unwrap();
    let notes = f.notes().await;
    let seen: Vec<_> = notes
        .iter()
        .map(|n| pick(n, &["step", "action", "by", "to", "verb"]))
        .collect();
    assert_eq!(
        seen,
        [
            json!({"step":"close","action":"set","by":"@lead","to":"@lead","verb":"say"}),
            json!({"step":"close","action":"clear","by":"@lead","to":"@lead","verb":"say"}),
            json!({"step":"close","action":"set","by":"@lead","to":"@lead","verb":"say"}),
            json!({"step":"x-close","action":"set","by":"@lead","to":"@lead","verb":"say"}),
        ]
    );
    // Registered by its bypass, @lead can be addressed in the project.
    f.call(
        "say",
        json!({"to":"@lead","body":"seen","data":null,"run":null,"me":"@peer"}),
    )
    .await
    .unwrap();

    let CommandReply::Records(page) = f
        .call(
            "log_read",
            json!({"since_seq":null,"kinds":null,"threads":null,"limit":1000}),
        )
        .await
        .unwrap()
    else {
        panic!("records")
    };
    assert!(page.records.len() > 10);
    for record in &page.records {
        let mut wire = serde_json::to_value(record).unwrap();
        let kind = wire["kind"].as_str().unwrap().to_owned();
        assert!(PINNED_KINDS.contains(&kind.as_str()), "{kind} is new");
        let fields = wire.as_object_mut().unwrap();
        for key in ["seq", "at", "project"] {
            fields.remove(key);
        }
        let event: sluice_model::events::Event =
            serde_json::from_value(wire.clone()).unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(serde_json::to_value(&event).unwrap(), wire);
    }
}
