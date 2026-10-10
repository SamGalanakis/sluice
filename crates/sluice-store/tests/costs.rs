//! The store's cost counters (`docs/design/plan-rows.md` §11). One test in its own binary: the
//! counters are process-wide.

#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
#[allow(dead_code)]
#[path = "support/plan_rows.rs"]
mod support;
use home::ScratchHome;
use serde_json::{Map, Value, json};
use sluice_model::cost::{Costs, Measurement};
use sluice_model::{
    ids::{ProjectId, StepId},
    plan_rows::{PlanEditCommit, PreparationReads, RowSelection, StepProjection},
    rpc::JsonMap,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    plans::{self, CommitOutcome},
};

fn plan(steps: usize, target: Value) -> Value {
    let mut map = Map::new();
    for n in 0..steps {
        map.insert(
            format!("s{n}"),
            json!({"run": "echo", "tags": [format!("unit:u{}", n / 6)]}),
        );
    }
    map.insert("target".into(), target);
    map.insert(
        "after-target".into(),
        json!({"run": "echo", "after": ["target"]}),
    );
    json!({"inputs": {"repo": "string"}, "steps": map})
}

async fn commit(
    measurement: &Measurement,
    writer: &Writer,
    reads: &ReadPool,
    project: ProjectId,
    document: Value,
) -> Costs {
    let document: JsonMap = serde_json::from_value(document).unwrap();
    let prepared: PlanEditCommit = reads
        .snapshot(move |c| {
            Ok(support::document_commit(
                c, project, &document, None, "sam", "edit",
            ))
        })
        .await
        .unwrap();
    measurement.reset();
    let outcome = writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            plans::commit_plan_edit(tx, project, &prepared, None)
        })
        .await
        .unwrap();
    assert!(matches!(outcome, CommitOutcome::Committed(_)));
    measurement.costs()
}

#[tokio::test]
async fn an_edit_costs_what_it_changes_whatever_the_plan_size() {
    let measurement = Measurement::start();
    let mut edits = vec![];
    let mut removals = vec![];
    for size in [20, 200, 2000] {
        let home = ScratchHome::new().unwrap();
        let writer = Writer::open(home.path()).unwrap();
        let reads = ReadPool::open(home.path(), 2).unwrap();
        let project = writer
            .write(RetrySafety::NonIdempotent, |tx| {
                support::create_project(tx, "p")
            })
            .await
            .unwrap();
        let target = json!({"run": "echo", "tags": ["unit:t"], "in": {"value": {"default": 1}}});
        commit(&measurement, &writer, &reads, project, plan(size, target)).await;
        // A binding and metadata change of one step.
        let changed =
            json!({"run": "echo", "tags": ["unit:t", "x"], "in": {"value": {"source": "repo"}}});
        let edit = commit(
            &measurement,
            &writer,
            &reads,
            project,
            plan(size, changed.clone()),
        )
        .await;
        assert_eq!(edit.full_exports, 0, "{size}: an edit exports nothing");
        assert_eq!(edit.positions_renumbered, 0, "{size}");
        edits.push((
            edit.declarations_decoded,
            edit.declarations_written,
            edit.rows_written,
        ));
        // A removal renumbers nothing.
        let mut without = plan(size, changed);
        without["steps"].as_object_mut().unwrap().shift_remove("s3");
        let removal = commit(&measurement, &writer, &reads, project, without).await;
        assert_eq!(removal.positions_renumbered, 0, "{size}");
        removals.push((removal.declarations_written, removal.rows_written));
        // A compact read decodes no declaration; a full one decodes each it returns.
        measurement.reset();
        let rows = reads
            .snapshot(move |c| {
                plans::read_steps(
                    c,
                    project,
                    &RowSelection {
                        limit: Some(50),
                        ..RowSelection::default()
                    },
                    StepProjection::Compact,
                )
            })
            .await
            .unwrap();
        assert_eq!(rows.steps.len(), 50.min(size + 1));
        assert_eq!(measurement.costs().declarations_decoded, 0);
        measurement.reset();
        reads
            .snapshot(move |c| {
                plans::read_steps(
                    c,
                    project,
                    &RowSelection {
                        steps: Some(vec!["target".parse::<StepId>().unwrap()]),
                        ..RowSelection::default()
                    },
                    StepProjection::Full,
                )
            })
            .await
            .unwrap();
        assert_eq!(measurement.costs().declarations_decoded, 1);
        // The read set counts exactly what it read.
        measurement.reset();
        reads
            .snapshot(move |c| {
                plans::read_scoped_state(
                    c,
                    project,
                    &PreparationReads {
                        steps: vec!["target".parse().unwrap(), "after-target".parse().unwrap()],
                        inputs: vec!["repo".into()],
                        ..PreparationReads::default()
                    },
                )
            })
            .await
            .unwrap();
        assert_eq!(
            measurement.costs().state_rows_read,
            2,
            "the input has no value"
        );
        measurement.reset();
        reads
            .snapshot(move |c| plans::export_plan(c, project))
            .await
            .unwrap();
        assert_eq!(measurement.costs().full_exports, 1);
    }
    assert!(edits.windows(2).all(|w| w[0] == w[1]), "{edits:?}");
    assert!(removals.windows(2).all(|w| w[0] == w[1]), "{removals:?}");
    assert!(edits[0].1 == 1 && edits[0].2 > 0, "{edits:?}");
}
