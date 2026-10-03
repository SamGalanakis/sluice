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
    units::PruneHolder,
};
fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn id(value: &str) -> StepId {
    value.parse().unwrap()
}
fn unit(value: &str) -> UnitName {
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
        prepare_edit(&self.context(None), edit)
    }
    fn context<'a>(
        &'a self,
        eligible: Option<&'a [UnitName]>,
    ) -> EditSnapshot<'a, IndexMap<String, FnSignature>> {
        EditSnapshot {
            snapshot: &self.snapshot,
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
fn edges_emit_exact_add_replace_remove_ops_and_idempotent_noops() {
    let mut f = Fixture::new(json!({"steps":{"a":ext(),"b":ext(),"c":worker(1)}}));
    let first = f
        .prepare(command(
            "edge_add",
            json!({"step":"c","after":["a","a"]}),
            false,
        ))
        .unwrap();
    assert_eq!(
        ops(&first),
        json!([{"op":"add","path":"/steps/c/after","value":["a"]}])
    );
    f.accept(first);
    let second = f
        .prepare(command(
            "edge_add",
            json!({"step":"c","after":["b","a","b"]}),
            false,
        ))
        .unwrap();
    assert_eq!(
        ops(&second),
        json!([{"op":"replace","path":"/steps/c/after","value":["a","b"]}])
    );
    f.accept(second);
    assert!(
        f.prepare(command(
            "edge_add",
            json!({"step":"c","after":["b","a"]}),
            false
        ))
        .unwrap()
        .ops
        .is_empty()
    );
    let third = f
        .prepare(command(
            "edge_remove",
            json!({"step":"c","after":["a"]}),
            false,
        ))
        .unwrap();
    f.accept(third);
    let last = f
        .prepare(command(
            "edge_remove",
            json!({"step":"c","after":["b"]}),
            false,
        ))
        .unwrap();
    assert_eq!(ops(&last), json!([{"op":"remove","path":"/steps/c/after"}]));
    f.accept(last);
    assert!(
        f.prepare(command(
            "edge_remove",
            json!({"step":"c","after":["b"]}),
            false
        ))
        .unwrap()
        .ops
        .is_empty()
    );
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
fn racing_edges_conflict_then_reprepare_keeps_both() {
    let mut f = Fixture::new(json!({"steps":{"a":ext(),"b":ext(),"c":worker(1)}}));
    let mut first = command("edge_add", json!({"step":"c","after":["a"]}), false);
    let mut second = command("edge_add", json!({"step":"c","after":["b"]}), false);
    if let PlanEdit::EdgeAdd(r) = &mut first {
        r.edit.expected = Some(Revision(7));
    }
    if let PlanEdit::EdgeAdd(r) = &mut second {
        r.edit.expected = Some(Revision(7));
    }
    let prepared = f.prepare(first).unwrap();
    f.accept(prepared);
    assert!(matches!(
        f.prepare(second),
        Err(PublicError::Conflict {
            current_rev: Some(Revision(8)),
            ..
        })
    ));
    let prepared = f
        .prepare(command(
            "edge_add",
            json!({"step":"c","after":["b"]}),
            false,
        ))
        .unwrap();
    assert_eq!(doc(&prepared)["steps"]["c"]["after"], json!(["a", "b"]));
}
#[test]
fn step_add_update_null_field_removal_and_remove_selection() {
    let mut f = Fixture::new(json!({"steps":{"a":worker(1)}}));
    let add = f
        .prepare(command(
            "step_add",
            json!({"step":"b","spec":worker(2)}),
            false,
        ))
        .unwrap();
    assert_eq!(add.ops.len(), 1);
    f.accept(add);
    assert!(matches!(
        f.prepare(command(
            "step_add",
            json!({"step":"b","spec":worker(2)}),
            false
        )),
        Err(PublicError::BadRequest { .. })
    ));
    let update = f
        .prepare(command(
            "step_update",
            json!({"step":"b","changes":{"doc":"new","tags":["arc:x"],"paused":true}}),
            false,
        ))
        .unwrap();
    f.accept(update);
    let update = f
        .prepare(command(
            "step_update",
            json!({"step":"b","changes":{"doc":null,"paused":null}}),
            false,
        ))
        .unwrap();
    assert!(doc(&update)["steps"]["b"].get("doc").is_none());
    f.accept(update);
    let remove = f
        .prepare(command(
            "step_remove",
            json!({"selection":{"steps":["a"],"tags":["arc:x"]}}),
            false,
        ))
        .unwrap();
    assert_eq!(
        ops(&remove),
        json!([{"op":"remove","path":"/steps/a"},{"op":"remove","path":"/steps/b"}])
    );
}
#[test]
fn updates_refuse_when_even_null_and_empty_changes() {
    let f = Fixture::new(json!({"steps":{"a":worker(1)}}));
    for changes in [json!({"when":"enabled"}), json!({"when":null}), json!({})] {
        assert!(
            f.prepare(command(
                "step_update",
                json!({"step":"a","changes":changes}),
                false
            ))
            .is_err()
        );
    }
    assert!(
        f.prepare(command(
            "step_update",
            json!({"step":"missing","changes":{"doc":"x"}}),
            false
        ))
        .is_err()
    );
}
#[test]
fn unit_add_is_one_prepared_edit_with_exit_and_staging() {
    let f = Fixture::new(json!({"steps":{"prior":ext()}}));
    let request = command(
        "unit_add",
        json!({"recipe":"pair","unit":"u","params":{},"start":false,"after":{"*":["prior?"]},"tags":["arc:x","heavy"],"inputs":{"a":{"a":9,"b":2}}}),
        false,
    );
    let prepared = f.prepare(request).unwrap();
    assert_eq!(prepared.ops.len(), 2);
    let doc = doc(&prepared);
    assert_eq!(
        doc["steps"]["u-a"]["tags"],
        json!(["unit:u", "heavy", "arc:x"])
    );
    assert_eq!(
        doc["steps"]["u-b"]["tags"],
        json!(["unit:u", "exit", "arc:x", "heavy"])
    );
    assert_eq!(doc["steps"]["u-a"]["after"], json!(["prior?"]));
    assert!(doc["steps"]["u-b"].get("after").is_none());
    assert_eq!(doc["steps"]["u-a"]["in"]["a"], json!({"default":9}));
    assert!(prepared.preview.would_start.is_empty());
}
#[test]
fn unit_add_invalid_staging_never_returns_a_partial_candidate() {
    let f = Fixture::new(json!({"steps":{}}));
    let before = f.snapshot.clone();
    let PlanEdit::UnitAdd(mut request) = command(
        "unit_add",
        json!({"recipe":"pair","unit":"u","params":{},"start":true,"after":{"*":["missing"]}}),
        false,
    ) else {
        panic!()
    };
    request.inputs.insert("a".into(), map(json!({"a":"bad"})));
    assert!(f.prepare(PlanEdit::UnitAdd(request)).is_err());
    assert_eq!(f.snapshot, before);
    for (name, params) in [("absent", json!({})), ("pair", json!({"unit":"other"}))] {
        assert!(
            f.prepare(command(
                "unit_add",
                json!({"recipe":name,"unit":"u","params":params,"start":true,"after":{}}),
                false
            ))
            .is_err()
        );
    }
}
#[test]
fn bulk_input_change_is_atomic_per_supported_step_and_reports_running_and_unsupported() {
    let mut f = Fixture::new(
        json!({"steps":{"good":worker(1),"running":worker(1),"partial":{"run":"open","in":{"a":{"default":1}}},"unsupported":ext()}}),
    );
    f.status("running", StepStatus::Running);
    f.status("good", StepStatus::Succeeded);
    let prepared=f.prepare(command("step_set_input",json!({"selection":{"steps":["unsupported","good","partial","running"],"tags":null},"inputs":{"a":2,"b":3}}),false)).unwrap();
    let report = prepared.inputs.as_ref().unwrap();
    assert_eq!(report.changed, vec![id("good")]);
    assert_eq!(report.running, vec![id("running")]);
    assert_eq!(
        report
            .unsupported
            .iter()
            .map(|r| (r.step.clone(), r.inputs.clone()))
            .collect::<Vec<_>>(),
        vec![
            (id("partial"), vec!["b".into()]),
            (id("unsupported"), vec!["a".into(), "b".into()])
        ]
    );
    assert_eq!(
        ops(&prepared),
        json!([{"op":"replace","path":"/steps/good/in","value":{"a":{"default":2},"b":{"default":3}}}])
    );
    assert_eq!(prepared.preview.would_stale, vec![id("good")]);
    assert_eq!(
        doc(&prepared)["steps"]["running"]["in"],
        serde_json::to_value(&f.snapshot.document).unwrap()["steps"]["running"]["in"]
    );
}
#[test]
fn inputs_map_racing_running_snapshot_skips_entire_step_and_rejects_no_change() {
    let mut f = Fixture::new(json!({"steps":{"a":worker(1),"b":worker(1)}}));
    let request = || {
        command(
            "step_set_input",
            json!({"selection":{"steps":["a","b"],"tags":null},"inputs":{"a":4,"b":5}}),
            false,
        )
    };
    assert_eq!(
        f.prepare(request()).unwrap().inputs.unwrap().changed,
        vec![id("a"), id("b")]
    );
    f.status("a", StepStatus::Running);
    let edit = f.prepare(request()).unwrap();
    assert_eq!(edit.inputs.as_ref().unwrap().running, vec![id("a")]);
    assert_eq!(edit.ops.len(), 1);
    f.status("b", StepStatus::Running);
    assert!(matches!(
        f.prepare(request()),
        Err(PublicError::BadRequest { .. })
    ));
}
#[test]
fn bulk_input_noop_invalid_type_and_empty_map_refused_without_mutation() {
    let f = Fixture::new(json!({"steps":{"a":worker(1),"b":worker(1)}}));
    for inputs in [json!({"a":1}), json!({"a":2,"b":"bad"}), json!({})] {
        assert!(
            f.prepare(command(
                "step_set_input",
                json!({"selection":{"steps":["a","b"],"tags":null},"inputs":inputs}),
                false
            ))
            .is_err()
        );
    }
    assert_eq!(f.snapshot.revision, Revision(7));
    assert_eq!(f.snapshot.document.0["steps"].as_value()["a"], worker(1));
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
fn unit_tag_changes_running_steps_and_refuses_reserved_conflicting_or_invalid_tags() {
    let mut a = worker(1);
    a["tags"] = json!(["unit:u", "heavy"]);
    let mut b = worker(1);
    b["tags"] = json!(["unit:u"]);
    let mut f = Fixture::new(json!({"steps":{"a":a,"b":b}}));
    f.status("a", StepStatus::Running);
    let prepared = f
        .prepare(command(
            "unit_tag",
            json!({"unit":"u","add":["arc:x","arc:x"],"remove":["heavy"]}),
            false,
        ))
        .unwrap();
    assert_eq!(prepared.ops.len(), 2);
    assert_eq!(
        doc(&prepared)["steps"]["a"]["tags"],
        json!(["unit:u", "arc:x"])
    );
    f.accept(prepared);
    assert!(
        f.prepare(command(
            "unit_tag",
            json!({"unit":"u","add":["arc:x"],"remove":["heavy"]}),
            false
        ))
        .unwrap()
        .ops
        .is_empty()
    );
    for (add, remove) in [
        (json!(["unit:v"]), json!(["unit:u"])),
        (json!(["x"]), json!(["x"])),
        (json!(["Not A Tag"]), json!([])),
    ] {
        assert!(
            f.prepare(command(
                "unit_tag",
                json!({"unit":"u","add":add,"remove":remove}),
                false
            ))
            .is_err()
        );
    }
    assert!(matches!(
        f.prepare(command(
            "unit_tag",
            json!({"unit":"missing","add":["x"],"remove":[]}),
            false
        )),
        Err(PublicError::NotFound { .. })
    ));
}
#[test]
fn running_steps_only_allow_pause_and_tags_for_every_edit_path() {
    let mut f = Fixture::new(json!({"steps":{"a":worker(1),"up":ext()}}));
    f.status("a", StepStatus::Running);
    for request in [
        command(
            "step_update",
            json!({"step":"a","changes":{"doc":"changed"}}),
            false,
        ),
        command(
            "step_remove",
            json!({"selection":{"steps":["a"],"tags":null}}),
            false,
        ),
        command("edge_add", json!({"step":"a","after":["up"]}), false),
        patch(json!([{"op":"replace","path":"/steps/a/in/a/default","value":2}])),
    ] {
        assert!(matches!(
            f.prepare(request),
            Err(PublicError::Invalid { .. })
        ));
    }
    let prepared = f
        .prepare(command(
            "step_update",
            json!({"step":"a","changes":{"paused":true,"tags":["arc:x"]}}),
            false,
        ))
        .unwrap();
    assert_eq!(prepared.ops.len(), 1);
    assert!(
        f.prepare(command(
            "step_pause",
            json!({"selection":{"steps":["a"],"tags":null},"paused":true}),
            false
        ))
        .is_ok()
    );
}
#[test]
fn pause_selection_union_deduplicates_and_unpause_removes_the_field() {
    let mut a = worker(1);
    a["tags"] = json!(["arc:x"]);
    let mut f = Fixture::new(json!({"steps":{"a":a,"b":worker(1),"c":worker(1)}}));
    let prepared = f
        .prepare(command(
            "step_pause",
            json!({"selection":{"steps":["a","b","b"],"tags":["arc:x"]},"paused":true}),
            false,
        ))
        .unwrap();
    assert_eq!(prepared.ops.len(), 2);
    assert!(!prepared.preview.would_start.contains(&id("a")));
    f.accept(prepared);
    assert!(
        f.prepare(command(
            "step_pause",
            json!({"selection":{"steps":["a"],"tags":null},"paused":true}),
            false
        ))
        .unwrap()
        .ops
        .is_empty()
    );
    let prepared = f
        .prepare(command(
            "step_pause",
            json!({"selection":{"steps":null,"tags":["arc:x"]},"paused":false}),
            false,
        ))
        .unwrap();
    assert_eq!(
        ops(&prepared),
        json!([{"op":"remove","path":"/steps/a/paused"}])
    );
    assert!(prepared.preview.would_start.contains(&id("a")));
    for selection in [
        json!({"steps":null,"tags":null}),
        json!({"steps":["missing"],"tags":null}),
    ] {
        assert!(
            f.prepare(command(
                "step_pause",
                json!({"selection":selection,"paused":true}),
                false
            ))
            .is_err()
        );
    }
}
#[test]
fn prune_returns_reference_closed_set_and_each_retention_witness() {
    let mut a = ext();
    a["tags"] = json!(["unit:kept", "exit"]);
    let mut b = ext();
    b["tags"] = json!(["unit:free"]);
    let mut f = Fixture::new(
        json!({"steps":{"a":a,"b":b,"consumer":{"run":"worker","in":{"a":{"default":1},"b":{"default":1}},"after":["unit:kept?"]}}}),
    );
    f.status("a", StepStatus::Skipped);
    f.status("b", StepStatus::Succeeded);
    let prepared = f
        .prepare(command(
            "plan_prune",
            json!({"units":null,"older_than_seconds":0}),
            false,
        ))
        .unwrap();
    assert_eq!(ops(&prepared), json!([{"op":"remove","path":"/steps/b"}]));
    let prune = prepared.prune.unwrap();
    assert_eq!(prune.units, vec![unit("free")]);
    assert_eq!(prune.kept[&unit("kept")], PruneHolder::Step(id("consumer")));
}
#[test]
fn prune_keeps_plan_output_and_rejects_unknown_not_done_and_unfiltered_age() {
    let mut f =
        Fixture::new(json!({"outputs":{"n":{"source":"a/n"}},"steps":{"a":ext(),"pending":ext()}}));
    f.status("a", StepStatus::Succeeded);
    let prepared = f
        .prepare(command(
            "plan_prune",
            json!({"units":["a"],"older_than_seconds":0}),
            false,
        ))
        .unwrap();
    assert!(prepared.ops.is_empty());
    assert_eq!(
        prepared.prune.unwrap().kept[&unit("a")],
        PruneHolder::PlanOutput("n".into())
    );
    for args in [
        json!({"units":["unknown"],"older_than_seconds":0}),
        json!({"units":["pending"],"older_than_seconds":0}),
        json!({"units":null,"older_than_seconds":10}),
    ] {
        assert!(f.prepare(command("plan_prune", args, false)).is_err());
    }
    let eligible = [unit("a")];
    assert!(
        prepare_edit(
            &f.context(Some(&eligible)),
            command(
                "plan_prune",
                json!({"units":null,"older_than_seconds":10}),
                false
            )
        )
        .is_ok()
    );
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
fn recut_new_ids_reset_but_remove_and_readd_same_id_retains_state() {
    let mut f = Fixture::new(json!({"steps":{"old":worker(1)}}));
    f.status("old", StepStatus::Succeeded);
    let fresh=f.prepare(patch(json!([{"op":"remove","path":"/steps/old"},{"op":"add","path":"/steps/new","value":worker(1)}]))).unwrap();
    assert_eq!(fresh.preview.would_start, vec![id("new")]);
    let reused=f.prepare(patch(json!([{"op":"remove","path":"/steps/old"},{"op":"add","path":"/steps/old","value":worker(1)}]))).unwrap();
    assert!(reused.preview.would_start.is_empty());
    assert!(reused.preview.would_stale.is_empty());
}
#[test]
fn recut_one_patch_preserves_unit_gate_resolving_to_new_exit() {
    let mut old = worker(1);
    old["tags"] = json!(["unit:up", "exit"]);
    let mut down = worker(1);
    down["after"] = json!(["unit:up"]);
    let mut f = Fixture::new(json!({"steps":{"old":old,"down":down}}));
    f.status("old", StepStatus::Succeeded);
    let mut new = worker(2);
    new["tags"] = json!(["unit:up", "exit"]);
    let prepared=f.prepare(patch(json!([{"op":"remove","path":"/steps/old"},{"op":"add","path":"/steps/new","value":new}]))).unwrap();
    assert_eq!(prepared.preview.would_start, vec![id("new")]);
    assert_eq!(doc(&prepared)["steps"]["down"]["after"], json!(["unit:up"]));
    assert!(
        f.prepare(patch(json!([{"op":"remove","path":"/steps/old"}])))
            .is_err()
    );
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
fn dry_run_of_each_edit_kind_uses_identical_candidate_ops_and_preview() {
    let mut a = worker(1);
    a["tags"] = json!(["unit:u", "heavy"]);
    let mut f = Fixture::new(json!({"steps":{"a":a,"up":ext(),"removable":ext()}}));
    f.status("removable", StepStatus::Skipped);
    let cases = [
        (
            "plan_patch",
            json!({"rev":7,"ops":[{"op":"add","path":"/steps/a/doc","value":"x"}]}),
        ),
        ("step_add", json!({"step":"new","spec":worker(1)})),
        (
            "unit_add",
            json!({"recipe":"pair","unit":"pair-unit","params":{},"start":true,"after":{}}),
        ),
        ("step_update", json!({"step":"a","changes":{"doc":"x"}})),
        (
            "step_remove",
            json!({"selection":{"steps":["removable"],"tags":null}}),
        ),
        ("edge_add", json!({"step":"a","after":["up?"]})),
        ("edge_remove", json!({"step":"a","after":["up?"]})),
        (
            "step_set_input",
            json!({"selection":{"steps":["a"],"tags":null},"inputs":{"a":5,"b":6}}),
        ),
        (
            "unit_tag",
            json!({"unit":"u","add":["arc:x"],"remove":["heavy"]}),
        ),
        (
            "step_pause",
            json!({"selection":{"steps":["a"],"tags":null},"paused":true}),
        ),
        (
            "plan_prune",
            json!({"units":["removable"],"older_than_seconds":0}),
        ),
    ];
    let before = f.snapshot.clone();
    let state = f.state.clone();
    for (name, args) in cases {
        let dry = f.prepare(command(name, args.clone(), true)).unwrap();
        let real = f.prepare(command(name, args, false)).unwrap();
        assert!(dry.dry_run);
        assert!(!real.dry_run);
        assert_eq!(dry.ops, real.ops, "{name}");
        assert_eq!(dry.plan, real.plan, "{name}");
        assert_eq!(dry.preview, real.preview, "{name}");
        assert_eq!(dry.preview.ops, dry.ops, "{name}");
        assert_eq!(f.snapshot, before);
        assert_eq!(f.state, state);
    }
}
#[test]
fn shared_preview_uses_cached_resources_and_skip_and_stale_deltas() {
    let mut f = Fixture::new(json!({"steps":{"a":worker(1),"gate":ext()}}));
    f.state.steps.insert(
        id("gate"),
        StepState {
            status: StepStatus::Succeeded,
            outputs: map(json!({"ok":false})),
            ..StepState::default()
        },
    );
    let plan = f.plan();
    f.state.steps.get_mut(&id("gate")).unwrap().inputs_hash =
        inputs_hash(&plan, &f.state, &plan.steps()[&id("gate")]);
    let skip = f
        .prepare(command(
            "edge_add",
            json!({"step":"a","after":["gate/ok"]}),
            true,
        ))
        .unwrap();
    assert_eq!(skip.preview.would_skip, vec![id("a")]);
    f.resources.capacities.insert("slot".into(), Some(0));
    f.limits.insert("slot".into(), ResourceLimit::Dynamic);
    let queued = f
        .prepare(command(
            "step_update",
            json!({"step":"a","changes":{"needs":{"slot":1}}}),
            true,
        ))
        .unwrap();
    assert_eq!(queued.preview.would_queue, vec![id("a")]);
    assert!(!queued.preview.would_start.contains(&id("gate")));
}
#[test]
fn command_conversion_rejects_nonplan_mutations_and_preserves_unit_transport() {
    assert!(PlanEdit::try_from(CommandRequest::ProjectsList).is_err());
    let PlanEdit::UnitAdd(request) = command(
        "unit_add",
        json!({"recipe":"pair","unit":"u","params":{},"start":true,"after":{}}),
        false,
    ) else {
        panic!()
    };
    assert_eq!(request.unit, unit("u"));
    assert!(request.tags.is_empty());
    assert!(request.inputs.is_empty());
    let f = Fixture::new(json!({"steps":{}}));
    let prepared = f
        .prepare(command(
            "step_add",
            json!({"step":"a","spec":worker(1)}),
            false,
        ))
        .unwrap();
    assert_eq!(
        ops(&prepared),
        json!([{"op":"add","path":"/steps/a","value":worker(1)}])
    );
    assert_eq!(prepared.plan.steps()[&id("a")].run, "worker");
    assert_eq!(prepared.reason, "test");
    assert!(!prepared.dry_run);
}

#[test]
fn duplicate_expanded_ids_are_bad_requests_and_broken_recipe_errors_keep_scope() {
    let mut f = Fixture::new(json!({"steps":{}}));
    let request = || {
        command(
            "unit_add",
            json!({"recipe":"pair","unit":"u","params":{},"start":true,"after":{}}),
            false,
        )
    };
    let prepared = f.prepare(request()).unwrap();
    f.accept(prepared);
    let error = f.prepare(request()).unwrap_err();
    assert!(matches!(error, PublicError::BadRequest { .. }));
    assert!(error.to_string().contains("u-a"));
    assert!(error.to_string().contains("u-b"));
    let broken = br#"{"name":"old","steps":{"{unit}":{"run":"worker","when":"yes"}}}"#;
    f.recipes
        .extend(catalog([("old", "project", broken.as_slice())]));
    let error = f
        .prepare(command(
            "unit_add",
            json!({"recipe":"old","unit":"u","params":{},"start":true,"after":{}}),
            false,
        ))
        .unwrap_err();
    let PublicError::Invalid { errors, .. } = error else {
        panic!()
    };
    assert!(
        errors.iter().any(
            |error| error.contains("recipe old (project)") && error.contains("when is removed")
        )
    );
}

#[test]
fn step_add_stages_only_unspecified_pauses_and_retains_authored_metadata() {
    let f = Fixture::new(json!({"steps":{}}));
    for pause in [
        None,
        Some(json!(false)),
        Some(json!(true)),
        Some(json!("hold")),
    ] {
        let mut spec = worker(1);
        if let Some(pause) = &pause {
            spec["paused"] = pause.clone();
        }
        let mut request = command(
            "step_add",
            json!({"step":"a","spec":spec,"start":false}),
            true,
        );
        if let PlanEdit::StepAdd(request) = &mut request {
            request.edit.author = Some("sam".into());
        }
        let prepared = f.prepare(request).unwrap();
        assert_eq!(
            doc(&prepared)["steps"]["a"]["paused"],
            pause.unwrap_or(json!(true))
        );
        assert_eq!(prepared.author.as_deref(), Some("sam"));
        assert_eq!(prepared.reason, "test");
        assert!(prepared.dry_run);
        assert_eq!(prepared.expected, Revision(7));
        assert_eq!(prepared.ops, prepared.preview.ops);
    }
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

#[test]
fn prune_resolves_unit_and_tag_union_then_age_and_reference_closure() {
    let mut f = Fixture::new(json!({"steps":{
        "a":{"run":"core.external","tags":["unit:u","arc:review"]},
        "b":{"run":"core.external","tags":["unit:v"]},
        "c":{"run":"core.external","tags":["unit:w","arc:review"]},
        "live":{"run":"core.external","tags":["arc:review"]},
        "holder":{"run":"core.external","after":["unit:u"]}
    }}));
    for name in ["a", "b", "c"] {
        f.status(name, StepStatus::Succeeded);
    }
    let request = || {
        command(
            "plan_prune",
            json!({"units":["v","v"],"tags":["arc:review"],"older_than_seconds":1}),
            true,
        )
    };
    let eligible = [unit("u"), unit("v")];
    let prepared = prepare_edit(&f.context(Some(&eligible)), request()).unwrap();
    let report = prepared.prune.as_ref().unwrap();
    assert_eq!(report.units, [unit("v")]);
    assert_eq!(report.steps, [id("b")]);
    assert_eq!(report.kept[&unit("u")], PruneHolder::Step(id("holder")));
    assert_eq!(prepared.reason, "test");
    assert!(prepared.dry_run);
    let serialized = serde_json::to_value(&prepared).unwrap();
    assert_eq!(
        serialized["prune"]["kept"]["u"],
        json!({"kind":"step","value":"holder"})
    );
    assert!(prepare_edit(&f.context(None), request()).is_err());
    let empty = f
        .prepare(command(
            "plan_prune",
            json!({"units":null,"tags":[],"older_than_seconds":0}),
            false,
        ))
        .unwrap();
    assert!(empty.ops.is_empty());
}

#[test]
fn wire_document_requires_whole_plan_validation_before_use() {
    let document: sluice_model::plan::PlanDocument =
        decode_json(br#"{"steps":{"a":{"run":"missing"}}}"#).unwrap();
    assert!(
        document
            .compile(&IndexMap::<String, FnSignature>::new())
            .is_err()
    );
}
