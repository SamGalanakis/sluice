//! Durable messages, answer ownership, delivery reservations, and independent readers.

use crate::{Result, StoreError, WriteTransaction};
use rusqlite::{Connection, OptionalExtension, params};
use sluice_model::{
    commands::{MarkRead, Message, MessagePost, MessageView},
    error::PublicError,
    events::{Event, NotificationOutcome},
    ids::{AttemptId, MessageId, ProjectId, ProjectSelector, RecordSeq, RunId},
    rpc::JsonValue,
};

pub const OWNER_STREAM: &str = "owner";
pub const ORCHESTRATOR_STREAM: &str = "orchestrator";

/// The plans owner implements this by calling its synchronous input setter.
/// It must validate the value and write the plan/input projections, authored record,
/// revision and invalidations in this transaction, or return an error.
pub trait PlanInputSetter {
    fn set_input(&self, tx: &mut WriteTransaction<'_>, update: InputAnswer<'_>) -> Result<()>;
}
#[derive(Debug)]
pub struct InputAnswer<'a> {
    pub project: ProjectId,
    pub name: &'a str,
    pub value: &'a JsonValue,
    pub author: &'a str,
    pub reason: &'a str,
}
/// Refuses input-bearing answers when no plans adapter has been composed.
pub struct NoPlanInputs;
impl PlanInputSetter for NoPlanInputs {
    fn set_input(&self, _: &mut WriteTransaction<'_>, _: InputAnswer<'_>) -> Result<()> {
        Err(invalid("input answers require a plans input setter"))
    }
}

fn invalid(message: impl Into<String>) -> StoreError {
    let message = message.into();
    PublicError::Invalid {
        errors: vec![message.clone()],
        message,
    }
    .into()
}
fn missing(message: impl Into<String>) -> StoreError {
    PublicError::NotFound {
        message: message.into(),
    }
    .into()
}
fn conflict(message: impl Into<String>) -> StoreError {
    PublicError::Conflict {
        message: message.into(),
        current_rev: None,
    }
    .into()
}
fn now() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| StoreError::InvalidDatabase(e.to_string()))
}

pub fn resolve_project(sql: &Connection, selector: &ProjectSelector) -> Result<ProjectId> {
    let (column, value) = match selector {
        ProjectSelector::Id(id) => ("project_id", id.to_string()),
        ProjectSelector::Name(name) => ("name", name.to_string()),
    };
    let id: String = sql
        .query_row(
            &format!("SELECT project_id FROM projects WHERE {column}=?1 AND deleted_at IS NULL"),
            [value],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| missing(format!("no project {selector}")))?;
    id.parse()
        .map_err(|e| StoreError::InvalidDatabase(format!("invalid stored project: {e}")))
}

// A JSON projection shares the strict model decoder without a second message wire shape.
const MESSAGE_JSON: &str = "json_object('id',id,'thread',thread,'from',\"from\",'to',\"to\",'title',title,'body',body,'needs_reply',json(CASE needs_reply WHEN 1 THEN 'true' ELSE 'false' END),'reply_to',reply_to,'answer',json(answer),'ui',ui,'input',input,'data',json(data),'run',run_id,'at',at,'claimed_by',claimed_by)";

