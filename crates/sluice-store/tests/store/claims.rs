//! Claiming and releasing units (SPEC §8.1, "Claim and release"): a compare-and-set on the
//! units' owner in one write, all or none; a unit is taken when unowned or when its owner is
//! not reading, a reading owner's only with `expected_owner`, the person's only with `force`;
//! the units' open questions follow them, and a note from `sluice` records each handoff.
//! Also a session's inbox: its own questions, never another session's.
use super::home;
use home::ScratchHome;
use rusqlite::params;
use serde_json::Value;
use sluice_model::{
    commands::{Message, MessageView},
    error::PublicError,
    identity::SessionName,
    ids::{AttemptId, MessageId, ProjectId, ProjectSelector, RunId},
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    identities::{self, Handoff},
    messages::*,
};

struct Fixture {
    _home: ScratchHome,
    writer: Writer,
    reads: ReadPool,
    project: ProjectId,
}
impl Fixture {
    /// Project `p` with unit `work` (step `work`, tagged `arc:y`) and unit `other` (step
    /// `other`, tagged `arc:y`), both unowned, no fallback; sessions `@lead` (never waited),
    /// `@gone` (last waited 30 minutes ago) and `@live` (waiting now) registered.
    async fn new() -> Self {
        let home = ScratchHome::new().unwrap();
        let writer = Writer::open(home.path()).unwrap();
        let project = ProjectId::new();
        writer.write(RetrySafety::NonIdempotent,move |tx| {
            let id = project.to_string();
            tx.sql().execute("INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",[&id])?;
            tx.sql().execute("INSERT INTO plans(project_id,rev,root_order) VALUES (?1,1,'[\"steps\"]')",[&id])?;
            tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,unit,paused,run,priority) VALUES (?1,'work',0,'{\"run\":\"x\",\"tags\":[\"arc:y\"]}','work','false','x',0),(?1,'other',1,'{\"run\":\"x\",\"tags\":[\"arc:y\"]}','other','false','x',0)",[&id])?;
            tx.sql().execute("INSERT INTO readers(project_id,identity,stream,thread,cursor,heartbeat_at) VALUES
                (?1,'@lead','orchestrator','',0,NULL),
                (?1,'@gone','orchestrator','',0,strftime('%Y-%m-%dT%H:%M:%SZ','now','-30 minutes')),
                (?1,'@live','orchestrator','',0,strftime('%Y-%m-%dT%H:%M:%SZ','now'))",[&id])?;
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
    /// A question from `run`'s step to its unit's owner (routed: `to` left out).
    async fn ask(&self, run: RunId) -> Message {
        let post = Post {
            project: ProjectSelector::Id(self.project),
            speaker: Speaker::Run(run),
            body: "which schema?".into(),
            verb: Verb::Ask {
                to: String::new(),
                title: None,
                ui: None,
                input: None,
                data: None,
            },
        };
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                Ok(sluice_store::messages::post(tx, post, &NoPlanInputs)?.message)
            })
            .await
            .unwrap()
    }
    async fn claim(
        &self,
        me: &str,
        units: &[&str],
        tags: &[&str],
        expected: Option<&str>,
        force: bool,
    ) -> Result<Handoff, PublicError> {
        let (p, me) = (self.project, SessionName::parse(me).unwrap());
        let units: Vec<String> = units.iter().map(|u| (*u).to_owned()).collect();
        let tags: Vec<String> = tags.iter().map(|t| (*t).to_owned()).collect();
        let expected = expected.map(str::to_owned);
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                let selected = identities::select_units(tx.sql(), p, None, &units, &tags, None)?;
                identities::claim(
                    tx,
                    p,
                    &me,
                    &selected,
                    expected.as_deref(),
                    force,
                    "taking over",
                )
            })
            .await
    }
    async fn release(
        &self,
        me: &str,
        unit: &str,
        to: Option<&str>,
        force: bool,
    ) -> Result<Handoff, PublicError> {
        let (p, me, unit) = (
            self.project,
            SessionName::parse(me).unwrap(),
            unit.to_owned(),
        );
        let to = to.map(str::to_owned);
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                let selected = identities::select_units(tx.sql(), p, Some(&unit), &[], &[], None)?;
                identities::release(tx, p, &me, &selected, to.as_deref(), force, "")
            })
            .await
    }
    async fn owner(&self, unit: &'static str) -> Option<String> {
        let p = self.project;
        self.reads
            .snapshot(move |sql| Ok(identities::unit_owner(sql, p, unit)?.flatten()))
            .await
            .unwrap()
    }
    async fn message(&self, id: MessageId) -> Message {
        let p = self.project;
        self.reads
            .snapshot(move |sql| message(sql, p, id))
            .await
            .unwrap()
    }
    async fn inbox(&self, identity: &'static str) -> Vec<Message> {
        let p = self.project;
        self.reads
            .snapshot(move |sql| messages(sql, p, MessageView::Inbox, None, None, identity))
            .await
            .unwrap()
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

/// The handoff note's record of who moved which unit.
fn handoff_data(note: &Message) -> Value {
    serde_json::to_value(note.data.as_ref().unwrap()).unwrap()["unit_owner"].clone()
}

/// An unowned unit, and a unit whose owner stopped reading, are taken without a question;
/// their open questions move to the claimer, later ones are routed to it, and the note from
/// `sluice` tells whoever answered the unit before.
#[tokio::test]
async fn a_claim_takes_an_unowned_or_silent_owners_unit_with_its_open_questions() {
    let f = Fixture::new().await;
    let run = f.run("work").await;
    // Unowned, no fallback: the person answers (and is notified).
    let first = f.ask(run).await;
    assert_eq!(first.to.as_deref(), Some("owner"));
    assert_eq!(f.notifications().await, 1);
    let took = f.claim("@lead", &["work"], &[], None, false).await.unwrap();
    assert_eq!(f.owner("work").await.as_deref(), Some("@lead"));
    assert_eq!(f.owner("other").await, None);
    assert_eq!(took.questions.len(), 1);
    assert_eq!(took.questions[0].id, first.id);
    assert_eq!(took.questions[0].to.as_deref(), Some("@lead"));
    assert_eq!(f.message(first.id).await.to.as_deref(), Some("@lead"));
    let note = f.message(took.notices[0]).await;
    assert_eq!(
        (note.from.as_str(), note.to.as_deref()),
        ("sluice", Some("owner"))
    );
    assert!(
        note.body
            .starts_with("@lead claimed unit work from nobody (unowned) (taking over)."),
        "{}",
        note.body
    );
    let data = handoff_data(&note);
    assert_eq!(data["by"], "@lead");
    assert_eq!(data["units"][0]["unit"], "work");
    assert_eq!(data["units"][0]["from"], Value::Null);
    assert_eq!(data["units"][0]["to"], "@lead");
    assert_eq!(data["questions"][0], first.id.0);
    // Its next question goes to its owner.
    assert_eq!(f.ask(run).await.to.as_deref(), Some("@lead"));
    // Claiming what is already yours changes nothing.
    let again = f.claim("@lead", &["work"], &[], None, false).await.unwrap();
    assert_eq!((again.units.len(), again.unchanged.len()), (0, 1));

    // @lead hands it to @gone, who has not waited in 30 minutes: @live takes it without asking,
    // and the questions follow.
    f.release("@lead", "work", Some("@gone"), false)
        .await
        .unwrap();
    assert_eq!(f.message(first.id).await.to.as_deref(), Some("@gone"));
    let took = f.claim("@live", &["work"], &[], None, false).await.unwrap();
    assert_eq!(
        (took.units[0].from.as_deref(), took.units[0].to.as_deref()),
        (Some("@gone"), Some("@live"))
    );
    assert_eq!(took.questions.len(), 2);
    assert_eq!(f.message(first.id).await.to.as_deref(), Some("@live"));
    // The note goes to the session that lost it.
    assert_eq!(
        f.message(took.notices[0]).await.to.as_deref(),
        Some("@gone")
    );

    // The owner now reads: takeover needs compare-and-set; a person needs force.
    let refused = f
        .claim("@lead", &["work"], &[], None, false)
        .await
        .unwrap_err();
    let PublicError::Conflict { message, .. } = &refused else {
        panic!("{refused:?}")
    };
    assert!(
        message.contains("unit work is owned by @live, which is reading (last heartbeat 20"),
        "{message}"
    );
    assert!(message.contains("expected_owner=\"@live\""), "{message}");
    assert_eq!(f.owner("work").await.as_deref(), Some("@live"));
    // The compare-and-set: the owner must be the one expected.
    let stale = f
        .claim("@lead", &["work"], &[], Some("@gone"), false)
        .await
        .unwrap_err();
    assert!(
        stale
            .to_string()
            .contains("unit work is owned by @live, not @gone"),
        "{stale}"
    );
    let stale = f
        .claim("@lead", &["work"], &[], Some("unowned"), false)
        .await
        .unwrap_err();
    assert!(
        stale.to_string().contains("not nobody (unowned)"),
        "{stale}"
    );
    f.claim("@lead", &["work"], &[], Some("@live"), false)
        .await
        .unwrap();
    assert_eq!(f.owner("work").await.as_deref(), Some("@lead"));

    // The person's unit.
    f.sql("UPDATE steps SET owner='owner' WHERE project_id=?1 AND unit='other'")
        .await;
    let refused = f
        .claim("@lead", &["other"], &[], Some("owner"), false)
        .await
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("owned by the person (owner): pass force"),
        "{refused}"
    );
    let took = f.claim("@lead", &["other"], &[], None, true).await.unwrap();
    assert_eq!(f.owner("other").await.as_deref(), Some("@lead"));
    // Recorded, to the person.
    assert_eq!(
        f.message(took.notices[0]).await.to.as_deref(),
        Some("owner")
    );
}

