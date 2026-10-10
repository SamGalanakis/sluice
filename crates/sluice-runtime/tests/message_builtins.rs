//! message.ask / say / reply / wait (and the retired message.post) builtin tests over a
//! real scratch home.
//! The fns park on durable-cursor subscriptions while the store keeps truth:
//! first answering reply, closes, take-up lineage and answer claims.
#[allow(dead_code)]
#[path = "../../../tests/support/messages.rs"]
mod stored_messages;
use serde_json::{Value, json};
use sluice_model::{
    commands::{Message, MessageAnswer, MessageVerb, MessageView},
    ids::{AttemptId, MessageId, ProjectId, ProjectSelector, RunId, StepId},
    rpc::{JsonMap, decode_json},
};
use sluice_runtime::builtins::{
    jev::FnFailure,
    messages::{self as fns, MessageCtx},
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    messages::{self, NoPlanInputs, Post, Question, Speaker, Verb},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use stored_messages::{Stored, stored};
use tokio_util::sync::CancellationToken;

fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn id(name: &str) -> StepId {
    name.parse().unwrap()
}
fn out(map: &JsonMap, name: &str) -> Value {
    map.0.get(name).unwrap().as_value().clone()
}

struct Scratch {
    dir: PathBuf,
}
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("sluice-p403-msg-{}", RunId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn path(&self) -> &Path {
        &self.dir
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Fixture {
    _home: Scratch,
    writer: Writer,
    reads: ReadPool,
    project: ProjectId,
}
impl Fixture {
    async fn new() -> Self {
        let home = Scratch::new();
        let writer = Writer::open(home.path()).unwrap();
        let project = ProjectId::new();
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
                    (project.to_string(),),
                )?;
                tx.sql().execute(
                    "INSERT INTO plans(project_id,rev,root_order) VALUES (?1,1,'[\"steps\"]')",
                    (project.to_string(),),
                )?;
                tx.sql().execute(
                    "INSERT INTO steps(project_id,step_id,position,declaration,unit,run,paused,priority)
                     VALUES (?1,'work',0,'{\"run\":\"test.fn\"}','work','test.fn','false',0)",
                    (project.to_string(),),
                )?;
                tx.changed(Some(project), "plan");
                Ok(())
            })
            .await
            .unwrap();
        let reads = ReadPool::open(home.path(), 2).unwrap();
        Self {
            _home: home,
            writer,
            reads,
            project,
        }
    }
    fn ctx(&self, run: Option<RunId>, cancel: CancellationToken) -> MessageCtx {
        MessageCtx {
            project: self.project,
            step: Some(id("work")),
            run,
            writer: self.writer.clone(),
            reads: self.reads.clone(),
            plan_inputs: Arc::new(NoPlanInputs),
            cancel,
            mutation_guard: None,
        }
    }
    /// A live executing attempt+run pair, like the scheduler would have started.
    async fn run(&self, step: &str, item: i64, generation: i64, prev: Option<RunId>) -> RunId {
        let project = self.project;
        let step = step.to_owned();
        let run = RunId::new();
        let attempt = AttemptId::new();
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "INSERT INTO attempts(attempt_id,project_id,step_id,generation,item_index,phase,request,inputs_hash,created_at) VALUES (?1,?2,?3,?4,?5,'executing','{}','same','now')",
                    (attempt.to_string(), project.to_string(), step.clone(), generation, item),
                )?;
                tx.sql().execute(
                    "INSERT INTO runs(run_id,project_id,attempt_id,step_id,generation,item_index,prev_run,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'now')",
                    (run.to_string(), project.to_string(), attempt.to_string(), step, generation, item, prev.map(|r| r.to_string())),
                )?;
                tx.changed(Some(project), "status");
                Ok(())
            })
            .await
            .unwrap();
        run
    }
    async fn stop(&self, run: RunId) {
        let project = self.project;
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "UPDATE attempts SET phase='terminal' WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?1)",
                    (run.to_string(),),
                )?;
                tx.sql().execute(
                    "UPDATE runs SET finished_at='now' WHERE run_id=?1",
                    (run.to_string(),),
                )?;
                tx.changed(Some(project), "status");
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn post(&self, post: Post) -> Message {
        self.writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                messages::post(tx, post, &NoPlanInputs).map(|p| p.message)
            })
            .await
            .unwrap()
    }
    /// The owner replies to a question, which answers it.
    async fn answer(&self, parent: MessageId, body: &str) -> Message {
        self.post(Post {
            project: ProjectSelector::Id(self.project),
            speaker: Speaker::Owner,
            body: body.into(),
            verb: Verb::Reply {
                to_message: parent,
                answer: None,
            },
        })
        .await
    }
    /// The owner closes a question without an answer.
    async fn close(&self, parent: MessageId) -> Message {
        self.post(Post {
            project: ProjectSelector::Id(self.project),
            speaker: Speaker::Owner,
            body: String::new(),
            verb: Verb::Reply {
                to_message: parent,
                answer: Some(MessageAnswer {
                    action: "close".into(),
                    params: None,
                    values: None,
                }),
            },
        })
        .await
    }
    async fn claimed(&self, reply: MessageId) -> Option<String> {
        self.reads
            .snapshot(move |sql| {
                Ok(sql.query_row(
                    "SELECT claimed_by FROM messages WHERE id=?1",
                    (reply.0,),
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    }
    async fn question(&self, id: MessageId) -> Question {
        let project = self.project;
        self.reads
            .snapshot(move |sql| messages::question(sql, project, id))
            .await
            .unwrap()
    }
    async fn thread(&self, thread: &str) -> Vec<Message> {
        let project = self.project;
        let thread = thread.to_owned();
        self.reads
            .snapshot(move |sql| {
                messages::messages(
                    sql,
                    project,
                    MessageView::Thread,
                    Some(&thread),
                    None,
                    "test",
                )
            })
            .await
            .unwrap()
    }
    /// Poll until the step's waiting ask has committed its question row.
    async fn asked_question(&self, run: RunId) -> Message {
        let project = self.project;
        for _ in 0..100 {
            let found = self
                .reads
                .snapshot(move |sql| {
                    Ok(sql
                        .query_row(
                            "SELECT id FROM messages WHERE project_id=?1 AND run_id=?2 AND needs_reply=1",
                            (project.to_string(), run.to_string()),
                            |r| r.get::<_, i64>(0),
                        )
                        .ok())
                })
                .await
                .unwrap();
            if let Some(id) = found {
                return self
                    .reads
                    .snapshot(move |sql| messages::message(sql, project, MessageId(id)))
                    .await
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("run {run} never posted its question");
    }
    async fn settle(
        &self,
        handle: tokio::task::JoinHandle<Result<JsonMap, FnFailure>>,
    ) -> Result<JsonMap, FnFailure> {
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("fn never returned")
            .expect("fn task panicked")
    }
}

fn ask_inputs(body: &str, title: &str) -> Value {
    json!({"to": "owner", "body": body, "title": title, "wait": true})
}

#[tokio::test]
async fn ask_say_and_reply_commit_the_message_and_return_its_receipt() {
    let f = Fixture::new().await;
    let run = f.run("work", -1, 1, None).await;
    let ctx = f.ctx(Some(run), CancellationToken::new());
    let said = fns::dispatch(
        "message.say",
        &map(json!({"to": "orchestrator", "body": "progress note"})),
        &ctx,
    )
    .await
    .unwrap();
    let posted = &f.thread("step-work").await[0];
    assert_eq!(out(&said, "id"), json!(posted.id.0));
    assert!(!said.0.contains_key("reply"));
    assert_eq!(
        out(&said, "receipt"),
        json!({"id": posted.id.0, "to": "orchestrator", "thread": "step-work", "delivery": "delivered"})
    );
    assert_eq!(
        (posted.verb, posted.from.as_str()),
        (MessageVerb::Say, "work")
    );
    let asked = fns::dispatch(
        "message.ask",
        &map(json!({"to": "owner", "body": "q", "title": "Pick"})),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(out(&asked, "reply"), Value::Null);
    let id = out(&asked, "id").as_i64().unwrap();
    // The orchestrator (a fn called with no run) replies, to the asking step.
    let ctx = f.ctx(None, CancellationToken::new());
    let replied = fns::dispatch(
        "message.reply",
        &map(json!({"to_message": id, "body": "that one"})),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(out(&replied, "receipt")["to"], json!("work"));
    assert_eq!(out(&replied, "receipt")["thread"], json!("step-work"));
    assert_eq!(
        f.question(MessageId(id)).await.state,
        messages::QuestionState::Answered
    );
    for (name, inputs) in [
        ("message.say", json!({"to": "nobody", "body": "x"})),
        ("message.ask", json!({"body": "x"})),
        ("message.reply", json!({"body": "x"})),
    ] {
        assert!(
            matches!(
                fns::dispatch(name, &map(inputs), &ctx).await,
                Err(FnFailure::Terminal(_))
            ),
            "{name}"
        );
    }
    assert_eq!(f.thread("step-work").await.len(), 3);
}

#[tokio::test]
async fn retired_message_post_still_runs_through_the_bridge_and_only_for_a_run() {
    let f = Fixture::new().await;
    let ctx = f.ctx(None, CancellationToken::new());
    match fns::dispatch(
        "message.post",
        &map(json!({"body": "x", "needs_reply": false, "thread": "t"})),
        &ctx,
    )
    .await
    {
        Err(FnFailure::Terminal(message)) => assert!(message.contains("retired"), "{message}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let run = f.run("work", -1, 1, None).await;
    let ctx = f.ctx(Some(run), CancellationToken::new());
    let note = fns::dispatch(
        "message.post",
        &map(json!({"body": "progress", "needs_reply": false, "thread": "ignored", "from": "x"})),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(out(&note, "reply"), Value::Null);
    assert!(!note.0.contains_key("receipt"));
    fns::dispatch("message.post", &map(json!({"body": "q"})), &ctx)
        .await
        .unwrap();
    let thread = f.thread("step-work").await;
    assert_eq!(out(&note, "id"), json!(thread[0].id.0));
    // A note to nobody goes to the orchestrator; a root post is a question.
    assert_eq!(
        (
            thread[0].verb,
            thread[0].to.as_deref(),
            thread[0].from.as_str()
        ),
        (MessageVerb::Say, Some("orchestrator"), "work")
    );
    assert_eq!(thread[1].verb, MessageVerb::Ask);
}
#[tokio::test]
async fn waiting_post_parks_then_claims_the_first_answering_reply() {
    let f = Fixture::new().await;
    let run = f.run("work", -1, 1, None).await;
    let ctx = f.ctx(Some(run), CancellationToken::new());
    let handle = tokio::spawn(async move {
        fns::dispatch("message.ask", &map(ask_inputs("which one?", "Pick")), &ctx).await
    });
    let question = f.asked_question(run).await;
    assert_eq!(question.thread, "step-work");
    assert!(f.question(question.id).await.waiting);
    let reply = f.answer(question.id, "yes, that one").await;
    let result = f.settle(handle).await.unwrap();
    assert_eq!(out(&result, "id"), json!(question.id.0));
    let got = out(&result, "reply");
    assert_eq!(got["id"], json!(reply.id.0));
    assert_eq!(got["body"], json!("yes, that one"));
    assert_eq!(got["verb"], json!("reply"));
    assert_eq!(out(&result, "receipt")["to"], json!("owner"));
    let answer = f.question(question.id).await.reply.unwrap();
    assert_eq!(f.claimed(answer.id).await, Some(run.to_string()));
}

#[tokio::test]
async fn waiting_post_fails_terminally_when_the_question_closes() {
    let f = Fixture::new().await;
    let run = f.run("work", -1, 1, None).await;
    let ctx = f.ctx(Some(run), CancellationToken::new());
    let handle = tokio::spawn(async move {
        fns::dispatch("message.ask", &map(ask_inputs("which?", "Pick")), &ctx).await
    });
    let question = f.asked_question(run).await;
    f.close(question.id).await;
    match f.settle(handle).await {
        Err(FnFailure::Terminal(message)) => assert_eq!(message, "question closed"),
        other => panic!("expected the closed question to fail, got {other:?}"),
    }
}

#[tokio::test]
async fn a_stopped_run_ends_the_wait_with_the_stopped_reason() {
    let f = Fixture::new().await;
    let run = f.run("work", -1, 1, None).await;
    let ctx = f.ctx(Some(run), CancellationToken::new());
    let handle = tokio::spawn(async move {
        fns::dispatch("message.ask", &map(ask_inputs("which?", "Pick")), &ctx).await
    });
    let question = f.asked_question(run).await;
    f.stop(run).await;
    assert!(!f.question(question.id).await.waiting);
    match f.settle(handle).await {
        Err(FnFailure::Terminal(message)) => assert!(message.contains("work"), "{message}"),
        other => panic!("expected the stopped run to fail the wait, got {other:?}"),
    }
}

#[tokio::test]
async fn a_retry_takes_up_the_open_question_and_claims_its_answer() {
    let f = Fixture::new().await;
    let run = f.run("work", -1, 1, None).await;
    let ctx = f.ctx(Some(run), CancellationToken::new());
    let first = tokio::spawn(async move {
        fns::dispatch("message.ask", &map(ask_inputs("which?", "Pick")), &ctx).await
    });
    let question = f.asked_question(run).await;
    f.stop(run).await;
    assert!(matches!(f.settle(first).await, Err(FnFailure::Terminal(_))));
    // The retry adopts the earlier question instead of posting a new one.
    let retry = f.run("work", -1, 1, Some(run)).await;
    let ctx = f.ctx(Some(retry), CancellationToken::new());
    let second = tokio::spawn(async move {
        fns::dispatch("message.ask", &map(ask_inputs("which?", "Pick")), &ctx).await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(f.question(question.id).await.waiting);
    assert_eq!(f.thread("step-work").await.len(), 1);
    let reply = f.answer(question.id, "the answer").await;
    let result = f.settle(second).await.unwrap();
    assert_eq!(out(&result, "id"), json!(question.id.0));
    assert_eq!(out(&result, "reply")["id"], json!(reply.id.0));
    assert_eq!(f.claimed(reply.id).await, Some(retry.to_string()));
}

#[tokio::test]
async fn a_claimed_answer_is_never_reused_by_a_later_retry() {
    let f = Fixture::new().await;
    let run = f.run("work", -1, 1, None).await;
    let ctx = f.ctx(Some(run), CancellationToken::new());
    let handle = tokio::spawn(async move {
        fns::dispatch("message.ask", &map(ask_inputs("which?", "Pick")), &ctx).await
    });
    let question = f.asked_question(run).await;
    f.answer(question.id, "first answer").await;
    let result = f.settle(handle).await.unwrap();
    let claimed = out(&result, "reply")["id"].as_i64().unwrap();
    f.stop(run).await;
    // A later retry in the same lineage cannot take the claimed answer back up:
    // it posts a fresh question, which the owner answers anew.
    let retry = f.run("work", -1, 1, Some(run)).await;
    let ctx = f.ctx(Some(retry), CancellationToken::new());
    let handle = tokio::spawn(async move {
        fns::dispatch("message.ask", &map(ask_inputs("which?", "Pick")), &ctx).await
    });
    let next = f.asked_question(retry).await;
    assert_ne!(next.id, question.id);
    let reply = f.answer(next.id, "second answer").await;
    let result = f.settle(handle).await.unwrap();
    assert_eq!(out(&result, "id"), json!(next.id.0));
    assert_eq!(out(&result, "reply")["id"], json!(reply.id.0));
    assert_ne!(reply.id.0, claimed);
}

#[tokio::test]
async fn waiting_post_requires_a_run_and_waiting_cancels_through_the_token() {
    let f = Fixture::new().await;
    let ctx = f.ctx(None, CancellationToken::new());
    match fns::dispatch("message.ask", &map(ask_inputs("q", "T")), &ctx).await {
        Err(FnFailure::Terminal(message)) => assert!(message.contains("run"), "{message}"),
        other => panic!("expected a terminal refusal, got {other:?}"),
    }
    let run = f.run("work", -1, 1, None).await;
    let cancel = CancellationToken::new();
    let ctx = f.ctx(Some(run), cancel.clone());
    let handle = tokio::spawn(async move {
        fns::dispatch("message.ask", &map(ask_inputs("q", "T")), &ctx).await
    });
    f.asked_question(run).await;
    cancel.cancel();
    match f.settle(handle).await {
        Err(FnFailure::Terminal(message)) => assert_eq!(message, "cancelled"),
        other => panic!("expected cancellation, got {other:?}"),
    }
}

#[tokio::test]
async fn message_wait_returns_thread_messages_after_since_and_filters_to() {
    let f = Fixture::new().await;
    let m1 = stored(
        &f.writer,
        f.project,
        Stored {
            thread: "t",
            from: "owner",
            to: Some("me"),
            body: "to me",
            question: false,
            ..Stored::default()
        },
    )
    .await;
    let m2 = stored(
        &f.writer,
        f.project,
        Stored {
            thread: "t",
            from: "owner",
            to: Some("other"),
            body: "to other",
            question: false,
            ..Stored::default()
        },
    )
    .await;
    let m3 = stored(
        &f.writer,
        f.project,
        Stored {
            thread: "t",
            from: "owner",
            to: None,
            body: "broadcast",
            question: false,
            ..Stored::default()
        },
    )
    .await;
    let m4 = stored(
        &f.writer,
        f.project,
        Stored {
            thread: "elsewhere",
            from: "owner",
            to: Some("me"),
            body: "other thread",
            question: false,
            ..Stored::default()
        },
    )
    .await;
    let ctx = f.ctx(None, CancellationToken::new());
    let result = fns::dispatch(
        "message.wait",
        &map(json!({"thread": "t", "since": 0, "to": "me", "timeout": 5})),
        &ctx,
    )
    .await
    .unwrap();
    let ids: Vec<i64> = out(&result, "messages")
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    // `to` means addressed to it or to nobody; other threads never appear.
    assert_eq!(ids, vec![m1.id.0, m3.id.0]);
    // last_seq is the log's high water, not the last returned message.
    assert_eq!(out(&result, "last_seq"), json!(m4.id.0));
    let ctx = f.ctx(None, CancellationToken::new());
    let later = fns::dispatch(
        "message.wait",
        &map(json!({"thread": "t", "since": m1.id.0, "timeout": 5})),
        &ctx,
    )
    .await
    .unwrap();
    let ids: Vec<i64> = out(&later, "messages")
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![m2.id.0, m3.id.0]);
}

#[tokio::test]
async fn message_wait_questions_hold_through_notes_until_a_question_lands() {
    let f = Fixture::new().await;
    let ctx = f.ctx(None, CancellationToken::new());
    let handle = tokio::spawn(async move {
        fns::dispatch(
            "message.wait",
            &map(json!({"thread": "t", "wake": "questions", "timeout": 60})),
            &ctx,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let note = stored(
        &f.writer,
        f.project,
        Stored {
            thread: "t",
            from: "owner",
            to: None,
            body: "just a note",
            question: false,
            ..Stored::default()
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!handle.is_finished(), "a note must not wake questions mode");
    let question = stored(
        &f.writer,
        f.project,
        Stored {
            thread: "t",
            from: "owner",
            to: None,
            body: "a real question",
            question: true,
            ..Stored::default()
        },
    )
    .await;
    let result = f.settle(handle).await.unwrap();
    let ids: Vec<i64> = out(&result, "messages")
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    // Notes keep riding with the question that wakes the wait.
    assert_eq!(ids, vec![note.id.0, question.id.0]);
}

#[tokio::test]
async fn message_wait_returns_at_the_timeout_with_everything_it_saw() {
    let f = Fixture::new().await;
    let ctx = f.ctx(None, CancellationToken::new());
    let result = fns::dispatch(
        "message.wait",
        &map(json!({"thread": "quiet", "timeout": 1})),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(out(&result, "messages"), json!([]));
    assert!(out(&result, "last_seq").as_i64().unwrap() >= 0);
    // Under wake "questions", accumulated notes still come back at the timeout.
    let ctx = f.ctx(None, CancellationToken::new());
    let handle = tokio::spawn(async move {
        fns::dispatch(
            "message.wait",
            &map(json!({"thread": "t2", "wake": "questions", "timeout": 1})),
            &ctx,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let note = stored(
        &f.writer,
        f.project,
        Stored {
            thread: "t2",
            from: "owner",
            to: None,
            body: "note only",
            question: false,
            ..Stored::default()
        },
    )
    .await;
    let result = f.settle(handle).await.unwrap();
    let ids: Vec<i64> = out(&result, "messages")
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![note.id.0]);
}

#[tokio::test]
async fn message_wait_wakes_on_any_post_by_default_and_cancels() {
    let f = Fixture::new().await;
    let ctx = f.ctx(None, CancellationToken::new());
    let handle = tokio::spawn(async move {
        fns::dispatch(
            "message.wait",
            &map(json!({"thread": "t", "timeout": 60})),
            &ctx,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let posted = stored(
        &f.writer,
        f.project,
        Stored {
            thread: "t",
            from: "owner",
            to: None,
            body: "a note wakes any-mode",
            question: false,
            ..Stored::default()
        },
    )
    .await;
    let result = f.settle(handle).await.unwrap();
    assert_eq!(out(&result, "messages").as_array().unwrap().len(), 1);
    assert_eq!(
        out(&result, "messages")[0]["id"].as_i64().unwrap(),
        posted.id.0
    );
    // Cancellation ends a parked wait.
    let cancel = CancellationToken::new();
    let ctx = f.ctx(None, cancel.clone());
    let handle = tokio::spawn(async move {
        fns::dispatch(
            "message.wait",
            &map(json!({"thread": "never", "timeout": 60})),
            &ctx,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel.cancel();
    match f.settle(handle).await {
        Err(FnFailure::Terminal(message)) => assert_eq!(message, "cancelled"),
        other => panic!("expected cancellation, got {other:?}"),
    }
}

#[tokio::test]
async fn bad_inputs_fail_terminally_before_any_wait() {
    let f = Fixture::new().await;
    let ctx = f.ctx(None, CancellationToken::new());
    for (name, inputs, want) in [
        ("message.wait", json!({}), "must be a string"),
        (
            "message.wait",
            json!({"thread": "t", "wake": "notes"}),
            "expected one of any, questions",
        ),
        (
            "message.wait",
            json!({"thread": "t", "timeout": -1}),
            "nonnegative",
        ),
        (
            "message.wait",
            json!({"thread": "t", "since": -2, "timeout": 5}),
            "nonnegative",
        ),
        (
            "message.post",
            json!({"body": "x", "wait": true, "needs_reply": false}),
            "question",
        ),
        ("message.post", json!({"wait": true}), "must be a string"),
        ("message.ask", json!({"wait": true}), "must be a string"),
        ("message.reply", json!({"body": "x"}), "to_message"),
        ("message.where", json!({}), "unknown message builtin"),
    ] {
        match fns::dispatch(name, &map(inputs.clone()), &ctx).await {
            Err(FnFailure::Terminal(message)) => {
                assert!(message.contains(want), "{name} {inputs}: {message}")
            }
            other => panic!("{name} {inputs}: expected terminal failure, got {other:?}"),
        }
    }
}
