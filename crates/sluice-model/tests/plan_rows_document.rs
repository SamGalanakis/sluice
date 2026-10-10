//! The plan document taken apart into rows and assembled from them (`PlanRows::from_document`,
//! `to_document`, `changes_from`, §10.6's replayed positions) and the index rows derived from
//! declarations (§2.5: `plan_index`).

use serde_json::{Value, json};
use sluice_model::{
    ids::{Revision, StepId},
    plan::{FnSignature, Plan, SignatureProvider},
    plan_index::{output_references, plan_edges, step_index},
    plan_rows::*,
    rpc::JsonMap,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::PathBuf,
};

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plan_rows")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}
fn map(value: Value) -> JsonMap {
    serde_json::from_value(value).unwrap()
}
fn compact(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap()
}
fn steps_rows(positions: &BTreeMap<String, u64>) -> PlanRows {
    let mut steps: Vec<StepRow> = positions
        .iter()
        .map(|(key, position)| StepRow {
            step: key.parse().unwrap(),
            position: *position,
            declaration: map(json!({"run": "x"})),
        })
        .collect();
    steps.sort_by_key(|row| row.position);
    PlanRows {
        header: PlanHeader {
            rev: Revision(1),
            root_order: vec![RootSection::Steps],
            state_epoch: StateEpoch(0),
        },
        inputs: vec![],
        outputs: vec![],
        steps,
    }
}