pub fn message(sql: &Connection, project: ProjectId, id: MessageId) -> Result<Message> {
    let json: String = sql
        .query_row(
            &format!("SELECT {MESSAGE_JSON} FROM messages WHERE project_id=?1 AND id=?2"),
            params![project.to_string(), id.0],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| missing(format!("no message {}", id.0)))?;
    Ok(serde_json::from_str(&json)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionState {
    Open,
    Answered,
    Closed,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub message: Message,
    pub state: QuestionState,
    pub reply: Option<Message>,
    pub waiting: bool,
    pub stopped: Option<String>,
}

pub fn question(sql: &Connection, project: ProjectId, id: MessageId) -> Result<Question> {
    let msg = message(sql, project, id)?;
    if !msg.needs_reply {
        return Err(invalid("message is not a question"));
    }
    let reply_id: Option<i64> = sql.query_row("SELECT id FROM messages WHERE project_id=?1 AND reply_to=?2 AND (needs_reply=0 OR answer IS NOT NULL) ORDER BY id LIMIT 1",
        params![project.to_string(),id.0], |r| r.get(0)).optional()?;
    let reply = reply_id
        .map(|id| message(sql, project, MessageId(id)))
        .transpose()?;
    let state = match reply.as_ref() {
        None => QuestionState::Open,
        Some(reply) if reply.answer.as_ref().is_some_and(|a| a.action == "close") => {
            QuestionState::Closed
        }
        Some(_) => QuestionState::Answered,
    };
    let attached: Option<String> = sql.query_row("SELECT run_id FROM question_attachments WHERE project_id=?1 AND message_id=?2 AND detached_at IS NULL",
        params![project.to_string(),id.0], |r| r.get(0)).optional()?;
    let asking = attached.or_else(|| msg.run.map(|r| r.to_string()));
    let (waiting, stopped) = if let Some(run) = asking {
        let live: Option<(Option<String>,String,bool)> = sql.query_row("SELECT r.finished_at,a.phase,a.cancel_requested FROM runs r JOIN attempts a ON a.attempt_id=r.attempt_id WHERE r.project_id=?1 AND r.run_id=?2",
            params![project.to_string(),run], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        match live {
            Some((None, phase, false)) if phase != "terminal" => (true, None),
            Some((_, _, true)) => (false, Some("asking run is being cancelled".into())),
            Some(_) => (false, Some(stopped_reason(sql, project, &run)?)),
            None => (false, Some("asking run is no longer present".into())),
        }
    } else {
        (false, None)
    };
    Ok(Question {
        message: msg,
        state,
        reply,
        waiting,
        stopped,
    })
}

fn stopped_reason(sql: &Connection, project: ProjectId, run: &str) -> Result<String> {
    let step: Option<String> = sql.query_row(
        "SELECT step_id FROM runs WHERE project_id=?1 AND run_id=?2",
        params![project.to_string(), run],
        |r| r.get(0),
    )?;
    if let Some(step) = step {
        let current: Option<(String, Option<String>)> = sql
            .query_row(
                "SELECT status,error FROM steps WHERE project_id=?1 AND step_id=?2",
                params![project.to_string(), step],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        return Ok(match current {
            None => format!("{step} is not in the plan"),
            Some((status, _)) if status == "running" => format!("{step} is running another run"),
            Some((status, error)) => {
                let cancelled = error
                    .as_deref()
                    .map(serde_json::from_str::<serde_json::Value>)
                    .transpose()?
                    .is_some_and(|e| e.get("error").and_then(|v| v.as_str()) == Some("cancelled"));
                format!(
                    "{step} is {}",
                    if cancelled { "cancelled" } else { &status }
                )
            }
        });
    }
    let status: Option<String> = sql
        .query_row(
            "SELECT status FROM calls WHERE project_id=?1 AND run_id=?2",
            params![project.to_string(), run],
            |r| r.get(0),
        )
        .optional()?;
    Ok(status.map_or_else(
        || "asking run has stopped".into(),
        |status| format!("call {run} is {status}"),
    ))
}

fn changed(tx: &mut WriteTransaction<'_>, project: ProjectId) {
    for view in ["messages", "questions", "status"] {
        tx.changed(Some(project), view);
    }
}

/// Posting and its record, first-answer resolution, optional plan input and notify
/// reservation all commit together. Root messages default to questions; replies
/// default to notes. Propagate errors out of Writer::write.
pub fn message_post(
    tx: &mut WriteTransaction<'_>,
    post: MessagePost,
    inputs: &impl PlanInputSetter,
) -> Result<Message> {
    let project = resolve_project(tx.sql(), &post.project)?;
    if post.title.as_ref().is_some_and(|s| s.trim().is_empty()) {
        return Err(invalid("title must not be blank"));
    }
    if post.thread.as_ref().is_some_and(|s| s.trim().is_empty()) {
        return Err(invalid("thread must not be blank"));
    }
    if post.answer.is_some() && post.reply_to.is_none() {
        return Err(invalid("answer requires reply_to"));
    }
    if post
        .answer
        .as_ref()
        .is_some_and(|a| a.action.trim().is_empty())
    {
        return Err(invalid("answer.action must not be blank"));
    }
    if let Some(input) = &post.input {
        let exists: bool = tx.sql().query_row(
            "SELECT EXISTS(SELECT 1 FROM inputs WHERE project_id=?1 AND name=?2)",
            params![project.to_string(), input],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(missing(format!("no plan input {input}")));
        }
    }
    let parent = post
        .reply_to
        .map(|id| message(tx.sql(), project, id))
        .transpose()?;
    if let Some(parent) = &parent {
        if post.thread.as_ref().is_some_and(|t| t != &parent.thread) {
            return Err(invalid("reply thread differs from parent thread"));
        }
        if post.answer.is_some() {
            if !parent.needs_reply {
                return Err(invalid("answer must reply to a question"));
            }
            if question(tx.sql(), project, parent.id)?.state != QuestionState::Open {
                return Err(conflict("question is no longer open"));
            }
        }
    }
    let needs_reply = post.needs_reply.unwrap_or(parent.is_none());
    let resolving = parent
        .as_ref()
        .filter(|p| p.needs_reply && (!needs_reply || post.answer.is_some()))
        .map(|p| question(tx.sql(), project, p.id))
        .transpose()?
        .filter(|q| q.state == QuestionState::Open);
    let from = post
        .from
        .filter(|s| !s.trim().is_empty())
        .or_else(|| post.author.filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "cli".into());
    let posting_run = post
        .run
        .map(|run| run_info(tx.sql(), project, run))
        .transpose()?;
    if let Some(q) = &resolving
        && !post.answer.as_ref().is_some_and(|a| a.action == "close")
        && let Some(name) = &q.message.input
    {
        // Present JSON null wins over all later fields, just like any other value.
        let structured = post.answer.as_ref().and_then(|a| {
            a.values
                .as_ref()
                .and_then(|v| v.0.get("value"))
                .or_else(|| a.params.as_ref().and_then(|v| v.0.get("value")))
        });
        let value = match structured {
            Some(value) => value.clone(),
            None if post.answer.is_some() && post.body.is_empty() => {
                return Err(invalid(
                    "answer needs values.value, params.value or a reply body",
                ));
            }
            None => JsonValue::try_from(serde_json::Value::String(post.body.clone()))?,
        };
        let reason = format!(
            "message {}: {}",
            q.message.id.0,
            q.message.title.as_deref().unwrap_or("")
        );
        inputs.set_input(
            tx,
            InputAnswer {
                project,
                name,
                value: &value,
                author: &from,
                reason: &reason,
            },
        )?;
    }
    let mut msg = Message {
        id: MessageId(0),
        thread: parent
            .as_ref()
            .map(|p| p.thread.clone())
            .or(post.thread)
            .or_else(|| {
                posting_run
                    .as_ref()
                    .and_then(|r| r.step.as_ref())
                    .map(|step| format!("step-{step}"))
            })
            .unwrap_or_default(),
        from,
        to: post.to.or_else(|| parent.as_ref().map(|p| p.from.clone())),
        title: post.title,
        body: post.body,
        needs_reply,
        reply_to: post.reply_to,
        answer: post.answer,
        ui: post.ui,
        input: post.input,
        data: post.data,
        run: post.run,
        at: now()?,
        claimed_by: None,
    };
    let record = tx.append_record(Some(project), Event::Message(Box::new(msg.clone())))?;
    msg.id = MessageId(record.seq.0);
    msg.at = record.at;
    if msg.thread.is_empty() {
        msg.thread = format!("m{}", msg.id.0);
    }
    // The writer allocates the id. Finalize the generated thread and event timestamp.
    tx.sql().execute(
        "UPDATE records SET thread=?1,payload=?2 WHERE seq=?3",
        params![
            msg.thread,
            serde_json::to_string(&Event::Message(Box::new(msg.clone())))?,
            msg.id.0
        ],
    )?;
    tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",title,body,needs_reply,reply_to,answer,ui,input,data,run_id,at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![msg.id.0,project.to_string(),msg.thread,msg.from,msg.to,msg.title,msg.body,msg.needs_reply,msg.reply_to.map(|id| id.0),
        msg.answer.as_ref().map(serde_json::to_string).transpose()?,msg.ui,msg.input,msg.data.as_ref().map(serde_json::to_string).transpose()?,msg.run.map(|id| id.to_string()),msg.at])?;
    if let Some(q) = resolving {
        tx.sql().execute("UPDATE messages SET resolved_by=?1,closed_at=?2 WHERE project_id=?3 AND id=?4 AND resolved_by IS NULL",
            params![msg.id.0,msg.answer.as_ref().filter(|a| a.action=="close").map(|_| msg.at.clone()),project.to_string(),q.message.id.0])?;
    }
    if msg.needs_reply {
        if let Some(run) = msg.run {
            let info = run_info(tx.sql(), project, run)?;
            if info.step.is_some() {
                attach(
                    tx,
                    project,
                    msg.id,
                    run,
                    &info,
                    msg.title.as_deref().unwrap_or(""),
                )?;
            }
        }
        if msg.to.as_deref() == Some("owner") {
            tx.sql().execute("INSERT INTO notification_attempts(project_id,message_id,attempt_id,outcome,reserved_at) VALUES (?1,?2,?3,'reserved',?4)",
                params![project.to_string(),msg.id.0,AttemptId::new().to_string(),msg.at])?;
            tx.append_record(
                Some(project),
                Event::ProjectNotify {
                    message: msg.id,
                    outcome: NotificationOutcome::Reserved,
                    error: None,
                },
            )?;
        }
    }
    changed(tx, project);
    Ok(msg)
}

#[derive(Debug)]
struct RunInfo {
    step: Option<String>,
    generation: i64,
    item: i64,
    work: i64,
    live: bool,
}
fn run_info(sql: &Connection, project: ProjectId, run: RunId) -> Result<RunInfo> {
    sql.query_row("SELECT r.step_id,r.generation,r.item_index,r.work_generation,r.finished_at IS NULL AND a.phase!='terminal' AND a.cancel_requested=0 FROM runs r JOIN attempts a ON a.attempt_id=r.attempt_id WHERE r.project_id=?1 AND r.run_id=?2",
        params![project.to_string(),run.to_string()],|r| Ok(RunInfo { step:r.get(0)?,generation:r.get(1)?,item:r.get(2)?,work:r.get(3)?,live:r.get(4)? })).optional()?.ok_or_else(|| missing("no run in this project"))
}
fn attach(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: MessageId,
    run: RunId,
    info: &RunInfo,
    title: &str,
) -> Result<()> {
    let step = info
        .step
        .as_deref()
        .ok_or_else(|| invalid("question attachment requires a step run"))?;
    let at = now()?;
    tx.sql().execute("UPDATE question_attachments SET detached_at=?1 WHERE project_id=?2 AND message_id=?3 AND detached_at IS NULL AND run_id!=?4",
        params![at,project.to_string(),id.0,run.to_string()])?;
    tx.sql().execute("INSERT INTO question_attachments(project_id,message_id,run_id,step_id,generation,item_index,title,attached_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(project_id,message_id,run_id) DO UPDATE SET detached_at=NULL",
        params![project.to_string(),id.0,run.to_string(),step,info.generation,info.item,title,at])?;
    tx.sql().execute(
        "UPDATE messages SET run_id=?1 WHERE project_id=?2 AND id=?3",
        params![run.to_string(), project.to_string(), id.0],
    )?;
    changed(tx, project);
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum AskResult {
    Waiting(Message),
    Answered {
        question: Message,
        reply: Box<Message>,
    },
    Closed(Message),
}

/// message.post(wait=true) takes up only the latest question in this exact item lineage.
pub fn ask(
    tx: &mut WriteTransaction<'_>,
    post: MessagePost,
    inputs: &impl PlanInputSetter,
) -> Result<AskResult> {
    let project = resolve_project(tx.sql(), &post.project)?;
    let run = post
        .run
        .ok_or_else(|| invalid("waiting ask requires a run"))?;
    let info = run_info(tx.sql(), project, run)?;
    if !info.live {
        return Err(conflict("asking run has stopped"));
    }
    let step = info
        .step
        .as_deref()
        .ok_or_else(|| invalid("take-up requires a step run"))?;
    let title = post
        .title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| invalid("waiting ask requires a title"))?;
    let prior: Option<i64>=tx.sql().query_row("SELECT q.message_id FROM question_attachments q JOIN messages m ON m.project_id=q.project_id AND m.id=q.message_id WHERE q.project_id=?1 AND q.step_id=?2 AND q.generation=?3 AND q.item_index=?4 AND q.title=?5 GROUP BY q.message_id ORDER BY q.message_id DESC LIMIT 1",
        params![project.to_string(),step,info.generation,info.item,title],|r| r.get(0)).optional()?;
    if let Some(id) = prior {
        let q = question(tx.sql(), project, MessageId(id))?;
        match q.state {
            QuestionState::Open => {
                let attached:Option<String>=tx.sql().query_row("SELECT run_id FROM question_attachments WHERE project_id=?1 AND message_id=?2 AND detached_at IS NULL",params![project.to_string(),id],|r|r.get(0)).optional()?;
                if !q.waiting || attached.as_deref() == Some(&run.to_string()) {
                    attach(tx, project, q.message.id, run, &info, title)?;
                    return Ok(AskResult::Waiting(message(
                        tx.sql(),
                        project,
                        q.message.id,
                    )?));
                }
            }
            QuestionState::Answered if q.reply.as_ref().is_some_and(|r| r.claimed_by.is_none()) => {
                attach(tx, project, q.message.id, run, &info, title)?;
                let reply = claim_answer(tx, project, q.message.id, run)?;
                return Ok(AskResult::Answered {
                    question: message(tx.sql(), project, q.message.id)?,
                    reply: Box::new(reply),
                });
            }
            QuestionState::Closed => {}
            _ => {}
        }
    }
    let mut post = post;
    post.needs_reply = Some(true);
    if post.thread.is_none() && post.reply_to.is_none() {
        post.thread = Some(format!("step-{step}"));
    }
    if post.from.as_ref().is_none_or(|s| s.trim().is_empty()) {
        post.from = Some(step.to_owned());
    }
    Ok(AskResult::Waiting(message_post(tx, post, inputs)?))
}

/// Claim only for the current attached asker. Repeated acknowledgements by that
/// same run are idempotent; another run can never consume the same answer.
pub fn claim_answer(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: MessageId,
    run: RunId,
) -> Result<Message> {
    let info = run_info(tx.sql(), project, run)?;
    if !info.live {
        return Err(conflict("claiming run has stopped"));
    }
    let q = question(tx.sql(), project, id)?;
    if q.state != QuestionState::Answered {
        return Err(conflict("question has no answer"));
    }
    let attached:Option<String>=tx.sql().query_row("SELECT run_id FROM question_attachments WHERE project_id=?1 AND message_id=?2 AND detached_at IS NULL",params![project.to_string(),id.0],|r|r.get(0)).optional()?;
    if attached.as_deref() != Some(&run.to_string()) {
        return Err(conflict("answer belongs to another asking run"));
    }
    let mut reply = q.reply.ok_or_else(|| conflict("question has no answer"))?;
    if reply.claimed_by.is_some_and(|owner| owner != run) {
        return Err(conflict("answer already claimed"));
    }
    tx.sql().execute(
        "UPDATE messages SET claimed_by=?1 WHERE project_id=?2 AND id=?3 AND claimed_by IS NULL",
        params![run.to_string(), project.to_string(), reply.id.0],
    )?;
    reply.claimed_by = Some(run);
    changed(tx, project);
    Ok(reply)
}

/// Questions lead the inbox, followed by unread owner notes grouped by thread.
/// History includes every message in any conversation the owner participated in.
pub fn messages(
    sql: &Connection,
    project: ProjectId,
    view: MessageView,
    thread: Option<&str>,
    since: Option<MessageId>,
    identity: &str,
) -> Result<Vec<Message>> {
    if matches!(view, MessageView::Thread) && thread.is_none() {
        return Err(invalid("thread view requires a thread"));
    }
    if since.is_some_and(|id| id.0 < 0) {
        return Err(invalid("message cursor must be nonnegative"));
    }
    let open = "needs_reply=1 AND resolved_by IS NULL AND closed_at IS NULL";
    let condition=match view {
        MessageView::Inbox => format!("\"to\"='owner' AND (({open}) OR (needs_reply=0 AND id>coalesce((SELECT cursor FROM readers r WHERE r.project_id=messages.project_id AND r.identity=?4 AND r.stream='owner' AND r.thread=messages.thread),0)))"),
        MessageView::Questions=>open.into(),
        MessageView::History=>"thread IN (SELECT thread FROM messages WHERE project_id=?1 AND (\"to\"='owner' OR \"from\"='owner'))".into(),
        MessageView::Thread=>"1".into(),
    };
    let order = if matches!(view, MessageView::Inbox) {
        "needs_reply DESC,thread,id"
    } else {
        "id"
    };
    let query = format!(
        "SELECT {MESSAGE_JSON} FROM messages WHERE project_id=?1 AND (?2 IS NULL OR thread=?2) AND id>?3 AND ({condition}) AND ?4 IS NOT NULL ORDER BY {order}"
    );
    let mut stmt = sql.prepare(&query)?;
    let json = stmt
        .query_map(
            params![
                project.to_string(),
                thread,
                since.map_or(0, |id| id.0),
                identity
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    json.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(StoreError::from))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedRange {
    pub after: MessageId,
    pub through: MessageId,
    pub messages: Vec<MessageId>,
}

fn range(
    sql: &Connection,
    project: ProjectId,
    step: &str,
    after: i64,
    through: Option<i64>,
) -> Result<AssignedRange> {
    let end = through.unwrap_or(sql.query_row(
        "SELECT coalesce(max(id),?3) FROM messages WHERE project_id=?1 AND \"to\"=?2 AND id>?3",
        params![project.to_string(), step, after],
        |r| r.get(0),
    )?);
    if after < 0 || end < after {
        return Err(invalid("invalid assigned range"));
    }
    let mut stmt=sql.prepare("SELECT id FROM messages WHERE project_id=?1 AND \"to\"=?2 AND id>?3 AND id<=?4 ORDER BY id")?;
    let messages = stmt
        .query_map(params![project.to_string(), step, after, end], |r| {
            Ok(MessageId(r.get(0)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(AssignedRange {
        after: MessageId(after),
        through: MessageId(end),
        messages,
    })
}

/// Freeze an already chosen batch window on each launched item. Kept items have no run.
fn assign_delivery(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    run: RunId,
    assigned: &AssignedRange,
) -> Result<()> {
    let info = run_info(tx.sql(), project, run)?;
    if !info.live {
        return Err(conflict("cannot assign a stopped run"));
    }
    let step = info
        .step
        .ok_or_else(|| invalid("delivery requires a step run"))?;
    let expected = range(
        tx.sql(),
        project,
        &step,
        assigned.after.0,
        Some(assigned.through.0),
    )?;
    if &expected != assigned {
        return Err(invalid("assigned messages do not match the step range"));
    }
    let started: bool = tx.sql().query_row(
        "SELECT started_at IS NOT NULL FROM runs WHERE run_id=?1",
        [run.to_string()],
        |r| r.get(0),
    )?;
    if started {
        return Err(conflict("cannot reassign a started run"));
    }
    // A reservation calls this once. A duplicate must preserve its existing range.
    let stored:(i64,i64,i64)=tx.sql().query_row("SELECT assigned_after,assigned_through,(SELECT count(*) FROM message_deliveries WHERE run_id=?1) FROM runs WHERE run_id=?1",[run.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    if (stored.0 != 0 || stored.1 != 0 || stored.2 != 0)
        && (stored.0 != assigned.after.0 || stored.1 != assigned.through.0)
    {
        return Err(conflict("run range is already assigned"));
    }
    tx.sql().execute(
        "UPDATE runs SET assigned_after=?1,assigned_through=?2 WHERE project_id=?3 AND run_id=?4",
        params![
            assigned.after.0,
            assigned.through.0,
            project.to_string(),
            run.to_string()
        ],
    )?;
    let at = now()?;
    for id in &assigned.messages {
        tx.sql().execute("INSERT INTO message_deliveries(project_id,run_id,message_id,assigned_at) VALUES (?1,?2,?3,?4) ON CONFLICT DO NOTHING",params![project.to_string(),run.to_string(),id.0,at])?;
    }
    changed(tx, project);
    Ok(())
}

/// Assign using the attempts owner's lower bound and optional exact sibling window.
/// Without an exact window, include all currently addressed messages after the bound.
/// Reservation replay, including empty windows, belongs to attempts::reserve.
pub fn assign_run_range(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    run: RunId,
    cursor: i64,
    exact: Option<&crate::attempts::AssignedRange>,
) -> Result<AssignedRange> {
    let info = run_info(tx.sql(), project, run)?;
    let step = info
        .step
        .ok_or_else(|| invalid("delivery requires a step run"))?;
    let assigned = range(
        tx.sql(),
        project,
        &step,
        exact.map_or(cursor, |window| window.after),
        exact.map(|window| window.through),
    )?;
    assign_delivery(tx, project, run, &assigned)?;
    Ok(assigned)
}

/// RunStarted must already have persisted actual process-start evidence. A stale
/// callback never advances a replacement step generation.
pub fn advance_cursor(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    run: RunId,
) -> Result<MessageId> {
    let info = run_info(tx.sql(), project, run)?;
    let step = info
        .step
        .ok_or_else(|| invalid("delivery requires a step run"))?;
    let (started,through):(bool,i64)=tx.sql().query_row("SELECT started_at IS NOT NULL,assigned_through FROM runs WHERE project_id=?1 AND run_id=?2",params![project.to_string(),run.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if !started {
        return Err(conflict("run has not started"));
    }
    let changed_rows=tx.sql().execute("UPDATE steps SET delivery_cursor=max(delivery_cursor,?1) WHERE project_id=?2 AND step_id=?3 AND generation=?4 AND work_generation=?5",params![through,project.to_string(),step,info.generation,info.work])?;
    if changed_rows == 0 {
        return Err(conflict("run belongs to an old step generation"));
    }
    changed(tx, project);
    Ok(MessageId(through))
}

/// Dispatch acknowledgements are independent of RunStarted's batch cursor.
pub fn acknowledge_delivery(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    run: RunId,
    id: MessageId,
) -> Result<()> {
    run_info(tx.sql(), project, run)?;
    if tx.sql().execute("UPDATE message_deliveries SET acknowledged_at=coalesce(acknowledged_at,?1) WHERE project_id=?2 AND run_id=?3 AND message_id=?4",params![now()?,project.to_string(),run.to_string(),id.0])?==0 { return Err(missing("message was not assigned to this run")); }
    let owns_answer:Option<i64>=tx.sql().query_row(
        "SELECT q.message_id FROM question_attachments q JOIN messages m ON m.project_id=q.project_id AND m.id=q.message_id WHERE q.project_id=?1 AND q.run_id=?2 AND q.detached_at IS NULL AND m.resolved_by=?3 AND m.closed_at IS NULL",
        params![project.to_string(),run.to_string(),id.0],|r|r.get(0)).optional()?;
    if let Some(question_id) = owns_answer {
        claim_answer(tx, project, MessageId(question_id), run)?;
    }
    changed(tx, project);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reader {
    pub cursor: RecordSeq,
    pub heartbeat_at: Option<String>,
    pub unread_alert_min: Option<i64>,
}
pub fn reader(
    sql: &Connection,
    project: ProjectId,
    identity: &str,
    stream: &str,
    thread: &str,
) -> Result<Reader> {
    Ok(sql.query_row("SELECT cursor,heartbeat_at,unread_alert_min FROM readers WHERE project_id=?1 AND identity=?2 AND stream=?3 AND thread=?4",params![project.to_string(),identity,stream,thread],|r|Ok(Reader { cursor:RecordSeq(r.get(0)?),heartbeat_at:r.get(1)?,unread_alert_min:r.get(2)? })).optional()?.unwrap_or(Reader {cursor:RecordSeq(0),heartbeat_at:None,unread_alert_min:None}))
}

pub fn mark_read(tx: &mut WriteTransaction<'_>, read: MarkRead) -> Result<MessageId> {
    if read.through.0 < 0 || read.identity.trim().is_empty() {
        return Err(invalid("read identity and watermark are invalid"));
    }
    // A global watermark is clamped to the last actually existing message in this thread.
    let watermark: i64 = tx.sql().query_row(
        "SELECT coalesce(max(id),0) FROM messages WHERE project_id=?1 AND thread=?2 AND id<=?3",
        params![read.project.to_string(), read.thread, read.through.0],
        |r| r.get(0),
    )?;
    tx.sql().execute("INSERT INTO readers(project_id,identity,stream,thread,cursor) VALUES (?1,?2,'owner',?3,?4) ON CONFLICT(project_id,identity,stream,thread) DO UPDATE SET cursor=max(cursor,excluded.cursor)",params![read.project.to_string(),read.identity,read.thread,watermark])?;
    changed(tx, read.project);
    Ok(MessageId(
        reader(
            tx.sql(),
            read.project,
            &read.identity,
            OWNER_STREAM,
            &read.thread,
        )?
        .cursor
        .0,
    ))
}

pub fn orchestrator_read(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    identity: &str,
    through: RecordSeq,
    unread_alert_min: Option<i64>,
) -> Result<Reader> {
    if through.0 < 0 || identity.trim().is_empty() || unread_alert_min.is_some_and(|n| n < 0) {
        return Err(invalid("invalid orchestrator reader"));
    }
    tx.sql().execute("INSERT INTO readers(project_id,identity,stream,thread,cursor,heartbeat_at,unread_alert_min) VALUES (?1,?2,'orchestrator','',?3,?4,?5) ON CONFLICT(project_id,identity,stream,thread) DO UPDATE SET cursor=max(cursor,excluded.cursor),heartbeat_at=excluded.heartbeat_at,unread_alert_min=excluded.unread_alert_min",params![project.to_string(),identity,through.0,now()?,unread_alert_min])?;
    tx.changed(Some(project), "readers");
    reader(tx.sql(), project, identity, ORCHESTRATOR_STREAM, "")
}

/// Store-side message wake predicate for next; trailing notes remain held by the caller.
pub fn message_wakes(
    sql: &Connection,
    project: ProjectId,
    msg: &Message,
    me: &str,
) -> Result<bool> {
    if msg.from == me {
        return Ok(false);
    }
    if msg.needs_reply && msg.to.as_deref().is_none_or(|to| to == me) {
        return Ok(true);
    }
    if (!msg.needs_reply || msg.answer.is_some())
        && let Some(id) = msg.reply_to
    {
        return Ok(message(sql, project, id)?.needs_reply);
    }
    Ok(false)
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotifyAttempt {
    pub message: MessageId,
    pub attempt: AttemptId,
    pub outcome: NotificationOutcome,
    pub stderr: Option<String>,
    pub error: Option<PublicError>,
}
pub fn notify_attempt(
    sql: &Connection,
    project: ProjectId,
    id: MessageId,
) -> Result<NotifyAttempt> {
    let (attempt,outcome,stderr,error):(String,String,Option<String>,Option<String>)=sql.query_row("SELECT attempt_id,outcome,stderr,error FROM notification_attempts WHERE project_id=?1 AND message_id=?2",params![project.to_string(),id.0],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or_else(||missing("no notify attempt"))?;
    Ok(NotifyAttempt {
        message: id,
        attempt: attempt
            .parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("invalid notify attempt: {e}")))?,
        outcome: serde_json::from_value(serde_json::Value::String(outcome))?,
        stderr,
        error: error.map(|e| serde_json::from_str(&e)).transpose()?,
    })
}

/// A reservation is the sole dispatch permit. Results never reset it for replay.
pub fn notify_result(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: MessageId,
    attempt: AttemptId,
    outcome: NotificationOutcome,
    stderr: Option<String>,
    error: Option<PublicError>,
) -> Result<NotifyAttempt> {
    if outcome == NotificationOutcome::Reserved {
        return Err(invalid("notification result cannot reserve again"));
    }
    let before = notify_attempt(tx.sql(), project, id)?;
    if before.attempt != attempt {
        return Err(conflict("notification attempt identity differs"));
    }
    if before.outcome != NotificationOutcome::Reserved {
        if before.outcome == outcome && before.stderr == stderr && before.error == error {
            tx.changed(Some(project), "messages");
            return Ok(before);
        }
        return Err(conflict("notification result is already recorded"));
    }
    tx.sql().execute("UPDATE notification_attempts SET outcome=?1,finished_at=?2,stderr=?3,error=?4 WHERE project_id=?5 AND message_id=?6",params![serde_json::to_value(&outcome)?.as_str(),now()?,stderr,error.as_ref().map(serde_json::to_string).transpose()?,project.to_string(),id.0])?;
    tx.append_record(
        Some(project),
        Event::ProjectNotify {
            message: id,
            outcome,
            error,
        },
    )?;
    changed(tx, project);
    notify_attempt(tx.sql(), project, id)
}

/// Reserve a system owner question once for the earliest unread settlement or
/// message wake. The durable message data is the deduplication key, so changing
/// the threshold or trimming the original record cannot alert it again.
/// Scheduling and project-wide reader policy remain the runtime's responsibility.
pub fn unread_alert(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    identity: &str,
    at: time::OffsetDateTime,
) -> Result<Option<Message>> {
    let position = reader(tx.sql(), project, identity, ORCHESTRATOR_STREAM, "")?;
    let Some(minutes) = position.unread_alert_min else {
        return Ok(None);
    };
    let Some(heartbeat) = position.heartbeat_at else {
        return Ok(None);
    };
    let heartbeat =
        time::OffsetDateTime::parse(&heartbeat, &time::format_description::well_known::Rfc3339)
            .map_err(|e| StoreError::InvalidDatabase(e.to_string()))?;
    if (at - heartbeat).whole_minutes() < minutes {
        return Ok(None);
    }
    let archived: bool = tx
        .sql()
        .query_row(
            "SELECT archived FROM projects WHERE project_id=?1 AND deleted_at IS NULL",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(true);
    if archived {
        return Ok(None);
    }
    let mut stmt=tx.sql().prepare("SELECT seq,kind,payload FROM records WHERE project_id=?1 AND seq>?2 AND kind IN ('message','unit.settled') ORDER BY seq")?;
    let rows = stmt
        .query_map(params![project.to_string(), position.cursor.0], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut first = None;
    for (seq, kind, payload) in rows {
        let event: Event = serde_json::from_str(&payload)?;
        let wakes = if let Event::Message(msg) = event {
            message_wakes(tx.sql(), project, &msg, identity)?
        } else {
            kind == "unit.settled"
        };
        if wakes {
            first = Some(seq);
            break;
        }
    }
    let Some(seq) = first else {
        return Ok(None);
    };
    let alerted:bool=tx.sql().query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE project_id=?1 AND \"from\"='sluice' AND json_extract(data,'$.unread_record')=?2 AND json_extract(data,'$.reader')=?3)",params![project.to_string(),seq,identity],|r|r.get(0))?;
    if alerted {
        return Ok(None);
    }
    let post = MessagePost {
        project: ProjectSelector::Id(project),
        body: format!(
            "No reader progress after record {seq}. Resnapshot status/messages and resume reading."
        ),
        thread: None,
        to: Some("owner".into()),
        needs_reply: Some(true),
        reply_to: None,
        answer: None,
        title: Some(format!(
            "No orchestrator has read for {minutes} min (seq {seq})"
        )),
        ui: None,
        input: None,
        data: Some(JsonValue::try_from(
            serde_json::json!({"unread_record":seq,"reader":identity}),
        )?),
        from: Some("sluice".into()),
        run: None,
        author: None,
    };
    Ok(Some(message_post(tx, post, &NoPlanInputs)?))
}
