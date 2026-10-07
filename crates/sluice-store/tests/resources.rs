#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId},
    plan::{FnSignature, Plan, SignatureProvider},
    types::Type,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    resources::{self as r, Needs},
};

struct Signatures;
impl SignatureProvider for Signatures {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        let mut signature = FnSignature::default();
        match name {
            "cap" | "other-cap" => {
                signature.outputs.insert("capacity".into(), Type::Int);
            }
            "required" => {
                signature.outputs.insert("capacity".into(), Type::Int);
                signature.inputs.insert("n".into(), Type::Int);
            }
            "job" | "core.external" => {}
            _ => return None,
        }
        Some(signature)
    }
}
struct Fixture {
    _home: ScratchHome,
    writer: Writer,
    reads: ReadPool,
    project: ProjectId,
    plan: Plan,
}
impl Fixture {
    async fn new(steps: Value, resources: Value) -> Self {
        let home = ScratchHome::new().unwrap();
        assert!(home.root().exists());
        assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
        let writer = Writer::open(home.path()).unwrap();
        let reads = ReadPool::open(home.path(), 2).unwrap();
        let project = ProjectId::new();
        let doc = json!({"inputs":{},"outputs":{},"steps":steps});
        let plan = Plan::parse_json(doc.to_string().as_bytes(), &Signatures).unwrap();
        writer.write(RetrySafety::NonIdempotent,move |tx| {
            tx.sql().execute("INSERT INTO projects(project_id,name,created_at) VALUES (?,'p','now')",[project.to_string()])?;
            tx.sql().execute("INSERT INTO plans(project_id,rev,doc) VALUES (?,1,?)",params![project.to_string(),doc.to_string()])?;
            for (position,(step,decl)) in doc["steps"].as_object().unwrap().iter().enumerate() {
                tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration) VALUES (?,?,?,?)",params![project.to_string(),step,position as i64,decl.to_string()])?;
            }
            r::patch_resources(tx,project,&resources,&Signatures)?;
            tx.changed(Some(project),"plan");
            Ok(())
        }).await.unwrap();
        Self {
            _home: home,
            writer,
            reads,
            project,
            plan,
        }
    }
    async fn patch(&self, patch: Value) -> Result<bool, PublicError> {
        let project = self.project;
        self.writer
            .write(RetrySafety::Idempotent, move |tx| {
                r::patch_resources(tx, project, &patch, &Signatures)
            })
            .await
    }
    async fn reserve(&self, step: &str, item: i64, needs: Needs) -> Result<RunId, PublicError> {
        let project = self.project;
        let step = step.to_owned();
        let run = RunId::new();
        let attempt = AttemptId::new();
        self.writer.write(RetrySafety::NonIdempotent,move |tx| {
            tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,item_index,phase,request,inputs_hash,created_at) VALUES (?,?,?,?,'reserved','{}','hash','now')",params![attempt.to_string(),project.to_string(),step,item])?;
            tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,item_index,created_at) VALUES (?,?,?,?,?,'now')",params![run.to_string(),project.to_string(),attempt.to_string(),step,item])?;
            tx.hold_needs(run,&needs)?;
            tx.sql().execute("UPDATE steps SET status='running' WHERE project_id=? AND step_id=?",params![project.to_string(),step])?;
            tx.changed(Some(project),"status");
            Ok(run)
        }).await
    }
    async fn stop(&self, run: RunId) -> bool {
        self.writer.write(RetrySafety::Idempotent,move |tx| {
            tx.sql().execute("UPDATE attempts SET phase='terminal' WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?)",[run.to_string()])?;
            tx.sql().execute("UPDATE runs SET finished_at='stopped' WHERE run_id=?",[run.to_string()])?;
            tx.sql().execute("UPDATE steps SET status='succeeded' WHERE (project_id,step_id)=(SELECT project_id,step_id FROM runs WHERE run_id=?)",[run.to_string()])?;
            tx.changed(None,"status");
            tx.release_needs(run)
        }).await.unwrap()
    }
    async fn held(&self) -> Needs {
        let p = self.project;
        self.reads.snapshot(move |c| r::held(c, p)).await.unwrap()
    }
    async fn fit(&self, needs: Needs) -> r::Fit {
        let p = self.project;
        self.reads
            .snapshot(move |c| r::fits(c, p, &needs))
            .await
            .unwrap()
    }
    async fn resource(&self, name: &str) -> r::Resource {
        let p = self.project;
        let name = name.to_owned();
        self.reads
            .snapshot(move |c| Ok(r::declarations(c, p)?.remove(&name).unwrap()))
            .await
            .unwrap()
    }
    async fn observe(&self, name: &str, revision: i64, value: Result<u64, PublicError>) -> bool {
        let p = self.project;
        let name = name.to_owned();
        self.writer
            .write(RetrySafety::Idempotent, move |tx| {
                r::observe_capacity(tx, p, &name, revision, value)
            })
            .await
            .unwrap()
    }
}
fn needs(items: &[(&str, u64)]) -> Needs {
    items.iter().map(|(n, a)| (n.to_string(), *a)).collect()
}
fn failure() -> PublicError {
    PublicError::FnFailure {
        message: "no reading".into(),
    }
}

