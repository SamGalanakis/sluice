use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandRequest, StepStatus},
    edit::{EditSnapshot, PlanEdit, PreparedEdit, prepare_edit},
    error::PublicError,
    gates::{CachedResources, StateSnapshot, StepState, reconcile},
    ids::{Revision, StepId, UnitName},
    plan::{FnSignature, Plan, ResourceLimit, Snapshot, inputs_hash},
    recipe::{RecipeEntry, catalog},
    rpc::{JsonMap, decode_json},
    types::Type,
};
fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn id(value: &str) -> StepId {
    value.parse().unwrap()
}

fn command(name: &str, mut args: Value, dry_run: bool) -> PlanEdit {
    args["project"] = json!({"kind":"name","value":"p"});
    if name == "plan_patch" {
        args["dry_run"] = json!(dry_run);
        args["reason"] = json!("test");
        args["author"] = Value::Null;
    } else {
        args["edit"] = json!({"expected":null,"dry_run":dry_run,"reason":"test","author":null});
    }
    let command: CommandRequest =
        decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap();
    command.try_into().unwrap()
}
fn ext() -> Value {
    json!({"run":"core.external","outputs":{"ok":"boolean","n":"int"}})
}
fn worker(n: i64) -> Value {
    json!({"run":"worker","in":{"a":{"default":n},"b":{"default":1}}})
}
struct Fixture {
    snapshot: Snapshot,
    state: StateSnapshot,
    signatures: IndexMap<String, FnSignature>,
    recipes: IndexMap<String, RecipeEntry>,
    resources: CachedResources,
    limits: IndexMap<String, ResourceLimit>,
}
impl Fixture {
    fn new(document: Value) -> Self {
        let r=br#"{"name":"pair","params":{"n":"int?"},"steps":{"{unit}-a":{"run":"worker","in":{"a":{"default":1},"b":{"default":1}},"tags":["heavy"]},"{unit}-b":{"run":"worker","in":{"a":{"source":"{unit}-a/sum"},"b":{"default":1}},"tags":["exit"]}}}"#;
        Self {
            snapshot: Snapshot {
                revision: Revision(7),
                document: map(document),
            },
            state: StateSnapshot::default(),
            signatures: IndexMap::from([
                (
                    "worker".into(),
                    FnSignature {
                        inputs: IndexMap::from([("a".into(), Type::Int), ("b".into(), Type::Int)]),
                        outputs: IndexMap::from([("sum".into(), Type::Int)]),
                        ..FnSignature::default()
                    },
                ),
                (
                    "open".into(),
                    FnSignature {
                        inputs: IndexMap::from([("optional".into(), "string?".parse().unwrap())]),
                        open: true,
                        ..FnSignature::default()
                    },
                ),
                (
                    "core.external".into(),
                    FnSignature {
                        open: true,
                        ..FnSignature::default()
                    },
                ),
            ]),
            recipes: catalog([("pair", "global", r.as_slice())]),
            resources: CachedResources::default(),
            limits: IndexMap::new(),
        }
    }
    fn prepare(&self, edit: PlanEdit) -> Result<PreparedEdit, PublicError> {
        prepare_edit(&self.context(&self.plan(), None), edit)
    }
    fn context<'a>(
        &'a self,
        plan: &'a Plan,
        eligible: Option<&'a [UnitName]>,
    ) -> EditSnapshot<'a, IndexMap<String, FnSignature>> {
        EditSnapshot {
            revision: self.snapshot.revision,
            plan,
            state: &self.state,
            signatures: &self.signatures,
            recipes: &self.recipes,
            resources: &self.resources,
            limits: &self.limits,
            prune_eligible: eligible,
        }
    }
    fn plan(&self) -> Plan {
        Plan::parse(&self.snapshot.document, &self.signatures).unwrap()
    }
    fn status(&mut self, name: &str, status: StepStatus) {
        let plan = self.plan();
        let hash = inputs_hash(&plan, &self.state, &plan.steps()[&id(name)]);
        self.state.steps.insert(
            id(name),
            StepState {
                status,
                inputs_hash: hash,
                ..StepState::default()
            },
        );
    }
    fn accept(&mut self, prepared: PreparedEdit) {
        assert!(!prepared.dry_run);
        self.snapshot.document = map(serde_json::to_value(prepared.plan).unwrap());
        self.snapshot.revision = Revision(self.snapshot.revision.0 + 1);
        self.state = reconcile(&self.plan(), &self.state);
    }
}
fn doc(edit: &PreparedEdit) -> Value {
    serde_json::to_value(&edit.plan).unwrap()
}
fn ops(edit: &PreparedEdit) -> Value {
    serde_json::to_value(&edit.ops).unwrap()
}
fn patch(ops: Value) -> PlanEdit {
    command("plan_patch", json!({"rev":7,"ops":ops}), false)
}

