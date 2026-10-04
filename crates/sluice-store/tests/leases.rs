#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use serde_json::{Value, json};
use sluice_model::{
    commands::LeaseState,
    error::PublicError,
    ids::{AttemptId, LeaseId, ProjectId, RunId},
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
        let mut s = FnSignature::default();
        if name == "cap" {
            s.outputs.insert("capacity".into(), Type::Int);
        }
        Some(s)
    }
}
struct Fixture {
    home: ScratchHome,
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
            home,
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
    async fn leases(&self) -> Vec<r::Lease> {
        let p = self.project;
        self.reads.snapshot(move |c| r::leases(c, p)).await.unwrap()
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
    async fn patch(&self, patch: Value) -> Result<bool, PublicError> {
        let p = self.project;
        self.writer
            .write(RetrySafety::Idempotent, move |tx| {
                r::patch_resources(tx, p, &patch, &Signatures)
            })
            .await
    }
    async fn observe(&self, n: u64) {
        let p = self.project;
        self.writer
            .write(RetrySafety::Idempotent, move |tx| {
                let rev = r::declarations(tx.sql(), p)?["land"].revision;
                r::observe_capacity(tx, p, "land", rev, Ok(n))?;
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn mode(&self, mode: &str) {
        let p = self.project;
        let mode = mode.to_owned();
        self.writer
            .write(RetrySafety::Idempotent, move |tx| {
                tx.sql()
                    .execute("UPDATE maintenance SET mode=? WHERE singleton=1", [mode])?;
                tx.changed(Some(p), "status");
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn records(&self) -> Vec<Value> {
        self.reads
            .snapshot(|c| {
                let mut s =
                    c.prepare("SELECT payload FROM records WHERE kind='step.lease' ORDER BY seq")?;
                let mut out = vec![];
                for row in s.query_map([], |r| r.get::<_, String>(0))? {
                    out.push(serde_json::from_str(&row?)?);
                }
                Ok(out)
            })
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn capacity_one_is_mutual_exclusion_and_each_grant_release_is_logged_once() {
    let f = Fixture::new(json!({"land":1})).await;
    let mut requests = vec![];
    for step in ["a", "b", "c"] {
        let run = f.run(step, 0).await;
        requests.push((f.request(run, "land", 1).await.unwrap(), run));
    }
    for (lease, run) in requests {
        assert_eq!(f.grant().await, [lease]);
        for _ in 0..3 {
            assert!(f.grant().await.is_empty());
        }
        assert_eq!(f.held().await, 1);
        assert!(f.release(lease, run).await);
        assert!(!f.release(lease, run).await);
        assert_eq!(f.held().await, 0);
    }
    assert!(f.leases().await.is_empty());
    let records = f.records().await;
    assert_eq!(
        records
            .iter()
            .map(|r| r["state"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["held", "released", "held", "released", "held", "released"]
    );
}

#[tokio::test]
async fn grant_order_is_descending_priority_then_increasing_lease_id() {
    let f = Fixture::new(json!({"land":1})).await;
    let first = f.run("first", 0).await;
    let owner = f.request(first, "land", 1).await.unwrap();
    assert_eq!(f.grant().await, [owner]);
    let mut pending = vec![];
    for (step, priority) in [("low", 1), ("top", 5), ("tie", 5), ("mid", 3)] {
        let run = f.run(step, priority).await;
        pending.push((f.request(run, "land", 1).await.unwrap(), run));
    }
    let waiting = f
        .leases()
        .await
        .into_iter()
        .filter(|l| l.state == LeaseState::Waiting)
        .map(|l| l.step.unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(waiting, ["top", "tie", "mid", "low"]);
    f.release(owner, first).await;
    for i in [1, 2, 3, 0] {
        let (lease, run) = pending[i];
        assert_eq!(f.grant().await, [lease]);
        f.release(lease, run).await;
    }
}

#[tokio::test]
async fn second_holder_remains_while_first_releases_builds_and_reacquires() {
    let f = Fixture::new(json!({"land":2})).await;
    let a = f.run("a", 0).await;
    let b = f.run("b", 0).await;
    let c = f.run("c", 0).await;
    let old = f.request(a, "land", 1).await.unwrap();
    let second = f.request(b, "land", 1).await.unwrap();
    assert_eq!(f.grant().await, [old, second]);
    let waiting = f.request(c, "land", 1).await.unwrap();
    assert!(f.release(old, a).await);
    assert_eq!(f.held().await, 1);
    assert_eq!(f.grant().await, [waiting]);
    let newer = f.request(a, "land", 1).await.unwrap();
    assert!(newer.0 > old.0);
    assert!(f.grant().await.is_empty());
    assert!(!f.release(old, a).await);
    assert!(!f.release(newer, b).await);
    assert_eq!(f.held().await, 2);
    f.release(waiting, c).await;
    assert_eq!(f.grant().await, [newer]);
    let owners = f
        .leases()
        .await
        .into_iter()
        .filter(|l| l.state == LeaseState::Held)
        .map(|l| l.run)
        .collect::<Vec<_>>();
    assert_eq!(owners, [b, a]);
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
async fn cancellation_removes_waiters_but_held_capacity_requires_proven_stop() {
    let f = Fixture::new(json!({"land":1})).await;
    let a = f.run("a", 0).await;
    let b = f.run("b", 0).await;
    let owner = f.request(a, "land", 1).await.unwrap();
    f.grant().await;
    f.request(a, "land", 1).await.unwrap();
    let waiting = f.request(b, "land", 1).await.unwrap();
    f.writer.write(RetrySafety::Idempotent,move |tx| {
        tx.sql().execute("UPDATE attempts SET cancel_requested=1 WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?)",[a.to_string()])?;
        assert_eq!(r::cancel_waiting(tx,a)?,1); assert!(!r::release_stopped_run(tx,a)?); Ok(())
    }).await.unwrap();
    assert_eq!(f.held().await, 1);
    assert!(f.grant().await.is_empty());
    assert!(f.stop(a).await);
    assert!(!f.stop(a).await);
    assert!(!f.release(owner, a).await);
    assert_eq!(f.grant().await, [waiting]);
    let records = f.records().await;
    assert_eq!(records[1]["reason"], "its run ended");
}

#[tokio::test]
async fn terminal_phase_without_finished_run_never_releases_abandoned_hold() {
    let f = Fixture::new(json!({"land":1})).await;
    let a = f.run("a", 0).await;
    f.request(a, "land", 1).await.unwrap();
    f.grant().await;
    let p = f.project;
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE attempts SET phase='terminal' WHERE project_id=?",
                [p.to_string()],
            )?;
            tx.changed(Some(p), "status");
            assert!(!r::release_stopped_run(tx, a)?);
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(f.held().await, 1);
    assert!(f.stop(a).await);
    assert_eq!(f.held().await, 0);
}

#[tokio::test]
async fn needs_and_sections_share_accounting_and_sections_go_before_new_admission() {
    let f = Fixture::new(json!({"land":2})).await;
    let a = f.run("a", 0).await;
    let b = f.run("b", 0).await;
    let c = f.run("c", 0).await;
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            tx.hold_needs(a, &Needs::from([("land".into(), 1)]))?;
            Ok(())
        })
        .await
        .unwrap();
    let blease = f.request(b, "land", 1).await.unwrap();
    assert_eq!(f.grant().await, [blease]);
    assert_eq!(f.held().await, 2);
    assert!(
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| tx
                .hold_needs(c, &Needs::from([("land".into(), 1)])))
            .await
            .is_err()
    );
    let clease = f.request(c, "land", 1).await.unwrap();
    assert!(f.grant().await.is_empty());
    assert!(f.stop(a).await);
    assert_eq!(f.grant().await, [clease]);
    let p = f.project;
    assert!(
        !f.reads
            .snapshot(move |conn| r::fits(conn, p, &Needs::from([("land".into(), 1)])))
            .await
            .unwrap()
            .fits()
    );
}

#[tokio::test]
async fn status_reports_lease_identity_amount_time_priority_and_zero_step_queue() {
    let f = Fixture::new(json!({"land":1})).await;
    let a = f.run("a", 0).await;
    let b = f.run("b", 5).await;
    let owner = f.request(a, "land", 1).await.unwrap();
    f.grant().await;
    let waiting = f.request(b, "land", 1).await.unwrap();
    let p = f.project;
    let row = f
        .reads
        .snapshot(move |c| {
            let doc: String = c.query_row(
                "SELECT doc FROM plans WHERE project_id=?",
                [p.to_string()],
                |r| r.get(0),
            )?;
            let plan = Plan::parse_json(doc.as_bytes(), &Signatures).unwrap();
            Ok(r::status(c, p, &plan)?.remove("land").unwrap())
        })
        .await
        .unwrap();
    assert_eq!(row.resource.capacity, Some(1));
    assert_eq!((row.held, row.queued), (1, 0));
    assert_eq!(
        (row.holders[0].id, row.holders[0].run, row.holders[0].amount),
        (owner, a, 1)
    );
    assert!(!row.holders[0].since.is_empty());
    assert_eq!(
        (
            row.waiting[0].id,
            row.waiting[0].run,
            row.waiting[0].priority
        ),
        (waiting, b, 5)
    );
    assert!(!row.waiting[0].since.is_empty());
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
async fn removal_is_refused_for_held_and_waiting_leases_but_allowed_after_release() {
    let f = Fixture::new(json!({"land":1})).await;
    let a = f.run("a", 0).await;
    let waiting = f.request(a, "land", 1).await.unwrap();
    assert!(f.patch(json!({"land":null})).await.is_err());
    f.grant().await;
    assert!(f.patch(json!({"land":null})).await.is_err());
    f.release(waiting, a).await;
    assert!(f.patch(json!({"land":null})).await.unwrap());
    f.patch(json!({"land":1})).await.unwrap();
    let newer = f.request(a, "land", 1).await.unwrap();
    assert!(newer.0 > waiting.0);
    assert!(!f.release(waiting, a).await);
    assert_eq!(f.grant().await, [newer]);
}

#[tokio::test]
async fn adopted_grants_remain_held_after_restart() {
    let f = Fixture::new(json!({"land":1})).await;
    let a = f.run("a", 0).await;
    let b = f.run("b", 0).await;
    let owner = f.request(a, "land", 1).await.unwrap();
    f.grant().await;
    let waiting = f.request(b, "land", 1).await.unwrap();
    f.writer.shutdown().await.unwrap();
    let reopened = Writer::open(f.home.path()).unwrap();
    let p = f.project;
    assert_eq!(f.held().await, 1);
    assert!(
        reopened
            .write(RetrySafety::Idempotent, move |tx| r::grant_leases(tx, p))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !reopened
            .write(RetrySafety::Idempotent, move |tx| r::release_stopped_run(
                tx, a
            ))
            .await
            .unwrap()
    );
    reopened
        .write(RetrySafety::Idempotent, move |tx| {
            assert!(r::release_lease(tx, owner, a)?);
            assert_eq!(r::grant_leases(tx, p)?, [waiting]);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn dynamic_requests_wait_above_capacity_and_zero_grows_during_drain() {
    let f =
        Fixture::new(json!({"land":{"capacity_fn":"cap"},"unused":{"capacity_fn":"cap"}})).await;
    let a = f.run("a", 0).await;
    let lease = f.request(a, "land", 2).await.unwrap();
    assert!(f.grant().await.is_empty());
    f.observe(0).await;
    f.mode("drain").await;
    let p = f.project;
    let observed = f
        .reads
        .snapshot(move |c| r::capacity_observations(c, p))
        .await
        .unwrap();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].name, "land");
    assert!(f.grant().await.is_empty());
    f.observe(2).await;
    assert_eq!(f.grant().await, [lease]);
    f.release(lease, a).await;
    assert!(
        f.reads
            .snapshot(move |c| r::capacity_observations(c, p))
            .await
            .unwrap()
            .is_empty()
    );
    // New section callbacks remain allowed for already admitted work in drain.
    let next = f.request(a, "land", 1).await.unwrap();
    assert_eq!(f.grant().await, [next]);
    assert!(f.release(next, a).await);
}

#[tokio::test]
async fn lowered_fixed_capacity_keeps_holds_and_waiters_then_growth_grants() {
    let f = Fixture::new(json!({"land":2})).await;
    let a = f.run("a", 0).await;
    let b = f.run("b", 0).await;
    let owner = f.request(a, "land", 2).await.unwrap();
    f.grant().await;
    let waiter = f.request(b, "land", 2).await.unwrap();
    f.patch(json!({"land":0})).await.unwrap();
    assert_eq!(f.held().await, 2);
    assert!(f.grant().await.is_empty());
    f.release(owner, a).await;
    assert!(f.grant().await.is_empty());
    f.patch(json!({"land":2})).await.unwrap();
    assert_eq!(f.grant().await, [waiter]);
}

#[tokio::test]
async fn cancelling_waiter_on_tick_does_not_grant_or_log_a_release() {
    let f = Fixture::new(json!({"land":1})).await;
    let a = f.run("a", 0).await;
    f.request(a, "land", 1).await.unwrap();
    let p = f.project;
    f.writer
        .write(RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE attempts SET cancel_requested=1 WHERE project_id=?",
                [p.to_string()],
            )?;
            tx.changed(Some(p), "status");
            Ok(())
        })
        .await
        .unwrap();
    assert!(f.grant().await.is_empty());
    assert!(f.leases().await.is_empty());
    assert!(f.records().await.is_empty());
}

#[tokio::test]
async fn voluntary_exception_release_has_no_cleanup_reason_and_needs_cannot_be_released_as_section()
{
    let f = Fixture::new(json!({"land":2})).await;
    let a = f.run("a", 0).await;
    let needs = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.hold_needs(a, &Needs::from([("land".into(), 1)]))
        })
        .await
        .unwrap();
    assert!(!f.release(needs[0], a).await);
    let section = f.request(a, "land", 1).await.unwrap();
    f.grant().await;
    assert!(f.release(section, a).await);
    assert_eq!(f.held().await, 1);
    let records = f.records().await;
    assert_eq!(records.len(), 2);
    assert!(records[1]["reason"].is_null());
}

#[tokio::test]
async fn priority_edit_reorders_waiters_without_changing_fifo_identity() {
    let f = Fixture::new(json!({"land": 1})).await;
    let a = f.run("a", 1).await;
    let b = f.run("b", 0).await;
    let first = f.request(a, "land", 1).await.unwrap();
    let second = f.request(b, "land", 1).await.unwrap();
    let p = f.project;
    f.writer.write(RetrySafety::Idempotent, move |tx| {
        tx.sql().execute("UPDATE steps SET declaration=json_set(declaration,'$.priority',9) WHERE project_id=? AND step_id='b'", [p.to_string()])?;
        tx.changed(Some(p), "plan");
        Ok(())
    }).await.unwrap();
    assert_eq!(f.grant().await, [second]);
    f.release(second, b).await;
    assert_eq!(f.grant().await, [first]);
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
