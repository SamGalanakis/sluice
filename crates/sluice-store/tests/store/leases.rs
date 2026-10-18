use crate::home;
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
                    "INSERT INTO plans(project_id,rev,root_order) VALUES (?,1,?)",
                    params![
                        project.to_string(),
                        json!(["inputs", "outputs", "steps"]).to_string()
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
            tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,status,unit,paused,run,priority) VALUES (?1,?2,(SELECT count(*) FROM steps WHERE project_id=?1),?3,'running',?2,'false',json_extract(?3,'$.run'),coalesce(json_extract(?3,'$.priority'),0))",params![p.to_string(),step,declaration.to_string()])?;
            tx.sql().execute("UPDATE plans SET rev=rev+1 WHERE project_id=?1",params![p.to_string()])?;
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
async fn lease_replay_refusals_and_release_preserve_resource_ownership() {
    let f = Fixture::new(json!({"land":2})).await;
    {
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
        assert!(f.release(new, a).await);
    }
    {
        let a = f.run("zero", 0).await;
        assert!(f.request(a, "gpu", 1).await.is_err());
        assert!(f.request(a, "land", 3).await.is_err());
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
    {
        let a = f.run("needs", 0).await;
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
    f.writer.shutdown().await.unwrap();
}

#[tokio::test]
async fn direct_calls_share_home_budget_and_unbounded_live_attempts_block_admission() {
    use sluice_model::limits::{ResourcePolicy, RunLimits};
    let f = Fixture::new(json!({})).await;
    let old = f.run("old", 0).await;
    let policy = ResourcePolicy::for_host(1 << 30, 8000).unwrap();
    let p = policy.clone();
    let status = f
        .reads
        .snapshot(move |sql| r::budget_status(sql, &p))
        .await
        .unwrap();
    assert_eq!(status.unbounded_attempts.len(), 1);
    assert!(status.reason.contains("drain old runs"));
    let limits = RunLimits {
        memory_bytes: 1024,
        cpu_millis: 100,
        tasks: 2,
    };
    let p = policy.clone();
    assert!(
        !f.reads
            .snapshot(move |sql| r::budget_fit(sql, &limits, &p))
            .await
            .unwrap()
            .fits()
    );
    f.stop(old).await;
    // Direct calls have no project or step. They reserve through the same public API.
    let call = AttemptId::new();
    let p = policy.clone();
    f.writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("INSERT INTO attempts(attempt_id,phase,request,inputs_hash,created_at) VALUES (?1,'reserved','{}','x','now')", [call.to_string()])?;
        r::reserve_budget(tx, call, limits, &p)
    }).await.unwrap();
    let p = policy.clone();
    let status = f
        .reads
        .snapshot(move |sql| r::budget_status(sql, &p))
        .await
        .unwrap();
    assert!(status.unbounded_attempts.is_empty());
    assert_eq!(
        status.held.memory_bytes,
        limits.memory_bytes + policy.guardian_memory_bytes
    );
    let p = ResourcePolicy {
        total: RunLimits {
            memory_bytes: policy.total.memory_bytes + 1,
            ..policy.total
        },
        ..policy.clone()
    };
    let changed = f
        .reads
        .snapshot(move |sql| r::budget_fit(sql, &limits, &p))
        .await
        .unwrap();
    assert!(!changed.fits());
    assert!(changed.reason.contains("draining active attempts"));
    let changed_policy = ResourcePolicy {
        guardian_memory_bytes: policy.guardian_memory_bytes / 2,
        ..policy.clone()
    };
    // Stored reservations keep their actual overhead even if installation defaults change.
    assert_eq!(
        f.reads
            .snapshot(move |sql| r::budget_status(sql, &changed_policy))
            .await
            .unwrap()
            .held,
        status.held
    );
}

#[tokio::test]
async fn terminal_attempt_capacity_releases_only_after_service_proof() {
    use sluice_model::limits::{ResourcePolicy, RunLimits};
    let f = Fixture::new(json!({})).await;
    let attempt = AttemptId::new();
    let policy = ResourcePolicy::for_host(1 << 30, 8000).unwrap();
    let p = policy.clone();
    let limits = RunLimits {
        memory_bytes: 1024,
        cpu_millis: 100,
        tasks: 2,
    };
    f.writer.write(RetrySafety::NonIdempotent, move |tx| {
        tx.sql().execute("INSERT INTO attempts(attempt_id,phase,request,inputs_hash,created_at) VALUES (?1,'reserved','{}','x','now')", [attempt.to_string()])?;
        r::reserve_budget(tx, attempt, limits, &p)?;
        assert!(!r::release_budget(tx, attempt)?);
        tx.sql().execute("UPDATE attempts SET phase='terminal' WHERE attempt_id=?1", [attempt.to_string()])?;
        Ok(())
    }).await.unwrap();
    let p = policy.clone();
    let held = f
        .reads
        .snapshot(move |sql| r::budget_status(sql, &p))
        .await
        .unwrap();
    assert_eq!(
        held.held.memory_bytes,
        limits.memory_bytes + policy.guardian_memory_bytes
    );
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            assert!(r::release_budget(tx, attempt)?);
            assert!(!r::release_budget(tx, attempt)?);
            Ok(())
        })
        .await
        .unwrap();
    let released = f
        .reads
        .snapshot(move |sql| r::budget_status(sql, &policy))
        .await
        .unwrap();
    assert_eq!(released.held.memory_bytes, 0);
    f.writer.shutdown().await.unwrap();
}
