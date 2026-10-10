//! The schema-3 converter (`docs/design/plan-rows.md` §10.2–§10.6) on schema-1 and schema-2
//! fixture homes: well-formed histories convert with round-trip equality at every revision,
//! gapped and malformed ones are blocked by name and leave the file untouched, a converted
//! home is schema-equivalent to a fresh one, and a schema-changing restore of a backup with
//! live work is refused before anything is written.

#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{HomeId, ProjectId, RecordSeq, Revision},
    plan_index::{output_references, plan_edges, step_index},
    plan_rows::{HistoryEvent, InputRow, OutputRow, PlanChange, PlanHeader, PlanRows, StepRow},
    rpc::JsonMap,
};
use sluice_store::{
    ReadPool, RetrySafety, StoreError, Writer, backup,
    convert::{self, Anchor, AnchorSource, convert_home, schema_fingerprint},
    plans, projects,
};
use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};

const SCHEMA_1: &str = include_str!("fixtures/schema1.sql");

/// A home at schema 1 (or 2, the interim board schema) as a release before the cutover left it.
fn legacy_home(dir: &Path, schema: i64) -> Connection {
    let c = Connection::open(dir.join("sluice.db")).unwrap();
    c.execute_batch(SCHEMA_1).unwrap();
    c.execute(
        "INSERT INTO home_meta(singleton,home_id,format_major,schema_version) VALUES (1,?1,1,?2)",
        params![HomeId::new().to_string(), schema],
    )
    .unwrap();
    c.pragma_update(None, "application_id", 0x534c5543).unwrap();
    c.pragma_update(None, "user_version", schema).unwrap();
    c
}

fn record(c: &Connection, project: &str, kind: &str, payload: Value) -> (i64, String) {
    let at = format!("2026-10-0{}T00:00:00Z", 1 + c.last_insert_rowid() % 8);
    c.execute(
        "INSERT INTO records(project_id,at,kind,payload_version,payload,step_id) VALUES (?1,?2,?3,1,?4,?5)",
        params![
            project,
            at,
            kind,
            payload.to_string(),
            payload["step"].as_str()
        ],
    )
    .unwrap();
    (c.last_insert_rowid(), at)
}

/// One legacy project: its history (each revision's RFC 6902 ops), the stored plan and its
/// projection as schema 1 wrote them (dense positions, normalized input declarations).
/// `trimmed` revisions have no record left; `history` overrides the `plan_edits` rows.
struct Legacy<'a> {
    name: &'a str,
    revisions: Vec<Value>,
    stored: Value,
    trimmed: Vec<i64>,
}
fn legacy_project(c: &Connection, project: &Legacy<'_>) -> ProjectId {
    let id = ProjectId::new();
    let p = id.to_string();
    c.execute(
        "INSERT INTO projects(project_id,name,created_at,board,board_rev) VALUES (?1,?2,'2026-10-01T00:00:00Z','root = Text(\"kept\")',1)",
        params![p, project.name],
    )
    .unwrap();
    for (index, ops) in project.revisions.iter().enumerate() {
        let rev = index as i64 + 1;
        let reason = if rev == 1 {
            "project created".to_owned()
        } else {
            format!("r{rev}")
        };
        let payload =
            json!({"kind": "plan.edit", "rev": rev, "author": "sam", "reason": reason, "ops": ops});
        let (seq, at) = record(c, &p, "plan.edit", payload);
        c.execute(
            "INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,ops) VALUES (?1,?2,?3,?4,'sam',?5,?6)",
            params![p, rev, seq, at, reason, ops.to_string()],
        )
        .unwrap();
        if project.trimmed.contains(&rev) {
            c.execute("DELETE FROM records WHERE seq=?1", [seq])
                .unwrap();
        }
    }
    c.execute(
        "INSERT INTO plans(project_id,rev,doc) VALUES (?1,?2,?3)",
        params![
            p,
            project.revisions.len() as i64,
            project.stored.to_string()
        ],
    )
    .unwrap();
    project_rows(c, &p, &project.stored);
    id
}
/// Schema 1's projection of a plan: steps at 0…n-1 with unit and paused, inputs normalized.
fn project_rows(c: &Connection, p: &str, stored: &Value) {
    for (position, (step, declaration)) in stored["steps"].as_object().unwrap().iter().enumerate() {
        let unit = declaration["tags"]
            .as_array()
            .and_then(|tags| {
                tags.iter()
                    .find_map(|t| t.as_str().and_then(|t| t.strip_prefix("unit:")))
            })
            .unwrap_or(step);
        let paused = match &declaration["paused"] {
            Value::Bool(true) => "true".to_owned(),
            Value::String(reason) => serde_json::to_string(reason).unwrap(),
            _ => "false".to_owned(),
        };
        c.execute(
            "INSERT INTO steps(project_id,step_id,position,declaration,unit,paused) VALUES (?1,?2,?3,?4,?5,?6)",
            params![p, step, position as i64, declaration.to_string(), unit, paused],
        )
        .unwrap();
    }
    if let Some(inputs) = stored["inputs"].as_object() {
        for (position, (name, declaration)) in inputs.iter().enumerate() {
            let (ty, doc) = match declaration {
                Value::String(ty) => (json!(ty), Value::Null),
                other => (
                    other["type"].clone(),
                    other.get("doc").cloned().unwrap_or(Value::Null),
                ),
            };
            c.execute(
                "INSERT INTO inputs(project_id,name,position,declaration,value) VALUES (?1,?2,?3,?4,?5)",
                params![
                    p,
                    name,
                    position as i64,
                    json!({"type": ty, "doc": doc}).to_string(),
                    (name == "repo").then_some("\"/repo\"")
                ],
            )
            .unwrap();
        }
    }
}

