#[allow(dead_code)]
#[path = "../../sluice-store/tests/support/plan_rows.rs"]
mod plan_rows;
use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    hash::InputsHash,
    ids::{AttemptId, ProjectId, ProjectSelector, RunId},
    plan::FnSignature,
    rpc::JsonMap,
    types::Type,
};
use sluice_runtime::verify::*;
use sluice_store::{ReadPool, RetrySafety, Writer};
use std::{path::PathBuf, sync::Arc};
struct Home(PathBuf);
impl Home {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("sluice-test-p3-04-{}", RunId::new()));
        let p = sluice_process::host::guard_scratch_home(&p).unwrap();
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
async fn setup() -> (Home, Writer, ReadPool) {
    let h = Home::new();
    let w = Writer::open(&h.0).unwrap();
    let r = ReadPool::open(&h.0, 2).unwrap();
    (h, w, r)
}
fn registry() -> Arc<FixedRegistry> {
    Arc::new(FixedRegistry(IndexMap::from([(
        "test.echo".into(),
        FnSignature {
            inputs: IndexMap::from([("value".into(), Type::String)]),
            outputs: IndexMap::from([("value".into(), Type::String)]),
            ..FnSignature::default()
        },
    )])))
}
async fn project(w: &Writer, doc: Value) -> ProjectId {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let name = format!(
        "p{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    let doc = serde_json::from_value::<JsonMap>(doc).unwrap();
    w.write(RetrySafety::NonIdempotent, move |tx| {
        let p = plan_rows::create_project(tx, &name)?;
        plan_rows::commit_document(tx, p, &doc, None, None)?;
        Ok(p)
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn fresh_and_working_paused_archived_homes_verify_without_writes() {
    let (_h, w, r) = setup().await;
    assert!(verify(&r, registry(), None).await.unwrap().is_empty());
    let p=project(&w,json!({"inputs":{"v":"string"},"steps":{"s":{"run":"test.echo","in":{"value":{"source":"v"}}}}})).await;
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE projects SET paused=1,archived=1 WHERE project_id=?1",
            [p.to_string()],
        )?;
        tx.changed(Some(p), "projects");
        Ok(())
    })
    .await
    .unwrap();
    let before = r
        .cursor(vec![sluice_store::ChangeKey::new(Some(p), "projects")])
        .await
        .unwrap();
    assert!(
        verify(&r, registry(), Some(ProjectSelector::Id(p)))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        before,
        r.cursor(vec![sluice_store::ChangeKey::new(Some(p), "projects")])
            .await
            .unwrap()
    );
    assert!(matches!(
        verify(&r, registry(), Some(ProjectSelector::Id(ProjectId::new()))).await,
        Err(PublicError::NotFound { .. })
    ));
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn plan_compiler_reports_every_bad_input_and_unknown_function() {
    let (_h, w, r) = setup().await;
    let p = project(&w, json!({"steps":{}})).await;
    w.write(RetrySafety::NonIdempotent,move|tx|{tx.sql().execute("UPDATE plans SET doc=?2 WHERE project_id=?1",(p.to_string(),json!({"steps":{"a":{"run":"test.echo","in":{"value":{"default":3}}},"b":{"run":"missing"}}}).to_string()))?;tx.changed(Some(p),"plan");Ok(())}).await.unwrap();
    let problems = verify(&r, registry(), Some(ProjectSelector::Id(p)))
        .await
        .unwrap();
    assert!(
        problems
            .iter()
            .any(|p| p.r#where.contains("steps.a.in.value"))
    );
    assert!(problems.iter().any(|p| p.r#where.contains("steps.b.run")));
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn typed_state_unknown_inputs_outputs_and_changed_graph_hash_are_checked() {
    let (_h, w, r) = setup().await;
    let p=project(&w,json!({"inputs":{"v":"string"},"steps":{"s":{"run":"test.echo","in":{"value":{"default":"old"}}}}})).await;
    w.write(RetrySafety::NonIdempotent,move|tx|{tx.sql().execute("UPDATE inputs SET value='3' WHERE project_id=?1",[p.to_string()])?;tx.sql().execute("INSERT INTO inputs(project_id,name,position,declaration,value) VALUES (?1,'gone',1,'{}','1')",[p.to_string()])?;tx.sql().execute("UPDATE steps SET status='succeeded',outputs=?2,inputs_hash='bad' WHERE project_id=?1",(p.to_string(),json!({"value":2}).to_string()))?;tx.changed(Some(p),"status");Ok(())}).await.unwrap();
    let problems = verify(&r, registry(), None).await.unwrap();
    for part in [
        "inputs.v",
        "inputs.gone",
        "steps.s#outputs.value",
        "steps.s.inputs_hash",
    ] {
        assert!(
            problems.iter().any(|p| p.r#where.contains(part)),
            "{part}: {problems:?}"
        );
    }
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn frozen_attempt_and_result_hashes_are_verified_independently_of_current_graph() {
    let (_h, w, r) = setup().await;
    let p = project(&w, json!({"steps":{}})).await;
    let attempt = AttemptId::new();
    let run = RunId::new();
    let hash = InputsHash::of(&serde_json::from_value::<JsonMap>(json!({"value":"old"})).unwrap())
        .unwrap();
    w.write(RetrySafety::NonIdempotent,move|tx|{let frozen=json!({"inputs":{"value":"old"},"effective_inputs":{"value":"old"},"returns":{"value":"string"},"declared":{}});tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,phase,request,inputs_hash,created_at) VALUES (?1,?2,'terminal',?3,?4,'now')",(attempt.to_string(),p.to_string(),frozen.to_string(),hash.to_string()))?;tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,created_at,finished_at) VALUES (?1,?2,?3,'now','now')",(run.to_string(),p.to_string(),attempt.to_string()))?;tx.sql().execute("INSERT INTO step_results(result_id,project_id,step_id,generation,attempt_id,declaration,inputs,inputs_hash,status,outputs,recorded_at,removed_at) VALUES (?1,?2,'old-step',1,?3,'{}',?4,?5,'succeeded',?6,'now','now')",(sluice_model::ids::ResultId::new().to_string(),p.to_string(),attempt.to_string(),json!({"value":"old"}).to_string(),hash.to_string(),json!({"value":"old"}).to_string()))?;tx.changed(Some(p),"status");Ok(())}).await.unwrap();
    assert!(verify(&r, registry(), None).await.unwrap().is_empty());
    w.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute(
            "UPDATE attempts SET inputs_hash='wrong' WHERE attempt_id=?1",
            [attempt.to_string()],
        )?;
        tx.changed(Some(p), "status");
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        verify(&r, registry(), None)
            .await
            .unwrap()
            .iter()
            .any(|p| p.r#where.contains("attempts#") && p.message.contains("hash"))
    );
    w.shutdown().await.unwrap();
}
#[tokio::test]
async fn env_files_and_stray_directories_are_reported_without_execution() {
    let (h, w, r) = setup().await;
    let p = project(&w, json!({"steps":{}})).await;
    let dir = h.0.join("projects").join(p.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env"), "OK=1\nnot valid\n").unwrap();
    std::fs::write(h.0.join(".env"), "=bad\n").unwrap();
    std::fs::create_dir(h.0.join("projects/stray")).unwrap();
    let problems = verify(&r, registry(), None).await.unwrap();
    assert_eq!(problems.len(), 3);
    assert!(problems.iter().any(|p| p.r#where == ".env:1"));
    assert!(problems.iter().any(|p| p.r#where == "projects/stray"));
    w.shutdown().await.unwrap();
}
#[test]
fn registry_reports_broken_entries_collisions_and_types_independently() {
    let h = Home::new();
    let root = h.0.join("fns");
    for (name, manifest, main) in [
        (
            "bad.type",
            json!({"name":"bad.type","inputs":{"x":"strin"},"outputs":{}}),
            true,
        ),
        (
            "test.echo",
            json!({"name":"test.echo","inputs":{},"outputs":{}}),
            true,
        ),
        (
            "missing.main",
            json!({"name":"missing.main","inputs":{},"outputs":{}}),
            false,
        ),
        (
            "good",
            json!({"name":"good","inputs":{},"outputs":{}}),
            true,
        ),
    ] {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fn.json"), manifest.to_string()).unwrap();
        if main {
            std::fs::write(
                dir.join("main.py"),
                "raise Exception('verify must never execute')",
            )
            .unwrap();
        }
    }
    let inspection = inspect_fns(&h.0, &[root], registry().0.clone());
    assert_eq!(inspection.problems.len(), 3);
    assert!(inspection.signatures.contains_key("good"));
    assert_eq!(
        inspection.signatures["test.echo"].outputs["value"],
        Type::String
    );
}

#[tokio::test]
async fn project_verify_omits_other_project_env_but_file_bindings_remain_read_only() {
    let (h, w, r) = setup().await;
    let file = h.0.join("brief");
    let p = project(
        &w,
        json!({"steps":{"s":{"run":"test.echo","in":{"value":{"file":file}}}}}),
    )
    .await;
    let q = project(&w, json!({"steps":{}})).await;
    let dir = h.0.join("projects").join(q.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env"), "bad").unwrap();
    let problems = verify(&r, registry(), Some(ProjectSelector::Id(p)))
        .await
        .unwrap();
    assert_eq!(problems.len(), 1);
    assert!(problems[0].r#where.contains(".file"));
    std::fs::write(&file, "secret brief").unwrap();
    assert!(
        verify(&r, registry(), Some(ProjectSelector::Id(p)))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "secret brief");
    w.shutdown().await.unwrap();
}