/// By tag or pattern a claim takes every unit it selects, or none: one unit a reading
/// session holds refuses the whole claim. Selecting nothing is refused.
#[tokio::test]
async fn a_claim_by_tag_is_all_or_none() {
    let f = Fixture::new().await;
    f.sql("UPDATE steps SET owner='@live' WHERE project_id=?1 AND unit='work'")
        .await;
    let refused = f
        .claim("@lead", &[], &["arc:y"], None, false)
        .await
        .unwrap_err();
    assert!(
        refused.to_string().starts_with("nothing changed"),
        "{refused}"
    );
    assert_eq!(f.owner("other").await, None);
    f.sql("UPDATE steps SET owner=NULL WHERE project_id=?1")
        .await;
    let took = f
        .claim("@lead", &[], &["arc:y"], None, false)
        .await
        .unwrap();
    assert_eq!(
        took.units
            .iter()
            .map(|c| c.unit.as_str())
            .collect::<Vec<_>>(),
        ["other", "work"]
    );
    // One note for both, to the person who answered them.
    assert_eq!(took.notices.len(), 1);
    let refused = f
        .claim("@lead", &["nope-*"], &[], None, false)
        .await
        .unwrap_err();
    assert!(
        matches!(refused, PublicError::NotFound { .. }),
        "{refused:?}"
    );
    let refused = f.claim("@lead", &[], &[], None, false).await.unwrap_err();
    assert!(
        refused.to_string().contains("select the units"),
        "{refused}"
    );
    // Patterns.
    f.sql("UPDATE steps SET owner=NULL WHERE project_id=?1")
        .await;
    let took = f.claim("@lead", &["w*"], &[], None, false).await.unwrap();
    assert_eq!(took.units.len(), 1);
    assert_eq!(f.owner("other").await, None);
}

