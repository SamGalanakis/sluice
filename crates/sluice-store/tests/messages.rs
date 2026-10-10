#[path = "../../../tests/support/home.rs"]
mod home;
use home::ScratchHome;
use rusqlite::params;
use serde_json::{Value, json};
use sluice_model::{
    commands::{Message, MessageAnswer, MessageVerb, QuestionState},
    error::PublicError,
    events::Event,
    ids::{AttemptId, MessageId, ProjectId, ProjectSelector, Revision, RunId},
    types::{Type, check_value_at},
};
use sluice_store::{ReadPool, Result, RetrySafety, WriteTransaction, Writer, messages::*};

struct Fixture {
    _home: ScratchHome,
    writer: Writer,
    reads: ReadPool,
    project: ProjectId,
}
impl Fixture {
    async fn new() -> Self {
        let home = ScratchHome::new().unwrap();
        assert!(home.root().exists());
        assert_eq!(ScratchHome::validate(home.path()).unwrap(), home.path());
        let writer = Writer::open(home.path()).unwrap();
        let project = ProjectId::new();
        writer.write(RetrySafety::NonIdempotent,move |tx| {
            tx.sql().execute("INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",[project.to_string()])?;
            tx.sql().execute("INSERT INTO plans(project_id,rev,root_order) VALUES (?1,1,'[\"steps\"]')",[project.to_string()])?;
            tx.sql().execute("INSERT INTO steps(project_id,step_id,position,declaration,unit,paused,run,priority) VALUES (?1,'work',0,'{\"run\":\"x\"}','work','false','x',0),(?1,'other',1,'{\"run\":\"x\"}','other','false','x',0)",[project.to_string()])?;
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
    async fn run(&self, step: &str, item: i64, generation: i64, prev: Option<RunId>) -> RunId {
        let p = self.project;
        let step = step.to_owned();
        let run = RunId::new();
        let attempt = AttemptId::new();
        self.writer.write(RetrySafety::NonIdempotent,move |tx| {
            tx.sql().execute("INSERT INTO attempts(attempt_id,project_id,step_id,generation,item_index,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,?4,?5,'executing','{}','same','now')",params![attempt.to_string(),p.to_string(),step,generation,item])?;
            tx.sql().execute("INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,item_index,prev_run,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'now')",params![run.to_string(),p.to_string(),attempt.to_string(),step,generation,item,prev.map(|r|r.to_string())])?;
            tx.changed(Some(p),"status");Ok(())
        }).await.unwrap();
        run
    }
    async fn stop(&self, run: RunId) {
        let p = self.project;
        self.writer.write(RetrySafety::NonIdempotent,move |tx| {
            tx.sql().execute("UPDATE attempts SET phase='terminal' WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?1)",[run.to_string()])?;
            tx.sql().execute("UPDATE runs SET finished_at='now' WHERE run_id=?1",[run.to_string()])?;
            tx.changed(Some(p),"status");Ok(())
        }).await.unwrap();
    }
    async fn ask(&self, run: RunId, title: &str) -> AskResult {
        let post = asking(self.project, "question")
            .title(title)
            .speaker(Speaker::Run(run));
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                ask_waiting(tx, post, &TestInputs)
            })
            .await
            .unwrap()
    }
    async fn post(&self, post: Post) -> std::result::Result<Message, PublicError> {
        self.posted(post).await.map(|p| p.message)
    }
    async fn posted(&self, post: Post) -> std::result::Result<Posted, PublicError> {
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                sluice_store::messages::post(tx, post, &TestInputs)
            })
            .await
    }

    async fn question(&self, id: MessageId) -> Question {
        let p = self.project;
        self.reads
            .snapshot(move |sql| question(sql, p, id))
            .await
            .unwrap()
    }

    async fn input(&self, typ: Value) {
        let p = self.project;
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "INSERT INTO inputs(project_id,name,position,declaration) VALUES (?1,'n',0,?2)",
                    params![p.to_string(), typ.to_string()],
                )?;
                tx.changed(Some(p), "plan");
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn count(&self, table: &'static str) -> i64 {
        self.reads
            .snapshot(move |c| {
                Ok(c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
            })
            .await
            .unwrap()
    }
}
/// The orchestrator tells the owner something.
fn draft(project: ProjectId, body: &str) -> Post {
    Post {
        project: ProjectSelector::Id(project),
        speaker: Speaker::Orchestrator,
        body: body.into(),
        verb: Verb::Say {
            to: "owner".into(),
            data: None,
        },
    }
}
/// The orchestrator asks the owner.
fn asking(project: ProjectId, body: &str) -> Post {
    Post {
        verb: Verb::Ask {
            to: "owner".into(),
            title: None,
            ui: None,
            input: None,
            data: None,
        },
        ..draft(project, body)
    }
}
/// The owner replies.
fn reply(project: ProjectId, id: MessageId, body: &str) -> Post {
    Post {
        speaker: Speaker::Owner,
        verb: Verb::Reply {
            to_message: id,
            answer: None,
        },
        ..draft(project, body)
    }
}
fn answer(project: ProjectId, id: MessageId, value: Value) -> Post {
    reply(project, id, "").answer(MessageAnswer {
        action: "submit".into(),
        params: Some(serde_json::from_value(json!({"value":7})).unwrap()),
        values: Some(serde_json::from_value(json!({"value":value})).unwrap()),
    })
}
trait Draft {
    fn to(self, to: &str) -> Self;
    fn speaker(self, speaker: Speaker) -> Self;
    fn input(self, input: &str) -> Self;
    fn title(self, title: &str) -> Self;

