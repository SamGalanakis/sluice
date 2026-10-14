//! The schema-3 plan contracts (`docs/design/plan-rows.md`): every JSON example the document
//! pins is a fixture here, equal to the document's block, and round-trips through its type.

use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, ProjectSelector, Revision, StepId, UnitName},
    plan_rows::*,
};
use std::{collections::BTreeMap, path::PathBuf};

static FIXTURES: std::sync::LazyLock<BTreeMap<String, Value>> =
    std::sync::LazyLock::new(load_fixtures);

const DOCUMENT: &str = include_str!("../../../../docs/design/plan-rows.md");

fn load_fixtures() -> BTreeMap<String, Value> {
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
    FIXTURES
        .get(name)
        .cloned()
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

#[test]
fn documented_plan_rows_codecs() {
    {
        let blocks = document_blocks();
        let files = &*FIXTURES;
        let names = |m: &BTreeMap<String, Value>| m.keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            names(&blocks),
            names(files),
            "document blocks vs fixture files"
        );
        for (name, value) in files {
            assert_eq!(
                &blocks[name], value,
                "{name}: document block differs from fixture"
            );
        }
    }

    {
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

    {
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

    {
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
        assert!(
            serde_json::from_value::<PlanChange>(json!({"op": "step.put", "step": "x"})).is_err()
        );
        assert!(StepChanges::default().is_empty());
    }

    {
        let op: PlanOp =
            serde_json::from_value(json!({"op": "unit.add", "recipe": "lane", "unit": "u"}))
                .unwrap();
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

    {
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

    {
        let _: CutoverReport = exact("cutover.report");
        let _: CancelRefusal = exact("cutover.refused");
    }
}