/// A release hands the unit on; released to nobody, the project's fallback answers it: its open
/// questions and later ones go there. Only the owner releases, unless it forces.
#[tokio::test]
async fn release_to_unowned_routes_to_the_fallback() {
    let f = Fixture::new().await;
    f.sql("UPDATE projects SET fallback='@gone' WHERE project_id=?1")
        .await;
    f.claim("@lead", &["work"], &[], None, false).await.unwrap();
    let run = f.run("work").await;
    let open = f.ask(run).await;
    assert_eq!(open.to.as_deref(), Some("@lead"));
    let refused = f.release("@live", "work", None, false).await.unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("unit work is owned by @lead, not you"),
        "{refused}"
    );
    let released = f.release("@lead", "work", None, false).await.unwrap();
    assert_eq!(f.owner("work").await, None);
    assert_eq!(f.message(open.id).await.to.as_deref(), Some("@gone"));
    let note = f.message(released.notices[0]).await;
    assert_eq!(note.to.as_deref(), Some("@gone"));
    assert!(
        note.body
            .contains("unowned now, so the fallback @gone answers"),
        "{}",
        note.body
    );
    assert_eq!(f.ask(run).await.to.as_deref(), Some("@gone"));
    // To an unknown session: refused.
    let refused = f
        .release("@lead", "other", Some("@laed"), true)
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("@laed is not a session"),
        "{refused}"
    );
}