    fn answer(self, answer: MessageAnswer) -> Self;
}
impl Draft for Post {
    fn to(mut self, recipient: &str) -> Self {
        match &mut self.verb {
            Verb::Ask { to, .. } | Verb::Say { to, .. } => *to = recipient.into(),
            Verb::Reply { .. } => panic!("a reply's recipient is derived"),
        }
        self
    }
    fn speaker(mut self, speaker: Speaker) -> Self {
        self.speaker = speaker;
        self
    }
    fn input(mut self, name: &str) -> Self {
        if let Verb::Ask { input, .. } = &mut self.verb {
            *input = Some(name.into());
        }
        self
    }
    fn title(mut self, text: &str) -> Self {
        if let Verb::Ask { title, .. } = &mut self.verb {
            *title = Some(text.into());
        }
        self
    }

    fn answer(mut self, given: MessageAnswer) -> Self {
        if let Verb::Reply { answer, .. } = &mut self.verb {
            *answer = Some(given);
        }
        self
    }
}

struct TestInputs;
impl PlanInputSetter for TestInputs {
    fn set_input(&self, tx: &mut WriteTransaction<'_>, update: InputAnswer<'_>) -> Result<()> {
        let raw: String = tx.sql().query_row(
            "SELECT declaration FROM inputs WHERE project_id=?1 AND name=?2",
            params![update.project.to_string(), update.name],
            |r| r.get(0),
        )?;
        let typ: Type = serde_json::from_str(&raw)?;
        check_value_at(
            &typ,
            update.value.as_value(),
            &format!("inputs.{}", update.name),
        )
        .map_err(|errors| PublicError::Invalid {
            message: "answer does not fit".into(),
            errors: errors.into_iter().map(|e| e.to_string()).collect(),
        })?;
        tx.sql().execute(
            "UPDATE inputs SET value=?1,generation=generation+1 WHERE project_id=?2 AND name=?3",
            params![
                serde_json::to_string(update.value)?,
                update.project.to_string(),
                update.name
            ],
        )?;
        tx.sql().execute(
            "UPDATE plans SET rev=rev+1 WHERE project_id=?1",
            [update.project.to_string()],
        )?;
        let rev: i64 = tx.sql().query_row(
            "SELECT rev FROM plans WHERE project_id=?1",
            [update.project.to_string()],
            |r| r.get(0),
        )?;
        tx.append_record(
            Some(update.project),
            Event::PlanInput {
                rev: Revision(rev as u64),
                author: update.author.into(),
                reason: update.reason.into(),
                name: update.name.into(),
                value: update.value.clone(),
            },
        )?;
        tx.changed(Some(update.project), "plan");
        Ok(())
    }
}

