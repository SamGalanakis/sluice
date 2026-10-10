//! Schema-3 plan storage (`docs/design/plan-rows.md` §2 to §5, §8): a fresh home's DDL, the
//! CHECK-held step columns, the state epoch's triggers, `commit_plan_edit` over hand-built
//! `PlanEditCommit`s, the row reads and paged history.

#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
#[allow(dead_code)]
#[path = "support/plan_rows.rs"]
mod support;
use home::ScratchHome;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    events::Event,
    ids::{ProjectId, RecordSeq, Revision, StepId, UnitName},
    plan_index::{output_references, plan_edges, step_index},
    plan_rows::{
        HistoryEvent, PauseValue, PlanChange, PlanEditCommit, PlanRows, PreparationReads,
        ReferenceSelection, RootSection, RowSelection, SourceKind, StateEpoch, StepProjection,
    },
    rpc::JsonMap,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    plans::{self, CommitOutcome},
};
use std::collections::{BTreeSet, HashSet};

fn map(value: Value) -> JsonMap {
    serde_json::from_value(value).unwrap()
}
fn compact(value: &impl serde::Serialize) -> String {
    serde_json::to_string(value).unwrap()
}
fn id(name: &str) -> StepId {
    name.parse().unwrap()
}

struct Home {
    home: ScratchHome,
    writer: Writer,
    reads: ReadPool,
    project: ProjectId,
}
impl Home {
    async fn new() -> Self {
        let home = ScratchHome::new().unwrap();
        let writer = Writer::open(home.path()).unwrap();
        let project = writer
            .write(RetrySafety::NonIdempotent, |tx| {
                support::create_project(tx, "lash")
            })
            .await
            .unwrap();
        let reads = ReadPool::open(home.path(), 2).unwrap();
        Self {
            home,
            writer,
            reads,
            project,
        }
    }
    fn sql(&self) -> Connection {
        Connection::open(self.home.path().join("sluice.db")).unwrap()
    }
    async fn put(&self, document: Value) -> Revision {
        let project = self.project;
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                support::commit_document(tx, project, &map(document), None, None)
            })
            .await
            .unwrap()
    }
    async fn prepare(&self, document: Value) -> PlanEditCommit {
        let project = self.project;
        self.reads
            .snapshot(move |c| {
                Ok(support::document_commit(
                    c,
                    project,
                    &map(document),
                    None,
                    "sam",
                    "change",
                ))
            })
            .await
            .unwrap()
    }
    async fn commit(&self, commit: PlanEditCommit) -> Result<CommitOutcome, PublicError> {
        let project = self.project;
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                plans::commit_plan_edit(tx, project, &commit, None)
            })
            .await
    }
    async fn sql_write(&self, statement: &'static str) -> Result<usize, PublicError> {
        let project = self.project;
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                let changed = tx.sql().execute(statement, [project.to_string()])?;
                tx.changed(Some(project), "status");
                Ok(changed)
            })
            .await
    }
    fn epoch(&self) -> u64 {
        plans::plan_header(&self.sql(), self.project)
            .unwrap()
            .state_epoch
            .0
    }
    fn count(&self, table: &str) -> i64 {
        self.sql()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    fn export(&self) -> String {
        compact(
            &plans::export_plan(&self.sql(), self.project)
                .unwrap()
                .document,
        )
    }
    /// The stored index rows equal those derived from the declarations.
    fn assert_indexes(&self) {
        let c = self.sql();
        let p = self.project.to_string();
        let rows = plans::read_plan_rows(&c, self.project).unwrap();
        let ids: HashSet<&str> = rows.steps.iter().map(|r| r.step.as_str()).collect();
        let mut units = BTreeSet::new();
        let mut tags = BTreeSet::new();
        let mut references = BTreeSet::new();
        for row in &rows.steps {
            let index = step_index(&row.step, &row.declaration, &|n| ids.contains(n));
            units.insert((row.step.to_string(), index.unit.to_string()));
            tags.extend(index.tags.into_iter().map(|t| (row.step.to_string(), t)));
            references.extend(index.references.iter().map(|r| format!("{r:?}")));
        }
        for output in &rows.outputs {
            references.extend(
                output_references(&output.name, &output.binding)
                    .iter()
                    .map(|r| format!("{r:?}")),
            );
        }
        let read = |sql: &str| -> BTreeSet<(String, String)> {
            c.prepare(sql)
                .unwrap()
                .query_map([&p], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            read("SELECT step_id,unit FROM steps WHERE project_id=?1"),
            units
        );
        assert_eq!(
            read("SELECT step_id,tag FROM step_tags WHERE project_id=?1"),
            tags
        );
        let stored: BTreeSet<String> = c
            .prepare(
                "SELECT consumer_kind,consumer_id,slot,ordinal FROM plan_refs WHERE project_id=?1",
            )
            .unwrap()
            .query_map([&p], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .unwrap()
            .map(|r| format!("{:?}", r.unwrap()))
            .collect();
        assert_eq!(
            stored.len(),
            references.len(),
            "plan_refs: {stored:?} {references:?}"
        );
        let selection =
            ReferenceSelection::Consumers(rows.steps.iter().map(|r| r.step.clone()).collect());
        let mut found: BTreeSet<String> = plans::read_references(&c, self.project, &selection)
            .unwrap()
            .0
            .iter()
            .map(|r| format!("{r:?}"))
            .collect();
        found.extend(
            plans::read_references(
                &c,
                self.project,
                &ReferenceSelection::Outputs(rows.outputs.iter().map(|r| r.name.clone()).collect()),
            )
            .unwrap()
            .0
            .iter()
            .map(|r| format!("{r:?}")),
        );
        assert_eq!(found, references);
        let edges: BTreeSet<String> = plan_edges(&rows).iter().map(|e| format!("{e:?}")).collect();
        let stored: BTreeSet<String> =
            plans::read_graph(&c, self.project, &RowSelection::default())
                .unwrap()
                .edges
                .iter()
                .map(|e| format!("{e:?}"))
                .collect();
        assert_eq!(stored, edges);
    }
}

#[tokio::test]
async fn a_fresh_home_is_schema_3_and_a_new_plan_has_three_empty_sections() {
    let h = Home::new().await;
    let c = h.sql();
    let versions: (i64, i64) = (
        c.query_row("SELECT schema_version FROM home_meta", [], |r| r.get(0))
            .unwrap(),
        c.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap(),
    );
    assert_eq!(versions, (3, 3));
    let tables: BTreeSet<String> = c
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(tables.len(), 27);
    for table in ["plan_outputs", "plan_refs", "plan_edges", "step_tags"] {
        assert!(tables.contains(table), "{table}");
    }
    let objects = |kind: &str| -> BTreeSet<String> {
        c.prepare("SELECT name FROM sqlite_schema WHERE type=?1")
            .unwrap()
            .query_map([kind], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        objects("trigger"),
        BTreeSet::from(
            [
                "steps_state_epoch",
                "inputs_state_epoch",
                "projects_state_epoch",
                "resources_insert_state_epoch",
                "resources_update_state_epoch",
                "resources_delete_state_epoch",
                "steps_refs_delete",
                "plan_outputs_refs_delete",
                "step_results_immutable"
            ]
            .map(String::from)
        )
    );
    assert_eq!(
        objects("view"),
        BTreeSet::from(["outcomes", "log", "step_changes", "edits", "questions"].map(String::from))
    );
    for index in [
        "steps_compact",
        "steps_unit_compact",
        "steps_status_compact",
        "steps_needs",
    ] {
        assert!(objects("index").contains(index), "{index}");
    }
    assert!(!objects("index").contains("steps_status"));
    let edits: Vec<String> = c
        .prepare("SELECT name FROM pragma_table_info('edits')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        edits,
        [
            "project_id",
            "rev",
            "seq",
            "at",
            "author",
            "reason",
            "changes"
        ]
    );
    let board_slots: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('projects') WHERE name='board_slots')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!board_slots);
    // A new plan: rev 1, the three sections, one header.put in its history and record.
    let header = plans::plan_header(&c, h.project).unwrap();
    assert_eq!(header.rev, Revision(1));
    assert_eq!(
        header.root_order,
        [
            RootSection::Inputs,
            RootSection::Outputs,
            RootSection::Steps
        ]
    );
    assert_eq!(h.export(), r#"{"inputs":{},"outputs":{},"steps":{}}"#);
    let (record, version): (String, i64) = c
        .query_row(
            "SELECT payload,payload_version FROM records WHERE kind='plan.edit'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(version, 2);
    let record: Value = serde_json::from_str(&record).unwrap();
    assert_eq!(
        record["changes"],
        json!([{"op": "header.put", "root_order": ["inputs", "outputs", "steps"]}])
    );
    let (page, next) = plans::history(&c, h.project, None, None, 10).unwrap();
    assert_eq!(next, None);
    assert!(
        matches!(&page[..], [entry] if matches!(&entry.event, HistoryEvent::PlanEdit(e) if e.rev == Revision(1) && e.reason == "project created"))
    );
}

/// `paused`, `run`, `priority` and `needs` are held equal to the declaration by CHECKs: a
/// write that lets one drift is refused, whatever wrote it.
#[tokio::test]
async fn a_step_column_that_disagrees_with_its_declaration_is_refused() {
    let h = Home::new().await;
    h.put(json!({"steps": {
        "a": {"run": "echo", "paused": "after the release", "priority": 5, "needs": {"cpu": 2}},
        "b": {"run": "echo", "paused": true},
        "c": {"run": "echo", "paused": false}
    }}))
    .await;
    let row: (String, String, i64, Option<String>) = h
        .sql()
        .query_row(
            "SELECT paused,run,priority,needs FROM steps WHERE step_id='a'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            r#""after the release""#.into(),
            "echo".into(),
            5,
            Some(r#"{"cpu":2}"#.into())
        )
    );
    let paused: Vec<String> = h
        .sql()
        .prepare("SELECT paused FROM steps ORDER BY position")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(paused, [r#""after the release""#, "true", "false"]);
    for statement in [
        "UPDATE steps SET priority=9 WHERE project_id=?1 AND step_id='a'",
        "UPDATE steps SET run='other' WHERE project_id=?1 AND step_id='a'",
        "UPDATE steps SET paused='false' WHERE project_id=?1 AND step_id='a'",
        "UPDATE steps SET paused='\"another reason\"' WHERE project_id=?1 AND step_id='a'",
        "UPDATE steps SET needs='{\"cpu\":1}' WHERE project_id=?1 AND step_id='a'",
        "UPDATE steps SET needs=NULL WHERE project_id=?1 AND step_id='a'",
        "UPDATE steps SET paused='true' WHERE project_id=?1 AND step_id='c'",
        "UPDATE steps SET declaration='{\"run\":\"echo\"}' WHERE project_id=?1 AND step_id='a'",
        "INSERT INTO steps(project_id,step_id,position,declaration,unit,paused,run,priority) VALUES (?1,'d',9,'{\"run\":\"x\",\"priority\":1}','d','false','x',0)",
    ] {
        match h.sql_write(statement).await {
            Err(PublicError::Invalid { message, .. }) => {
                assert!(
                    message.starts_with("CHECK constraint failed"),
                    "{statement}: {message}"
                )
            }
            other => panic!("{statement}: {other:?}"),
        }
    }
    // A write that keeps them equal is taken.
    h.sql_write(
        "UPDATE steps SET declaration='{\"run\":\"echo\"}',paused='false',priority=0,needs=NULL WHERE project_id=?1 AND step_id='a'",
    )
    .await
    .unwrap();
}

/// The state epoch moves with the execution state an edit's preparation reads (statuses,
/// outputs, results, input values, the project's pause, its resources) and nothing else.
#[tokio::test]
async fn the_state_epoch_moves_with_execution_state_only() {
    let h = Home::new().await;
    h.put(json!({"inputs": {"n": "int"}, "steps": {"a": {"run": "echo"}}}))
        .await;
    let start = h.epoch();
    let moves: [(&'static str, bool); 10] = [
        (
            "UPDATE steps SET status='running' WHERE project_id=?1",
            true,
        ),
        ("UPDATE steps SET outputs='{}' WHERE project_id=?1", true),
        (
            "UPDATE steps SET progress='{\"p\":1}',progress_at='now' WHERE project_id=?1",
            false,
        ),
        (
            "UPDATE steps SET delivery_cursor=3 WHERE project_id=?1",
            false,
        ),
        ("UPDATE inputs SET value='3' WHERE project_id=?1", true),
        ("UPDATE projects SET paused=1 WHERE project_id=?1", true),
        (
            "UPDATE projects SET description='x' WHERE project_id=?1",
            false,
        ),
        (
            "INSERT INTO resources(scope,project_id,name,declaration,capacity) VALUES (?1,?1,'cpu','2',2)",
            true,
        ),
        ("UPDATE resources SET capacity=3 WHERE project_id=?1", true),
        ("DELETE FROM resources WHERE project_id=?1", true),
    ];
    let mut expected = start;
    for (statement, moves) in moves {
        h.sql_write(statement).await.unwrap();
        expected += u64::from(moves);
        assert_eq!(h.epoch(), expected, "{statement}");
    }
    // Plan edits move the revision, not the epoch.
    h.put(json!({"inputs": {"n": "int"}, "steps": {"a": {"run": "echo", "doc": "x"}}}))
        .await;
    assert_eq!(h.epoch(), expected);
}

#[tokio::test]
async fn commit_applies_a_hand_built_edit_and_refuses_what_moved() {
    let h = Home::new().await;
    let p = h.project;
    let rev = h
        .put(json!({
            "inputs": {"repo": "string"},
            "outputs": {"notes": {"source": "reader/value"}},
            "steps": {
                "source": {"run": "echo", "tags": ["unit:build"]},
                "reader": {"run": "echo", "after": ["unit:build"], "in": {"value": {"source": "source/value.0"}, "cwd": {"source": "repo"}}},
                "other": {"run": "echo"}
            }
        }))
        .await;
    assert_eq!(rev, Revision(2));
    h.assert_indexes();
    let generation: i64 = h
        .sql()
        .query_row(
            "SELECT generation FROM steps WHERE step_id='reader'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(generation, 2, "a new step's generation is its revision");
    let records = h.count("records");
    // An edit that changes nothing commits nothing: no revision, record or history row.
    let commit = h.prepare(json!({
        "inputs": {"repo": "string"},
        "outputs": {"notes": {"source": "reader/value"}},
        "steps": {
            "source": {"run": "echo", "tags": ["unit:build"]},
            "reader": {"run": "echo", "after": ["unit:build"], "in": {"value": {"source": "source/value.0"}, "cwd": {"source": "repo"}}},
            "other": {"run": "echo"}
        }
    }))
    .await;
    assert!(commit.rows.changes.is_empty());
    assert_eq!(
        h.commit(commit).await.unwrap(),
        CommitOutcome::Committed(Revision(2))
    );
    assert_eq!((h.count("records"), h.count("plan_edits")), (records, 2));
    // A token that moved since preparation: stale, nothing written.
    let edit = json!({"inputs": {}, "outputs": {}, "steps": {"source": {"run": "echo", "tags": ["unit:build"]}, "other": {"run": "echo", "doc": "d"}}});
    let commit = h.prepare(edit.clone()).await;
    h.sql_write("UPDATE steps SET status='succeeded',outputs='{\"value\":[1]}' WHERE project_id=?1 AND step_id='reader'")
        .await
        .unwrap();
    let records = h.count("records");
    assert_eq!(
        h.commit(commit.clone()).await.unwrap(),
        CommitOutcome::Stale
    );
    let board = h.prepare(edit.clone()).await;
    h.sql_write("UPDATE projects SET board_rev=board_rev+1 WHERE project_id=?1")
        .await
        .unwrap();
    assert_eq!(h.commit(board).await.unwrap(), CommitOutcome::Stale);
    assert_eq!(h.count("records"), records);
    // An explicit revision that is not current: conflict with the current one.
    let mut explicit = h.prepare(edit.clone()).await;
    explicit.rev = Some(Revision(1));
    assert_eq!(
        h.commit(explicit).await.unwrap_err(),
        PublicError::Conflict {
            message: "plan is at rev 2".into(),
            current_rev: Some(Revision(2))
        }
    );
    // A removal: the reader goes with its references, tags and edges, its outcome archived;
    // the input and the output go too.
    let removal = h.prepare(edit.clone()).await;
    assert_eq!(removal.state.removed, [id("reader")]);
    assert_eq!(
        h.commit(removal).await.unwrap(),
        CommitOutcome::Committed(Revision(3))
    );
    let c = h.sql();
    let left: i64 = c
        .query_row(
            "SELECT count(*) FROM plan_refs WHERE project_id=?1 AND (consumer_id='reader' OR consumer_kind='output')",
            [p.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(left, 0);
    assert_eq!(h.count("plan_edges"), 0);
    assert_eq!(h.count("outcomes"), 1);
    h.assert_indexes();
    // Removing the source the removed reader read is then accepted.
    let rev = h
        .put(json!({"inputs": {}, "outputs": {}, "steps": {"other": {"run": "echo", "doc": "d"}}}))
        .await;
    assert_eq!(rev, Revision(4));
    assert_eq!((h.count("plan_refs"), h.count("step_tags")), (0, 0));
    h.assert_indexes();
    let edits: Vec<(i64, String)> = c
        .prepare("SELECT rev,changes FROM edits WHERE project_id=?1 ORDER BY rev")
        .unwrap()
        .query_map([p.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(edits.len(), 4);
    assert_eq!(
        edits[2].1,
        r#"[{"op":"input.delete","name":"repo"},{"op":"output.delete","name":"notes"},{"op":"step.delete","step":"reader"},{"op":"step.put","step":"other","position":2,"declaration":{"run":"echo","doc":"d"}}]"#
    );
    let record: Value = serde_json::from_str(
        &c.query_row(
            "SELECT payload FROM records WHERE kind='plan.edit' AND json_extract(payload,'$.rev')=3",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(compact(&record["changes"]), edits[2].1);
    // A commit whose parts disagree is refused.
    let mut inconsistent = h
        .prepare(json!({"inputs": {}, "outputs": {}, "steps": {}}))
        .await;
    inconsistent.state.removed.clear();
    assert!(
        matches!(h.commit(inconsistent).await, Err(PublicError::Invalid { message, .. }) if message.contains("inconsistent"))
    );
    // The drain refuses plan edits in the writer too.
    let drained = h
        .prepare(json!({"inputs": {}, "outputs": {}, "steps": {}}))
        .await;
    h.sql_write("UPDATE maintenance SET mode='drain' WHERE ?1 IS NOT NULL")
        .await
        .unwrap();
    assert_eq!(
        h.commit(drained).await.unwrap_err(),
        PublicError::Busy {
            message: "drain rejects new plan work and user calls".into(),
            retryable: false
        }
    );
}

/// `order.set` rewrites a collection's positions: rows swap places through two phases, so no
/// two rows ever share a position, and removals never renumber survivors.
#[tokio::test]
async fn reordering_moves_rows_through_two_phases_and_removal_leaves_gaps() {
    let h = Home::new().await;
    h.put(json!({"steps": {"a": {"run": "x"}, "b": {"run": "x"}, "c": {"run": "x"}, "d": {"run": "x"}}}))
        .await;
    let positions = |h: &Home| -> Vec<(String, i64)> {
        h.sql()
            .prepare("SELECT step_id,position FROM steps ORDER BY position")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    // Remove b: a gap, nobody moves.
    h.put(json!({"steps": {"a": {"run": "x"}, "c": {"run": "x"}, "d": {"run": "x"}}}))
        .await;
    assert_eq!(
        positions(&h),
        [("a".into(), 0), ("c".into(), 2), ("d".into(), 3)]
    );
    // order.set [d, c, a]: positions 0, 1, 2 (d takes a's, a takes c's old one).
    let project = h.project;
    let commit = h
        .reads
        .snapshot(move |c| {
            let current = plans::read_plan_rows(c, project)?;
            let mut next = current.clone();
            next.steps.reverse();
            for (position, row) in next.steps.iter_mut().enumerate() {
                row.position = position as u64;
            }
            let mut commit =
                support::rows_commit(c, project, &current, &next, None, "sam", "order");
            commit.rev = Some(current.header.rev);
            Ok(commit)
        })
        .await
        .unwrap();
    assert_eq!(commit.rows.changes.len(), 3);
    assert_eq!(
        h.commit(commit).await.unwrap(),
        CommitOutcome::Committed(Revision(4))
    );
    assert_eq!(
        positions(&h),
        [("d".into(), 0), ("c".into(), 1), ("a".into(), 2)]
    );
    assert_eq!(
        h.export(),
        r#"{"steps":{"d":{"run":"x"},"c":{"run":"x"},"a":{"run":"x"}}}"#
    );
    // Inserting before a survivor goes through the same moves.
    h.put(json!({"inputs": {"z": "int"}, "steps": {"d": {"run": "x"}, "c": {"run": "x"}, "a": {"run": "x"}}}))
        .await;
    h.put(json!({"inputs": {"a1": "int", "b1": "int", "z": "int"}, "steps": {"d": {"run": "x"}, "c": {"run": "x"}, "a": {"run": "x"}}}))
        .await;
    assert_eq!(
        h.export(),
        r#"{"inputs":{"a1":"int","b1":"int","z":"int"},"steps":{"d":{"run":"x"},"c":{"run":"x"},"a":{"run":"x"}}}"#
    );
}

#[tokio::test]
async fn reads_select_steps_references_graphs_and_scoped_state() {
    let h = Home::new().await;
    h.put(json!({
        "inputs": {"repo": "string"},
        "steps": {
            "fork": {"run": "git.fork", "tags": ["unit:lane"], "in": {"cwd": {"source": "repo"}}},
            "work": {"run": "agent.run", "tags": ["unit:lane"], "after": ["fork"], "priority": 10, "paused": "later", "needs": {"cpu": 1}},
            "land": {"run": "agent.run", "tags": ["unit:lane"], "after": ["work"], "needs": {"cpu": 2, "gpu": 1}},
            "tests": {"run": "agent.run", "after": ["unit:lane"]},
            "other": {"run": "agent.run", "needs": {"gpu": 1}}
        }
    }))
    .await;
    let p = h.project;
    let c = h.sql();
    c.execute("UPDATE steps SET status='failed' WHERE step_id='fork'", [])
        .unwrap();
    c.execute("UPDATE inputs SET value='\"/repo\"' WHERE name='repo'", [])
        .unwrap();
    let read = |selection: RowSelection, projection| {
        plans::read_steps(&c, p, &selection, projection).unwrap()
    };
    let ids = |rows: &sluice_model::plan_rows::StepRows| -> Vec<String> {
        rows.steps.iter().map(|s| s.step.to_string()).collect()
    };
    let all = read(RowSelection::default(), StepProjection::Compact);
    assert_eq!(ids(&all), ["fork", "work", "land", "tests", "other"]);
    assert_eq!((all.rev, all.more), (Revision(2), false));
    assert!(all.steps.iter().all(|s| s.declaration.is_none()));
    let work = &all.steps[1];
    assert_eq!(
        (
            work.unit.as_str(),
            work.run.as_str(),
            work.priority,
            work.paused.clone(),
            work.status.clone()
        ),
        (
            "lane",
            "agent.run",
            10,
            PauseValue::Reason("later".into()),
            StepStatus::Pending
        )
    );
    let lane = RowSelection {
        units: Some(vec!["lane".parse().unwrap()]),
        status: Some(vec![StepStatus::Pending, StepStatus::Failed]),
        limit: Some(2),
        ..RowSelection::default()
    };
    let page = read(lane.clone(), StepProjection::Compact);
    assert_eq!(
        (ids(&page), page.more),
        (vec!["fork".to_owned(), "work".into()], true)
    );
    let last = page.steps.last().unwrap();
    let next = read(
        RowSelection {
            after: Some((last.position, last.step.clone())),
            ..lane
        },
        StepProjection::Full,
    );
    assert_eq!((ids(&next), next.more), (vec!["land".to_owned()], false));
    assert_eq!(
        compact(next.steps[0].declaration.as_ref().unwrap()),
        r#"{"run":"agent.run","tags":["unit:lane"],"after":["work"],"needs":{"cpu":2,"gpu":1}}"#
    );
    for empty in [
        RowSelection {
            units: Some(vec![]),
            ..RowSelection::default()
        },
        RowSelection {
            steps: Some(vec![]),
            ..RowSelection::default()
        },
        RowSelection {
            steps: Some(vec![id("nope")]),
            ..RowSelection::default()
        },
    ] {
        assert!(read(empty, StepProjection::Compact).steps.is_empty());
    }
    // References by consumer and by source.
    let readers = plans::read_references(
        &c,
        p,
        &ReferenceSelection::Sources {
            kind: SourceKind::Input,
            ids: vec!["repo".into()],
        },
    )
    .unwrap();
    assert_eq!(
        readers
            .0
            .iter()
            .map(|r| r.consumer_id.as_str())
            .collect::<Vec<_>>(),
        ["fork"]
    );
    let gates = plans::read_references(
        &c,
        p,
        &ReferenceSelection::Sources {
            kind: SourceKind::Unit,
            ids: vec!["lane".into()],
        },
    )
    .unwrap();
    assert_eq!(
        gates
            .0
            .iter()
            .map(|r| r.consumer_id.as_str())
            .collect::<Vec<_>>(),
        ["tests"]
    );
    // A unit's graph draws its neighbours as boundary nodes.
    let graph = plans::read_graph(
        &c,
        p,
        &RowSelection {
            steps: Some(vec![id("work")]),
            ..RowSelection::default()
        },
    )
    .unwrap();
    assert_eq!(graph.steps.len(), 1);
    assert_eq!(
        graph
            .boundary
            .iter()
            .map(|s| s.step.as_str())
            .collect::<Vec<_>>(),
        ["fork", "land"]
    );
    let graph = plans::read_graph(
        &c,
        p,
        &RowSelection {
            units: Some(vec![UnitName::new("lane").unwrap()]),
            ..RowSelection::default()
        },
    )
    .unwrap();
    assert_eq!(
        graph
            .boundary
            .iter()
            .map(|s| s.step.as_str())
            .collect::<Vec<_>>(),
        ["tests"]
    );
    assert_eq!(
        graph.edges.iter().filter(|e| e.via_unit.is_some()).count(),
        1
    );
    // The read set: exactly the listed steps, inputs, leases and competitors.
    c.execute("INSERT INTO resources(scope,project_id,name,declaration,capacity) VALUES (?1,?1,'cpu','4',4)", [p.to_string()]).unwrap();
    let (attempt, run) = (
        sluice_model::ids::AttemptId::new().to_string(),
        sluice_model::ids::RunId::new().to_string(),
    );
    c.execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'other','executing','{}','h','now')", params![attempt, p.to_string()]).unwrap();
    c.execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at) VALUES (?1,?2,?3,'other','now')", params![run, p.to_string(), attempt]).unwrap();
    c.execute("INSERT INTO leases(project_id,run_id,request_id,scope,resource,amount,state,created_at) VALUES (?1,?2,'r1',?1,'cpu',3,'held','now')", params![p.to_string(), run]).unwrap();
    let scoped = plans::read_scoped_state(
        &c,
        p,
        &PreparationReads {
            steps: vec![id("fork"), id("work")],
            inputs: vec!["repo".into()],
            resources: vec!["cpu".into()],
            competitors: vec!["gpu".into()],
        },
    )
    .unwrap();
    assert_eq!(
        scoped
            .state
            .steps
            .keys()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
        ["fork", "work"]
    );
    assert_eq!(scoped.state.status(&id("fork")), StepStatus::Failed);
    assert_eq!(compact(&scoped.state.inputs), r#"{"repo":"/repo"}"#);
    assert_eq!(scoped.leases.len(), 1);
    assert_eq!(
        (
            scoped.leases[0].step.as_ref().map(|s| s.as_str()),
            scoped.leases[0].held,
            scoped.leases[0].amount
        ),
        (Some("other"), true, 3)
    );
    assert_eq!(
        scoped
            .competitors
            .iter()
            .map(|c| c.step.as_str())
            .collect::<Vec<_>>(),
        ["land", "other"]
    );
}

#[tokio::test]
async fn history_pages_by_seq_and_revision() {
    let h = Home::new().await;
    for n in 0..4 {
        h.put(json!({"steps": {"a": {"run": "x", "doc": format!("v{n}")}}}))
            .await;
    }
    let p = h.project;
    h.writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.append_record(
                Some(p),
                Event::PlanInput {
                    rev: Revision(5),
                    author: "owner".into(),
                    reason: String::new(),
                    name: "n".into(),
                    value: json!(3).try_into().unwrap(),
                },
            )?;
            tx.append_record(
                Some(p),
                Event::ProjectUpdate {
                    fields: vec![],
                    reason: None,
                    author: "x".into(),
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let c = h.sql();
    let (all, next) = plans::history(&c, p, None, None, 200).unwrap();
    assert_eq!(next, None);
    assert_eq!(
        all.len(),
        6,
        "five edits and the input, never other records"
    );
    assert!(all.windows(2).all(|w| w[0].seq < w[1].seq));
    let (page, next) = plans::history(&c, p, None, None, 2).unwrap();
    assert_eq!((page.len(), next), (2, Some(page[1].seq)));
    let (page, _) = plans::history(&c, p, None, next, 2).unwrap();
    assert_eq!(page[0].seq, all[2].seq);
    let (page, next) = plans::history(&c, p, Some(Revision(4)), None, 200).unwrap();
    assert_eq!(page.len(), 2, "rev 5's edit and its input");
    assert_eq!(next, None);
    let (page, _) = plans::history(&c, p, Some(Revision(2)), Some(all[3].seq), 200).unwrap();
    assert_eq!(
        page.iter().map(|e| e.seq).collect::<Vec<_>>(),
        [all[4].seq, all[5].seq]
    );
    assert_eq!(
        plans::history(&c, p, None, None, 0)
            .unwrap_err()
            .into_public(false),
        PublicError::BadRequest {
            message: "limit must be 1 to 1000".into()
        }
    );
    assert_eq!(plans::history(&c, p, None, None, 5000).unwrap().0.len(), 6);
    // Trimming the log leaves the plan's edits.
    c.execute("DELETE FROM records WHERE project_id=?1", [p.to_string()])
        .unwrap();
    let (page, _) = plans::history(&c, p, None, None, 200).unwrap();
    assert_eq!(page.len(), 5);
    let _ = (
        RecordSeq(0),
        StateEpoch(0),
        PlanChange::StepDelete { step: id("a") },
        PlanRows::from_document(&map(json!({"steps": {}})), None),
    );
}
