//! The schema-3 plan contracts (`docs/design/plan-rows.md`): every JSON example the document
//! pins is a fixture here, equal to the document's block, and round-trips through its type.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    ids::{ProjectId, ProjectSelector, Revision, StepId, UnitName},
    plan_rows::*,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

const DOCUMENT: &str = include_str!("../../../docs/design/plan-rows.md");

fn fixtures() -> BTreeMap<String, Value> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan_rows");
    std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            (name, serde_json::from_str(&text).unwrap())
        })
        .collect()
}
fn fixture(name: &str) -> Value {
    fixtures()
        .remove(name)
        .unwrap_or_else(|| panic!("no fixture {name}"))
}

/// Every ```` ```json fixture=<name> ```` block of the document, by name.
fn document_blocks() -> BTreeMap<String, Value> {
    let mut blocks = BTreeMap::new();
    let mut lines = DOCUMENT.lines();
    while let Some(line) = lines.next() {
        let Some(name) = line.trim().strip_prefix("```json fixture=") else {
            continue;
        };
        let body: Vec<&str> = lines.by_ref().take_while(|l| l.trim() != "```").collect();
        let value: Value = serde_json::from_str(&body.join("\n"))
            .unwrap_or_else(|e| panic!("document block {name} is not JSON: {e}"));
        assert!(
            blocks.insert(name.trim().to_owned(), value).is_none(),
            "document block {name} appears twice"
        );
    }
    blocks
}

#[test]
fn every_document_example_is_a_fixture_and_every_fixture_is_in_the_document() {
    let blocks = document_blocks();
    let files = fixtures();
    let names = |m: &BTreeMap<String, Value>| m.keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        names(&blocks),
        names(&files),
        "document blocks vs fixture files"
    );
    for (name, value) in &files {
        assert_eq!(
            &blocks[name], value,
            "{name}: document block differs from fixture"
        );
    }
}

/// A public request's `project` as the wire's selector.
fn wire_request(mut public: Value) -> Value {
    let project = public["project"].as_str().unwrap();
    public["project"] = serde_json::to_value(project.parse::<ProjectSelector>().unwrap()).unwrap();
    public
}
/// Decodes as `T`; re-encoding keeps every given field and decodes to the same value.
fn request<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str) -> T {
    let wire = wire_request(fixture(name));
    let decoded: T = serde_json::from_value(wire.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    let again = serde_json::to_value(&decoded).unwrap();
    for (key, value) in wire.as_object().unwrap() {
        assert_eq!(&again[key], value, "{name}.{key} after a round trip");
    }
    assert_eq!(serde_json::from_value::<T>(again).unwrap(), decoded);
    decoded
}
/// Decodes as `T` and re-encodes to exactly the fixture.
fn exact<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(name: &str) -> T {
    exact_value(name, fixture(name))
}
fn exact_value<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    name: &str,
    value: Value,
) -> T {
    let decoded: T =
        serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(serde_json::to_value(&decoded).unwrap(), value, "{name}");
    decoded
}

#[test]
fn read_requests_and_replies_round_trip() {
    let first: PlanRead = request("plan_read.request");
    assert!(first.compact, "plan_read is compact unless asked");
    assert_eq!(first.limit, 2);
    let next: PlanRead = request("plan_read.next.request");
    assert_eq!(next.filter(), first.filter());
    let full: PlanRead = request("plan_read.full.request");
    assert!(!full.compact);
    assert_eq!(full.limit, DEFAULT_LIMIT);

    let page: PlanReadResult = exact("plan_read.reply");
    assert!(page.steps.iter().all(|s| matches!(s, StepView::Compact(_))));
    assert_eq!(page.next_cursor.as_deref(), next.cursor.as_deref());
    let page: PlanReadResult = exact("plan_read.full.reply");
    let [StepView::Full(notes)] = page.steps.as_slice() else {
        panic!("one full step")
    };
    assert_eq!(notes.references.len(), 3);
    assert!(page.next_cursor.is_none());

    let get: StepGet = request("step_get.request");
    assert!(!get.compact, "step_get is full unless asked");
    let step: StepGetResult = exact("step_get.reply");
    assert!(matches!(
        step.step,
        StepView::Full(FullStep {
            paused: PauseValue::Reason(_),
            ..
        })
    ));

    let get: UnitGet = request("unit_get.request");
    assert!(get.compact);
    let unit: UnitGetResult = exact("unit_get.reply");
    assert_eq!(unit.unit.steps.len(), 3);

    let _: PlanGetResult = exact("plan_get.reply");
    let view: PlanViewQuery = request("plan_view.request");
    assert!(!view.all);
    let history: PlanHistoryQuery = request("plan_history.request");
    assert_eq!(history.since_rev, None);
    let page: PlanHistoryPage = exact("plan_history.reply");
    assert_eq!(page.next_after_seq, Some(page.entries.last().unwrap().seq));
    let origin: HistoryRecord = exact("plan_history.origin");
    let HistoryEvent::PlanEdit(origin) = origin.event else {
        panic!("rev 1 is a plan edit")
    };
    assert_eq!(
        origin.changes,
        vec![PlanChange::HeaderPut {
            root_order: vec![RootSection::Steps]
        }]
    );
}

