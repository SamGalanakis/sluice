//! Lane H's independent readings of the plan rows hold against the contract's own examples,
//! before any lane's code is compared with them: the index rows §2.5 derives for the plans in
//! the fixtures, a plan rebuilt from logged changes and exported in order, the schema
//! comparison that names what differs, and the logical dump that says whether a database
//! changed.
#[path = "../../../tests/support/home.rs"]
mod home;
#[path = "../../../tests/support/plan_rows.rs"]
mod plan_rows;

use home::ScratchHome;
use plan_rows::*;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};
use sluice_model::plan_rows::{
    FullStep, PlanChange, PlanGetResult, PlanReadResult, RootSection, StepGetResult, StepView,
};
use sluice_store::Writer;

macro_rules! fixture {
    ($name:expr) => {
        serde_json::from_slice(
            &std::fs::read(format!(
                "{}/../sluice-model/tests/fixtures/plan_rows/{}.json",
                env!("CARGO_MANIFEST_DIR"),
                $name
            ))
            .unwrap(),
        )
        .unwrap()
    };
}
fn object(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}
fn steps_of(plan: &Map<String, Value>) -> Vec<(String, Map<String, Value>)> {
    plan["steps"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), object(v.clone())))
        .collect()
}
fn keys(step: &FullStep) -> Vec<RefKey> {
    step.references
        .iter()
        .map(|r| {
            ref_key(&sluice_model::plan_rows::ReferenceRow {
                consumer_kind: sluice_model::plan_rows::ConsumerKind::Step,
                consumer_id: step.id.to_string(),
                slot: r.slot.clone(),
                ordinal: r.ordinal,
                kind: r.kind,
                source_kind: r.source_kind,
                source_id: r.source_id.clone(),
                source_port: r.source_port.clone(),
                source_path: r.source_path.clone(),
            })
        })
        .collect()
}

#[test]
fn the_derivation_gives_the_references_units_and_edges_the_fixtures_show() {
    let got: PlanGetResult = fixture!("plan_get.reply");
    let plan = got
        .plan
        .0
        .iter()
        .map(|(k, v)| (k.clone(), v.as_value().clone()));
    let plan: Map<String, Value> = plan.collect();
    let steps = steps_of(&plan);
    let ids: Vec<&str> = steps.iter().map(|(id, _)| id.as_str()).collect();
    let is_step = |id: &str| ids.contains(&id);

    let read: PlanReadResult = fixture!("plan_read.full.reply");
    let StepView::Full(notes) = &read.steps[0] else {
        panic!("a full step")
    };
    let declaration = &steps.iter().find(|(id, _)| id == "notes").unwrap().1;
    let derived: Vec<RefKey> = step_references("notes", declaration, &is_step)
        .iter()
        .map(ref_key)
        .collect();
    assert_eq!(
        derived,
        keys(notes),
        "notes' references, in (slot, ordinal) order"
    );

    let outputs = vec![("notes".to_owned(), object(plan["outputs"]["notes"].clone()))];
    let indexes = Indexes::derive(&steps, &outputs);
    assert_eq!(indexes.units["work"], "build");
    assert_eq!(indexes.units["gate"], "build");
    assert_eq!(indexes.units["notes"], "notes");
    assert!(indexes.tags.contains(&("gate".into(), "exit".into())));
    let edge = |s: &str, t: &str, k: &'static str, v: &str| Edge {
        source: s.into(),
        target: t.into(),
        kind: k,
        via_unit: v.into(),
    };
    assert_eq!(
        indexes.edges.iter().cloned().collect::<Vec<_>>(),
        vec![
            edge("gate", "notes", "data", ""),
            edge("gate", "notes", "gate", "build"),
            edge("work", "gate", "data", ""),
        ]
    );
    assert!(
        indexes
            .references
            .iter()
            .any(|r| r.0 == "output" && r.1 == "notes" && r.6 == "notes" && r.7 == "final"),
        "the plan output's one row: {:?}",
        indexes.references
    );

    let got: StepGetResult = fixture!("step_get.reply");
    let StepView::Full(work) = &got.step else {
        panic!("a full step")
    };
    let spec: Map<String, Value> = work
        .spec
        .0
        .iter()
        .map(|(k, v)| (k.clone(), v.as_value().clone()))
        .collect();
    let derived: Vec<RefKey> =
        step_references("normalize-work", &spec, &|id| id == "normalize-fork")
            .iter()
            .map(ref_key)
            .collect();
    assert_eq!(derived, keys(work), "a file binding makes no row");
}