fn a_v1() -> Value {
    json!({"run": "echo", "in": {"value": {"source": "repo"}}, "tags": ["unit:u"]})
}
fn b_v1() -> Value {
    json!({"run": "echo", "after": ["a"], "in": {"value": {"source": "a/value"}}, "tags": ["unit:u", "exit"]})
}
fn c_v1() -> Value {
    json!({"run": "echo", "paused": "waiting for review", "priority": 5, "needs": {"cpu": 1}})
}
/// Seven revisions: sections added in a new order, an append, a whole-plan replace that
/// inserts before survivors (renumbered), a removal (a gap), a move (a rename) and in-place
/// edits guarded by a test.
fn history() -> (Vec<Value>, Value) {
    let revisions = vec![
        json!([]),
        json!([
            {"op": "add", "path": "/inputs", "value": {"repo": "string"}},
            {"op": "add", "path": "/steps/a", "value": a_v1()}
        ]),
        json!([{"op": "add", "path": "/steps/b", "value": b_v1()}]),
        json!([{"op": "replace", "path": "", "value": {
            "inputs": {"repo": "string", "limit": {"type": "int", "doc": "How many"}},
            "outputs": {"result": {"source": "b/value"}},
            "steps": {"c": c_v1(), "a": a_v1(), "b": b_v1()}
        }}]),
        json!([{"op": "remove", "path": "/steps/c"}]),
        json!([{"op": "move", "from": "/outputs/result", "path": "/outputs/final"}]),
        json!([
            {"op": "test", "path": "/steps/a/run", "value": "echo"},
            {"op": "replace", "path": "/steps/a/tags", "value": ["unit:u", "exit"]},
            {"op": "remove", "path": "/steps/b/tags/1"}
        ]),
    ];
    let stored = json!({
        "inputs": {"repo": "string", "limit": {"type": "int", "doc": "How many"}},
        "outputs": {"final": {"source": "b/value"}},
        "steps": {
            "a": {"run": "echo", "in": {"value": {"source": "repo"}}, "tags": ["unit:u", "exit"]},
            "b": {"run": "echo", "after": ["a"], "in": {"value": {"source": "a/value"}}, "tags": ["unit:u"]}
        }
    });
    (revisions, stored)
}

/// Runtime state the conversion keeps: a's result, a finished run and terminal attempts with
/// completion snapshots (one whose declaration differs from its snapshot's), and records.
fn runtime(c: &Connection, project: ProjectId) -> (String, String) {
    let p = project.to_string();
    let result = sluice_model::ids::ResultId::new().to_string();
    c.execute(
        "INSERT INTO step_results(result_id,project_id,step_id,generation,declaration,status,outputs,recorded_at) VALUES (?1,?2,'a',1,?3,'succeeded','{\"value\":\"x\"}','2026-10-01T00:00:00Z')",
        params![result, p, a_v1().to_string()],
    )
    .unwrap();
    c.execute(
        "UPDATE steps SET status='succeeded',outputs='{\"value\":\"x\"}',result_id=?2 WHERE project_id=?1 AND step_id='a'",
        params![p, result],
    )
    .unwrap();
    let snapshot = |declaration: Value| {
        json!({"runtime": {"capability": "cap", "execution": "exec", "completion": {
            "revision": 3, "document": {"steps": {"a": declaration}}, "signatures": {}
        }}, "files": {}})
    };
    let mut attempts = vec![];
    for (n, snapshot) in [snapshot(a_v1()), snapshot(json!({"run": "other"}))]
        .into_iter()
        .enumerate()
    {
        let attempt = sluice_model::ids::AttemptId::new().to_string();
        let request = json!({"declaration": a_v1(), "inputs": {}, "provenance": snapshot});
        c.execute(
            "INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,provenance,created_at,finished_at) VALUES (?1,?2,'a','terminal',?3,'hash',?4,'now','now')",
            params![attempt, p, request.to_string(), snapshot.to_string()],
        )
        .unwrap();
        c.execute(
            "INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at,finished_at) VALUES (?1,?2,?3,'a','now','now')",
            params![sluice_model::ids::RunId::new().to_string(), p, attempt],
        )
        .unwrap();
        if n == 0 {
            attempts.push(attempt);
        }
    }
    record(
        c,
        &p,
        "step.status",
        json!({"kind": "step.status", "step": "a", "from": "running", "to": "succeeded", "error": null, "run_ids": [], "needs": {}}),
    );
    record(
        c,
        &p,
        "plan.input",
        json!({"kind": "plan.input", "rev": 7, "author": "sam", "reason": "", "name": "repo", "value": "/repo"}),
    );
    (result, attempts.remove(0))
}

fn compact(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap()
}
fn map(value: Value) -> JsonMap {
    serde_json::from_value(value).unwrap()
}
fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}
fn fresh_fingerprint() -> Vec<String> {
    let home = ScratchHome::new().unwrap();
    drop(Writer::open(home.path()).unwrap());
    schema_fingerprint(&Connection::open(home.path().join("sluice.db")).unwrap()).unwrap()
}

/// Rebuild a plan from its history alone (§5.3): from no rows, each revision's changes as a
/// set, deletes then puts.
fn rebuild(history: &[Vec<PlanChange>]) -> PlanRows {
    let mut rows = PlanRows {
        header: PlanHeader {
            rev: Revision(0),
            root_order: vec![],
            state_epoch: Default::default(),
        },
        inputs: vec![],
        outputs: vec![],
        steps: vec![],
    };
    for changes in history {
        rows.header.rev = Revision(rows.header.rev.0 + 1);
        for change in changes {
            match change.clone() {
                PlanChange::HeaderPut { root_order } => rows.header.root_order = root_order,
                PlanChange::InputDelete { name } => rows.inputs.retain(|r| r.name != name),
                PlanChange::OutputDelete { name } => rows.outputs.retain(|r| r.name != name),
                PlanChange::StepDelete { step } => rows.steps.retain(|r| r.step != step),
                PlanChange::InputPut {
                    name,
                    position,
                    declaration,
                } => {
                    rows.inputs.retain(|r| r.name != name);
                    rows.inputs.push(InputRow {
                        name,
                        position,
                        declaration,
                    });
                }
                PlanChange::OutputPut {
                    name,
                    position,
                    binding,
                } => {
                    rows.outputs.retain(|r| r.name != name);
                    rows.outputs.push(OutputRow {
                        name,
                        position,
                        binding,
                    });
                }
                PlanChange::StepPut {
                    step,
                    position,
                    declaration,
                } => {
                    rows.steps.retain(|r| r.step != step);
                    rows.steps.push(StepRow {
                        step,
                        position,
                        declaration,
                    });
                }
            }
        }
        rows.inputs.sort_by_key(|r| r.position);
        rows.outputs.sort_by_key(|r| r.position);
        rows.steps.sort_by_key(|r| r.position);
    }
    rows
}