#[test]
fn edges_support_every_gate_form_in_order_and_refuse_invalid_forms() {
    let mut up = ext();
    up["tags"] = json!(["unit:up", "exit"]);
    let mut f =
        Fixture::new(json!({"inputs":{"flag":"boolean"},"steps":{"up":up,"down":worker(1)}}));
    let entries = json!([
        "up", "up?", "up/ok", "!up/ok", "flag", "!flag", "unit:up", "unit:up?"
    ]);
    let prepared = f
        .prepare(command(
            "edge_add",
            json!({"step":"down","after":entries}),
            false,
        ))
        .unwrap();
    assert_eq!(doc(&prepared)["steps"]["down"]["after"], entries);
    f.accept(prepared);
    for entry in [
        "up/ok?",
        "!up",
        "!unit:up",
        "up/n",
        "missing",
        "unit:missing",
    ] {
        assert!(
            f.prepare(command(
                "edge_add",
                json!({"step":"down","after":[entry]}),
                false
            ))
            .is_err(),
            "{entry}"
        );
    }
    assert!(
        f.prepare(command(
            "edge_add",
            json!({"step":"down","after":[]}),
            false
        ))
        .is_err()
    );
}
#[test]
fn unit_edge_targets_current_entries_including_handoffs_and_boolean_gates() {
    let mut a = worker(1);
    a["tags"] = json!(["unit:down"]);
    let mut b = worker(1);
    b["tags"] = json!(["unit:down"]);
    b["in"]["a"] = json!({"source":"a/sum"});
    let mut c = ext();
    c["tags"] = json!(["unit:down"]);
    let mut d = worker(1);
    d["tags"] = json!(["unit:down"]);
    d["after"] = json!(["c/ok"]);
    let mut u = ext();
    u["tags"] = json!(["unit:up", "exit"]);
    let f = Fixture::new(json!({"steps":{"u":u,"a":a,"b":b,"c":c,"d":d}}));
    let prepared = f
        .prepare(command(
            "edge_add",
            json!({"step":"unit:down","after":["unit:up"]}),
            false,
        ))
        .unwrap();
    assert_eq!(
        ops(&prepared),
        json!([{"op":"add","path":"/steps/a/after","value":["unit:up"]},{"op":"add","path":"/steps/c/after","value":["unit:up"]}])
    );
    let mut f = f;
    f.accept(prepared);
    let remove = f
        .prepare(command(
            "edge_remove",
            json!({"step":"unit:down","after":["unit:up"]}),
            false,
        ))
        .unwrap();
    assert_eq!(remove.ops.len(), 2);
}
#[test]
fn edges_collect_unknown_target_and_entries_and_refuse_cycles_atomically() {
    let f = Fixture::new(
        json!({"steps":{"a":worker(1),"b":{ "run":"worker","in":{"a":{"default":1},"b":{"default":1}},"after":["a"]}}}),
    );
    let before = f.snapshot.clone();
    let error = f
        .prepare(command(
            "edge_add",
            json!({"step":"missing","after":["other","unit:unknown"]}),
            false,
        ))
        .unwrap_err();
    if let PublicError::Invalid { errors, .. } = error {
        assert_eq!(errors.len(), 3);
    } else {
        panic!("{error:?}");
    }
    assert!(
        f.prepare(command(
            "edge_add",
            json!({"step":"a","after":["b"]}),
            false
        ))
        .is_err()
    );
    assert_eq!(f.snapshot, before);
}

#[test]
fn bulk_inputs_support_optional_and_bound_open_inputs_preserving_pause() {
    let f = Fixture::new(
        json!({"steps":{"a":{"run":"open","paused":"hold","in":{"effort":{"default":"high"}}}}}),
    );
    let prepared=f.prepare(command("step_set_input",json!({"selection":{"steps":["a"],"tags":null},"inputs":{"effort":"xhigh","optional":"set"}}),false)).unwrap();
    assert_eq!(doc(&prepared)["steps"]["a"]["paused"], "hold");
    assert!(prepared.preview.would_start.is_empty());
    assert!(
        f.prepare(command(
            "step_set_input",
            json!({"selection":{"steps":["a"],"tags":null},"inputs":{"new":1}}),
            false
        ))
        .is_err()
    );
}