#[test]
fn gates_classify_as_the_contract_says_and_unclassifiable_entries_make_no_row() {
    let declaration = object(json!({
        "after": ["a", "a?", "unit:u?", "flag", "!a/ok.deep", "x?", "!a", "!unit:u", "b/c"],
        "in": {"many": {"source": ["a/x", "brief.title"]}, "lit": {"default": 1}}
    }));
    let rows = step_references("t", &declaration, &|id| id == "a" || id == "b");
    let seen: Vec<(String, u32, String, String, String, String)> = rows
        .iter()
        .map(|r| {
            let key = ref_key(r);
            (key.2, key.3, key.5, key.6, key.7, key.8)
        })
        .collect();
    let s = |slot: &str, o: u32, k: &str, id: &str, port: &str, path: &str| {
        (
            slot.to_owned(),
            o,
            k.to_owned(),
            id.to_owned(),
            port.to_owned(),
            path.to_owned(),
        )
    };
    assert_eq!(
        seen,
        vec![
            s("after", 0, "step", "a", "", ""),
            s("after", 1, "step", "a", "", ""),
            s("after", 2, "unit", "u", "", ""),
            s("after", 3, "input", "flag", "", ""),
            s("after", 4, "step", "a", "ok", "deep"),
            s("after", 8, "step", "b", "c", ""),
            s("in.many", 0, "step", "a", "x", ""),
            s("in.many", 1, "input", "brief", "", "title"),
        ]
    );
}

#[test]
fn exits_are_the_tagged_steps_else_the_sinks_inside_the_unit() {
    let steps = vec![
        (
            "u-a".to_owned(),
            object(json!({"tags": ["unit:u"], "in": {"v": {"default": 1}}})),
        ),
        (
            "u-b".to_owned(),
            object(json!({"tags": ["unit:u"], "after": ["u-a"]})),
        ),
        (
            "u-c".to_owned(),
            object(json!({"tags": ["unit:u"], "in": {"v": {"source": "u-a/value"}}})),
        ),
        (
            "v-a".to_owned(),
            object(json!({"tags": ["unit:v", "exit"]})),
        ),
        ("v-b".to_owned(), object(json!({"tags": ["unit:v"]}))),
        (
            "w".to_owned(),
            object(json!({"after": ["unit:u", "unit:v?"]})),
        ),
    ];
    let exits = unit_exits(&steps);
    assert_eq!(exits["u"], ["u-b", "u-c"]);
    assert_eq!(exits["v"], ["v-a"]);
    assert_eq!(exits["w"], ["w"]);
    let into_w: Vec<(String, String)> = edges(&steps)
        .into_iter()
        .filter(|e| e.target == "w")
        .map(|e| (e.source, e.via_unit))
        .collect();
    assert_eq!(
        into_w,
        [
            ("u-b".to_owned(), "u".to_owned()),
            ("u-c".to_owned(), "u".to_owned()),
            ("v-a".to_owned(), "v".to_owned())
        ]
    );
}