#[tokio::test]
async fn plain_body_and_params_fallback_set_input_and_name_the_answering_author() {
    let f = Fixture::new().await;
    f.input(json!("string")).await;
    for structured in [false, true] {
        let q = f
            .post(asking(f.project, "q").input("n").title("Which word?"))
            .await
            .unwrap();
        let mut r = reply(f.project, q.id, "body");
        if structured {
            r = r.answer(MessageAnswer {
                action: "submit".into(),
                values: None,
                params: Some(serde_json::from_value(json!({"value":"param"})).unwrap()),
            });
        }
        f.post(r).await.unwrap();
        let payload: String = f
            .reads
            .snapshot(|c| {
                Ok(c.query_row(
                    "SELECT payload FROM records WHERE kind='plan.input' ORDER BY seq DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap();
        let event: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(event["author"], "owner");
        assert_eq!(event["reason"], format!("message {}: Which word?", q.id.0));
        assert_eq!(event["value"], if structured { "param" } else { "body" });
    }
}

#[tokio::test]
async fn errors_after_the_setter_wrote_still_roll_back_the_whole_transaction() {
    struct Fault;
    impl PlanInputSetter for Fault {
        fn set_input(&self, tx: &mut WriteTransaction<'_>, u: InputAnswer<'_>) -> Result<()> {
            TestInputs.set_input(tx, u)?;
            Err(PublicError::Invalid {
                message: "fault after write".into(),
                errors: vec![],
            }
            .into())
        }
    }
    let f = Fixture::new().await;
    f.input(json!("int")).await;
    let q = f.post(asking(f.project, "q").input("n")).await.unwrap();
    let r = answer(f.project, q.id, json!(4));
    assert!(
        f.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                sluice_store::messages::post(tx, r, &Fault)
            })
            .await
            .is_err()
    );
    assert_eq!(f.count("records").await, 2);
    assert_eq!(f.question(q.id).await.state, QuestionState::Open);
    let rev: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT rev FROM plans", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(rev, 1);
}

#[tokio::test]
async fn simultaneous_ui_answers_set_input_only_once() {
    let f = Fixture::new().await;
    f.input(json!("int")).await;
    let q = f.post(asking(f.project, "q").input("n")).await.unwrap();
    let (a, b) = tokio::join!(
        f.post(answer(f.project, q.id, json!(1))),
        f.post(answer(f.project, q.id, json!(2)))
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(f.count("messages").await, 2);
    let rev: i64 = f
        .reads
        .snapshot(|c| Ok(c.query_row("SELECT rev FROM plans", [], |r| r.get(0))?))
        .await
        .unwrap();
    assert_eq!(rev, 2);
}

#[tokio::test]
async fn rows_and_records_stored_before_the_verbs_read_in_the_current_shape() {
    let f = Fixture::new().await;
    let p = f.project;
    let read = f
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            let project = p.to_string();
            tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",body,needs_reply,at) VALUES (1,?1,'m1','cli',NULL,'old question',1,'then'),(2,?1,'m1','owner','cli','old reply',0,'then'),(3,?1,'m1','cli','owner','old note',0,'then')",[&project])?;
            tx.sql().execute("UPDATE messages SET reply_to=1 WHERE id=2", [])?;
            tx.sql().execute("UPDATE messages SET resolved_by=2 WHERE id=1", [])?;
            tx.changed(Some(p), "messages");
            let legacy = json!({"kind":"message","id":2,"thread":"m1","from":"owner","to":"cli","title":null,"body":"old reply","needs_reply":false,"reply_to":1,"answer":null,"ui":null,"input":null,"data":null,"run":null,"posted_at":"then","claimed_by":null});
            let event: Event = serde_json::from_value(legacy).unwrap();
            Ok((
                message(tx.sql(), p, MessageId(1))?,
                message(tx.sql(), p, MessageId(2))?,
                message(tx.sql(), p, MessageId(3))?,
                event,
            ))
        })
        .await
        .unwrap();
    let (question, reply, note, event) = read;
    assert_eq!(question.verb, MessageVerb::Ask);
    assert_eq!(question.state, Some(QuestionState::Answered));
    assert_eq!(question.answered_by, Some(MessageId(2)));
    assert_eq!(reply.verb, MessageVerb::Reply);
    assert_eq!(reply.to_message, Some(MessageId(1)));
    assert_eq!(note.verb, MessageVerb::Say);
    assert_eq!(note.state, None);
    let Event::Message(legacy) = event else {
        panic!("message record")
    };
    assert_eq!(legacy.verb, MessageVerb::Reply);
    assert_eq!(legacy.to_message, Some(MessageId(1)));
    let fields: Vec<String> = serde_json::to_value(Event::Message(legacy))
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert!(
        !fields
            .iter()
            .any(|k| k == "needs_reply" || k == "reply_to" || k == "claimed_by")
    );
}
#[tokio::test]
async fn open_questions_of_a_settled_step_can_be_answered_or_closed() {
    let f = Fixture::new().await;
    let p = f.project;
    let run = f.run("work", -1, 1, None).await;
    let q = f
        .post(asking(p, "q").title("Title").speaker(Speaker::Run(run)))
        .await
        .unwrap();
    let other = f
        .post(asking(p, "other").title("Other").speaker(Speaker::Run(run)))
        .await
        .unwrap();
    f.stop(run).await;
    let settled = |status: &'static str| {
        let w = f.writer.clone();
        async move {
            w.write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "UPDATE steps SET status=?2 WHERE project_id=?1 AND step_id='work'",
                    params![p.to_string(), status],
                )?;
                tx.changed(Some(p), "status");
                Ok(())
            })
            .await
            .unwrap()
        }
    };
    let refused = |result: std::result::Result<Posted, PublicError>| {
        assert!(
            matches!(&result, Err(PublicError::Conflict { message, .. }) if message.contains("step work is settled")),
            "{result:?}"
        );
    };
    for status in ["succeeded", "failed", "stale", "skipped"] {
        settled(status).await;
        let before = f.count("messages").await;
        refused(f.posted(draft(p, "hi").to("work")).await);
        refused(f.posted(asking(p, "why").to("work")).await);
        assert_eq!(f.count("messages").await, before, "{status}");
    }
    settled("succeeded").await;
    let answered = f.posted(reply(p, q.id, "yes")).await.unwrap();
    assert_eq!(answered.receipt.to, "work");
    assert_eq!(f.question(q.id).await.state, QuestionState::Answered);
    let before = f.count("messages").await;
    refused(f.posted(reply(p, q.id, "again")).await);
    assert_eq!(f.count("messages").await, before);
    let close = reply(p, other.id, "").answer(MessageAnswer {
        action: "close".into(),
        params: None,
        values: None,
    });
    assert_eq!(f.posted(close).await.unwrap().receipt.to, "work");
    assert_eq!(f.question(other.id).await.state, QuestionState::Closed);
    // The step's retry asks again and takes up the answer.
    let retry = f.run("work", -1, 1, Some(run)).await;
    match f.ask(retry, "Title").await {
        AskResult::Answered { question, reply } => {
            assert_eq!(question.id, q.id);
            assert_eq!(reply.id, answered.message.id);
        }
        _ => panic!("expected the answer"),
    }
}