#[test]
fn edit_requests_and_replies_round_trip() {
    let edit: PlanEditRequest = request("plan_edit.request");
    assert!(edit.start && !edit.dry_run);
    assert_eq!(edit.preview_scope, PreviewScope::Impact);
    assert_eq!(edit.rev, Some(Revision(41)));
    let reply: EditResult = exact("plan_edit.reply");
    assert_eq!(reply.preview.scope, PreviewScope::Impact);
    let record: HistoryRecord = exact("plan_edit.record");
    let HistoryEvent::PlanEdit(event) = record.event else {
        panic!("a plan edit record")
    };
    assert_eq!(
        event.changes, reply.preview.changes,
        "record and preview agree"
    );
    assert_eq!(event.rev, reply.rev);

    let dry: PlanEditRequest = request("plan_edit.dry_run.request");
    assert!(dry.dry_run);
    assert_eq!(dry.preview_scope, PreviewScope::All);
    let [PlanOp::StepUpdate { changes, .. }] = dry.ops.as_slice() else {
        panic!("one step.update")
    };
    assert_eq!(changes.paused, Some(None), "null removes a field");
    assert_eq!(changes.fields(), vec![("paused", None)]);
    let preview: EditPreview = exact("plan_edit.dry_run.reply");
    assert_eq!(preview.scope, PreviewScope::All);

    let update: UnitUpdate = request("unit_update.request");
    assert_eq!(update.changes.len(), 2);
    let remove: UnitRemove = request("unit_remove.request");
    assert_eq!(remove.rev, Some(Revision(44)));
    let reply: EditResult = exact("unit_remove.reply");
    assert_eq!(reply.steps.unwrap().len(), 3);
    let prune: PruneResult = exact("plan_prune.reply");
    assert_eq!(prune.kept.len(), 2);
    let inputs: InputEditResult = exact("step_set_input.reply");
    assert_eq!(inputs.running.len(), 1);
}