#[test]
fn a_rebuilt_plan_exports_its_sections_and_rows_in_order_and_refuses_a_malformed_set() {
    let origin: Value = fixture!("plan_history.origin");
    let origin: Vec<PlanChange> = serde_json::from_value(origin["changes"].clone()).unwrap();
    let mut rows = Rebuilt::default();
    rows.apply(&origin).unwrap();
    assert_eq!(compact(&rows.export()), r#"{"steps":{}}"#);

    let changes: Vec<PlanChange> = serde_json::from_value(json!([
        {"op": "header.put", "root_order": ["steps", "inputs"]},
        {"op": "input.put", "name": "b", "position": 1, "declaration": "int"},
        {"op": "input.put", "name": "a", "position": 4, "declaration": {"type": "string"}},
        {"op": "step.put", "step": "z", "position": 0, "declaration": {"run": "x", "doc": "d"}},
        {"op": "step.put", "step": "y", "position": 2, "declaration": {"run": "x"}}
    ]))
    .unwrap();
    rows.apply(&changes).unwrap();
    assert_eq!(
        compact(&rows.export()),
        r#"{"steps":{"z":{"run":"x","doc":"d"},"y":{"run":"x"}},"inputs":{"b":"int","a":{"type":"string"}}}"#
    );
    rows.apply(
        &serde_json::from_value::<Vec<PlanChange>>(json!([
            {"op": "step.delete", "step": "z"},
            {"op": "step.put", "step": "x", "position": 3, "declaration": {"run": "x"}}
        ]))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        compact(&rows.export()),
        r#"{"steps":{"y":{"run":"x"},"x":{"run":"x"}},"inputs":{"b":"int","a":{"type":"string"}}}"#
    );
    assert_eq!(rows.root_order, [RootSection::Steps, RootSection::Inputs]);

    let set = |v: Value| serde_json::from_value::<Vec<PlanChange>>(v).unwrap();
    assert!(
        check_change_set(&set(json!([
            {"op": "step.put", "step": "a", "position": 0, "declaration": {}},
            {"op": "step.delete", "step": "b"}
        ])))
        .is_err(),
        "puts come after deletes"
    );
    assert!(
        check_change_set(&set(json!([
            {"op": "step.delete", "step": "a"},
            {"op": "step.put", "step": "a", "position": 0, "declaration": {}}
        ])))
        .is_err(),
        "one change per key"
    );
    assert!(
        check_change_set(&set(json!([
            {"op": "step.put", "step": "a", "position": 2, "declaration": {}},
            {"op": "step.put", "step": "b", "position": 1, "declaration": {}}
        ])))
        .is_err(),
        "puts in position order"
    );
    assert!(
        rows.apply(&set(json!([
            {"op": "step.put", "step": "w", "position": 2, "declaration": {}}
        ])))
        .is_err(),
        "two rows never share a position"
    );
    let all: Vec<PlanChange> = fixture!("plan_change.all");
    check_change_set(&all).unwrap();
}

fn database(home: &ScratchHome) -> Connection {
    Connection::open_with_flags(
        home.path().join("sluice.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .unwrap()
}

#[test]
fn two_fresh_homes_are_schema_equivalent_and_each_kind_of_drift_is_named() {
    let (a, b) = (ScratchHome::new().unwrap(), ScratchHome::new().unwrap());
    drop(Writer::open(a.path()).unwrap());
    drop(Writer::open(b.path()).unwrap());
    let (left, right) = (database(&a), database(&b));
    let fresh = schema_shape(&left).unwrap();
    assert!(fresh.keys().any(|k| k == "table steps columns"));
    assert_eq!(
        schema_differences(&fresh, &schema_shape(&right).unwrap()),
        Vec::<String>::new()
    );

    right
        .execute_batch(
            "CREATE INDEX extra ON steps(project_id, step_id);
             DROP VIEW step_changes;
             ALTER TABLE messages ADD COLUMN extra TEXT CHECK (extra IS NULL OR length(extra) > 0);",
        )
        .unwrap();
    let differences = schema_differences(&fresh, &schema_shape(&right).unwrap());
    let named: Vec<&str> = differences
        .iter()
        .map(|d| d.split(':').next().unwrap())
        .collect();
    assert_eq!(
        named,
        [
            "index extra",
            "table messages checks",
            "table messages columns",
            "table steps indexes",
            "view step_changes"
        ],
        "{differences:#?}"
    );
}

#[test]
fn the_logical_dump_changes_exactly_when_the_content_does() {
    let home = ScratchHome::new().unwrap();
    drop(Writer::open(home.path()).unwrap());
    let sql = database(&home);
    let before = logical_dump(&sql).unwrap();
    assert_eq!(before, logical_dump(&database(&home)).unwrap());
    assert!(before.contains("PRAGMA user_version="));
    sql.execute("UPDATE maintenance SET changed_at='x'", [])
        .unwrap();
    assert_ne!(before, logical_dump(&sql).unwrap());
    let copy = home.root().join("copy.db");
    let mut dump = String::new();
    sql.execute_batch("UPDATE maintenance SET changed_at=NULL")
        .unwrap();
    for statement in [
        "CREATE TABLE t (a TEXT, b INTEGER)",
        "INSERT INTO t VALUES ('x', 1)",
    ] {
        dump.push_str(statement);
        dump.push_str(";\n");
    }
    load_dump(&copy, &dump).unwrap();
    let loaded = Connection::open(&copy).unwrap();
    assert!(logical_dump(&loaded).unwrap().contains("t: \"x\", 1"));
}