#[test]
fn keep_patterns_match_whole_names_with_star_and_question_mark() {
    use sluice_model::units::keep_match;
    let p = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(keep_match(&p(&["ta-*"]), "ta-harness-work"), Some("ta-*"));
    assert_eq!(keep_match(&p(&["ta-*"]), "ta-"), Some("ta-*"));
    assert_eq!(keep_match(&p(&["ta-*"]), "beta-x"), None);
    assert_eq!(keep_match(&p(&["fig-?"]), "fig-1"), Some("fig-?"));
    assert_eq!(keep_match(&p(&["fig-?"]), "fig-12"), None);
    assert_eq!(
        keep_match(&p(&["*-landed"]), "fig-4200-landed"),
        Some("*-landed")
    );
    assert_eq!(keep_match(&p(&["a*b*c"]), "axxbyyc"), Some("a*b*c"));
    assert_eq!(keep_match(&p(&["a*b*c"]), "axxbyy"), None);
    assert_eq!(keep_match(&p(&["x", "*"]), "y"), Some("*"));
    assert_eq!(keep_match(&p(&[]), "y"), None);
}

#[test]
fn pruning_mutually_referencing_candidate_units_removes_them_together() {
    let mut a1 = ext();
    a1["tags"] = json!(["unit:a"]);
    let mut a2 = ext();
    a2["tags"] = json!(["unit:a"]);
    a2["after"] = json!(["b1"]);
    let mut b1 = ext();
    b1["tags"] = json!(["unit:b"]);
    b1["after"] = json!(["a1"]);
    let mut f = Fixture::new(json!({"steps":{"a1":a1,"a2":a2,"b1":b1}}));
    for name in ["a1", "a2", "b1"] {
        f.status(name, StepStatus::Skipped);
    }
    let prepared = f
        .prepare(command(
            "plan_prune",
            json!({"units":null,"older_than_seconds":0}),
            false,
        ))
        .unwrap();
    assert_eq!(prepared.ops.len(), 3);
    assert!(prepared.prune.unwrap().kept.is_empty());
}

#[test]
fn raw_patch_revision_tests_and_late_failure_are_all_or_nothing() {
    let f = Fixture::new(json!({"steps":{"a":worker(1),"b":worker(1)}}));
    let before = f.snapshot.clone();
    for operations in [
        json!([{"op":"replace","path":"/steps/a/in/a/default","value":3},{"op":"replace","path":"/steps/b/in/b/default","value":"bad"}]),
        json!([{"op":"replace","path":"/steps/a/in/a/default","value":3},{"op":"test","path":"/steps/b/in/b/default","value":99}]),
        json!([{"op":"remove","path":"/steps/absent"}]),
    ] {
        assert!(f.prepare(patch(operations)).is_err());
        assert_eq!(f.snapshot, before);
    }
    let mut request = command("plan_patch", json!({"rev":6,"ops":[]}), true);
    assert!(matches!(
        f.prepare(request.clone()),
        Err(PublicError::Conflict {
            current_rev: Some(Revision(7)),
            ..
        })
    ));
    if let PlanEdit::Patch(r) = &mut request {
        r.rev = Revision(7);
    }
    assert!(f.prepare(request).unwrap().ops.is_empty());
}

#[test]
fn patch_stages_new_ids_in_final_graph_and_preserves_reused_ids_and_explicit_pauses() {
    let mut f = Fixture::new(json!({"steps":{"existing":worker(1)}}));
    f.status("existing", StepStatus::Succeeded);
    let prepared = f.prepare(command("plan_patch", json!({"rev":7,"start":false,"ops":[
        {"op":"replace","path":"/steps","value":{
            "existing":worker(1),"new":worker(1),
            "active":{"run":"worker","in":{"a":{"default":1},"b":{"default":1}},"paused":false},
            "held":{"run":"worker","in":{"a":{"default":1},"b":{"default":1}},"paused":"review"}
        }}
    ]}), false)).unwrap();
    let document = doc(&prepared);
    assert!(document["steps"]["existing"].get("paused").is_none());
    assert_eq!(document["steps"]["new"]["paused"], true);
    assert_eq!(document["steps"]["active"]["paused"], false);
    assert_eq!(document["steps"]["held"]["paused"], "review");
    assert_eq!(prepared.ops.len(), 2);
    assert_eq!(prepared.preview.would_start, [id("active")]);
    assert_eq!(prepared.preview.ops, prepared.ops);
    assert_eq!(prepared.reason, "test");
    assert!(prepared.preview.would_stale.is_empty());
    assert!(
        serde_json::to_value(&f.snapshot.document).unwrap()["steps"]
            .get("new")
            .is_none()
    );
}