#[test]
fn every_operation_and_change_round_trips() {
    let ops: Vec<PlanOp> = exact("plan_op.all");
    let kinds: Vec<_> = fixture("plan_op.all")
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["op"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        kinds,
        [
            "input.put",
            "input.remove",
            "output.put",
            "output.remove",
            "step.add",
            "step.update",
            "step.remove",
            "edge.add",
            "edge.remove",
            "unit.add",
            "unit.update",
            "unit.remove",
            "order.set"
        ],
        "one fixture per operation, in the document's order"
    );
    assert_eq!(ops.len(), 13);
    let changes: Vec<PlanChange> = exact("plan_change.all");
    assert_eq!(changes.len(), 7, "one fixture per change kind");

    // Closed unions: an unknown operation, change or field is refused.
    for bad in [
        json!({"op": "plan.patch", "ops": []}),
        json!({"op": "step.add", "step": "x", "spec": {}, "start": false}),
        json!({"op": "step.update", "step": "x", "changes": {"when": "y"}}),
    ] {
        assert!(
            serde_json::from_value::<PlanOp>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    assert!(serde_json::from_value::<PlanChange>(json!({"op": "step.put", "step": "x"})).is_err());
    assert!(StepChanges::default().is_empty());
}

#[test]
fn unit_add_defaults_and_order_collections() {
    let op: PlanOp =
        serde_json::from_value(json!({"op": "unit.add", "recipe": "lane", "unit": "u"})).unwrap();
    let PlanOp::UnitAdd {
        params,
        after,
        inputs,
        tags,
        ..
    } = &op
    else {
        panic!("unit.add")
    };
    assert!(params.0.is_empty() && after.is_empty() && inputs.is_empty() && tags.is_empty());
    assert_eq!(
        serde_json::to_value(&op).unwrap(),
        json!({"op": "unit.add", "recipe": "lane", "unit": "u", "params": {}})
    );
    for (word, collection) in [
        ("steps", OrderCollection::Steps),
        ("inputs", OrderCollection::Inputs),
        ("outputs", OrderCollection::Outputs),
    ] {
        assert_eq!(serde_json::to_value(collection).unwrap(), json!(word));
    }
}

#[test]
fn cursor_text_and_filter_digest_are_pinned() {
    let f = fixture("cursor");
    let project: ProjectId = f["project_id"].as_str().unwrap().parse().unwrap();
    let filter = PlanReadFilter {
        units: Some(vec![UnitName::new("normalize").unwrap()]),
        steps: None,
        status: serde_json::from_value(f["filter"]["status"].clone()).unwrap(),
        recipe: None,
    };
    assert_eq!(filter.digest(project), f["digest"].as_str().unwrap());
    // Filters are sets: order and repeats do not change the digest.
    let mut shuffled = filter.clone();
    shuffled.status.as_mut().unwrap().reverse();
    shuffled
        .units
        .as_mut()
        .unwrap()
        .push(UnitName::new("normalize").unwrap());
    assert_eq!(shuffled.digest(project), filter.digest(project));
    assert_ne!(
        PlanReadFilter::default().digest(project),
        filter.digest(project)
    );
    assert!(filter.binds_state() && !filter.binds_recipes());

    let cursor = PlanCursor {
        filter: filter.digest(project),
        rev: Revision(f["rev"].as_u64().unwrap()),
        state_epoch: Some(StateEpoch(f["state_epoch"].as_u64().unwrap())),
        recipe_generation: None,
        position: f["position"].as_u64().unwrap(),
        step: StepId::new(f["step"].as_str().unwrap()).unwrap(),
    };
    let text = f["text"].as_str().unwrap();
    assert_eq!(cursor.encode(), text);
    assert_eq!(PlanCursor::parse(text).unwrap(), cursor);
    assert_eq!(
        fixture("plan_read.reply")["next_cursor"],
        json!(text),
        "the reply's cursor is this one"
    );
    let with_recipes = PlanCursor {
        recipe_generation: Some(RecipeGeneration("5f0c1e9a7b3d2468".into())),
        state_epoch: None,
        ..cursor
    };
    assert_eq!(
        PlanCursor::parse(&with_recipes.encode()).unwrap(),
        with_recipes
    );
    for bad in [
        "",
        "v2:8a7c2b92c2848cc3:41:9120:-:503:normalize-work",
        "v1:8a7c2b92:41:9120:-:503:normalize-work",
        "v1:8a7c2b92c2848cc3:41:x:-:503:normalize-work",
        "v1:8a7c2b92c2848cc3:41:9120:-:503:Normalize",
        "v1:8a7c2b92c2848cc3:41:9120:-:503:normalize-work:extra",
    ] {
        assert_eq!(
            PlanCursor::parse(bad),
            Err(PlanRowsError::CursorMalformed),
            "{bad}"
        );
    }
}

#[test]
fn refusals_have_their_pinned_kind_and_message() {
    for case in fixture("errors").as_array().unwrap() {
        let name = case["case"].as_str().unwrap();
        let error: PublicError = exact_value(name, case["error"].clone());
        let made: PublicError = match name {
            "limit" => PlanRowsError::Limit,
            "no_ops" => PlanRowsError::NoOps,
            "no_changes" => PlanRowsError::Empty {
                path: "ops[0].changes".into(),
                what: "field",
            },
            "preview_all_needs_dry_run" => PlanRowsError::PreviewAllNeedsDryRun,
            "order_needs_rev" => PlanRowsError::OrderNeedsRev,
            "cursor_malformed" => PlanRowsError::CursorMalformed,
            "cursor_mismatch" => PlanRowsError::CursorMismatch,
            "cursor_expired" => PlanRowsError::CursorExpired,
            "no_step" => PlanRowsError::NoStep {
                step: StepId::new("normalize-wrk").unwrap(),
            },
            "no_unit" => PlanRowsError::NoUnit {
                unit: UnitName::new("normalise").unwrap(),
            },
            "stale_rev" => PlanRowsError::StaleRev {
                current: Revision(43),
            },
            "no_steps" => PlanRowsError::Empty {
                path: "ops[1].steps".into(),
                what: "step",
            },
            "no_entries" => PlanRowsError::Empty {
                path: "ops[2].after".into(),
                what: "entry",
            },
            "edit_contended" => PlanRowsError::EditContended,
            "live_work_in_backup" => PlanRowsError::LiveWorkInBackup {
                attempts: 2,
                runs: 2,
                leases: 1,
                calls: 0,
            },
            // Validation keeps the existing `invalid` shape, with per-operation paths.
            "invalid_ops" => continue,
            other => panic!("unknown case {other}"),
        }
        .into();
        assert_eq!(made, error, "{name}");
    }
}

#[test]
fn request_schemas_name_their_public_fields() {
    fn fields<T: schemars::JsonSchema>() -> Vec<String> {
        let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
        schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }
    assert_eq!(
        fields::<PlanRead>(),
        [
            "project", "units", "steps", "status", "recipe", "compact", "limit", "cursor"
        ]
    );
    assert_eq!(fields::<StepGet>(), ["project", "step", "compact"]);
    assert_eq!(fields::<UnitGet>(), ["project", "unit", "compact"]);
    assert_eq!(
        fields::<PlanEditRequest>(),
        [
            "project",
            "ops",
            "rev",
            "dry_run",
            "preview_scope",
            "start",
            "reason",
            "author"
        ]
    );
    assert_eq!(
        fields::<UnitUpdate>(),
        [
            "project",
            "unit",
            "changes",
            "rev",
            "dry_run",
            "preview_scope",
            "reason",
            "author"
        ]
    );
    assert_eq!(
        fields::<UnitRemove>(),
        [
            "project",
            "unit",
            "rev",
            "dry_run",
            "preview_scope",
            "reason",
            "author"
        ]
    );
    assert_eq!(
        fields::<PlanViewQuery>(),
        [
            "project", "format", "all", "units", "steps", "status", "recipe"
        ]
    );
    assert_eq!(
        fields::<PlanHistoryQuery>(),
        ["project", "since_rev", "after_seq", "limit"]
    );
    assert_eq!(StepChanges::KEYS.len(), fields::<StepChanges>().len());
    for key in StepChanges::KEYS {
        assert!(fields::<StepChanges>().contains(&key.to_owned()), "{key}");
    }
    let required = |schema: Value| -> Vec<String> {
        serde_json::from_value(schema["required"].clone()).unwrap_or_default()
    };
    assert_eq!(
        required(serde_json::to_value(schemars::schema_for!(PlanEditRequest)).unwrap()),
        ["project", "ops", "reason"]
    );
    assert_eq!(
        required(serde_json::to_value(schemars::schema_for!(PlanRead)).unwrap()),
        ["project"]
    );
}

/// The supplied-shape refusals (§7.6): only what a caller sends is checked, the first refusal
/// wins, and the fixtures' own requests pass.
#[test]
fn supplied_shapes_are_checked_in_order() {
    fn edit(ops: Value, extra: Value) -> PlanEditRequest {
        let mut request = json!({"project": "lash", "reason": "r", "ops": ops});
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        serde_json::from_value(wire_request(request)).unwrap()
    }
    let message = |error: PlanRowsError| match PublicError::from(error) {
        PublicError::BadRequest { message } => message,
        other => panic!("{other:?}"),
    };
    let refused =
        |ops: Value, extra: Value| message(edit(ops, extra).check_supplied().unwrap_err());
    let update = json!({"op": "step.update", "step": "a", "changes": {"priority": 1}});
    assert_eq!(
        refused(json!([]), json!({})),
        "ops: name at least one operation"
    );
    assert_eq!(
        refused(
            json!([{"op": "step.update", "step": "a", "changes": {}}]),
            json!({})
        ),
        "ops[0].changes: name at least one field"
    );
    assert_eq!(
        refused(
            json!([update, {"op": "step.remove", "steps": []}]),
            json!({})
        ),
        "ops[1].steps: name at least one step"
    );
    assert_eq!(
        refused(
            json!([update, update, {"op": "edge.remove", "step": "a", "after": []}]),
            json!({})
        ),
        "ops[2].after: name at least one entry"
    );
    assert_eq!(
        refused(
            json!([{"op": "unit.update", "unit": "u", "changes": {}}]),
            json!({})
        ),
        "ops[0].changes: name at least one step"
    );
    assert_eq!(
        refused(
            json!([{"op": "unit.update", "unit": "u", "changes": {"u-a": {"priority": 1}, "u-b": {}}}]),
            json!({})
        ),
        "ops[0].changes.u-b: name at least one field"
    );
    // Empty lists come before the options, and preview_scope before order.set's rev.
    assert_eq!(
        refused(
            json!([{"op": "step.remove", "steps": []}]),
            json!({"preview_scope": "all"})
        ),
        "ops[0].steps: name at least one step"
    );
    let order = json!({"op": "order.set", "collection": "steps", "ids": []});
    assert_eq!(
        refused(json!([order]), json!({"preview_scope": "all"})),
        "preview_scope \"all\" needs dry_run: true"
    );
    assert_eq!(
        refused(json!([order]), json!({})),
        "order.set needs rev: the revision whose order it lists"
    );
    // An empty collection's order.set lists no ids, and that is a valid (no-op) operation.
    edit(json!([order]), json!({"rev": 4}))
        .check_supplied()
        .unwrap();
    edit(
        json!([order]),
        json!({"rev": 4, "preview_scope": "all", "dry_run": true}),
    )
    .check_supplied()
    .unwrap();
    request::<PlanEditRequest>("plan_edit.request")
        .check_supplied()
        .unwrap();
    request::<PlanEditRequest>("plan_edit.dry_run.request")
        .check_supplied()
        .unwrap();

    let unit = |changes: Value| -> UnitUpdate {
        serde_json::from_value(wire_request(
            json!({"project": "lash", "unit": "u", "reason": "r", "changes": changes}),
        ))
        .unwrap()
    };
    assert_eq!(
        message(unit(json!({})).check_supplied().unwrap_err()),
        "changes: name at least one step"
    );
    assert_eq!(
        message(unit(json!({"u-a": {}})).check_supplied().unwrap_err()),
        "changes.u-a: name at least one field"
    );
    request::<UnitUpdate>("unit_update.request")
        .check_supplied()
        .unwrap();
}

/// The validation matrix's counterexamples (§6.1), checked against today's whole-plan
/// compiler (the reference's): each `base` compiles, and each `candidate` (the base with `ops`
/// applied) fails with exactly `errors`. `tests/incremental.rs` runs the same cases through
/// the incremental path.
#[test]
fn validation_counterexamples_match_the_whole_plan_compiler() {
    use sluice_reference::{Plan as Reference, harness::Open};
    let compile = |document: &Value| Reference::parse_json(document.to_string().as_bytes(), &Open);
    let cases = fixture("validation.differential");
    let cases = cases.as_array().unwrap();
    let names: BTreeSet<_> = cases.iter().map(|c| c["case"].as_str().unwrap()).collect();
    for required in [
        "output_put_missing_source",
        "input_put_collides_with_untouched_step",
    ] {
        assert!(
            names.contains(required),
            "the review's counterexample {required}"
        );
    }
    for case in cases {
        let name = case["case"].as_str().unwrap();
        let _: Vec<PlanOp> = exact_value(name, case["ops"].clone());
        compile(&case["base"]).unwrap_or_else(|e| panic!("{name}: base does not compile: {e:?}"));
        let expected: Vec<String> = serde_json::from_value(case["errors"].clone()).unwrap();
        let found: Vec<String> = match (compile(&case["candidate"]), case.get("values")) {
            (Err(errors), _) => errors.iter().map(ToString::to_string).collect(),
            (Ok(plan), Some(values)) => plan
                .validate_input_values(&serde_json::from_value(values.clone()).unwrap())
                .expect_err(name)
                .iter()
                .map(ToString::to_string)
                .collect(),
            (Ok(_), None) => panic!("{name}: the candidate compiles"),
        };
        assert_eq!(found, expected, "{name}");
    }
}

/// §10.6's positions, by a reference of its four steps: assign retain/append/gap, compare the
/// complete key sequence with the document's, renumber when they differ.
#[test]
fn replayed_positions_follow_the_complete_sequence_rule() {
    fn assign(before: &BTreeMap<String, u64>, keys: &[String]) -> (BTreeMap<String, u64>, bool) {
        let mut next = before.values().max().map_or(0, |m| m + 1);
        let tentative: BTreeMap<String, u64> = keys
            .iter()
            .map(|k| {
                let position = before.get(k).copied().unwrap_or_else(|| {
                    next += 1;
                    next - 1
                });
                (k.clone(), position)
            })
            .collect();
        let mut sequence: Vec<_> = tentative.iter().collect();
        sequence.sort_by_key(|(_, p)| **p);
        if sequence
            .iter()
            .map(|(k, _)| k.as_str())
            .eq(keys.iter().map(String::as_str))
        {
            (tentative, false)
        } else {
            let renumbered = keys.iter().cloned().zip(0..).collect();
            (renumbered, true)
        }
    }
    for case in fixture("replay.positions").as_array().unwrap() {
        let name = case["case"].as_str().unwrap();
        let before: BTreeMap<String, u64> = serde_json::from_value(case["before"].clone()).unwrap();
        let keys: Vec<String> = serde_json::from_value(case["keys"].clone()).unwrap();
        let (positions, renumbered) = assign(&before, &keys);
        let mut puts: Vec<_> = positions
            .iter()
            .filter(|(k, p)| before.get(*k) != Some(p))
            .collect();
        puts.sort_by_key(|(_, p)| **p);
        let puts: Vec<String> = puts.into_iter().map(|(k, _)| k.clone()).collect();
        let deletes: Vec<String> = before
            .keys()
            .filter(|k| !positions.contains_key(*k))
            .cloned()
            .collect();
        let expected: BTreeMap<String, u64> =
            serde_json::from_value(case["positions"].clone()).unwrap();
        assert_eq!(positions, expected, "{name}: positions");
        assert_eq!(json!(renumbered), case["renumbered"], "{name}: renumbered");
        assert_eq!(json!(puts), case["puts"], "{name}: puts");
        assert_eq!(json!(deletes), case["deletes"], "{name}: deletes");
        // The positions always give the document's order back.
        let mut order: Vec<_> = positions.iter().collect();
        order.sort_by_key(|(_, p)| **p);
        assert!(
            order.iter().map(|(k, _)| *k).eq(keys.iter()),
            "{name}: order"
        );
    }
}

/// The cutover's report (§10.1): its shape, and advice that follows each run's actual outcome,
/// never the request.
#[test]
fn cutover_report_advice_follows_the_actual_outcome() {
    let report: CutoverReport = exact("cutover.report");
    let _: CancelRefusal = exact("cutover.refused");
    assert!(report.refused.is_empty());
    for run in &report.stopped {
        assert!(
            run.step.is_some() != run.call.is_some(),
            "a step run or a call"
        );
        let advice = match (&run.call, &run.step_status, run.step_error.as_deref()) {
            (Some(_), _, _) => RetryAdvice::CallAgain,
            (None, Some(StepStatus::Succeeded), _) => RetryAdvice::None,
            (None, None, _) if run.outcome.status == StepStatus::Succeeded => RetryAdvice::None,
            (None, _, Some("cancelled")) => RetryAdvice::Retry,
            (None, _, _) => RetryAdvice::ReadThenRetry,
        };
        assert_eq!(run.advice, advice, "{run:?}");
        assert_eq!(
            run.requested,
            if run.call.is_some() {
                StopRequest::Stop
            } else {
                StopRequest::Cancel
            }
        );
    }
    let outcomes: BTreeSet<_> = report
        .stopped
        .iter()
        .map(|r| (r.outcome.status.clone(), r.advice))
        .map(|(s, a)| format!("{s:?}/{a:?}"))
        .collect();
    assert!(
        outcomes.len() >= 4,
        "the fixture covers a settled success, a cancel, an earlier scatter failure and a call"
    );
}