/// The index rows stored for a project equal those derived from its declarations.
fn assert_indexes(c: &Connection, project: ProjectId) {
    let rows = plans::read_plan_rows(c, project).unwrap();
    let p = project.to_string();
    let ids: HashSet<&str> = rows.steps.iter().map(|r| r.step.as_str()).collect();
    let mut tags = BTreeSet::new();
    let mut references = BTreeSet::new();
    for row in &rows.steps {
        let index = step_index(&row.step, &row.declaration, &|n| ids.contains(n));
        let unit: String = c
            .query_row(
                "SELECT unit FROM steps WHERE project_id=?1 AND step_id=?2",
                params![p, row.step.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unit, index.unit.as_str());
        for tag in index.tags {
            tags.insert((row.step.to_string(), tag));
        }
        for r in index.references {
            references.insert(format!("{r:?}"));
        }
    }
    for output in &rows.outputs {
        for r in output_references(&output.name, &output.binding) {
            references.insert(format!("{r:?}"));
        }
    }
    let stored_tags: BTreeSet<(String, String)> = c
        .prepare("SELECT step_id,tag FROM step_tags WHERE project_id=?1")
        .unwrap()
        .query_map([&p], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(stored_tags, tags);
    let stored_refs: BTreeSet<String> = plans::read_references(
        c,
        project,
        &sluice_model::plan_rows::ReferenceSelection::Consumers(
            rows.steps.iter().map(|r| r.step.clone()).collect(),
        ),
    )
    .unwrap()
    .0
    .into_iter()
    .chain(
        plans::read_references(
            c,
            project,
            &sluice_model::plan_rows::ReferenceSelection::Outputs(
                rows.outputs.iter().map(|r| r.name.clone()).collect(),
            ),
        )
        .unwrap()
        .0,
    )
    .map(|r| format!("{r:?}"))
    .collect();
    assert_eq!(stored_refs, references);
    let edges: BTreeSet<String> = plan_edges(&rows).iter().map(|e| format!("{e:?}")).collect();
    let graph = plans::read_graph(c, project, &Default::default()).unwrap();
    let stored_edges: BTreeSet<String> = graph.edges.iter().map(|e| format!("{e:?}")).collect();
    assert_eq!(stored_edges, edges);
}

#[tokio::test]
async fn a_schema_1_home_converts_with_every_revision_round_tripped() {
    let home = ScratchHome::new().unwrap();
    let database = home.path().join("sluice.db");
    let (revisions, stored) = history();
    let (project, other, result, attempt) = {
        let c = legacy_home(home.path(), 1);
        let project = legacy_project(
            &c,
            &Legacy {
                name: "lash",
                revisions: revisions.clone(),
                stored: stored.clone(),
                trimmed: vec![3],
            },
        );
        let other = legacy_project(
            &c,
            &Legacy {
                name: "empty",
                revisions: vec![json!([])],
                stored: json!({"steps": {}}),
                trimmed: vec![],
            },
        );
        // A record whose author the history row does not share: a warning, not a blocker.
        c.execute(
            "UPDATE records SET payload=json_set(payload,'$.author','someone') WHERE kind='plan.edit' AND json_extract(payload,'$.rev')=7",
            [],
        )
        .unwrap();
        let (result, attempt) = runtime(&c, project);
        (project, other, result, attempt)
    };
    // Readers and writers of schema 3 refuse it and never convert it on open.
    let before = bytes(&database);
    for refusal in [
        Writer::open(home.path()).map(|_| ()).unwrap_err(),
        ReadPool::open(home.path(), 1).map(|_| ()).unwrap_err(),
    ] {
        assert!(matches!(
            refusal,
            StoreError::MigrationRequired { found: 1 }
        ));
        assert_eq!(
            refusal.to_string(),
            "this home is at schema 1; run \"sluice home migrate\" to bring it to schema 3"
        );
    }
    assert_eq!(bytes(&database), before);

    let report = convert_home(&database).unwrap();
    assert_eq!(report.from_schema, 1);
    let lash = report
        .projects
        .iter()
        .find(|p| p.project_id == project)
        .unwrap();
    assert_eq!(
        (lash.revisions, lash.steps, lash.inputs, lash.outputs),
        (7, 2, 2, 1)
    );
    assert_eq!(lash.attempt_snapshots_removed, 2);
    assert!(lash.records_rewritten >= 8, "{report:?}");
    assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("at rev 7") && w.contains("author or reason"))
    );
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("completion snapshot"))
    );
    assert_eq!(report.to_json()["projects"].as_array().unwrap().len(), 2);

    let c = Connection::open(&database).unwrap();
    // The home is schema 3 and schema-equivalent to a fresh one.
    let versions: (i64, i64) = (
        c.query_row("SELECT schema_version FROM home_meta", [], |r| r.get(0))
            .unwrap(),
        c.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap(),
    );
    assert_eq!(versions, (3, 3));
    assert_eq!(schema_fingerprint(&c).unwrap(), fresh_fingerprint());
    // The plan is its rows: the export is the stored plan, byte for byte.
    let exported = plans::export_plan(&c, project).unwrap();
    assert_eq!(exported.rev, Revision(7));
    assert_eq!(compact(&exported.document), compact(&stored));
    assert_eq!(
        compact(&plans::export_plan(&c, other).unwrap().document),
        r#"{"steps":{}}"#
    );
    // Every revision's changes, positioned by §10.6.
    let changes: Vec<Vec<PlanChange>> = c
        .prepare("SELECT changes FROM plan_edits WHERE project_id=?1 ORDER BY rev")
        .unwrap()
        .query_map([project.to_string()], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|t| serde_json::from_str(&t.unwrap()).unwrap())
        .collect();
    let ops = |changes: &[PlanChange]| -> Vec<String> {
        changes
            .iter()
            .map(|c| {
                let v = serde_json::to_value(c).unwrap();
                format!(
                    "{} {}{}",
                    v["op"].as_str().unwrap(),
                    v.get("step")
                        .or(v.get("name"))
                        .or(v.get("root_order"))
                        .unwrap(),
                    v.get("position")
                        .map_or(String::new(), |p| format!(" @{p}"))
                )
            })
            .collect()
    };
    assert_eq!(
        changes.iter().map(|c| ops(c)).collect::<Vec<_>>(),
        vec![
            vec![r#"header.put ["steps"]"#.to_owned()],
            vec![
                r#"header.put ["steps","inputs"]"#.into(),
                r#"input.put "repo" @0"#.into(),
                r#"step.put "a" @0"#.into()
            ],
            vec![r#"step.put "b" @1"#.into()],
            vec![
                r#"header.put ["inputs","outputs","steps"]"#.into(),
                r#"input.put "limit" @1"#.into(),
                r#"output.put "result" @0"#.into(),
                r#"step.put "c" @0"#.into(),
                r#"step.put "a" @1"#.into(),
                r#"step.put "b" @2"#.into()
            ],
            vec![r#"step.delete "c""#.into()],
            vec![
                r#"output.delete "result""#.into(),
                r#"output.put "final" @1"#.into()
            ],
            vec![r#"step.put "a" @1"#.into(), r#"step.put "b" @2"#.into()],
        ]
    );
    // Declaration history alone rebuilds the plan's rows.
    let rows = plans::read_plan_rows(&c, project).unwrap();
    let rebuilt = rebuild(&changes);
    assert_eq!(rebuilt.header.root_order, rows.header.root_order);
    assert_eq!(
        (rebuilt.inputs, rebuilt.outputs, rebuilt.steps),
        (rows.inputs, rows.outputs, rows.steps.clone())
    );
    // Runtime state is kept; inputs hold their authored declaration and value.
    let state = plans::read_state(&c, project).unwrap();
    assert_eq!(
        state.status(&"a".parse().unwrap()),
        sluice_model::commands::StepStatus::Succeeded
    );
    assert_eq!(compact(&state.inputs), r#"{"repo":"/repo"}"#);
    let row: (i64, String, String, String, i64, Option<String>, String) = c
        .query_row(
            "SELECT position,unit,paused,run,priority,needs,result_id FROM steps WHERE project_id=?1 AND step_id='a'",
            [project.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            1,
            "u".into(),
            "false".into(),
            "echo".into(),
            0,
            None,
            result
        )
    );
    let declarations: Vec<String> = c
        .prepare("SELECT declaration FROM inputs WHERE project_id=?1 ORDER BY position")
        .unwrap()
        .query_map([project.to_string()], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        declarations,
        [r#""string""#, r#"{"type":"int","doc":"How many"}"#]
    );
    assert_indexes(&c, project);
    // Records are payload version 2; a plan.edit carries its revision's changes.
    let versions: Vec<i64> = c
        .prepare("SELECT DISTINCT payload_version FROM records")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(versions, [2]);
    let edit: Value = serde_json::from_str(
        &c.query_row(
            "SELECT payload FROM records WHERE kind='plan.edit' AND project_id=?1 AND json_extract(payload,'$.rev')=4",
            [project.to_string()],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(edit.get("ops").is_none());
    assert_eq!(edit["changes"], serde_json::to_value(&changes[3]).unwrap());
    // Attempts lose their completion snapshots and keep everything else.
    let (request, provenance): (String, String) = c
        .query_row(
            "SELECT request,provenance FROM attempts WHERE attempt_id=?1",
            [&attempt],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let (request, provenance): (Value, Value) = (
        serde_json::from_str(&request).unwrap(),
        serde_json::from_str(&provenance).unwrap(),
    );
    assert_eq!(
        request["provenance"]["runtime"],
        json!({"capability": "cap", "execution": "exec"})
    );
    assert_eq!(
        provenance["runtime"],
        json!({"capability": "cap", "execution": "exec"})
    );
    assert_eq!(request["declaration"], a_v1());
    drop(c);

    // The converted home opens, its history pages back to the origin, and it edits.
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let (page, next) = reads
        .snapshot(move |c| plans::history(c, project, None, None, 3))
        .await
        .unwrap();
    assert_eq!(next, Some(page[2].seq));
    match &page[0].event {
        HistoryEvent::PlanEdit(edit) => {
            assert_eq!(edit.rev, Revision(1));
            assert_eq!(edit.reason, "project created");
            assert_eq!(
                compact(&edit.changes),
                r#"[{"op":"header.put","root_order":["steps"]}]"#
            );
        }
        other => panic!("{other:?}"),
    }
    let writer = Writer::open(home.path()).unwrap();
    let rev = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let mut document = stored.clone();
            document["steps"]["d"] = json!({"run": "echo", "after": ["unit:u"]});
            sluice_store_support::commit_document(tx, project, &map(document), None, None)
        })
        .await
        .unwrap();
    assert_eq!(rev, Revision(8));
    let (_, next) = reads
        .snapshot(move |c| plans::history(c, project, Some(Revision(7)), None, 10))
        .await
        .unwrap();
    assert_eq!(next, None);
    let _ = RecordSeq(0);
}

#[allow(dead_code)]
#[path = "support/plan_rows.rs"]
mod sluice_store_support;

/// A schema-2 home (the interim board schema) that also predates later schema-1 columns
/// converts to the same shape, even where a column it gains would land after a later one (as
/// the live home's `projects` has `prune_*` before `board_doc*`): such a table is rebuilt in
/// schema 3's column order. Its boards are kept and still work.
#[tokio::test]
async fn a_schema_2_home_without_later_columns_converts_and_keeps_its_boards() {
    let home = ScratchHome::new().unwrap();
    let database = home.path().join("sluice.db");
    let project = {
        let c = legacy_home(home.path(), 2);
        let project = legacy_project(
            &c,
            &Legacy {
                name: "boards",
                revisions: vec![
                    json!([]),
                    json!([{"op": "add", "path": "/steps/a", "value": {"run": "echo", "paused": true}}]),
                ],
                stored: json!({"steps": {"a": {"run": "echo", "paused": true}}}),
                trimmed: vec![1, 2],
            },
        );
        c.execute_batch(
            "ALTER TABLE runs DROP COLUMN stopped; ALTER TABLE messages DROP COLUMN read_at;
             ALTER TABLE steps DROP COLUMN progress_run; ALTER TABLE steps DROP COLUMN progress_at;
             ALTER TABLE steps DROP COLUMN progress;
             ALTER TABLE projects DROP COLUMN board_doc_author; ALTER TABLE projects DROP COLUMN board_doc_at;
             ALTER TABLE projects DROP COLUMN board_doc_rev; ALTER TABLE projects DROP COLUMN board_doc;
             DROP VIEW board_slots; ALTER TABLE projects DROP COLUMN board_slots;",
        )
        .unwrap();
        project
    };
    let report = convert_home(&database).unwrap();
    assert_eq!(report.from_schema, 2);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let c = Connection::open(&database).unwrap();
    assert_eq!(schema_fingerprint(&c).unwrap(), fresh_fingerprint());
    let paused: String = c
        .query_row("SELECT paused FROM steps WHERE step_id='a'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(paused, "true");
    drop(c);
    let writer = Writer::open(home.path()).unwrap();
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let p = reads
        .snapshot(move |c| projects::resolve(c, &sluice_model::ids::ProjectSelector::Id(project)))
        .await
        .unwrap();
    assert_eq!(
        (p.board.as_deref(), p.board_rev, p.prune_done_after),
        (Some("root = Text(\"kept\")"), Revision(1), None)
    );
    let rev = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::board_set(
                tx,
                &sluice_model::ids::ProjectSelector::Id(project),
                projects::SetBoard {
                    program: Some("root = Text(\"new\")".into()),
                    expected_rev: Some(Revision(1)),
                    reason: None,
                    author: "orch".into(),
                },
            )
        })
        .await
        .unwrap()
        .rev;
    assert_eq!(rev, Revision(2));
}

/// Each blocked home is refused with blockers naming the project, the check and the revision,
/// and its file is left exactly as it was.
#[test]
fn gapped_malformed_and_unknown_histories_are_blocked_and_leave_the_home_untouched() {
    let (revisions, stored) = history();
    type Damage = Box<dyn Fn(&Connection)>;
    type Case = (&'static str, Vec<Value>, Value, Damage, Vec<&'static str>);
    let cases: Vec<Case> = vec![
        (
            "gapped",
            revisions.clone(),
            stored.clone(),
            Box::new(|c| {
                c.execute("DELETE FROM plan_edits WHERE rev=5", []).unwrap();
            }),
            vec![
                "project gapped (",
                "history is incomplete at rev 5: unknown origin",
            ],
        ),
        (
            "origin",
            vec![json!([{"op": "add", "path": "/steps/a", "value": {"run": "echo"}}])],
            json!({"steps": {"a": {"run": "echo"}}}),
            Box::new(|_| {}),
            vec!["project origin (", "rev 1: unknown origin"],
        ),
        (
            "malformed",
            vec![
                json!([]),
                json!([{"op": "remove", "path": "/steps/missing"}]),
            ],
            json!({"steps": {}}),
            Box::new(|_| {}),
            vec![
                "project malformed (",
                "rev 2: ops[0]: the patch does not apply",
            ],
        ),
        (
            "drifted",
            revisions.clone(),
            json!({"steps": {}}),
            Box::new(|_| {}),
            vec![
                "project drifted (",
                "rev 7: the replayed plan differs from the stored plan",
            ],
        ),
        (
            "unrowed",
            vec![
                json!([]),
                json!([{"op": "add", "path": "/when", "value": {}}]),
            ],
            json!({"steps": {}, "when": {}}),
            Box::new(|_| {}),
            vec![
                "project unrowed (",
                "rev 2: the replayed plan has no rows: plan has an unknown root field when",
            ],
        ),
        (
            "projection",
            revisions.clone(),
            stored.clone(),
            Box::new(|c| {
                c.execute(
                    "UPDATE steps SET declaration='{\"run\":\"x\"}' WHERE step_id='b'",
                    [],
                )
                .unwrap();
            }),
            vec![
                "project projection (",
                "rev 7: step row b's declaration differs from the plan's",
            ],
        ),
        (
            "unlogged",
            revisions.clone(),
            stored.clone(),
            Box::new(|c| {
                c.execute(
                    "UPDATE records SET payload=json_set(payload,'$.rev',9) WHERE kind='plan.edit' AND json_extract(payload,'$.rev')=6",
                    [],
                )
                .unwrap();
            }),
            vec![
                "project unlogged (",
                "at rev 9: no plan_edits row for its revision",
            ],
        ),
        (
            "imported",
            revisions.clone(),
            stored.clone(),
            Box::new(|c| {
                c.execute("DELETE FROM plan_edits WHERE rev=1", []).unwrap();
            }),
            vec![
                "project imported (",
                "history starts at rev 2: unknown origin (no completion snapshot replays through revs 2 to 7 to the stored plan, and more than one revision is logged)",
            ],
        ),
        (
            "live",
            revisions.clone(),
            stored.clone(),
            Box::new(|c| {
                let attempt = sluice_model::ids::AttemptId::new().to_string();
                c.execute(
                    "INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) SELECT ?1,project_id,'b','executing','{}','h','now' FROM projects",
                    [&attempt],
                )
                .unwrap();
            }),
            vec!["home: live work (1 attempts, 0 runs, 0 leases, 0 calls)"],
        ),
    ];
    for (name, revisions, stored, damage, expected) in cases {
        let home = ScratchHome::new().unwrap();
        let database = home.path().join("sluice.db");
        {
            let c = legacy_home(home.path(), 1);
            legacy_project(
                &c,
                &Legacy {
                    name,
                    revisions,
                    stored,
                    trimmed: vec![],
                },
            );
            damage(&c);
        }
        let before = bytes(&database);
        match convert_home(&database) {
            Err(StoreError::ConversionBlocked(blockers)) => {
                let text = blockers.join("\n");
                for part in expected {
                    assert!(text.contains(part), "{name}: {text}");
                }
                if name != "live" {
                    assert!(text.contains("rev "), "{name}: names the revision: {text}");
                }
            }
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(bytes(&database), before, "{name}: the home is untouched");
        let public = convert_home(&database).unwrap_err().into_public(false);
        assert!(matches!(public, PublicError::Invalid { .. }), "{name}");
    }
}

/// A project the Python importer brought in: its logged history is `revisions` from rev
/// `first` (each with a record), and `snapshots` are terminal attempts' completion snapshots
/// `(revision, document)`, created in that order, in both `request` and `provenance`.
fn imported_project(
    c: &Connection,
    name: &str,
    first: i64,
    revisions: &[Value],
    stored: &Value,
    snapshots: &[(i64, Value)],
) -> ProjectId {
    let id = ProjectId::new();
    let p = id.to_string();
    c.execute(
        "INSERT INTO projects(project_id,name,created_at) VALUES (?1,?2,'2026-09-28T10:48:41Z')",
        params![p, name],
    )
    .unwrap();
    // Below the first logged revision's record: another project's (free for this one) and
    // this project's own (taken), as an import leaves them.
    c.execute(
        "INSERT INTO records(project_id,at,kind,payload) VALUES (NULL,'2026-09-28T00:00:00Z','home.note','{\"kind\":\"home.note\"}')",
        [],
    )
    .unwrap();
    record(c, &p, "project.update", json!({"kind": "project.update"}));
    for (index, ops) in revisions.iter().enumerate() {
        let rev = first + index as i64;
        let reason = format!("r{rev}");
        let payload =
            json!({"kind": "plan.edit", "rev": rev, "author": "sam", "reason": reason, "ops": ops});
        let (seq, at) = record(c, &p, "plan.edit", payload);
        c.execute(
            "INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,ops) VALUES (?1,?2,?3,?4,'sam',?5,?6)",
            params![p, rev, seq, at, reason, ops.to_string()],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO plans(project_id,rev,doc) VALUES (?1,?2,?3)",
        params![p, first + revisions.len() as i64 - 1, stored.to_string()],
    )
    .unwrap();
    project_rows(c, &p, stored);
    for (n, (revision, document)) in snapshots.iter().enumerate() {
        let snapshot = json!({"runtime": {"completion": {
            "revision": revision, "document": document, "signatures": {}
        }}});
        c.execute(
            "INSERT INTO attempts(attempt_id,project_id,phase,request,inputs_hash,provenance,created_at,finished_at) VALUES (?1,?2,'terminal',?3,'hash',?4,?5,'now')",
            params![
                sluice_model::ids::AttemptId::new().to_string(),
                p,
                json!({"provenance": snapshot}).to_string(),
                snapshot.to_string(),
                format!("2026-10-01T00:00:{n:02}Z")
            ],
        )
        .unwrap();
    }
    id
}

/// An imported plan's documents: rev 1 as the importer wrote it (no history row), then three
/// logged revisions (a step added, a section appended, a tag replaced).
fn imported() -> (Vec<Value>, Vec<Value>) {
    let d1 = json!({"inputs": {"repo": "string"}, "steps": {"a": a_v1()}});
    let d2 = json!({"inputs": {"repo": "string"}, "steps": {"a": a_v1(), "b": b_v1()}});
    let d3 = json!({"inputs": {"repo": "string"}, "steps": {"a": a_v1(), "b": b_v1()},
        "outputs": {"final": {"source": "b/value"}}});
    let mut d4 = d3.clone();
    d4["steps"]["a"]["tags"] = json!(["unit:u", "exit"]);
    let ops = vec![
        json!([{"op": "add", "path": "/steps/b", "value": b_v1()}]),
        json!([{"op": "add", "path": "/outputs", "value": {"final": {"source": "b/value"}}}]),
        json!([{"op": "replace", "path": "/steps/a/tags", "value": ["unit:u", "exit"]}]),
    ];
    (vec![d1, d2, d3, d4], ops)
}

/// §10.4.1: a project whose logged history starts above rev 1 is anchored at the earliest
/// revision whose completion snapshot replays through the later logged revisions to the stored
/// plan (a snapshot that does not, earlier or of the same revision, is passed over), or with no
/// snapshot and one logged revision at the stored plan. The baseline is that revision's row
/// (author `sluice`), the revisions after it convert as any other, each revision at or below it
/// is folded with its record kept, and the report lists each anchor.
#[tokio::test]
async fn imported_projects_are_anchored_at_an_exact_baseline() {
    let (docs, ops) = imported();
    let stored = docs[3].clone();
    let home = ScratchHome::new().unwrap();
    let database = home.path().join("sluice.db");
    let (whole, partial, single) = {
        let c = legacy_home(home.path(), 1);
        // The importer's rev 1 survives in a snapshot, after one that does not replay to the
        // stored plan: the whole history is recovered.
        let mut stale = docs[0].clone();
        stale["steps"]["a"]["run"] = json!("other");
        let whole = imported_project(
            &c,
            "whole",
            2,
            &ops,
            &stored,
            &[
                (1, stale.clone()),
                (1, docs[0].clone()),
                (3, docs[2].clone()),
            ],
        );
        // The earliest snapshot is rev 2's (rev 1's is stale): rev 2's own edit is folded.
        let partial = imported_project(
            &c,
            "partial",
            2,
            &ops,
            &stored,
            &[(1, stale), (2, docs[1].clone()), (3, docs[2].clone())],
        );
        // One logged revision and no snapshot: the stored plan is the baseline.
        let single = imported_project(&c, "single", 4, &ops[2..], &stored, &[]);
        (whole, partial, single)
    };
    let report = convert_home(&database).unwrap();
    let anchor = |name: &str| {
        report
            .projects
            .iter()
            .find(|p| p.name == name)
            .unwrap()
            .clone()
    };
    let expect = [
        (
            whole,
            "whole",
            1,
            0,
            AnchorSource::Snapshot,
            vec![1, 2, 3, 4],
        ),
        (
            partial,
            "partial",
            2,
            1,
            AnchorSource::Snapshot,
            vec![2, 3, 4],
        ),
        (single, "single", 4, 1, AnchorSource::Current, vec![4]),
    ];
    let json = report.to_json();
    let c = Connection::open(&database).unwrap();
    for (project, name, k, folded, source, revs) in expect {
        let converted = anchor(name);
        assert_eq!(
            converted.anchor,
            Some(Anchor {
                rev: k,
                folded_edits: folded,
                source
            }),
            "{name}"
        );
        assert_eq!(converted.revisions, revs.len() as u64, "{name}");
        assert!(
            json["anchored"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["name"] == name
                    && a["rev"] == k
                    && a["folded_edits"] == folded
                    && a["source"] == source.as_str()),
            "{name}: {json}"
        );
        let p = project.to_string();
        let (header_rev, last_edit_rev): (i64, i64) = c
            .query_row(
                "SELECT rev, (SELECT max(rev) FROM plan_edits WHERE project_id=?1) FROM plans WHERE project_id=?1",
                [&p],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(header_rev, 4, "{name}: keeps the old plans.rev");
        assert_eq!(
            header_rev, last_edit_rev,
            "{name}: next edit follows history"
        );
        // The stored plan exports exactly, and the history from the baseline rebuilds it.
        let exported = plans::export_plan(&c, project).unwrap();
        assert_eq!(compact(&exported.document), compact(&stored), "{name}");
        type Row = (i64, i64, String, String, String);
        let rows: Vec<Row> = c
            .prepare("SELECT rev,seq,author,reason,changes FROM plan_edits WHERE project_id=?1 ORDER BY rev")
            .unwrap()
            .query_map([&p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(rows.iter().map(|r| r.0).collect::<Vec<_>>(), revs, "{name}");
        let (_, seq, author, reason, changes) = &rows[0];
        assert_eq!(author, "sluice", "{name}");
        assert_eq!(
            reason,
            &format!("imported baseline: history before rev {k} was not logged"),
            "{name}"
        );
        let baseline: Vec<PlanChange> = serde_json::from_str(changes).unwrap();
        let rows_at_k = PlanRows::from_document(&map(docs[k as usize - 1].clone()), None).unwrap();
        assert_eq!(
            baseline,
            rows_at_k.changes_from(None),
            "{name}: rev {k}'s document"
        );
        let logged_seq: i64 = c
            .query_row(
                "SELECT min(seq) FROM records WHERE project_id=?1 AND kind='plan.edit'",
                [&p],
                |r| r.get(0),
            )
            .unwrap();
        if k == 1 {
            // Below the first logged revision, on a sequence no record of the project holds.
            assert!(*seq < logged_seq, "{name}");
            let held: bool = c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM records WHERE seq=?1 AND project_id=?2)",
                    params![seq, p],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!held, "{name}");
        } else {
            assert_eq!(*seq, logged_seq, "{name}: rev {k}'s own row");
        }
        let history: Vec<Vec<PlanChange>> = rows
            .iter()
            .map(|r| serde_json::from_str(&r.4).unwrap())
            .collect();
        let rebuilt = rebuild(&history);
        let current = plans::read_plan_rows(&c, project).unwrap();
        assert_eq!(
            (
                rebuilt.header.root_order,
                rebuilt.inputs,
                rebuilt.outputs,
                rebuilt.steps
            ),
            (
                current.header.root_order,
                current.inputs,
                current.outputs,
                current.steps
            ),
            "{name}: the history from the baseline rebuilds the rows"
        );
        assert_indexes(&c, project);
        // Each folded revision's record is kept, rev k's carrying the baseline's changes.
        let folded_records: Vec<(i64, String)> = c
            .prepare("SELECT seq,payload FROM records WHERE project_id=?1 AND kind='plan.edit' AND json_extract(payload,'$.rev')<=?2")
            .unwrap()
            .query_map(params![p, k as i64], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(folded_records.len() as u64, folded, "{name}");
        for (record, payload) in folded_records {
            let payload: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(
                payload["reason"],
                format!("r{k}"),
                "{name}: keeps its reason"
            );
            assert_eq!(payload["changes"], serde_json::to_value(&baseline).unwrap());
            assert!(
                report.warnings.iter().any(|w| w.contains(&format!(
                    "plan.edit record {record} at rev {k} is folded into the imported baseline at rev {k}"
                ))),
                "{name}: {:?}",
                report.warnings
            );
        }
        // The snapshots were read before they were removed.
        let left: i64 = c
            .query_row(
                "SELECT count(*) FROM attempts WHERE project_id=?1 AND (json_type(provenance,'$.runtime.completion') IS NOT NULL OR json_type(request,'$.provenance.runtime.completion') IS NOT NULL)",
                [&p],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0, "{name}");
    }
    assert_eq!(schema_fingerprint(&c).unwrap(), fresh_fingerprint());
    drop(c);
    // The converted home opens, and paged history starts at the baseline.
    let reads = ReadPool::open(home.path(), 1).unwrap();
    let (page, _) = reads
        .snapshot(move |c| plans::history(c, partial, None, None, 10))
        .await
        .unwrap();
    let edits: Vec<(u64, String)> = page
        .iter()
        .filter_map(|r| match &r.event {
            HistoryEvent::PlanEdit(edit) => Some((edit.rev.0, edit.author.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        edits,
        vec![(2, "sluice".into()), (3, "sam".into()), (4, "sam".into())]
    );
}

/// A restore of a schema-1 backup converts it on its private destination; one holding live
/// work is refused before anything is written, with §10.2's message.
#[test]
fn a_schema_changing_restore_converts_and_refuses_live_work() {
    let (revisions, stored) = history();
    let scratch = ScratchHome::new().unwrap();
    let source = scratch.root().join("legacy");
    std::fs::create_dir(&source).unwrap();
    let c = legacy_home(&source, 1);
    let project = legacy_project(
        &c,
        &Legacy {
            name: "lash",
            revisions,
            stored: stored.clone(),
            trimmed: vec![],
        },
    );
    let p = project.to_string();
    let attempt = sluice_model::ids::AttemptId::new().to_string();
    let run = sluice_model::ids::RunId::new().to_string();
    c.execute(
        "INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'b','executing','{}','h','now')",
        params![attempt, p],
    )
    .unwrap();
    c.execute(
        "INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at,unit_name,cgroup) VALUES (?1,?2,?3,'b','now','sluice-run-x','/x')",
        params![run, p, attempt],
    )
    .unwrap();
    let destination = scratch.root().join("restored");
    let refused =
        backup::restore_into_fresh_home(&source.join("sluice.db"), &destination).unwrap_err();
    assert!(matches!(
        refused,
        StoreError::LiveWorkInBackup {
            attempts: 1,
            runs: 1,
            leases: 0,
            calls: 0
        }
    ));
    assert_eq!(
        refused.into_public(false),
        PublicError::Invalid {
            message: "this backup holds live work (1 attempts, 1 runs, 0 leases, 0 calls); a schema-changing restore needs a backup taken with nothing live".into(),
            errors: vec![],
        }
    );
    assert!(!destination.exists(), "the refused restore wrote nothing");
    // With the run ended, the same backup restores and converts.
    c.execute("UPDATE attempts SET phase='terminal'", [])
        .unwrap();
    c.execute("UPDATE runs SET finished_at='now'", []).unwrap();
    drop(c);
    backup::restore_into_fresh_home(&source.join("sluice.db"), &destination).unwrap();
    ReadPool::open(&destination, 1).unwrap();
    let exported = plans::export_plan(
        &Connection::open(destination.join("sluice.db")).unwrap(),
        project,
    )
    .unwrap();
    assert_eq!(compact(&exported.document), compact(&stored));
    // A schema-3 backup restores without converting.
    let again = scratch.root().join("again");
    backup::restore_into_fresh_home(&destination.join("sluice.db"), &again).unwrap();
    ReadPool::open(&again, 1).unwrap();
    // Converting a schema-3 home is refused as already done.
    assert!(matches!(
        convert_home(&again.join("sluice.db")),
        Err(StoreError::ConversionBlocked(b)) if b == ["home: already at schema 3"]
    ));
    let _ = convert::live_work;
}

/// A read-only backup copy of a live home (`SLUICE_CONVERT_COPY=<copy of sluice.db>`): its
/// live work is refused by name; with that work ended on the scratch copy only, it converts,
/// a project whose rev 1 has no history row (the Python importer's) anchored at its exact
/// baseline (§10.4.1); every project round-trips at every revision from its first, exports
/// its stored plan, rebuilds from its history, and the home is schema-equivalent to a fresh
/// one.
#[test]
#[ignore = "needs SLUICE_CONVERT_COPY: a backup-API copy of a home's sluice.db"]
fn a_copy_of_the_live_home_converts() {
    let copy = PathBuf::from(std::env::var("SLUICE_CONVERT_COPY").expect("SLUICE_CONVERT_COPY"));
    let stored: Vec<(String, String, String)> = {
        let c = Connection::open(&copy).unwrap();
        let live = convert::live_work(&c).unwrap();
        eprintln!("live work on the copy: {live:?}");
        if !live.is_empty() {
            match convert_home(&copy) {
                Err(StoreError::ConversionBlocked(blockers)) => {
                    eprintln!("refused: {blockers:?}");
                    assert!(blockers[0].starts_with("home: live work ("));
                }
                other => panic!("live work must block: {other:?}"),
            }
            // The scratch copy's work is ended by hand: the real cutover ends it with the old
            // release's own cancel and adoption paths (§10.1, §10.10).
            c.execute_batch(
                "UPDATE attempts SET phase='terminal',finished_at='copy' WHERE phase<>'terminal';
                 UPDATE runs SET finished_at='copy' WHERE finished_at IS NULL;
                 UPDATE leases SET state='released',released_at='copy' WHERE state IN ('waiting','held');
                 UPDATE calls SET status='failed',finished_at='copy' WHERE status='running';",
            )
            .unwrap();
        }
        c.prepare("SELECT p.project_id,coalesce(r.name,''),p.doc FROM plans p LEFT JOIN projects r USING(project_id)")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    // The copy converts as it is: a project the Python importer brought in (no rev-1 history
    // row) is anchored at its exact baseline (§10.4.1).
    let started = std::time::Instant::now();
    let report = match convert_home(&copy) {
        Ok(report) => report,
        Err(StoreError::ConversionBlocked(blockers)) => panic!("blocked: {blockers:#?}"),
        Err(other) => panic!("{other:?}"),
    };
    eprintln!(
        "converted in {:?}: {}",
        started.elapsed(),
        serde_json::to_string_pretty(&report.to_json()).unwrap()
    );
    let exact_from: std::collections::HashMap<String, u64> = report
        .projects
        .iter()
        .map(|p| {
            (
                p.project_id.to_string(),
                p.anchor.as_ref().map_or(1, |a| a.rev),
            )
        })
        .collect();
    for project in &report.projects {
        if let Some(anchor) = &project.anchor {
            eprintln!(
                "anchored {} at rev {} ({} folded edits, from the {} plan)",
                project.name,
                anchor.rev,
                anchor.folded_edits,
                anchor.source.as_str()
            );
        }
    }
    let c = Connection::open(&copy).unwrap();
    assert_eq!(schema_fingerprint(&c).unwrap(), fresh_fingerprint());
    let mut revisions = 0;
    for (project, name, doc) in stored {
        let project: ProjectId = project.parse().unwrap();
        let exported = plans::export_plan(&c, project).unwrap();
        let doc: JsonMap = serde_json::from_str(&doc).unwrap();
        assert_eq!(compact(&exported.document), compact(&doc), "{name}");
        let changes: Vec<Vec<PlanChange>> = c
            .prepare("SELECT changes FROM plan_edits WHERE project_id=?1 ORDER BY rev")
            .unwrap()
            .query_map([project.to_string()], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|t| serde_json::from_str(&t.unwrap()).unwrap())
            .collect();
        revisions += changes.len();
        let rows = plans::read_plan_rows(&c, project).unwrap();
        let rebuilt = rebuild(&changes);
        assert_eq!(
            (
                rebuilt.header.root_order,
                rebuilt.inputs,
                rebuilt.outputs,
                rebuilt.steps
            ),
            (
                rows.header.root_order,
                rows.inputs,
                rows.outputs,
                rows.steps
            ),
            "{name}: history rebuilds the rows"
        );
        assert_indexes(&c, project);
        eprintln!(
            "{name}: {} revisions ok (round trip checked from rev {})",
            changes.len(),
            exact_from[&project.to_string()]
        );
    }
    eprintln!("{revisions} revisions converted");
}
