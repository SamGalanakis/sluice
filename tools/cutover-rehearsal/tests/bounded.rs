//! A normalized home keeps execution reservations until rehearsal proves its copied
//! services gone. Rehearsing the copy must not release the original home's reservation.
use cutover_rehearsal::{
    dump,
    rehearse::{self, Options},
    scratch::{Launcher, command},
};
use serde_json::json;
use sluice_model::commands::CommandReply;
use sluice_runtime::{coordinator::Coordinator, dispatch::Catalog, scheduler::reconcile_project};
use std::path::Path;

fn held(home: &Path) -> i64 {
    let sql = rusqlite::Connection::open_with_flags(
        home.join("sluice.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    sql.query_row(
        "SELECT count(*) FROM attempts WHERE reservation IS NOT NULL
         AND COALESCE(json_extract(request,'$.resource_released'),0)=0",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

#[tokio::test]
async fn normalized_rehearsal_releases_only_the_copied_execution_reservation() {
    let root = tempfile::Builder::new().prefix("cr.").tempdir().unwrap();
    let home = root.path().join("original");
    std::fs::create_dir(&home).unwrap();
    let host = Launcher::default();
    let broker = Coordinator::open(home.clone(), Catalog::fixtures(), host.clone())
        .await
        .unwrap();
    let CommandReply::Project(project) = command(
        &broker,
        json!({"command":"project_create","args":{"name":"bounded","description":"","icon":null,"resources":{},"author":"test"}}),
    )
    .await
    .unwrap() else {
        panic!("project")
    };
    let project = project.project_id;
    command(&broker, json!({"command":"plan_edit","args":{
        "project":{"kind":"id","value":project},"rev":1,
        "ops":[{"op":"step.add","step":"work","spec":{"run":"fixture.echo","in":{"value":{"default":1}}}}],
        "start":true,"dry_run":false,"reason":"rehearsal","author":"test"
    }})).await.unwrap();
    broker.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&broker, project, "s").await.unwrap();
    assert_eq!(host.launches().len(), 1);
    broker.release_scheduler("s".into()).await.unwrap();
    assert_eq!(held(&home), 1);
    let original =
        dump::dump(&rusqlite::Connection::open(home.join("sluice.db")).unwrap()).unwrap();
    let copy = root.path().join("copy");
    std::fs::create_dir(&copy).unwrap();
    dump::backup_copy(&home.join("sluice.db"), &copy.join("sluice.db")).unwrap();
    let (result, host) = rehearse::rehearse(
        &copy,
        Catalog::fixtures(),
        &Options {
            schema: (sluice_store::schema::SCHEMA_VERSION + 1)
                .try_into()
                .unwrap(),
            deadline: "2026-10-12T18:00:00Z".into(),
            sha: "candidate".into(),
        },
    )
    .await
    .unwrap();
    assert!(result.ok(), "{result:?}");
    assert_eq!(result.report.stopped.len(), 1);
    assert_eq!(host.asked().reconciled.len(), 1);
    assert_eq!(held(&copy), 0);
    assert_eq!(held(&home), 1);
    assert_eq!(
        dump::dump(&rusqlite::Connection::open(home.join("sluice.db")).unwrap()).unwrap(),
        original
    );
}
