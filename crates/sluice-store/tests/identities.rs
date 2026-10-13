//! Routing a run's message (SPEC §8.1): a run that names nobody, or names the retired
//! `orchestrator` as runs pinned to older releases still do, reaches its unit's owner, else
//! the project's fallback, else the person (`owner`). The stored `to` is who it reached, and
//! a run's receipt never carries a delivery note.
#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use sluice_model::{
    commands::{Message, MessagePost, MessageReceipt},
    error::PublicError,
    ids::{AttemptId, ProjectId, ProjectSelector, RunId},
};
use sluice_store::{ReadPool, RetrySafety, Writer, messages::*};

struct Fixture {
    _home: ScratchHome,
    writer: Writer,
    reads: ReadPool,
    project: ProjectId,
}
impl Fixture {
    /// Project `p` with unit `work` (step `work`) and unit `other` (step `other`), both
    /// unowned, and session `@lead` registered.
    async fn new() -> Self {
        let home = ScratchHome::new().unwrap();
        let writer = Writer::open(home.path()).unwrap();
        let project = ProjectId::new();
        writer.write(RetrySafety::NonIdempotent,move |tx| {
            tx.sql().execute("INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",[project.to_string()])?;
            tx.sql().execute("INSERT INTO plans(project_id,rev,root_order) VALUES (?1,1,'[\"steps\"]')",[project.to_string()])?;
            tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,unit,paused,run,priority) VALUES (?1,'work',0,'{\"run\":\"x\"}','work','false','x',0),(?1,'other',1,'{\"run\":\"x\"}','other','false','x',0)",[project.to_string()])?;
            tx.sql().execute("INSERT INTO readers(project_id,identity,stream,thread,cursor) VALUES (?1,'@lead','orchestrator','',0)",[project.to_string()])?;
            tx.changed(Some(project),"plan");Ok(())
        }).await.unwrap();
        let reads = ReadPool::open(home.path(), 2).unwrap();
        Self {
            _home: home,
            writer,
            reads,
            project,
        }
    }
    async fn sql(&self, sql: &'static str) {
        let p = self.project;
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(sql, [p.to_string()])?;
                tx.changed(Some(p), "plan");
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn run(&self, step: &str) -> RunId {
        let p = self.project;
        let step = step.to_owned();
        let run = RunId::new();
        let attempt = AttemptId::new();
        self.writer.write(RetrySafety::NonIdempotent,move |tx| {
            tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,item_index,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,1,0,'executing','{}','same','now')",params![attempt.to_string(),p.to_string(),step])?;
            tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,item_index,created_at) VALUES (?1,?2,?3,?4,1,0,'now')",params![run.to_string(),p.to_string(),attempt.to_string(),step])?;
            tx.changed(Some(p),"status");Ok(())
        }).await.unwrap();
        run
    }
    async fn post(&self, post: Post) -> Result<(Message, MessageReceipt), PublicError> {
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                let posted = sluice_store::messages::post(tx, post, &NoPlanInputs)?;
                Ok((posted.message, posted.receipt))
            })
            .await
    }
    async fn notifications(&self) -> i64 {
        self.reads
            .snapshot(|c| {
                Ok(
                    c.query_row("SELECT count(*) FROM notification_attempts", [], |r| {
                        r.get(0)
                    })?,
                )
            })
            .await
            .unwrap()
    }
}
fn ask(project: ProjectId, run: RunId, to: &str) -> Post {
    Post {
        project: ProjectSelector::Id(project),
        speaker: Speaker::Run(run),
        body: "which schema?".into(),
        verb: Verb::Ask {
            to: to.into(),
            title: None,
            ui: None,
            input: None,
            data: None,
        },
    }
}
fn say(project: ProjectId, run: RunId, to: &str) -> Post {
    Post {
        verb: Verb::Say {
            to: to.into(),
            data: None,
        },
        ..ask(project, run, to)
    }
}

#[tokio::test]
async fn a_runs_message_to_nobody_or_the_orchestrator_reaches_its_units_owner() {
    let f = Fixture::new().await;
    f.sql("UPDATE steps SET owner='@lead' WHERE project_id=?1 AND unit='work'")
        .await;
    let run = f.run("work").await;
    for post in [
        ask(f.project, run, ""),
        ask(f.project, run, "orchestrator"),
        say(f.project, run, ""),
        say(f.project, run, "orchestrator"),
    ] {
        let (message, receipt) = f.post(post).await.unwrap();
        assert_eq!(message.from, "work");
        assert_eq!(message.to.as_deref(), Some("@lead"));
        assert_eq!(message.thread, "step-work");
        assert_eq!(receipt.to, "@lead");
        // @lead is not reading, but a run's receipt keeps the shape older releases decode.
        let receipt = serde_json::to_value(&receipt).unwrap();
        assert!(receipt.get("note").is_none(), "{receipt}");
    }
    // A step named explicitly is still reached.
    let (message, _) = f.post(say(f.project, run, "other")).await.unwrap();
    assert_eq!(message.to.as_deref(), Some("other"));
}

#[tokio::test]
async fn an_unowned_unit_reaches_the_fallback_else_the_person() {
    let f = Fixture::new().await;
    let run = f.run("other").await;
    let (message, receipt) = f.post(ask(f.project, run, "orchestrator")).await.unwrap();
    assert_eq!(message.to.as_deref(), Some("owner"));
    assert_eq!(receipt.to, "owner");
    // A question to the person reserves its notification, as any question to the owner does.
    assert_eq!(f.notifications().await, 1);
    f.sql("UPDATE projects SET fallback='@lead' WHERE project_id=?1")
        .await;
    let (message, _) = f.post(ask(f.project, run, "")).await.unwrap();
    assert_eq!(message.to.as_deref(), Some("@lead"));
    assert_eq!(f.notifications().await, 1);
}

/// A run of an older release posts through the retired `message_post`, naming nobody or the
/// orchestrator: its unit's owner gets it.
#[tokio::test]
async fn the_retired_message_post_reaches_the_units_owner() {
    let f = Fixture::new().await;
    f.sql("UPDATE steps SET owner='@lead' WHERE project_id=?1 AND unit='work'")
        .await;
    let run = f.run("work").await;
    for to in [None, Some("orchestrator".to_owned())] {
        let post = bridge(MessagePost {
            project: ProjectSelector::Id(f.project),
            body: "stuck on the parser".into(),
            thread: None,
            to,
            needs_reply: Some(false),
            reply_to: None,
            answer: None,
            title: None,
            ui: None,
            input: None,
            data: None,
            from: None,
            run: Some(run),
            author: None,
        })
        .unwrap();
        let (message, _) = f.post(post).await.unwrap();
        assert_eq!(message.to.as_deref(), Some("@lead"));
    }
}

/// A run names a session or a step that is not there: refused, nothing stored.
#[tokio::test]
async fn a_run_naming_an_unknown_session_is_refused() {
    let f = Fixture::new().await;
    let run = f.run("work").await;
    let refused = f.post(ask(f.project, run, "@laed")).await.unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("@laed is not a session of project p"),
        "{refused}"
    );
    let (message, _) = f.post(ask(f.project, run, "@lead")).await.unwrap();
    assert_eq!(message.to.as_deref(), Some("@lead"));
}
