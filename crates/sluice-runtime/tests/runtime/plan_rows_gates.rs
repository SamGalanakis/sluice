//! The release gates of `docs/design/plan-rows.md` §11, read from the cost counters
//! (`sluice_model::cost`) and the rows themselves, on a coordinator in process:
//!
//! - the same local edit at 6, 60 and 600 unrelated steps decodes and writes the
//!   same counts, and no ordinary edit on a warm cache exports or compiles the whole plan;
//! - removal and prune renumber nothing, and after each the index rows equal what the
//!   declarations derive (lane H's own derivation), so removing a source whose reader was
//!   removed is accepted;
//! - compact reads decode no declaration, and neither does the competitor read;
//! - a high-fanout edit scales with its readers, not with the plan;
//! - three stale preparations end in `busy` with nothing written, none on the writer's
//!   thread, while an independent write commits within its budget;
//! - cold compile and cold recipe matching are measured and reported;
//! - a reader of an edit's revision between its commit and the cache's install compiles
//!   nothing.
//!
//! Each measures the whole pipeline (the store's rows and counters, the model's preparation,
//! the runtime's cache and the tools). The counters are process-wide and count every
//! coordinator in the process, measured or not, so the gates take turns (`serial`): a
//! measurement (`cost::Measurement`) excludes other measurements, but not another gate's
//! unmeasured setup, whose first edit compiles its new, empty plan whole.
use super::home;
use super::index_plan_rows as plan_rows;

use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest},
    cost::{self, CommitPoint, Costs, Measurement, PreparationPoint},
    error::PublicError,
    ids::{ProjectId, Revision},
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_process::{
    guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence},
    journal::{CleanupEvidence, CompletionJournal, PayloadResult},
};
use sluice_runtime::{
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
    scheduler::reconcile_project,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// The independent small write's budget while an edit prepares (§11).
const INDEPENDENT_WRITE_BUDGET: Duration = Duration::from_millis(100);

/// One gate at a time in this process: every coordinator's work counts in the one set of
/// counters, so a gate's measurement would otherwise count another gate's setup.
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static GATES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    GATES.lock().await
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Vec<Launch>>>);
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
    async fn launch(&self, launch: Launch) -> Result<LaunchOutcome, PublicError> {
        self.0.lock().unwrap().push(launch);
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(&self, _: &CompletionJournal) -> Result<bool, PublicError> {
        Ok(true)
    }
}

