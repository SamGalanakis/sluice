#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use serde_json::{Value, json};
use sluice_model::{
    error::PublicError,
    ids::{AttemptId, LeaseId, ProjectId, RunId},
    plan::{FnSignature, SignatureProvider},
    types::Type,
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    resources::{self as r, Needs},
};

struct Signatures;
impl SignatureProvider for Signatures {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        let mut s = FnSignature::default();
        if name == "cap" {
            s.outputs.insert("capacity".into(), Type::Int);
        }
        Some(s)
    }
}
struct Fixture {
    _home: ScratchHome,
    writer: Writer,
    reads: ReadPool,
    project: ProjectId,
}
impl Fixture {
    async fn new(resources: Value) -> Self {
        let home = ScratchHome::new().unwrap();
        assert!(home.root().exists());
        assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
        let writer = Writer::open(home.path()).unwrap();
        let reads = ReadPool::open(home.path(), 2).unwrap();
        let project = ProjectId::new();
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "INSERT INTO projects(project_id,name,created_at) VALUES (?,'p','now')",
                    [project.to_string()],
                )?;
                tx.sql().execute(
                    "INSERT INTO plans(project_id,rev,doc) VALUES (?,1,?)",
                    params![
                        project.to_string(),
                        json!({"inputs":{},"outputs":{},"steps":{}}).to_string()
                    ],
                )?;
                r::patch_resources(tx, project, &resources, &Signatures)?;
                tx.changed(Some(project), "plan");
                Ok(())
            })
            .await
            .unwrap();
        Self {
            _home: home,
            writer,
            reads,
            project,
        }
    }
    async fn run(&self, step: &str, priority: i64) -> RunId {
        let p = self.project;
        let step = step.to_owned();
        let run = RunId::new();
        let attempt = AttemptId::new();
        self.writer.write(RetrySafety::NonIdempotent,move |tx| {
            let declaration=json!({"run":"job","priority":priority});
            tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status) VALUES (?1,?2,(SELECT count(*) FROM steps WHERE project_id=?1),?3,'running')",params![p.to_string(),step,declaration.to_string()])?;
            let doc:String=tx.sql().query_row("SELECT doc FROM plans WHERE project_id=?",[p.to_string()],|r| r.get(0))?;
            let mut doc:Value=serde_json::from_str(&doc)?; doc["steps"][&step]=declaration;
            tx.sql().execute("UPDATE plans SET doc=?2,rev=rev+1 WHERE project_id=?1",params![p.to_string(),doc.to_string()])?;
            tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,phase,request,inputs_hash,created_at) VALUES (?,?,?,'executing','{}','hash','now')",params![attempt.to_string(),p.to_string(),step])?;
            tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,created_at) VALUES (?,?,?,?,'now')",params![run.to_string(),p.to_string(),attempt.to_string(),step])?;
            tx.changed(Some(p),"status"); Ok(run)
        }).await.unwrap()
    }
    async fn request(&self, run: RunId, name: &str, amount: u64) -> Result<LeaseId, PublicError> {
        let name = name.to_owned();
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                r::request_lease(tx, run, &name, amount)
            })
            .await
    }
    async fn keyed(&self, run: RunId, amount: u64, key: &str) -> Result<LeaseId, PublicError> {
        let key = key.to_owned();
        self.writer
            .write(RetrySafety::Idempotent, move |tx| {
                r::request_lease_keyed(tx, run, "land", amount, &key)
            })
            .await
    }
    async fn grant(&self) -> Vec<LeaseId> {
        let p = self.project;
        self.writer
            .write(RetrySafety::Idempotent, move |tx| r::grant_leases(tx, p))
            .await
            .unwrap()
    }
    async fn release(&self, lease: LeaseId, run: RunId) -> bool {
        self.writer
            .write(RetrySafety::Idempotent, move |tx| {
                r::release_lease(tx, lease, run)
            })
            .await
            .unwrap()
    }

    async fn held(&self) -> u64 {
        let p = self.project;
        self.reads
            .snapshot(move |c| Ok(r::held(c, p)?.get("land").copied().unwrap_or(0)))
            .await
            .unwrap()
    }
    async fn stop(&self, run: RunId) -> bool {
        self.writer.write(RetrySafety::Idempotent,move |tx| {
            tx.sql().execute("UPDATE attempts SET phase='terminal' WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?)",[run.to_string()])?;
            tx.sql().execute("UPDATE runs SET finished_at='stopped' WHERE run_id=?",[run.to_string()])?;
            tx.changed(None,"status"); r::release_stopped_run(tx,run)
        }).await.unwrap()
    }
}

#[tokio::test]
async fn keyed_request_replay_cannot_reacquire_or_cross_run_and_payload() {
    let f = Fixture::new(json!({"land":2})).await;
    let a = f.run("a", 0).await;
    let b = f.run("b", 0).await;
    let lease = f.keyed(a, 1, "request").await.unwrap();
    assert_eq!(f.keyed(a, 1, "request").await.unwrap(), lease);
    assert!(f.keyed(b, 1, "request").await.is_err());
    assert!(f.keyed(a, 2, "request").await.is_err());
    f.grant().await;
    f.release(lease, a).await;
    assert_eq!(f.keyed(a, 1, "request").await.unwrap(), lease);
    assert_eq!(f.held().await, 0);
    assert!(f.grant().await.is_empty());
    let new = f.keyed(a, 1, "next-request").await.unwrap();
    assert!(new.0 > lease.0);
    assert_eq!(f.grant().await, [new]);
}

#[tokio::test]
async fn unknown_oversized_invalid_run_and_zero_requests_are_checked_immediately() {
    let f = Fixture::new(json!({"land":1})).await;
    let a = f.run("a", 0).await;
    assert!(f.request(a, "gpu", 1).await.is_err());
    assert!(f.request(a, "land", 2).await.is_err());
    assert!(f.request(RunId::new(), "land", 1).await.is_err());
    assert!(f.request(a, "land", u64::MAX).await.is_err());
    let owner = f.request(a, "land", 1).await.unwrap();
    f.grant().await;
    let zero = f.request(a, "land", 0).await.unwrap();
    assert_eq!(f.grant().await, [zero]);
    assert_eq!(f.held().await, 1);
    assert!(f.release(zero, a).await);
    assert_eq!(f.held().await, 1);
    f.release(owner, a).await;
    f.stop(a).await;
    assert!(f.request(a, "land", 1).await.is_err());
}

#[tokio::test]
async fn section_request_key_cannot_impersonate_a_needs_bundle() {
    let f = Fixture::new(json!({"land": 2})).await;
    let a = f.run("a", 0).await;
    let needs = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.hold_needs(a, &Needs::from([("land".into(), 1)]))
        })
        .await
        .unwrap();
    let key = format!("needs/{a}/land");
    assert!(f.keyed(a, 1, &key).await.is_err());
    assert!(!f.release(needs[0], a).await);
    assert!(!f.release(LeaseId(i64::MAX), a).await);
    assert_eq!(f.held().await, 1);
}