/// A session's inbox is its own: questions to it (and those sent to the retired orchestrator
/// before sessions named themselves), never another session's.
#[tokio::test]
async fn a_sessions_inbox_is_its_own() {
    let f = Fixture::new().await;
    f.sql("UPDATE steps SET owner='@lead' WHERE project_id=?1 AND unit='work'")
        .await;
    f.sql("UPDATE steps SET owner='@live' WHERE project_id=?1 AND unit='other'")
        .await;
    let to_lead = f.ask(f.run("work").await).await;
    let to_live = f.ask(f.run("other").await).await;
    let p = f.project;
    let legacy = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.sql().execute(
                "INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,needs_reply,at) VALUES (999999,?1,'owner','work','orchestrator','old question',1,'now')",
                [p.to_string()],
            )?;
            tx.changed(Some(p), "messages");
            Ok(MessageId(999999))
        })
        .await
        .unwrap();
    let ids = |messages: Vec<Message>| messages.iter().map(|m| m.id).collect::<Vec<_>>();
    // Open questions first, by thread: the legacy one lives on `owner`.
    assert_eq!(ids(f.inbox("@lead").await), [legacy, to_lead.id]);
    assert_eq!(ids(f.inbox("@live").await), [legacy, to_live.id]);
    // The legacy inbox, read without a name, is what was addressed to orchestrator.
    assert_eq!(ids(f.inbox("orchestrator").await), [legacy]);
}

/// A stopped session's units are taken by who holds them: `owned_by` selects only the units it
/// owns now (alone, all of them), so another session's units in the same pattern are left
/// alone, and the owner it names is the one expected.
#[tokio::test]
async fn owned_by_takes_a_stopped_sessions_units_only() {
    let f = Fixture::new().await;
    f.sql("UPDATE steps SET owner='@gone' WHERE project_id=?1 AND unit='work'")
        .await;
    f.sql("UPDATE steps SET owner='@live' WHERE project_id=?1 AND unit='other'")
        .await;
    let p = f.project;
    let take = |owned_by: &'static str, units: Vec<String>| {
        let me = SessionName::parse("@lead").unwrap();
        f.writer.write(RetrySafety::NonIdempotent, move |tx| {
            let selected =
                identities::select_units(tx.sql(), p, None, &units, &[], Some(owned_by))?;
            identities::claim(tx, p, &me, &selected, Some(owned_by), false, "")
        })
    };
    let missing = take("@lead", vec![]).await.unwrap_err();
    assert!(
        missing
            .to_string()
            .contains("no unit selected is owned by @lead"),
        "{missing}"
    );
    let took = take("@gone", vec!["*".into()]).await.unwrap();
    assert_eq!(
        took.units
            .iter()
            .map(|c| c.unit.as_str())
            .collect::<Vec<_>>(),
        ["work"]
    );
    assert_eq!(f.owner("work").await.as_deref(), Some("@lead"));
    assert_eq!(f.owner("other").await.as_deref(), Some("@live"));
}