struct Plan {
    home: home::ScratchHome,
    broker: Coordinator<Fake>,
    host: Fake,
    project: ProjectId,
}
fn request(command: &str, args: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&json!({"command": command, "args": args})).unwrap())
        .unwrap_or_else(|e| panic!("{command} {args}: {e}"))
}
/// A reply's object, whatever variant carries it.
fn payload(reply: CommandReply) -> Value {
    let value = serde_json::to_value(&reply).unwrap();
    match value.get("data") {
        Some(data) => data.clone(),
        None => value,
    }
}
fn step(value: Value) -> Value {
    json!({"run": "fixture.echo", "in": {"value": {"default": value}}})
}
impl Plan {
    async fn new(resources: Value) -> Self {
        let home = home::ScratchHome::new().unwrap();
        let host = Fake::default();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), host.clone())
            .await
            .unwrap();
        let reply = broker
            .command(request(
                "project_create",
                json!({"name": "gates", "description": "", "icon": null,
                    "resources": resources, "author": "test"}),
            ))
            .await
            .unwrap();
        let project = serde_json::from_value(payload(reply)["project_id"].clone()).unwrap();
        Self {
            home,
            broker,
            host,
            project,
        }
    }
    async fn call(&self, command: &str, mut args: Value) -> Result<Value, PublicError> {
        args["project"] = json!({"kind": "id", "value": self.project});
        self.broker
            .command(request(command, args))
            .await
            .map(payload)
    }
    async fn edit(&self, ops: Value) -> Value {
        self.call("plan_edit", json!({"ops": ops, "reason": "gate"}))
            .await
            .unwrap_or_else(|e| panic!("{ops}: {e}"))
    }
    async fn rev(&self) -> u64 {
        self.call("plan_get", json!({})).await.unwrap()["rev"]
            .as_u64()
            .unwrap()
    }
    /// The home's database, read beside the running coordinator.
    fn sql(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.home.path().join("sluice.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }
    fn check_indexes(&self, after: &str) {
        let sql = self.sql();
        plan_rows::check_indexes(&sql, &self.project.to_string())
            .unwrap_or_else(|d| panic!("after {after}: {d:#?}"));
        let dangling: i64 = sql
            .query_row(
                "SELECT count(*) FROM plan_refs r WHERE r.project_id=?1 AND (
                   (r.consumer_kind='step' AND NOT EXISTS (SELECT 1 FROM steps s
                      WHERE s.project_id=r.project_id AND s.step_id=r.consumer_id))
                   OR (r.consumer_kind='output' AND NOT EXISTS (SELECT 1 FROM plan_outputs o
                      WHERE o.project_id=r.project_id AND o.name=r.consumer_id)))",
                [self.project.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dangling, 0,
            "after {after}: no reference names a removed consumer"
        );
    }
    /// Launch what is ready and finish every launch as succeeded.
    async fn run_ready(&self) {
        let before = self.host.0.lock().unwrap().len();
        self.broker.acquire_scheduler("gate".into()).await.unwrap();
        reconcile_project(&self.broker, self.project, "gate")
            .await
            .unwrap();
        self.broker.release_scheduler("gate".into()).await.unwrap();
        let launches: Vec<Launch> = self.host.0.lock().unwrap()[before..].to_vec();
        for launch in launches {
            self.broker
                .complete(CompletionJournal {
                    protocol: 1,
                    identity: launch.identity.clone(),
                    completion_id: format!("c-{}", launch.identity.run),
                    result: PayloadResult::Succeeded(decode_json(br#"{"value":1}"#).unwrap()),
                    starts: vec![],
                    exits: vec![],
                    cleanup: vec![CleanupEvidence {
                        cgroup: format!("fake/{}", launch.identity.run),
                        empty: true,
                        escalated: false,
                    }],
                    submissions: JsonMap::default(),
                    submission_version: None,
                    delivery_acks: vec![],
                })
                .await
                .unwrap();
        }
    }
}

/// `n` unrelated steps, paused so nothing launches, as `step.add` operations.
fn filler(n: usize, prefix: &str) -> Vec<Value> {
    (0..n)
        .map(|i| {
            let mut spec = step(json!(i));
            spec["paused"] = json!(true);
            spec["tags"] = json!([format!("unit:{prefix}-{}", i / 6), "filler"]);
            json!({"op": "step.add", "step": format!("{prefix}-{i:05}"), "spec": spec})
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_the_same_local_edit_costs_the_same_at_198_1980_and_19800_steps() {
    let _serial = serial().await;
    let mut seen: Vec<(usize, &str, Costs)> = Vec::new();
    for size in [6, 60, 600] {
        let plan = Plan::new(json!({})).await;
        let mut ops = filler(size, "x");
        ops.push(json!({"op": "input.put", "name": "brief", "declaration": "string"}));
        ops.push(json!({"op": "step.add", "step": "source", "spec": step(json!("s"))}));
        ops.push(
            json!({"op": "step.add", "step": "probe", "spec": {"run": "fixture.echo",
            "paused": true, "in": {"value": {"source": "source/value"}}}}),
        );
        plan.edit(Value::Array(ops)).await;
        // Warm the cache: one read and one edit before measuring.
        plan.edit(json!([{"op": "step.update", "step": "probe", "changes": {"doc": "warm"}}]))
            .await;
        let measurement = Measurement::start();
        for (what, changes) in [
            ("metadata", json!({"doc": "measured"})),
            ("binding", json!({"in": {"value": {"source": "brief"}}})),
        ] {
            measurement.reset();
            plan.edit(json!([{"op": "step.update", "step": "probe", "changes": changes}]))
                .await;
            let costs = measurement.costs();
            assert_eq!(
                (costs.full_exports, costs.full_compiles),
                (0, 0),
                "{what} at {size}: no whole-plan export or compile on a warm cache: {costs:?}"
            );
            assert_eq!(costs.writer_preparations, 0);
            assert!(
                costs.declarations_written > 0 && costs.rows_written > 0,
                "{costs:?}"
            );
            seen.push((size, what, costs));
        }
        drop(measurement);
    }
    for what in ["metadata", "binding"] {
        let counts: Vec<(usize, u64, u64, u64)> = seen
            .iter()
            .filter(|(_, w, _)| *w == what)
            .map(|(size, _, c)| {
                (
                    *size,
                    c.declarations_decoded,
                    c.declarations_written,
                    c.rows_written,
                )
            })
            .collect();
        eprintln!("{what} edit (size, decoded, written, rows): {counts:?}");
        assert!(
            counts
                .windows(2)
                .all(|w| (w[0].1, w[0].2, w[0].3) == (w[1].1, w[1].2, w[1].3)),
            "{what}: the same counts at every size: {counts:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_no_ordinary_edit_on_a_warm_cache_exports_or_compiles_the_whole_plan() {
    let _serial = serial().await;
    let plan = Plan::new(json!({"cpu": 2})).await;
    let recipes = plan
        .home
        .path()
        .join("projects")
        .join(plan.project.to_string())
        .join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::write(
        recipes.join("lane.json"),
        json!({"name": "lane", "steps": {
            "{unit}-fork": step(json!("fork")),
            "{unit}-work": {"run": "fixture.echo", "after": ["{unit}-fork"],
                "in": {"value": {"source": "{unit}-fork/value"}}}}})
        .to_string(),
    )
    .unwrap();
    let mut ops = filler(600, "x");
    ops.push(json!({"op": "step.add", "step": "a", "spec": step(json!(1))}));
    plan.edit(Value::Array(ops)).await;
    plan.edit(json!([{"op": "step.update", "step": "a", "changes": {"doc": "warm"}}]))
        .await;
    let measurement = Measurement::start();
    for (what, ops) in [
        (
            "step.add",
            json!([{"op": "step.add", "step": "b", "spec": {"run": "fixture.echo",
            "paused": true, "in": {"value": {"source": "a/value"}}}}]),
        ),
        (
            "edge.add",
            json!([{"op": "edge.add", "step": "b", "after": ["a"]}]),
        ),
        (
            "step.update",
            json!([{"op": "step.update", "step": "b", "changes": {"priority": 3}}]),
        ),
        (
            "needs",
            json!([{"op": "step.update", "step": "b", "changes": {"needs": {"cpu": 1}}}]),
        ),
        (
            "input.put",
            json!([{"op": "input.put", "name": "repo", "declaration": "string"}]),
        ),
        (
            "output.put",
            json!([{"op": "output.put", "name": "out", "source": "b/value"}]),
        ),
        (
            "unit.add",
            json!([{"op": "unit.add", "recipe": "lane", "unit": "fig-1",
            "after": {"*": ["a"]}}]),
        ),
        (
            "unit.update",
            json!([{"op": "unit.update", "unit": "fig-1",
            "changes": {"fig-1-work": {"doc": "w"}}}]),
        ),
        (
            "output.remove",
            json!([{"op": "output.remove", "name": "out"}]),
        ),
        (
            "step.remove",
            json!([{"op": "step.remove", "steps": ["b"]}]),
        ),
        (
            "unit.remove",
            json!([{"op": "unit.remove", "unit": "fig-1"}]),
        ),
    ] {
        measurement.reset();
        plan.edit(ops).await;
        let costs = measurement.costs();
        assert_eq!(
            (
                costs.full_exports,
                costs.full_compiles,
                costs.writer_preparations
            ),
            (0, 0, 0),
            "{what}: {costs:?}"
        );
        assert_eq!(costs.positions_renumbered, 0, "{what}: {costs:?}");
        plan.check_indexes(what);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_removal_and_prune_renumber_nothing_and_leave_exactly_the_derived_indexes() {
    let _serial = serial().await;
    let plan = Plan::new(json!({})).await;
    plan.edit(json!([
        {"op": "step.add", "step": "s1", "spec": step(json!(1))},
        {"op": "step.add", "step": "r1", "spec": {"run": "fixture.echo",
            "in": {"value": {"source": "s1/value"}}, "after": ["s1"]}},
        {"op": "step.add", "step": "s2", "spec": step(json!(2))},
        {"op": "step.add", "step": "r2", "spec": {"run": "fixture.echo",
            "in": {"value": {"source": ["s2/value", "s1/value"]}}, "after": ["s2?"]}},
        {"op": "step.add", "step": "s3", "spec": step(json!(3))},
        {"op": "step.add", "step": "u-a", "spec": {"run": "fixture.echo", "tags": ["unit:u"],
            "in": {"value": {"source": "s3/value"}}}},
        {"op": "step.add", "step": "u-b", "spec": {"run": "fixture.echo", "tags": ["unit:u"],
            "after": ["u-a"], "in": {"value": {"source": "u-a/value"}}}},
        {"op": "step.add", "step": "s4", "spec": step(json!(4))},
        {"op": "step.add", "step": "v-a", "spec": {"run": "fixture.echo", "tags": ["unit:v"],
            "in": {"value": {"source": "s4/value"}}}},
        {"op": "step.add", "step": "tail", "spec": step(json!(5))},
        {"op": "output.put", "name": "out", "source": "r1/value"}
    ]))
    .await;
    plan.check_indexes("the seed");
    let positions = |plan: &Plan| -> Vec<(String, i64)> {
        let sql = plan.sql();
        let mut q = sql
            .prepare("SELECT step_id, position FROM steps WHERE project_id=?1 ORDER BY position")
            .unwrap();
        q.query_map([plan.project.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let measurement = Measurement::start();
    let check = |plan: &Plan, what: &str, before: Vec<(String, i64)>| {
        let costs = measurement.costs();
        assert_eq!(costs.positions_renumbered, 0, "{what}: {costs:?}");
        let after = positions(plan);
        for (step, position) in &after {
            let kept = before.iter().find(|(s, _)| s == step).map(|(_, p)| *p);
            assert_eq!(kept, Some(*position), "{what}: {step} keeps its position");
        }
        plan.check_indexes(what);
        measurement.reset();
    };
    // A reader removed, then the source it read: accepted, its references gone with it.
    let before = positions(&plan);
    plan.edit(
        json!([{"op": "output.remove", "name": "out"}, {"op": "step.remove", "steps": ["r1"]}]),
    )
    .await;
    check(&plan, "step.remove of a reader", before);
    let before = positions(&plan);
    plan.edit(json!([{"op": "step.update", "step": "r2", "changes":
        {"in": {"value": {"source": "s2/value"}}}}]))
        .await;
    check(&plan, "a step.update dropping a reference", before);
    let before = positions(&plan);
    plan.edit(json!([{"op": "step.remove", "steps": ["s1"]}]))
        .await;
    check(
        &plan,
        "step.remove of the source a removed reader read",
        before,
    );
    let before = positions(&plan);
    plan.edit(json!([{"op": "step.update", "step": "r2", "changes":
        {"in": {"value": {"default": 0}}, "after": null}}]))
        .await;
    plan.edit(json!([{"op": "step.remove", "steps": ["s2"]}]))
        .await;
    check(&plan, "a removing step.update chain", before);
    // A unit removed, then the source its steps read.
    let before = positions(&plan);
    plan.call("unit_remove", json!({"unit": "u", "reason": "gate"}))
        .await
        .unwrap();
    check(&plan, "unit_remove", before);
    let before = positions(&plan);
    plan.edit(json!([{"op": "step.remove", "steps": ["s3"]}]))
        .await;
    check(&plan, "step.remove of the removed unit's source", before);
    // A done unit pruned, then the source it read.
    plan.run_ready().await;
    plan.run_ready().await;
    let before = positions(&plan);
    let pruned = plan
        .call(
            "plan_prune",
            json!({"units": ["v"], "tags": null, "older_than_seconds": 0,
                "edit": {"dry_run": false, "reason": "gate"}}),
        )
        .await
        .unwrap();
    assert_eq!(pruned["units"], json!(["v"]), "{pruned}");
    check(&plan, "plan_prune", before);
    let before = positions(&plan);
    plan.edit(json!([{"op": "step.remove", "steps": ["s4"]}]))
        .await;
    check(&plan, "step.remove of the pruned unit's source", before);
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_compact_reads_and_the_competitor_read_decode_no_declaration() {
    let _serial = serial().await;
    let mut decoded_by_competitors = Vec::new();
    for competitors in [2, 60] {
        let plan = Plan::new(json!({"gpu": 1})).await;
        let mut ops = filler(300, "x");
        for i in 0..competitors {
            ops.push(
                json!({"op": "step.add", "step": format!("c-{i:03}"), "spec": {
                "run": "fixture.echo", "needs": {"gpu": 1}, "paused": true,
                "tags": ["unit:pool"], "in": {"value": {"default": i}}}}),
            );
        }
        ops.push(
            json!({"op": "step.add", "step": "target", "spec": {"run": "fixture.echo",
            "needs": {"gpu": 1}, "paused": true, "in": {"value": {"default": 0}}}}),
        );
        plan.edit(Value::Array(ops)).await;
        plan.edit(json!([{"op": "step.update", "step": "target", "changes": {"doc": "warm"}}]))
            .await;
        let measurement = Measurement::start();
        for (tool, args) in [
            ("plan_read", json!({})),
            ("plan_read", json!({"units": ["pool"], "limit": 5})),
            ("plan_read", json!({"status": ["pending"], "limit": 5})),
            ("step_get", json!({"step": "c-001", "compact": true})),
            ("unit_get", json!({"unit": "pool", "compact": true})),
        ] {
            measurement.reset();
            plan.call(tool, args.clone()).await.unwrap();
            assert_eq!(
                measurement.costs().declarations_decoded,
                0,
                "{tool} {args}: compact reads decode no declaration"
            );
        }
        // A priority change reads the resource's competitors from steps_needs.
        measurement.reset();
        plan.edit(json!([{"op": "step.update", "step": "target", "changes": {"priority": 9}}]))
            .await;
        decoded_by_competitors.push(measurement.costs().declarations_decoded);
    }
    assert_eq!(
        decoded_by_competitors[0], decoded_by_competitors[1],
        "the competitor read decodes no declaration: 2 and 60 competitors decode the same"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_a_high_fanout_edit_scales_with_its_readers_not_with_the_plan() {
    let _serial = serial().await;
    let mut seen = Vec::new();
    for (readers, unrelated) in [(5, 0), (5, 200), (10, 0)] {
        let plan = Plan::new(json!({})).await;
        let mut ops = filler(unrelated, "x");
        ops.push(json!({"op": "input.put", "name": "src", "declaration": "string"}));
        for i in 0..readers {
            ops.push(
                json!({"op": "step.add", "step": format!("r-{i:03}"), "spec": {
                "run": "fixture.echo", "paused": true, "in": {"value": {"source": "src"}}}}),
            );
        }
        plan.edit(Value::Array(ops)).await;
        plan.edit(json!([{"op": "step.update", "step": "r-000", "changes": {"doc": "warm"}}]))
            .await;
        let measurement = Measurement::start();
        plan.edit(json!([{"op": "input.put", "name": "src", "declaration":
            {"type": "string", "doc": "Every reader re-checks this"}}]))
            .await;
        let costs = measurement.costs();
        assert_eq!(
            (costs.full_exports, costs.full_compiles),
            (0, 0),
            "{costs:?}"
        );
        assert!(
            costs.state_rows_read <= readers as u64 + 8,
            "the read set is the readers and their sources: {costs:?}"
        );
        seen.push((
            readers,
            unrelated,
            costs.declarations_decoded,
            costs.state_rows_read,
        ));
    }
    eprintln!("high fanout (readers, unrelated, decoded, state rows): {seen:?}");
    assert_eq!(
        (seen[0].2, seen[0].3),
        (seen[1].2, seen[1].3),
        "200 unrelated steps change nothing"
    );
    assert!(
        seen[2].3 > seen[0].3,
        "twice the readers read more: {seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn slow_three_stale_preparations_end_busy_with_nothing_written_and_none_on_the_writer() {
    let _serial = serial().await;
    let plan = Plan::new(json!({})).await;
    let mut ops = vec![
        json!({"op": "input.put", "name": "src", "declaration": "string"}),
        json!({"op": "input.put", "name": "other", "declaration": "int"}),
    ];
    for i in 0..16 {
        ops.push(
            json!({"op": "step.add", "step": format!("r-{i:04}"), "spec": {
            "run": "fixture.echo", "paused": true, "in": {"value": {"source": "src"}}}}),
        );
    }
    plan.edit(Value::Array(ops)).await;
    let rev = plan.rev().await;
    let records = |plan: &Plan| -> i64 {
        plan.sql()
            .query_row(
                "SELECT count(*) FROM records WHERE project_id=?1 AND kind='plan.edit'",
                [plan.project.to_string()],
                |r| r.get(0),
            )
            .unwrap()
    };
    let edits_before = records(&plan);

    // The barrier: each preparation of this project's edit says where it is and waits.
    let (reached, at_barrier) = std::sync::mpsc::channel::<usize>();
    let (release, go) = std::sync::mpsc::channel::<()>();
    let go = Arc::new(Mutex::new(go));
    let project = plan.project;
    let _hook = cost::hook_preparations(move |point: &PreparationPoint| {
        if point.project == project {
            reached.send(point.attempt).unwrap();
            go.lock().unwrap().recv().unwrap();
        }
    });
    let measurement = Measurement::start();
    let edit = {
        let broker = plan.broker.clone();
        let project = plan.project;
        tokio::spawn(async move {
            broker
                .command(request(
                    "plan_edit",
                    json!({"project": {"kind": "id", "value": project}, "reason": "contended",
                        "ops": [{"op": "input.put", "name": "src",
                            "declaration": {"type": "string", "doc": "contended"}}]}),
                ))
                .await
        })
    };
    let at_barrier = Arc::new(Mutex::new(at_barrier));
    for attempt in 1..=3 {
        let at = at_barrier.clone();
        let reached = tokio::task::spawn_blocking(move || {
            at.lock().unwrap().recv_timeout(Duration::from_secs(120))
        })
        .await
        .unwrap()
        .expect("the preparation reaches the barrier");
        assert_eq!(reached, attempt);
        // An independent status write that moves state_epoch, while the preparation waits.
        let started = Instant::now();
        plan.call(
            "plan_set_input",
            json!({"name": "other", "value": attempt, "edit": {"dry_run": false, "reason": "move"}}),
        )
        .await
        .unwrap();
        let took = started.elapsed();
        assert!(
            took < INDEPENDENT_WRITE_BUDGET,
            "an independent write during a preparation took {took:?}"
        );
        release.send(()).unwrap();
    }
    let refused = edit.await.unwrap();
    assert_eq!(
        refused.err(),
        Some(PublicError::Busy {
            message: "the plan's state kept changing while this edit was prepared (3 tries); send it again".into(),
            retryable: true,
        })
    );
    let costs = measurement.costs();
    assert_eq!(costs.writer_preparations, 0, "{costs:?}");
    assert_eq!(costs.preparations.contended, 1, "{costs:?}");
    assert_eq!(plan.rev().await, rev, "nothing was written");
    assert_eq!(records(&plan), edits_before, "no plan.edit record");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_of_an_edits_revision_before_the_cache_holds_it_compiles_nothing() {
    let _serial = serial().await;
    let plan = Plan::new(json!({})).await;
    let mut ops = filler(198, "x");
    ops.push(json!({"op": "step.add", "step": "a", "spec": step(json!(1))}));
    plan.edit(Value::Array(ops)).await;
    plan.edit(json!([{"op": "step.update", "step": "a", "changes": {"doc": "warm"}}]))
        .await;
    let rev = plan.rev().await;

    // The probe: the edit has committed and the cache does not hold its plan yet. A reader
    // of the new revision runs there, as the scheduler or a status read can after any commit.
    let (reached, at_commit) = std::sync::mpsc::channel::<Revision>();
    let (release, go) = std::sync::mpsc::channel::<()>();
    let go = Mutex::new(go);
    let project = plan.project;
    let hook = cost::hook_commits(move |point: &CommitPoint| {
        if point.project == project {
            reached.send(point.rev).unwrap();
            go.lock().unwrap().recv().unwrap();
        }
    });
    let measurement = Measurement::start();
    let edit = {
        let broker = plan.broker.clone();
        tokio::spawn(async move {
            broker
                .command(request(
                    "plan_edit",
                    json!({"project": {"kind": "id", "value": project}, "reason": "probed",
                        "ops": [{"op": "step.update", "step": "a", "changes": {"doc": "probed"}}]}),
                ))
                .await
        })
    };
    let committed =
        tokio::task::spawn_blocking(move || at_commit.recv_timeout(Duration::from_secs(120)))
            .await
            .unwrap()
            .expect("the edit reaches the commit probe");
    assert_eq!(committed, Revision(rev + 1));
    let read = plan
        .call(
            "status",
            json!({"selection": {"steps": null, "tags": null}}),
        )
        .await;
    release.send(()).unwrap();
    edit.await.unwrap().unwrap();
    drop(hook);
    let read = read.unwrap();
    assert_eq!(
        read["rev"],
        json!(rev + 1),
        "the reader sees the edit: {read}"
    );
    let costs = measurement.costs();
    assert_eq!(
        (costs.full_exports, costs.full_compiles),
        (0, 0),
        "a reader of the edit's revision takes the edit's plan: {costs:?}"
    );
    // And the cache holds it after: the next edit is warm too.
    measurement.reset();
    plan.edit(json!([{"op": "step.update", "step": "a", "changes": {"doc": "after"}}]))
        .await;
    let costs = measurement.costs();
    assert_eq!(
        (costs.full_exports, costs.full_compiles),
        (0, 0),
        "{costs:?}"
    );
}