#[tokio::test]
async fn multi_resource_reservation_rolls_back_entire_attempt_and_names_shortages() {
    let f = Fixture::new(
        json!({"a":{"run":"job"},"b":{"run":"job"}}),
        json!({"lane":56,"gpu":1}),
    )
    .await;
    f.reserve("a", -1, needs(&[("lane", 56), ("gpu", 1)]))
        .await
        .unwrap();
    let fit = f.fit(needs(&[("lane", 1), ("gpu", 1)])).await;
    assert_eq!(fit.blocked, ["gpu", "lane"]);
    assert_eq!(fit.reason, "needs gpu 1 (1/1 held), lane 1 (56/56 held)");
    assert!(
        f.reserve("b", -1, needs(&[("lane", 1), ("gpu", 1)]))
            .await
            .is_err()
    );
    let count: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT count(*) FROM attempts", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn holds_and_cached_capacity_survive_writer_restart() {
    let f = Fixture::new(
        json!({"a":{"run":"job"}}),
        json!({"cpu":{"capacity_fn":"cap"}}),
    )
    .await;
    let rev = f.resource("cpu").await.revision;
    assert!(f.observe("cpu", rev, Ok(2)).await);
    let run = f.reserve("a", -1, needs(&[("cpu", 1)])).await.unwrap();
    f.writer.shutdown().await.unwrap();
    let writer = Writer::open(f._home.path()).unwrap();
    assert_eq!(f.resource("cpu").await.capacity, Some(2));
    assert_eq!(f.held().await["cpu"], 1);
    writer
        .write(RetrySafety::Idempotent, move |tx| {
            assert!(!tx.release_needs(run)?);
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(f.held().await["cpu"], 1);
}

#[tokio::test]
async fn declaration_changes_reset_cache_and_fence_old_observations() {
    let f = Fixture::new(json!({}), json!({"cpu":{"capacity_fn":"cap"}})).await;
    let rev = f.resource("cpu").await.revision;
    f.observe("cpu", rev, Ok(9)).await;
    f.patch(json!({"cpu":{"capacity_fn":"other-cap"}}))
        .await
        .unwrap();
    assert_eq!(f.resource("cpu").await.capacity, None);
    assert!(!f.observe("cpu", rev, Ok(100)).await);
    let next = f.resource("cpu").await.revision;
    f.observe("cpu", next, Ok(3)).await;
    f.patch(json!({"cpu":4})).await.unwrap();
    assert_eq!(f.resource("cpu").await.capacity, Some(4));
    assert!(!f.observe("cpu", next, Err(failure())).await);
    f.patch(json!({"cpu":null})).await.unwrap();
    f.patch(json!({"cpu":{"capacity_fn":"cap"}})).await.unwrap();
    assert!(!f.observe("cpu", rev, Ok(100)).await);
    assert_eq!(f.resource("cpu").await.capacity, None);
}

#[tokio::test]
async fn patches_keep_unmentioned_resources_normalize_and_noop_without_version_change() {
    let f = Fixture::new(json!({}), json!({"lane":56,"codex":{"capacity":12}})).await;
    f.patch(json!({"codex":8,"gpu":0})).await.unwrap();
    f.patch(json!({"lane":null})).await.unwrap();
    let p = f.project;
    let before = f
        .reads
        .cursor(vec![sluice_store::ChangeKey::new(Some(p), "resources")])
        .await
        .unwrap();
    assert!(
        !f.patch(json!({"codex":{"capacity":8},"missing":null}))
            .await
            .unwrap()
    );
    let after = f
        .reads
        .cursor(vec![sluice_store::ChangeKey::new(Some(p), "resources")])
        .await
        .unwrap();
    assert_eq!(before, after);
    let names = f
        .reads
        .snapshot(move |c| Ok(r::declarations(c, p)?.into_keys().collect::<Vec<_>>()))
        .await
        .unwrap();
    assert_eq!(names, ["codex", "gpu"]);
}

#[tokio::test]
async fn malformed_declarations_and_invalid_capacity_signatures_are_refused_atomically() {
    let f = Fixture::new(json!({}), json!({"lane":1})).await;
    for bad in [
        json!([1]),
        json!({"Lane":1}),
        json!({"lane":-1}),
        json!({"lane":true}),
        json!({"lane":{"capacity":1,"capacity_fn":"cap"}}),
        json!({"lane":{"capacity_fn":"missing"}}),
        json!({"lane":{"capacity_fn":"job"}}),
        json!({"lane":{"capacity_fn":"required"}}),
        json!({"lane":2,"z":{"capacity_fn":"missing"}}),
        json!({"lane":u64::MAX}),
    ] {
        assert!(f.patch(bad).await.is_err());
        assert_eq!(f.resource("lane").await.capacity, Some(1));
    }
}

#[tokio::test]
async fn removal_refused_for_needs_even_zero_and_patch_does_not_partially_apply() {
    let f = Fixture::new(
        json!({"a":{"run":"job","needs":{"lane":0}}}),
        json!({"lane":1,"gpu":2}),
    )
    .await;
    assert!(f.patch(json!({"gpu":0,"lane":null})).await.is_err());
    assert_eq!(f.resource("gpu").await.capacity, Some(2));
    f.patch(json!({"lane":0})).await.unwrap();
}

#[tokio::test]
async fn stale_compiled_plan_cannot_admit() {
    let f = Fixture::new(
        json!({"a":{"run":"job","needs":{"lane":1}}}),
        json!({"lane":1}),
    )
    .await;
    let p = f.project;
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE plans SET doc='{}',rev=rev+1 WHERE project_id=?",
                [p.to_string()],
            )?;
            tx.changed(Some(p), "plan");
            Ok(())
        })
        .await
        .unwrap();
    let plan = f.plan.clone();
    assert!(
        f.reads
            .snapshot(move |c| r::admit_order(c, p, &plan))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn duplicate_hold_release_and_old_generation_cannot_touch_new_hold() {
    let f = Fixture::new(json!({"a":{"run":"job"}}), json!({"lane":1})).await;
    let old = f.reserve("a", -1, needs(&[("lane", 1)])).await.unwrap();
    let first = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            tx.hold_needs(old, &needs(&[("lane", 1)]))
        })
        .await
        .unwrap();
    let second = f
        .writer
        .write(RetrySafety::Idempotent, move |tx| {
            tx.hold_needs(old, &needs(&[("lane", 1)]))
        })
        .await
        .unwrap();
    assert_eq!(first, second);
    assert!(
        f.writer
            .write(RetrySafety::Idempotent, move |tx| tx
                .hold_needs(old, &needs(&[("lane", 0)])))
            .await
            .is_err()
    );
    assert!(f.stop(old).await);
    assert!(!f.stop(old).await);
    let p = f.project;
    f.writer.write(RetrySafety::Idempotent,move |tx| { tx.sql().execute("UPDATE steps SET generation=2,work_generation=2,status='pending' WHERE project_id=?",[p.to_string()])?; tx.changed(Some(p),"status"); Ok(()) }).await.unwrap();
    let newer = RunId::new();
    let attempt = AttemptId::new();
    f.writer.write(RetrySafety::NonIdempotent,move |tx| {
        tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,work_generation,phase,request,inputs_hash,created_at) VALUES (?,?,'a',2,2,'reserved','{}','hash','now')",params![attempt.to_string(),p.to_string()])?;
        tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,work_generation,created_at) VALUES (?,?,?,'a',2,2,'now')",params![newer.to_string(),p.to_string(),attempt.to_string()])?;
        tx.hold_needs(newer,&needs(&[("lane",1)]))?;
        assert!(!tx.release_needs(old)?); Ok(())
    }).await.unwrap();
    assert_eq!(f.held().await["lane"], 1);
}

#[tokio::test]
async fn concurrent_reservations_cannot_overcommit_capacity() {
    let mut steps = serde_json::Map::new();
    for i in 0..12 {
        steps.insert(format!("s{i}"), json!({"run":"job","needs":{"lane":1}}));
    }
    let f = Fixture::new(Value::Object(steps), json!({"lane":3})).await;
    let mut requests = vec![];
    for i in 0..12 {
        let writer = f.writer.clone();
        let p = f.project;
        requests.push(tokio::spawn(async move {
            let run = RunId::new(); let attempt = AttemptId::new(); let step = format!("s{i}");
            writer.write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES (?,?,?,'reserved','{}','hash','now')", params![attempt.to_string(),p.to_string(),step])?;
                tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at) VALUES (?,?,?,?,'now')", params![run.to_string(),p.to_string(),attempt.to_string(),step])?;
                tx.hold_needs(run, &needs(&[("lane",1)]))?;
                Ok(run)
            }).await
        }));
    }
    let mut admitted = 0;
    for request in requests {
        if request.await.unwrap().is_ok() {
            admitted += 1;
        }
    }
    assert_eq!(admitted, 3);
    assert_eq!(f.held().await["lane"], 3);
    let count: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT count(*) FROM runs", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(count, 3);
}