/// `from_document` gives `replay.positions`' positions, and the changes it implies are the
/// fixture's puts (in position order) and deletes (in key order); `replay_positions` says when
/// it renumbered.
#[test]
fn from_document_passes_replay_positions() {
    for case in fixture("replay.positions").as_array().unwrap() {
        let name = case["case"].as_str().unwrap();
        let before: BTreeMap<String, u64> = serde_json::from_value(case["before"].clone()).unwrap();
        let keys: Vec<String> = serde_json::from_value(case["keys"].clone()).unwrap();
        let base = steps_rows(&before);
        let document = map(json!({
            "steps": keys.iter().map(|k| (k.clone(), json!({"run": "x"}))).collect::<serde_json::Map<_, _>>()
        }));
        let rows = PlanRows::from_document(&document, Some(&base)).unwrap();
        let positions: BTreeMap<String, u64> = rows
            .steps
            .iter()
            .map(|row| (row.step.to_string(), row.position))
            .collect();
        let expected: BTreeMap<String, u64> =
            serde_json::from_value(case["positions"].clone()).unwrap();
        assert_eq!(positions, expected, "{name}: positions");
        let (_, renumbered) = replay_positions(
            &before
                .iter()
                .map(|(k, p)| (k.clone(), *p))
                .collect::<Vec<_>>(),
            &keys,
        );
        assert_eq!(json!(renumbered), case["renumbered"], "{name}: renumbered");
        let changes = rows.changes_from(Some(&base));
        let puts: Vec<String> = changes
            .iter()
            .filter_map(|c| match c {
                PlanChange::StepPut { step, .. } => Some(step.to_string()),
                _ => None,
            })
            .collect();
        let deletes: Vec<String> = changes
            .iter()
            .filter_map(|c| match c {
                PlanChange::StepDelete { step } => Some(step.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(json!(puts), case["puts"], "{name}: puts");
        assert_eq!(json!(deletes), case["deletes"], "{name}: deletes");
        assert!(
            !changes
                .iter()
                .any(|c| matches!(c, PlanChange::HeaderPut { .. })),
            "{name}: the sections did not change"
        );
        assert_eq!(compact(&rows.to_document()), compact(&document), "{name}");
    }
}

/// The export of §7.2's plan is the document exactly (sections, keys and every key inside a
/// declaration in order), and with no base its changes are its header and a put of each row.
#[test]
fn rows_export_the_document_they_were_taken_from() {
    let plan = map(fixture("plan_get.reply")["plan"].clone());
    let rows = PlanRows::from_document(&plan, None).unwrap();
    assert_eq!(
        rows.header.root_order,
        vec![
            RootSection::Inputs,
            RootSection::Outputs,
            RootSection::Steps
        ]
    );
    assert_eq!(
        rows.steps.iter().map(|r| r.position).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(compact(&rows.to_document()), compact(&plan));
    let changes = rows.changes_from(None);
    assert_eq!(
        changes
            .iter()
            .map(|c| compact(c).split('"').nth(3).unwrap().to_owned())
            .collect::<Vec<_>>(),
        [
            "header.put",
            "input.put",
            "input.put",
            "output.put",
            "step.put",
            "step.put",
            "step.put"
        ]
    );
    // Nothing changes against itself; a key order inside a declaration is a change.
    assert!(rows.changes_from(Some(&rows)).is_empty());
    let mut reordered = plan.clone();
    let steps = reordered.0.get_mut("steps").unwrap();
    let mut value = steps.as_value().clone();
    let work = value["work"].as_object_mut().unwrap();
    let run = work.shift_remove("run").unwrap();
    work.insert("run".into(), run);
    *steps = value.try_into().unwrap();
    let moved = PlanRows::from_document(&reordered, Some(&rows)).unwrap();
    assert!(matches!(
        moved.changes_from(Some(&rows)).as_slice(),
        [PlanChange::StepPut { step, position: 0, .. }] if step.as_str() == "work"
    ));
    // Omitted and empty sections differ, and so does their order.
    for (document, root) in [
        (json!({"steps": {}}), vec![RootSection::Steps]),
        (
            json!({"steps": {}, "inputs": {}}),
            vec![RootSection::Steps, RootSection::Inputs],
        ),
    ] {
        let rows = PlanRows::from_document(&map(document.clone()), None).unwrap();
        assert_eq!(rows.header.root_order, root);
        assert_eq!(compact(&rows.to_document()), compact(&document));
    }
    for (document, error) in [
        (json!({"inputs": {}}), "plan has no steps"),
        (
            json!({"steps": {}, "when": {}}),
            "plan has an unknown root field when",
        ),
        (json!({"steps": {"a": 1}}), "steps.a: expected an object"),
        (
            json!({"steps": [], "inputs": {}}),
            "steps: expected an object",
        ),
        (
            json!({"steps": {}, "outputs": {"o": "a/b"}}),
            "outputs.o: expected an object",
        ),
    ] {
        assert_eq!(
            PlanRows::from_document(&map(document), None).unwrap_err(),
            error
        );
    }
}

fn references(rows: &[ReferenceRow]) -> Value {
    json!(
        rows.iter()
            .map(|r| json!({
                "kind": r.kind, "slot": r.slot, "ordinal": r.ordinal, "source_kind": r.source_kind,
                "source_id": r.source_id, "source_port": r.source_port, "source_path": r.source_path,
            }))
            .collect::<Vec<_>>()
    )
}

/// §2.5's derivation gives the references the full step views pin, and a plan output's one row.
#[test]
fn step_index_gives_the_pinned_references() {
    let plan = map(fixture("plan_get.reply")["plan"].clone());
    let rows = PlanRows::from_document(&plan, None).unwrap();
    let ids: HashSet<String> = rows.steps.iter().map(|r| r.step.to_string()).collect();
    let is_step = |name: &str| ids.contains(name);
    let notes = rows
        .steps
        .iter()
        .find(|r| r.step.as_str() == "notes")
        .unwrap();
    let index = step_index(&notes.step, &notes.declaration, &is_step);
    assert_eq!(index.unit.as_str(), "notes");
    assert!(index.tags.is_empty());
    assert_eq!(
        references(&index.references),
        fixture("plan_read.full.reply")["steps"][0]["references"]
    );
    let gate = rows
        .steps
        .iter()
        .find(|r| r.step.as_str() == "gate")
        .unwrap();
    let index = step_index(&gate.step, &gate.declaration, &is_step);
    assert_eq!(index.unit.as_str(), "build");
    assert_eq!(index.tags, ["unit:build", "exit"]);
    let output = &rows.outputs[0];
    assert_eq!(
        output_references(&output.name, &output.binding),
        vec![ReferenceRow {
            consumer_kind: ConsumerKind::Output,
            consumer_id: "notes".into(),
            slot: "source".into(),
            ordinal: 0,
            kind: RefKind::Output,
            source_kind: SourceKind::Step,
            source_id: "notes".into(),
            source_port: "final".into(),
            source_path: "".into(),
        }]
    );
    let step = fixture("step_get.reply")["step"].clone();
    let declaration = map(step["spec"].clone());
    let index = step_index(&"normalize-work".parse().unwrap(), &declaration, &|name| {
        name == "normalize-fork"
    });
    assert_eq!(index.unit.as_str(), "normalize");
    assert_eq!(references(&index.references), step["references"]);
    // Gate entries of every kind, a fan-in and duplicate tags.
    let declaration = map(json!({
        "run": "x",
        "tags": ["a", "unit:u", "a", "unit:v"],
        "after": ["s?", "unit:w?", "!flag.on", "s/ok", "missing?", "!unit:w", "brief.ok"],
        "in": {"many": {"source": ["s/out.0", "brief"]}, "lit": {"default": 1}, "f": {"file": "x.md"}}
    }));
    let index = step_index(&"t".parse().unwrap(), &declaration, &|name| name == "s");
    assert_eq!(index.unit.as_str(), "u");
    assert_eq!(index.tags, ["a", "unit:u", "unit:v"]);
    let found: Vec<(String, u32, String, String, String, String)> = index
        .references
        .iter()
        .map(|r| {
            (
                r.slot.clone(),
                r.ordinal,
                format!("{:?}", r.source_kind),
                r.source_id.clone(),
                r.source_port.clone(),
                r.source_path.clone(),
            )
        })
        .collect();
    let row = |slot: &str, ordinal, kind: &str, id: &str, port: &str, path: &str| {
        (
            slot.to_owned(),
            ordinal,
            kind.to_owned(),
            id.to_owned(),
            port.to_owned(),
            path.to_owned(),
        )
    };
    assert_eq!(
        found,
        vec![
            row("after", 0, "Step", "s", "", ""),
            row("after", 1, "Unit", "w", "", ""),
            row("after", 2, "Input", "flag", "", "on"),
            row("after", 3, "Step", "s", "ok", ""),
            row("after", 6, "Input", "brief", "", "ok"),
            row("in.many", 0, "Step", "s", "out", "0"),
            row("in.many", 1, "Input", "brief", "", ""),
        ]
    );
}

/// `plan_edges` are today's expanded dependencies split by kind: data edges from bindings,
/// gate edges from step and boolean gates, and a unit gate's edges from that unit's exits.
#[test]
fn plan_edges_are_the_expanded_dependencies() {
    struct Open;
    impl SignatureProvider for Open {
        fn signature(&self, name: &str) -> Option<FnSignature> {
            (name != "nope").then(|| FnSignature {
                open: true,
                ..Default::default()
            })
        }
    }
    let plan = json!({
        "inputs": {"go": "boolean"},
        "steps": {
            "a-fork": {"run": "x", "tags": ["unit:a"], "outputs": {"out": "string", "ok": "boolean"}},
            "a-work": {"run": "x", "tags": ["unit:a"], "in": {"spec": {"source": "a-fork/out"}},
                       "outputs": {"out": "string"}},
            "a-land": {"run": "x", "tags": ["unit:a", "exit"], "after": ["a-fork"], "outputs": {"out": "string"}},
            "b": {"run": "x", "after": ["unit:a", "a-fork/ok", "go", "a-work?"],
                  "in": {"all": {"source": ["a-work/out", "a-land/out", "a-work/out"]}}},
            "c": {"run": "x", "after": ["unit:b"], "in": {"x": {"source": "b/missing"}}},
            "d-one": {"run": "x", "tags": ["unit:d"], "outputs": {"v": "string"}},
            "d-two": {"run": "x", "tags": ["unit:d"], "outputs": {"v": "string"}},
            "e": {"run": "x", "after": ["unit:d"]}
        }
    });
    let compiled = Plan::parse(&map(plan.clone()), &Open);
    // c reads an output b does not declare: today's compiler refuses it, the edges still come.
    assert!(compiled.is_err());
    let mut valid = plan.clone();
    valid["steps"]["c"]["in"]["x"] = json!({"default": "1"});
    let compiled = Plan::parse(&map(valid.clone()), &Open).unwrap();
    let rows = PlanRows::from_document(&map(valid), None).unwrap();
    let edges = plan_edges(&rows);
    let expanded: BTreeSet<(String, String)> = compiled
        .steps()
        .keys()
        .flat_map(|target| {
            compiled
                .dependencies(target)
                .iter()
                .map(move |source| (source.to_string(), target.to_string()))
        })
        .collect();
    let derived: BTreeSet<(String, String)> = edges
        .iter()
        .map(|e| (e.source.to_string(), e.target.to_string()))
        .collect();
    assert_eq!(derived, expanded);
    let of = |target: &str| -> BTreeSet<String> {
        edges
            .iter()
            .filter(|e| e.target.as_str() == target)
            .map(|e| {
                format!(
                    "{:?}:{}:{}",
                    e.kind,
                    e.source,
                    e.via_unit.as_ref().map_or("", |u| u.as_str())
                )
            })
            .collect()
    };
    assert_eq!(
        of("b"),
        BTreeSet::from([
            "Data:a-work:".to_owned(),
            "Data:a-land:".to_owned(),
            "Gate:a-fork:".to_owned(),
            "Gate:a-work:".to_owned(),
            "Gate:a-land:a".to_owned(),
        ])
    );
    assert_eq!(of("c"), BTreeSet::from(["Gate:b:b".to_owned()]));
    assert_eq!(
        of("e"),
        BTreeSet::from(["Gate:d-one:d".to_owned(), "Gate:d-two:d".to_owned()]),
        "an untagged unit's exits are its sinks"
    );
    let _ = StepId::new("x").unwrap();
}
