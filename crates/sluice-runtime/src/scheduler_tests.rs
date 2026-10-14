use super::*;
use crate::{dispatch::Catalog, execution::OsHost};
use sluice_model::{commands::CommandReply, rpc::decode_json};

async fn command(broker: &Coordinator<OsHost>, value: Value) -> CommandReply {
    broker
        .command(decode_json(&serde_json::to_vec(&value).unwrap()).unwrap())
        .await
        .unwrap()
}

async fn fixture() -> (tempfile::TempDir, Coordinator<OsHost>, ProjectId) {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    sluice_process::host::guard_scratch_home(home.path()).unwrap();
    let broker = Coordinator::open(
        home.path().into(),
        Catalog::core(),
        OsHost {
            home: home.path().into(),
            program: home.path().join("unused"),
        },
    )
    .await
    .unwrap();
    let CommandReply::Project(project) = command(
        &broker,
        json!({"command": "project_create", "args": {
            "name": "retry", "description": "", "icon": null,
            "resources": {"cpu": 1}, "author": "test"
        }}),
    )
    .await
    else {
        panic!("project reply")
    };
    let project = project.project_id;
    command(
        &broker,
        json!({"command": "plan_edit", "args": {
            "project": {"kind": "id", "value": project}, "rev": 1,
            "ops": [
                {"op": "step.add", "step": "first", "spec": {
                    "run": "core.echo", "priority": 1, "needs": {"cpu": 1},
                    "in": {"value": {"default": "first"}}
                }},
                {"op": "step.add", "step": "raced", "spec": {
                    "run": "core.echo", "needs": {"cpu": 1},
                    "in": {"value": {"default": "old"}}
                }}
            ], "start": true, "dry_run": false, "reason": "test", "author": "test"
        }}),
    )
    .await;
    broker.acquire_scheduler("test".into()).await.unwrap();
    (home, broker, project)
}

async fn edit(broker: &Coordinator<OsHost>, context: &plans::PlanContext) {
    command(
        broker,
        json!({"command": "plan_edit", "args": {
            "project": {"kind": "id", "value": context.project}, "rev": context.revision,
            "ops": [{"op": "step.update", "step": "raced", "changes": {
                "in": {"value": {"default": context.revision.0 + 1}}
            }}], "start": true, "dry_run": false, "reason": "race", "author": "test"
        }}),
    )
    .await;
}

#[tokio::test]
async fn edits_restart_admission_then_defer_and_lease_loss_refuses_unrun_work() {
    let (_home, broker, project) = fixture().await;
    let mut snapshots = 0;
    let admitted = reconcile_project_with(&broker, project, "test", async |context| {
        snapshots += 1;
        if snapshots == 2 {
            edit(&broker, &context).await;
        }
    })
    .await
    .expect("the same call admits the new revision without a stale deferral");
    assert_eq!(admitted, 2, "retain the admission before the restart");
    let state = broker
        .reads()
        .snapshot(move |sql| plans::read_state(sql, project))
        .await
        .unwrap();
    assert_eq!(
        state.steps[&StepId::new("first").unwrap()].outputs.0["value"].as_value(),
        "first"
    );
    assert_eq!(
        state.steps[&StepId::new("raced").unwrap()].outputs.0["value"].as_value(),
        3
    );
    assert_eq!(broker.context(project).await.unwrap().revision, Revision(3));
    broker.writer().shutdown().await.unwrap();
    repeated_edits_defer_after_three_attempts_and_lease_loss_never_retries().await;
}

async fn repeated_edits_defer_after_three_attempts_and_lease_loss_never_retries() {
    let (_home, broker, project) = fixture().await;
    let result = reconcile_project_with(&broker, project, "test", async |context| {
        edit(&broker, &context).await;
    })
    .await;
    assert!(
        matches!(result, Err(PublicError::Conflict { message, current_rev: None }) if message == "compiled plan is stale")
    );
    assert_eq!(broker.context(project).await.unwrap().revision, Revision(5));

    let result = reconcile_project_with(&broker, project, "test", async |_| {
        broker.release_scheduler("test".into()).await.unwrap();
        broker.acquire_scheduler("another".into()).await.unwrap();
    })
    .await;
    assert!(
        matches!(result, Err(PublicError::Conflict { message, current_rev: None }) if message == "scheduler lease lost")
    );
    let state = broker
        .reads()
        .snapshot(move |sql| plans::read_state(sql, project))
        .await
        .unwrap();
    assert!(
        state
            .steps
            .values()
            .all(|step| step.status == StepStatus::Pending && step.outputs.0.is_empty())
    );
    broker.writer().shutdown().await.unwrap();
}
