//! Durable messages, answer ownership, delivery reservations, and independent readers.

use crate::{Result, StoreError, WriteTransaction, identities};
use rusqlite::{Connection, OptionalExtension, params};
pub use sluice_model::commands::QuestionState;
use sluice_model::{
    commands::{
        Ask, Delivery, MarkRead, Message, MessageAnswer, MessagePost, MessageReceipt, MessageVerb,
        MessageView, NeedsDismiss, Reply, Say, StepDismiss,
    },
    error::PublicError,
    events::{Event, NotificationOutcome},
    identity::{self, LEGACY_ORCHESTRATOR, NAME_THIS_SESSION, Recipient, SessionName, refusal},
    ids::{AttemptId, MessageId, ProjectId, ProjectSelector, RecordSeq, RunId},
    rpc::JsonValue,
};

pub const OWNER_STREAM: &str = "owner";
/// The readers stream sessions read under (`identities::READER_STREAM`), and the retired
/// shared address (`identity::LEGACY_ORCHESTRATOR`) that messages stored before identities
/// still name.
pub const ORCHESTRATOR_STREAM: &str = "orchestrator";
/// The readers identity an unnamed `next` keeps its position and heartbeat under.
pub const ANONYMOUS_READER: &str = "orchestrator";

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
// Rows stored before the verbs read with the verb derived from needs_reply/reply_to; a
// question with an empty body shows its plan input's doc.
const MESSAGE_JSON: &str = "json_object('id',id,'verb',CASE WHEN reply_to IS NOT NULL THEN 'reply' WHEN needs_reply=1 THEN 'ask' ELSE 'say' END,'from',\"from\",'to',\"to\",'thread',thread,'body',CASE WHEN body='' AND needs_reply=1 AND input IS NOT NULL THEN coalesce((SELECT json_extract(i.declaration,'$.doc') FROM inputs i WHERE i.project_id=messages.project_id AND i.name=messages.input),'') ELSE body END,'title',title,'ui',ui,'input',input,'data',json(data),'run',run_id,'at',at,'to_message',reply_to,'answer',json(answer),'state',CASE WHEN needs_reply=1 THEN CASE WHEN closed_at IS NOT NULL THEN 'closed' WHEN resolved_by IS NOT NULL THEN 'answered' ELSE 'open' END END,'answered_by',CASE WHEN needs_reply=1 AND closed_at IS NULL THEN resolved_by END)";

pub fn message(sql: &Connection, project: ProjectId, id: MessageId) -> Result<Message> {
    find_message(sql, project, id)?.ok_or_else(|| missing(format!("no message {}", id.0)))
}

fn find_message(sql: &Connection, project: ProjectId, id: MessageId) -> Result<Option<Message>> {
    let json: Option<String> = sql
        .query_row(
            &format!("SELECT {MESSAGE_JSON} FROM messages WHERE project_id=?1 AND id=?2"),
            params![project.to_string(), id.0],
            |r| r.get(0),
        )
        .optional()?;
    json.map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(Into::into)
}

fn reply_message(
    sql: &Connection,
    project: ProjectId,
    id: MessageId,
    caller: &str,
) -> Result<Message> {
    if let Some(message) = find_message(sql, project, id)? {
        return Ok(message);
    }
    let other_project: Option<String> = sql
        .query_row(
            "SELECT p.name FROM messages m JOIN projects p ON p.project_id=m.project_id WHERE m.id=?1",
            [id.0],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(name) = other_project {
        return Err(invalid(format!("message {} is in project {name}", id.0)));
    }
    let kind: Option<String> = sql
        .query_row(
            "SELECT kind FROM records WHERE project_id=?1 AND seq=?2",
            params![project.to_string(), id.0],
            |row| row.get(0),
        )
        .optional()?;
    let Some(kind) = kind else {
        return Err(missing(format!("no message {}", id.0)));
    };
    let mut query = sql.prepare(&format!(
        "SELECT {MESSAGE_JSON} FROM messages WHERE project_id=?1 AND \"to\" IN (?2,?3)
         AND needs_reply=1 AND resolved_by IS NULL AND closed_at IS NULL ORDER BY id LIMIT 5"
    ))?;
    let mut choices = Vec::new();
    for row in query.query_map(
        params![project.to_string(), caller, legacy_too(caller)],
        |row| row.get::<_, String>(0),
    )? {
        let question: Message = serde_json::from_str(&row?)?;
        let body: String = question
            .body
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        choices.push(format!("#{} {}: {body}", question.id.0, question.from));
    }
    let questions = if choices.is_empty() {
        " none.".into()
    } else {
        format!("\n{}", choices.join("\n"))
    };
    Err(invalid(format!(
        "{} is a log seq (a {kind} record), not a message. A message's id is the # number on its line. Open questions to you:{questions}",
        id.0
    )))
}

/// The run that has claimed this answering reply, if any.
fn claimed_by(sql: &Connection, project: ProjectId, id: MessageId) -> Result<Option<RunId>> {
    let claimed: Option<String> = sql.query_row(
        "SELECT claimed_by FROM messages WHERE project_id=?1 AND id=?2",
        params![project.to_string(), id.0],
        |r| r.get(0),
    )?;
    claimed
        .map(|run| {
            run.parse()
                .map_err(|e| StoreError::InvalidDatabase(format!("invalid claimed_by: {e}")))
        })
        .transpose()
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
    if !msg.is_question() {
        return Err(invalid("message is not a question"));
    }
    let reply_id: Option<i64> = sql.query_row(
        "SELECT resolved_by FROM messages WHERE project_id=?1 AND id=?2",
        params![project.to_string(), id.0],
        |r| r.get(0),
    )?;
    let reply = reply_id
        .map(|id| message(sql, project, MessageId(id)))
        .transpose()?;
    let state = msg.state.unwrap_or(QuestionState::Open);
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

/// Who posts (SPEC §8.1). The transport derives it: the dashboard is the owner, a run is its
/// step, and an MCP or command-line session is the `@name` it gave (`me`); a session with no
/// name may not speak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Speaker {
    /// The dashboard.
    Owner,
    /// An MCP or command-line caller that named itself.
    Session(SessionName),
    /// An agent or fn run; a step's run speaks as its step.
    Run(RunId),
    /// A step speaking through a write with no run at hand (a run's `step_retry` message).
    Step(String),
    /// Sluice itself (unread alerts to the owner).
    Sluice,
}
impl Speaker {
    /// `owner` comes from the dashboard; a run identity makes the run the speaker; else the
    /// session must name itself with `me`.
    pub fn of(owner: bool, run: Option<RunId>, me: Option<&str>) -> Result<Self> {
        let me = me.map(str::trim).filter(|me| !me.is_empty());
        match (owner, run, me) {
            (true, Some(_), _) => Err(invalid("a run cannot speak as the owner")),
            (true, None, Some(_)) => Err(refusal(
                "me",
                "the dashboard speaks as the owner and names no session",
            )
            .into()),
            (true, None, None) => Ok(Self::Owner),
            (false, Some(_), Some(_)) => {
                Err(refusal("me", "a run speaks as its step: leave out me (or run)").into())
            }
            (false, Some(run), None) => Ok(Self::Run(run)),
            (false, None, Some(me)) => Ok(Self::Session(SessionName::parse(me)?)),
            (false, None, None) => Err(refusal("me", NAME_THIS_SESSION).into()),
        }
    }
    /// The speaker a write's author names (retry feedback): the owner, a session, a run's
    /// `step:<id>`, or a step of the plan (a completion action is authored by the step that
    /// registered it); any other author is the system's. An unnamed session's retry with a
    /// message is refused before it gets here (`check_speaking_author`).
    pub fn of_author(sql: &Connection, project: ProjectId, author: &str) -> Result<Self> {
        let author = author.trim();
        if author == OWNER_STREAM {
            return Ok(Self::Owner);
        }
        if let Some(step) = author.strip_prefix("step:") {
            return Ok(Self::Step(step.to_owned()));
        }
        if identity::is_session(author) {
            return Ok(Self::Session(SessionName::parse(author)?));
        }
        let step: bool = sql.query_row(
            "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND step_id=?2)",
            params![project.to_string(), author],
            |r| r.get(0),
        )?;
        Ok(if step {
            Self::Step(author.to_owned())
        } else {
            Self::Sluice
        })
    }
}
/// A write that speaks (a `step_retry` with a message) from a caller: its author must name
/// the owner, a session or a run's step; any other is an unnamed session, refused.
pub fn check_speaking_author(author: &str) -> Result<()> {
    let author = author.trim();
    if author == OWNER_STREAM || author.starts_with("step:") || identity::is_session(author) {
        return Ok(());
    }
    Err(refusal("me", NAME_THIS_SESSION).into())
}
impl Speaker {}
/// The retired `orchestrator` beside a session's own name: questions sent to it before
/// identities are still the session's to answer. Any other caller matches only itself.
fn legacy_too(caller: &str) -> &str {
    if identity::is_session(caller) {
        LEGACY_ORCHESTRATOR
    } else {
        caller
    }
}

/// What is posted: the three verbs.
#[derive(Debug, Clone, PartialEq)]
pub enum Verb {
    Ask {
        to: String,
        title: Option<String>,
        ui: Option<String>,
        input: Option<String>,
        data: Option<JsonValue>,
    },
    Say {
        to: String,
        data: Option<JsonValue>,
    },
    Reply {
        to_message: MessageId,
        answer: Option<MessageAnswer>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Post {
    pub project: ProjectSelector,
    pub speaker: Speaker,
    pub body: String,
    pub verb: Verb,
}
impl TryFrom<Ask> for Post {
    type Error = StoreError;
    fn try_from(a: Ask) -> Result<Self> {
        Ok(Self {
            project: a.project,
            speaker: Speaker::of(a.owner, a.run, a.me.as_deref())?,
            body: a.body,
            verb: Verb::Ask {
                to: a.to,
                title: a.title,
                ui: a.ui,
                input: a.input,
                data: a.data,
            },
        })
    }
}
impl TryFrom<Say> for Post {
    type Error = StoreError;
    fn try_from(s: Say) -> Result<Self> {
        Ok(Self {
            project: s.project,
            speaker: Speaker::of(s.owner, s.run, s.me.as_deref())?,
            body: s.body,
            verb: Verb::Say {
                to: s.to,
                data: s.data,
            },
        })
    }
}
/// Resolve a reply's sender selection in the same transaction that posts it.
/// Only open questions addressed to the transport-derived caller are candidates.
pub fn reply_post(sql: &Connection, reply: Reply) -> Result<Post> {
    let speaker = Speaker::of(reply.owner, reply.run, reply.me.as_deref())?;
    let to_message = match (reply.to_message, reply.to) {
        (Some(id), None) => id,
        (None, Some(sender)) if !sender.trim().is_empty() => {
            let project = resolve_project(sql, &reply.project)?;
            let caller = speaker_name(sql, project, &speaker)?;
            let mut query = sql.prepare(
                "SELECT id,substr(CASE WHEN body='' AND input IS NOT NULL THEN coalesce((SELECT json_extract(i.declaration,'$.doc') FROM inputs i WHERE i.project_id=messages.project_id AND i.name=messages.input),'') ELSE body END,1,200)
                 FROM messages WHERE project_id=?1 AND \"from\"=?2 AND \"to\" IN (?3,?4)
                 AND needs_reply=1 AND resolved_by IS NULL AND closed_at IS NULL ORDER BY id",
            )?;
            let candidates = query
                .query_map(
                    params![project.to_string(), sender, caller, legacy_too(&caller)],
                    |row| Ok((MessageId(row.get(0)?), row.get::<_, String>(1)?)),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            match candidates.as_slice() {
                [(id, _)] => *id,
                [] => {
                    return Err(conflict(format!(
                        "no open question from {sender} addressed to {caller}"
                    )));
                }
                _ => {
                    let choices = candidates
                        .iter()
                        .map(|(id, body)| {
                            format!("#{} {}", id.0, body.lines().next().unwrap_or(""))
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Err(conflict(format!(
                        "several open questions from {sender} addressed to {caller}; use to_message:\n{choices}"
                    )));
                }
            }
        }
        _ => {
            return Err(invalid(
                "reply requires exactly one of to_message or a nonblank to",
            ));
        }
    };
    Ok(Post {
        project: reply.project,
        speaker,
        body: reply.body,
        verb: Verb::Reply {
            to_message,
            answer: reply.answer,
        },
    })
}
/// The retired message_post, as runs started on an older release still send it:
/// accepted only with a run identity, a reply when it names `reply_to`, else a question
/// unless `needs_reply` is false, addressed to the orchestrator when it names nobody.
/// Its `thread`, `from` and `author` are ignored: both are derived now.
pub fn bridge(post: MessagePost) -> Result<Post> {
    let Some(run) = post.run else {
        return Err(invalid(
            "message_post is retired: use ask, say or reply (sluice docs threads)",
        ));
    };
    // Naming nobody, it asks the run's unit's owner, as `orchestrator` from a run does.
    let to = post.to.unwrap_or_default();
    let verb = match post.reply_to {
        Some(to_message) => Verb::Reply {
            to_message,
            answer: post.answer,
        },
        None if post.answer.is_some() => return Err(invalid("answer requires reply_to")),
        None if post.needs_reply.unwrap_or(true) => Verb::Ask {
            to,
            title: post.title,
            ui: post.ui,
            input: post.input,
            data: post.data,
        },
        None => Verb::Say {
            to,
            data: post.data,
        },
    };
    Ok(Post {
        project: post.project,
        speaker: Speaker::Run(run),
        body: post.body,
        verb,
    })
}

/// The agent fns. A run reserved by an older release has no frozen `listens`; such a
/// run listens if its step runs one of these, or if it has acknowledged a message.
const LISTENING_FNS: &[&str] = &[
    "agent.claude",
    "agent.codex",
    "agent.devin",
    "agent.review",
    "agent.run",
];

/// How a message to `to` reaches it now, read from the same durable rows the delivery
/// path reads: a session and the owner read inboxes; a step's live, started run that
/// listens is offered it by its guardian, whether that guardian holds a watch or polls;
/// a step that will run (pending, or a run not yet started) gets it with its next run;
/// anything else (a paused step, a run that does not listen) keeps it for a run started
/// later. A settled step, or one whose runs have all submitted, never gets here: post
/// refuses messages to it. Whether a run
/// listens is frozen in its reservation from its fn's contract (it takes `listen`) and
/// its inputs, so a custom fn that runs an agent counts as one.
pub fn delivery(
    sql: &Connection,
    project: ProjectId,
    to: &str,
) -> Result<(Delivery, Option<RunId>)> {
    if to == OWNER_STREAM || to == ORCHESTRATOR_STREAM || identity::is_session(to) {
        return Ok((Delivery::Delivered, None));
    }
    let step: Option<(String, String, String)> = sql
        .query_row(
            "SELECT status,coalesce(paused,'false'),declaration FROM steps WHERE project_id=?1 AND step_id=?2",
            params![project.to_string(), to],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((status, paused, declaration)) = step else {
        return Ok((Delivery::NoLiveRun, None));
    };
    let live: Option<(String, bool, Option<bool>, bool)> = sql
        .query_row(
            "SELECT r.run_id,a.phase='executing',json_extract(a.request,'$.listens'),EXISTS(SELECT 1 FROM message_deliveries d WHERE d.project_id=r.project_id AND d.run_id=r.run_id AND d.acknowledged_at IS NOT NULL) FROM runs r JOIN attempts a ON a.attempt_id=r.attempt_id JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id AND s.generation=r.generation AND s.work_generation=r.work_generation WHERE r.project_id=?1 AND r.step_id=?2 AND r.finished_at IS NULL AND a.phase!='terminal' AND a.cancel_requested=0 ORDER BY a.phase='executing' DESC,r.item_index LIMIT 1",
            params![project.to_string(), to],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let parse = |run: String| {
        run.parse::<RunId>()
            .map_err(|e| StoreError::InvalidDatabase(format!("invalid run: {e}")))
    };
    Ok(match live {
        Some((run, true, frozen, acknowledged)) => {
            let listening = match frozen {
                Some(listens) => listens,
                None => acknowledged || legacy_listens(&declaration)?,
            };
            if listening {
                (Delivery::Delivered, Some(parse(run)?))
            } else {
                (Delivery::NoLiveRun, None)
            }
        }
        Some((run, false, _, _)) => (Delivery::Queued, Some(parse(run)?)),
        None if matches!(status.as_str(), "pending" | "running") && paused == "false" => {
            (Delivery::Queued, None)
        }
        None => (Delivery::NoLiveRun, None),
    })
}
/// Listening for a run reserved before reservations froze it: an agent fn whose step
/// does not bind `listen` to false.
fn legacy_listens(declaration: &str) -> Result<bool> {
    let declaration: serde_json::Value = serde_json::from_str(declaration)?;
    let agent = declaration["run"]
        .as_str()
        .is_some_and(|f| LISTENING_FNS.contains(&f));
    let listen = &declaration["in"]["listen"];
    let off = listen == &serde_json::Value::Bool(false)
        || listen["default"] == serde_json::Value::Bool(false);
    Ok(agent && !off)
}

/// The recipient of an ask or say, resolved (SPEC §8.1): a step of the current plan, a
/// session registered in the project, or the owner. A run that names nobody, or the retired
/// `orchestrator`, reaches its unit's owner (`identities::route_run`); the dashboard naming
/// nobody reaches the project's fallback (a board Button); a session must name someone.
fn recipient(
    sql: &Connection,
    project: ProjectId,
    speaker: &Speaker,
    posting_run: Option<&RunInfo>,
    to: &str,
) -> Result<String> {
    const WHO: &str = "a step of the plan, an @session (for example @lash-f4) or owner";
    let routed = || -> Result<String> {
        let run = posting_run.ok_or_else(|| invalid("a run speaks with its run"))?;
        identities::route_run(sql, project, run.step.as_deref(), run.caller.as_deref())
    };
    match (Recipient::of(to)?, speaker) {
        (Recipient::Omitted | Recipient::LegacyOrchestrator, Speaker::Run(_)) => routed(),
        (Recipient::Omitted, Speaker::Owner) => identities::route_owner(sql, project),
        (Recipient::Omitted, _) => Err(refusal("to", &format!("to is required: {WHO}")).into()),
        (Recipient::LegacyOrchestrator, _) => Err(refusal(
            "to",
            &format!("orchestrator is no longer an address: name the recipient, {WHO}"),
        )
        .into()),
        (Recipient::Owner, _) => Ok(OWNER_STREAM.into()),
        (Recipient::Session(name), _) => {
            identities::check_registered(sql, project, "to", name.as_str())?;
            Ok(name.to_string())
        }
        (Recipient::Step(step), _) => {
            let found: bool = sql.query_row(
                "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND step_id=?2)",
                params![project.to_string(), step],
                |r| r.get(0),
            )?;
            if !found {
                return Err(invalid(format!(
                    "to {step:?} is not a step of the current plan, an @session or owner"
                )));
            }
            Ok(step)
        }
    }
}

/// The name a speaker posts as.
fn speaker_name(sql: &Connection, project: ProjectId, speaker: &Speaker) -> Result<String> {
    Ok(match speaker {
        Speaker::Owner => OWNER_STREAM.into(),
        Speaker::Sluice => identity::SLUICE.into(),
        Speaker::Session(name) => name.to_string(),
        Speaker::Step(step) => step.clone(),
        Speaker::Run(run) => run_info(sql, project, *run)?
            .step
            .unwrap_or_else(|| format!("call:{run}")),
    })
}

/// Why a step takes no more messages, if it does not: it is settled (it has its result),
/// or every run it has submitted its outputs, which ended its agent's session, and is
/// only finishing.
fn closed(sql: &Connection, project: ProjectId, step: &str) -> Result<Option<String>> {
    Ok(sql
        .query_row(
            "SELECT CASE WHEN s.status IN ('succeeded','failed','stale','skipped') THEN 'is settled ('||s.status||')'
                ELSE 'has submitted its outputs and is finishing' END
             FROM steps s WHERE s.project_id=?1 AND s.step_id=?2 AND (s.status IN ('succeeded','failed','stale','skipped')
                OR (s.status='running' AND json_array_length(s.run_ids)>0
                    AND NOT EXISTS (SELECT 1 FROM json_each(s.run_ids) j WHERE NOT EXISTS (SELECT 1 FROM submissions m WHERE m.run_id=j.value))))",
            params![project.to_string(), step],
            |r| r.get(0),
        )
        .optional()?)
}

/// A posted message and its receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct Posted {
    pub message: Message,
    pub receipt: MessageReceipt,
}

/// Posting and its record, first-answer resolution, optional plan input and notify
/// reservation all commit together. The sender, recipient of a reply and thread are
/// derived: a step's messages live on `step-<step>`, the orchestrator and owner talk
/// on `owner`, and a reply stays on its parent's thread, addressed to its sender.
/// Propagate errors out of Writer::write.
pub fn post(
    tx: &mut WriteTransaction<'_>,
    post: Post,
    inputs: &impl PlanInputSetter,
) -> Result<Posted> {
    let project = resolve_project(tx.sql(), &post.project)?;
    let posting_run = match post.speaker {
        Speaker::Run(run) => Some(run_info(tx.sql(), project, run)?),
        _ => None,
    };
    let from = speaker_name(tx.sql(), project, &post.speaker)?;
    let run = match post.speaker {
        Speaker::Run(run) => Some(run),
        _ => None,
    };
    // A session's message registers it in the project: a session may be addressed there
    // once it has spoken, edited or waited.
    if let Speaker::Session(me) = &post.speaker {
        identities::register(tx, project, me)?;
    }
    let (to, verb, parent) = match post.verb {
        Verb::Ask {
            to,
            title,
            ui,
            input,
            data,
        } => {
            let to = recipient(tx.sql(), project, &post.speaker, posting_run.as_ref(), &to)?;
            if title.as_ref().is_some_and(|s| s.trim().is_empty()) {
                return Err(invalid("title must not be blank"));
            }
            if post.body.trim().is_empty() && input.is_none() {
                return Err(invalid("body must not be blank"));
            }
            if let Some(input) = &input {
                let exists: bool = tx.sql().query_row(
                    "SELECT EXISTS(SELECT 1 FROM inputs WHERE project_id=?1 AND name=?2)",
                    params![project.to_string(), input],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err(missing(format!("no plan input {input}")));
                }
            }
            let ask = Verb::Ask {
                to: to.clone(),
                title,
                ui,
                input,
                data,
            };
            (to, ask, None)
        }
        Verb::Say { to, data } => {
            let to = recipient(tx.sql(), project, &post.speaker, posting_run.as_ref(), &to)?;
            if post.body.trim().is_empty() {
                return Err(invalid("body must not be blank"));
            }
            (to.clone(), Verb::Say { to, data }, None)
        }
        Verb::Reply { to_message, answer } => {
            let parent = reply_message(tx.sql(), project, to_message, &from)?;
            if let Some(answer) = &answer {
                if answer.action.trim().is_empty() {
                    return Err(invalid("answer.action must not be blank"));
                }
                if !parent.is_question() {
                    return Err(invalid("answer must reply to a question"));
                }
                let question = question(tx.sql(), project, parent.id)?;
                if answer.action == "reopen" {
                    let recent: bool = tx.sql().query_row(
                        "SELECT closed_at IS NOT NULL AND julianday(closed_at)>=julianday('now')-10.0/1440.0 FROM messages WHERE project_id=?1 AND id=?2",
                        params![project.to_string(), parent.id.0], |r| r.get(0))?;
                    if from != OWNER_STREAM
                        || question.state != QuestionState::Closed
                        || !recent
                        || !question
                            .reply
                            .as_ref()
                            .is_some_and(|r| r.from == OWNER_STREAM)
                    {
                        return Err(conflict(
                            "Undo is available for ten minutes after you close a question",
                        ));
                    }
                } else if question.state != QuestionState::Open {
                    return Err(conflict("question is no longer open"));
                }
            } else if post.body.trim().is_empty() {
                return Err(invalid("a reply needs a body or an answer"));
            }
            (
                parent.from.clone(),
                Verb::Reply { to_message, answer },
                Some(parent),
            )
        }
    };
    if to == from {
        return Err(invalid(format!("{from} cannot address itself")));
    }
    let resolving = parent
        .as_ref()
        .filter(|p| p.is_question())
        .map(|p| question(tx.sql(), project, p.id))
        .transpose()?
        .filter(|q| q.state == QuestionState::Open);
    // A settled or submitted step has nobody left to read a message: refuse it rather
    // than keep it.
    // Answering or closing a question it asked is still taken: a question that does not
    // wait leaves its step settled at once, and its answer is for the plan input it sets,
    // whoever reads the inbox, or the step's retry to take up.
    let reopening = matches!(&verb, Verb::Reply { answer: Some(a), .. } if a.action == "reopen");
    if resolving.is_none()
        && !reopening
        && let Some(why) = closed(tx.sql(), project, &to)?
    {
        return Err(conflict(format!(
            "step {to} {why}, so it takes no more messages; to send it work, retry it with step_retry and a message"
        )));
    }
    let answer = match &verb {
        Verb::Reply { answer, .. } => answer.clone(),
        _ => None,
    };
    if let Some(q) = &resolving
        && !answer.as_ref().is_some_and(|a| a.action == "close")
        && let Some(name) = &q.message.input
    {
        // Present JSON null wins over all later fields, just like any other value.
        let structured = answer.as_ref().and_then(|a| {
            a.values
                .as_ref()
                .and_then(|v| v.0.get("value"))
                .or_else(|| a.params.as_ref().and_then(|v| v.0.get("value")))
        });
        let value = match structured {
            Some(value) => value.clone(),
            None if answer.is_some() && post.body.is_empty() => {
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
    // A reply keeps its parent's thread; a step's run speaks on its step's; a message to a
    // step lives on that step's; one between a session and the owner or another session on
    // the session's own (the recipient's when it is one); the system's alerts on `owner`.
    let thread = match (&parent, posting_run.as_ref().and_then(|r| r.step.as_ref())) {
        (Some(parent), _) => parent.thread.clone(),
        (None, Some(step)) => format!("step-{step}"),
        (None, None) if identity::is_session(&to) => to.clone(),
        (None, None) if to != OWNER_STREAM && to != ORCHESTRATOR_STREAM => format!("step-{to}"),
        (None, None) if identity::is_session(&from) => from.clone(),
        (None, None) => OWNER_STREAM.to_owned(),
    };
    let mut msg = Message {
        id: MessageId(0),
        verb: MessageVerb::Say,
        from,
        to: Some(to.clone()),
        thread,
        body: post.body,
        title: None,
        ui: None,
        input: None,
        data: None,
        run,
        at: now()?,
        to_message: None,
        answer: None,
        state: None,
        answered_by: None,
    };
    match verb {
        Verb::Ask {
            title,
            ui,
            input,
            data,
            ..
        } => {
            msg.verb = MessageVerb::Ask;
            msg.title = title;
            msg.ui = ui;
            msg.input = input;
            msg.data = data;
        }
        Verb::Say { data, .. } => msg.data = data,
        Verb::Reply { to_message, answer } => {
            msg.verb = MessageVerb::Reply;
            msg.to_message = Some(to_message);
            msg.answer = answer;
        }
    }
    if reopening && msg.body.is_empty() {
        msg.body = "Question reopened.".into();
    }
    let record = tx.append_record(Some(project), Event::Message(Box::new(msg.clone())))?;
    msg.id = MessageId(record.seq.0);
    msg.at = record.at;
    // The writer allocates the id. Finalize the record with it and the event timestamp.
    tx.sql().execute(
        "UPDATE records SET thread=?1,payload=?2 WHERE seq=?3",
        params![
            msg.thread,
            serde_json::to_string(&Event::Message(Box::new(msg.clone())))?,
            msg.id.0
        ],
    )?;
    let asking = msg.verb == MessageVerb::Ask;
    tx.sql().execute("INSERT INTO messages(id,project_id,thread,\"from\",\"to\",title,body,needs_reply,reply_to,answer,ui,input,data,run_id,at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![msg.id.0,project.to_string(),msg.thread,msg.from,msg.to,msg.title,msg.body,asking,msg.to_message.map(|id| id.0),
        msg.answer.as_ref().map(serde_json::to_string).transpose()?,msg.ui,msg.input,msg.data.as_ref().map(serde_json::to_string).transpose()?,msg.run.map(|id| id.to_string()),msg.at])?;
    if reopening {
        let parent = parent.as_ref().expect("reopening replies to a question");
        tx.sql().execute(
            "UPDATE messages SET resolved_by=NULL,closed_at=NULL WHERE project_id=?1 AND id=?2",
            params![project.to_string(), parent.id.0],
        )?;
    }
    if let Some(q) = resolving {
        tx.sql().execute("UPDATE messages SET resolved_by=?1,closed_at=?2 WHERE project_id=?3 AND id=?4 AND resolved_by IS NULL",
            params![msg.id.0,msg.answer.as_ref().filter(|a| a.action=="close").map(|_| msg.at.clone()),project.to_string(),q.message.id.0])?;
    }
    if asking {
        if let (Some(run), Some(info)) = (msg.run, posting_run.as_ref())
            && info.step.is_some()
        {
            attach(
                tx,
                project,
                msg.id,
                run,
                info,
                msg.title.as_deref().unwrap_or(""),
            )?;
        }
        if to == OWNER_STREAM {
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
    let (delivery, delivered_to) = delivery(tx.sql(), project, &to)?;
    // Only a session or the dashboard hears that nobody is reading: a run's receipt keeps the
    // shape a run pinned to an older release decodes.
    let note = if run.is_none() {
        identities::delivery_note(tx.sql(), project, &to)?
    } else {
        None
    };
    let message = message(tx.sql(), project, msg.id)?;
    Ok(Posted {
        receipt: MessageReceipt {
            id: message.id,
            to,
            thread: message.thread.clone(),
            delivery,
            run: delivered_to,
            note,
        },
        message,
    })
}

#[derive(Debug)]
struct RunInfo {
    step: Option<String>,
    generation: i64,
    item: i64,
    work: i64,
    live: bool,
    /// A call's run: who called it (the call's author).
    caller: Option<String>,
}
fn run_info(sql: &Connection, project: ProjectId, run: RunId) -> Result<RunInfo> {
    sql.query_row("SELECT r.step_id,r.generation,r.item_index,r.work_generation,r.finished_at IS NULL AND a.phase!='terminal' AND a.cancel_requested=0,(SELECT c.author FROM calls c WHERE c.run_id=r.run_id) FROM runs r JOIN attempts a ON a.attempt_id=r.attempt_id WHERE r.project_id=?1 AND r.run_id=?2",
        params![project.to_string(),run.to_string()],|r| Ok(RunInfo { step:r.get(0)?,generation:r.get(1)?,item:r.get(2)?,work:r.get(3)?,live:r.get(4)?,caller:r.get(5)? })).optional()?.ok_or_else(|| missing("no run in this project"))
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
    Waiting(Posted),
    Answered {
        question: Message,
        reply: Box<Message>,
    },
    Closed(Message),
}

/// message.ask(wait=true) takes up only the latest question in this exact item lineage.
pub fn ask_waiting(
    tx: &mut WriteTransaction<'_>,
    post: Post,
    inputs: &impl PlanInputSetter,
) -> Result<AskResult> {
    let project = resolve_project(tx.sql(), &post.project)?;
    let Speaker::Run(run) = post.speaker else {
        return Err(invalid("waiting ask requires a run"));
    };
    let Verb::Ask { to, title, .. } = &post.verb else {
        return Err(invalid("only a question waits"));
    };
    let info = run_info(tx.sql(), project, run)?;
    let to = &recipient(tx.sql(), project, &post.speaker, Some(&info), to)?;
    if !info.live {
        return Err(conflict("asking run has stopped"));
    }
    let step = info
        .step
        .as_deref()
        .ok_or_else(|| invalid("take-up requires a step run"))?;
    let title = title
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| invalid("waiting ask requires a title"))?;
    let prior: Option<i64>=tx.sql().query_row("SELECT q.message_id FROM question_attachments q JOIN messages m ON m.project_id=q.project_id AND m.id=q.message_id WHERE q.project_id=?1 AND q.step_id=?2 AND q.generation=?3 AND q.item_index=?4 AND q.title=?5 AND m.\"to\"=?6 GROUP BY q.message_id ORDER BY q.message_id DESC LIMIT 1",
        params![project.to_string(),step,info.generation,info.item,title,to],|r| r.get(0)).optional()?;
    if let Some(id) = prior {
        let q = question(tx.sql(), project, MessageId(id))?;
        match q.state {
            QuestionState::Open => {
                let attached:Option<String>=tx.sql().query_row("SELECT run_id FROM question_attachments WHERE project_id=?1 AND message_id=?2 AND detached_at IS NULL",params![project.to_string(),id],|r|r.get(0)).optional()?;
                if !q.waiting || attached.as_deref() == Some(&run.to_string()) {
                    attach(tx, project, q.message.id, run, &info, title)?;
                    let message = message(tx.sql(), project, q.message.id)?;
                    let (delivery, delivered_to) = delivery(tx.sql(), project, to)?;
                    return Ok(AskResult::Waiting(Posted {
                        receipt: MessageReceipt {
                            id: message.id,
                            to: to.clone(),
                            thread: message.thread.clone(),
                            delivery,
                            run: delivered_to,
                            note: None,
                        },
                        message,
                    }));
                }
            }
            QuestionState::Answered => {
                let unclaimed = match &q.reply {
                    Some(reply) => claimed_by(tx.sql(), project, reply.id)?.is_none(),
                    None => false,
                };
                if unclaimed {
                    attach(tx, project, q.message.id, run, &info, title)?;
                    let reply = claim_answer(tx, project, q.message.id, run)?;
                    return Ok(AskResult::Answered {
                        question: message(tx.sql(), project, q.message.id)?,
                        reply: Box::new(reply),
                    });
                }
            }
            QuestionState::Closed => {}
        }
    }
    Ok(AskResult::Waiting(self::post(tx, post, inputs)?))
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
    claim_attached_answer(tx, project, id, run)
}

/// The claim behind `claim_answer`, without the liveness check: an acknowledged
/// delivery proves the run was handed the answer, even when the acknowledgement is
/// only recorded as the run completes after a cancel.
fn claim_attached_answer(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: MessageId,
    run: RunId,
) -> Result<Message> {
    let q = question(tx.sql(), project, id)?;
    if q.state != QuestionState::Answered {
        return Err(conflict("question has no answer"));
    }
    let attached:Option<String>=tx.sql().query_row("SELECT run_id FROM question_attachments WHERE project_id=?1 AND message_id=?2 AND detached_at IS NULL",params![project.to_string(),id.0],|r|r.get(0)).optional()?;
    if attached.as_deref() != Some(&run.to_string()) {
        return Err(conflict("answer belongs to another asking run"));
    }
    let reply = q.reply.ok_or_else(|| conflict("question has no answer"))?;
    if claimed_by(tx.sql(), project, reply.id)?.is_some_and(|owner| owner != run) {
        return Err(conflict("answer already claimed"));
    }
    tx.sql().execute(
        "UPDATE messages SET claimed_by=?1 WHERE project_id=?2 AND id=?3 AND claimed_by IS NULL",
        params![run.to_string(), project.to_string(), reply.id.0],
    )?;
    changed(tx, project);
    Ok(reply)
}

/// The notes to `identity` it marked read at or after `since` (RFC 3339), oldest first: what
/// the inbox keeps under "Read today", read from the store, not from a page's memory.
pub fn read_notes(
    sql: &Connection,
    project: ProjectId,
    identity: &str,
    since: &str,
) -> Result<Vec<Message>> {
    let mut stmt = sql.prepare_cached(&format!(
        "SELECT {MESSAGE_JSON} FROM messages WHERE project_id=?1 AND \"to\"=?2 AND needs_reply=0 AND read_at>=?3 AND id<=coalesce((SELECT cursor FROM readers r WHERE r.project_id=messages.project_id AND r.identity=?2 AND r.stream='owner' AND r.thread=messages.thread),0) ORDER BY thread,id"
    ))?;
    let json = stmt
        .query_map(params![project.to_string(), identity, since], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    json.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(StoreError::from))
        .collect()
}

/// A step's conversation, oldest first: its own thread (`step-<step>`) and every message it sent
/// or was sent on any other thread (a question it put to another step, that step's reply).
pub fn conversation(sql: &Connection, project: ProjectId, step: &str) -> Result<Vec<Message>> {
    let mut stmt = sql.prepare_cached(&format!(
        "SELECT {MESSAGE_JSON} FROM messages WHERE project_id=?1 AND (thread=?2 OR \"to\"=?3 OR \"from\"=?3) ORDER BY id"
    ))?;
    let json = stmt
        .query_map(
            params![project.to_string(), format!("step-{step}"), step],
            |r| r.get::<_, String>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    json.into_iter()
        .map(|s| serde_json::from_str(&s).map_err(StoreError::from))
        .collect()
}

/// The caller's inbox: its open questions lead, followed by its unread notes grouped
/// by thread, unread by `identity`'s read watermarks. A session's inbox also holds the open
/// questions sent to the retired `orchestrator` before sessions named themselves (they are
/// any session's to answer). History is every message in any conversation `identity` took
/// part in.
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
    let legacy = if identity::is_session(identity) {
        format!(" OR (\"to\"='{LEGACY_ORCHESTRATOR}' AND {open})")
    } else {
        String::new()
    };
    let condition=match view {
        MessageView::Inbox => format!("(\"to\"=?4 AND (({open}) OR (needs_reply=0 AND id>coalesce((SELECT cursor FROM readers r WHERE r.project_id=messages.project_id AND r.identity=?4 AND r.stream='owner' AND r.thread=messages.thread),0)))){legacy}"),
        MessageView::Questions=>open.into(),
        MessageView::History=>"thread IN (SELECT thread FROM messages WHERE project_id=?1 AND (\"to\"=?4 OR \"from\"=?4))".into(),
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

/// A message a run of `step` acknowledged is in that session, so no later run is assigned it
/// again: the step's cursor moves over each message after it that some run of the step's
/// generation has acknowledged, and stops at the first one none has (it is assigned again).
/// A stale run of an older work generation never moves it. Scattered items each have their
/// own session and keep the step's batch window instead.
fn delivered_through(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    step: &str,
    info: &RunInfo,
) -> Result<()> {
    let p = project.to_string();
    let Some(cursor) = tx
        .sql()
        .query_row(
            "SELECT delivery_cursor FROM steps WHERE project_id=?1 AND step_id=?2 AND generation=?3 AND work_generation=?4",
            params![p, step, info.generation, info.work],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(());
    };
    let runs: Vec<String> = tx
        .sql()
        .prepare("SELECT run_id FROM runs WHERE project_id=?1 AND step_id=?2 AND generation=?3")?
        .query_map(params![p, step, info.generation], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let messages: Vec<i64> = tx
        .sql()
        .prepare("SELECT id FROM messages WHERE project_id=?1 AND \"to\"=?2 AND id>?3 ORDER BY id")?
        .query_map(params![p, step, cursor], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut acknowledged = tx.sql().prepare(
        "SELECT EXISTS(SELECT 1 FROM message_deliveries WHERE project_id=?1 AND run_id=?2 AND message_id=?3 AND acknowledged_at IS NOT NULL)",
    )?;
    let mut through = cursor;
    'messages: for id in messages {
        for run in &runs {
            if acknowledged.query_row(params![p, run, id], |r| r.get::<_, bool>(0))? {
                through = id;
                continue 'messages;
            }
        }
        break;
    }
    drop(acknowledged);
    if through > cursor {
        tx.sql().execute(
            "UPDATE steps SET delivery_cursor=max(delivery_cursor,?1) WHERE project_id=?2 AND step_id=?3 AND generation=?4 AND work_generation=?5",
            params![through, p, step, info.generation, info.work],
        )?;
    }
    Ok(())
}

/// Dispatch acknowledgements are independent of RunStarted's batch cursor.
pub fn acknowledge_delivery(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    run: RunId,
    id: MessageId,
) -> Result<()> {
    let info = run_info(tx.sql(), project, run)?;
    if tx.sql().execute("UPDATE message_deliveries SET acknowledged_at=coalesce(acknowledged_at,?1) WHERE project_id=?2 AND run_id=?3 AND message_id=?4",params![now()?,project.to_string(),run.to_string(),id.0])?==0 { return Err(missing("message was not assigned to this run")); }
    if let Some(step) = &info.step
        && info.item < 0
    {
        delivered_through(tx, project, step, &info)?;
    }
    let owns_answer:Option<i64>=tx.sql().query_row(
        "SELECT q.message_id FROM question_attachments q JOIN messages m ON m.project_id=q.project_id AND m.id=q.message_id WHERE q.project_id=?1 AND q.run_id=?2 AND q.detached_at IS NULL AND m.resolved_by=?3 AND m.closed_at IS NULL",
        params![project.to_string(),run.to_string(),id.0],|r|r.get(0)).optional()?;
    if let Some(question_id) = owns_answer {
        claim_attached_answer(tx, project, MessageId(question_id), run)?;
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

/// The owner's mark on a cancelled step it has set aside: the stream its `readers` rows are
/// kept under, a row a step (its `thread`), its cursor the step's work generation,
/// `unread_alert_min` its generation (unused for alerts on this stream), and its
/// `heartbeat_at` when the owner set it (`dismissed_lately`).
const DISMISSED_STREAM: &str = "dismissed";

/// Legacy cursors are record sequences and have no generation. Bind them once to
/// the current cancel, preserving Undo's timestamp. Drop marks on other states so
/// a future cancel cannot inherit one. Called only on a writable startup connection.
pub(crate) fn upgrade_dismissals(sql: &mut Connection) -> Result<()> {
    let transaction = sql.transaction()?;
    transaction.execute(
        &format!(
            "DELETE FROM readers WHERE identity='owner' AND stream=?1 AND unread_alert_min IS NULL
         AND NOT EXISTS (SELECT 1 FROM steps s WHERE s.project_id=readers.project_id
           AND s.step_id=readers.thread AND s.status='failed' AND {})",
            sluice_model::shown::cancel_sql("s.error", "$")
        ),
        [DISMISSED_STREAM],
    )?;
    transaction.execute(
        "UPDATE readers SET
           cursor=(SELECT s.work_generation FROM steps s WHERE s.project_id=readers.project_id AND s.step_id=readers.thread),
           unread_alert_min=(SELECT s.generation FROM steps s WHERE s.project_id=readers.project_id AND s.step_id=readers.thread)
         WHERE identity='owner' AND stream=?1 AND unread_alert_min IS NULL",
        [DISMISSED_STREAM],
    )?;
    transaction.commit()?;
    Ok(())
}
/// Set a cancelled step aside, or back (`StepDismiss`): only a step whose failure is a cancel.
pub fn dismiss(tx: &mut WriteTransaction<'_>, request: StepDismiss) -> Result<()> {
    let project = request.project.to_string();
    let step = request.step.as_str();
    if request.dismissed {
        let found: Option<(String, Option<String>, i64, i64)> = tx
            .sql()
            .query_row(
                "SELECT status,error,generation,work_generation FROM steps WHERE project_id=?1 AND step_id=?2",
                params![project, step],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((status, error, generation, work)) = found else {
            return Err(missing(format!("no step {step}")));
        };
        if status != "failed" || !error.is_some_and(|e| sluice_model::shown::stored_is_cancel(&e)) {
            return Err(invalid(format!(
                "step {step} is not cancelled: only a cancel can be dismissed"
            )));
        }
        tx.sql().execute(
            "INSERT INTO readers(project_id,identity,stream,thread,cursor,unread_alert_min,heartbeat_at) VALUES (?1,'owner',?2,?3,?4,?5,?6) ON CONFLICT(project_id,identity,stream,thread) DO UPDATE SET cursor=excluded.cursor,unread_alert_min=excluded.unread_alert_min,heartbeat_at=excluded.heartbeat_at",
            params![project, DISMISSED_STREAM, step, work, generation, now()?],
        )?;
    } else {
        tx.sql().execute(
            "DELETE FROM readers WHERE project_id=?1 AND identity='owner' AND stream=?2 AND thread=?3",
            params![project, DISMISSED_STREAM, step],
        )?;
    }
    changed(tx, request.project);
    Ok(())
}
/// The project's cancelled steps the owner set aside, each still on the cancel it was set aside
/// at (cancelled again since, a step stands out again).
pub fn dismissed(
    sql: &Connection,
    project: ProjectId,
) -> Result<std::collections::BTreeSet<String>> {
    let mut q = sql.prepare_cached(
        "SELECT r.thread,s.error FROM readers r JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.thread WHERE r.project_id=?1 AND r.identity='owner' AND r.stream=?2 AND s.status='failed' AND r.cursor=s.work_generation AND r.unread_alert_min=s.generation",
    )?;
    let marks = q
        .query_map(params![project.to_string(), DISMISSED_STREAM], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut out = std::collections::BTreeSet::new();
    for (step, error) in marks {
        if error.is_some_and(|e| sluice_model::shown::stored_is_cancel(&e)) {
            out.insert(step);
        }
    }
    Ok(out)
}
/// The cancels the owner set aside in the last `minutes`, still set aside, each with when: the
/// plan says each, with its Undo, for that long.
pub fn dismissed_lately(
    sql: &Connection,
    project: ProjectId,
    minutes: f64,
) -> Result<std::collections::BTreeMap<String, String>> {
    let still = dismissed(sql, project)?;
    let mut q = sql.prepare_cached(
        "SELECT thread,heartbeat_at FROM readers WHERE project_id=?1 AND identity='owner' AND stream=?2 AND heartbeat_at IS NOT NULL AND julianday(heartbeat_at)>=julianday('now')-?3/1440.0",
    )?;
    let rows = q
        .query_map(
            params![project.to_string(), DISMISSED_STREAM, minutes],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(step, _)| still.contains(step))
        .collect())
}
/// The owner's marks on items of a plan's Needs you it set aside (`NeedsDismiss`): the stream
/// its `readers` rows are kept under, a row an item (its `thread` the item's key). A stop's
/// (`stop:<step>`) `cursor` is the step's work generation and `unread_alert_min` its
/// generation, so a retry or recreating the step brings it back; any other item's `cursor` is
/// its stamp. `heartbeat_at` is when the owner set it aside.
const NEEDS_STREAM: &str = "needs";
/// An item of Needs you the owner set aside, still as it was (`needs_set_aside`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetAside {
    pub key: String,
    pub stamp: i64,
    /// When it was set aside (RFC 3339).
    pub at: String,
}
/// Set items of a plan's Needs you aside, or back (`NeedsDismiss`). A key is `stop:<step>`,
/// `quiet:<step>` or `ask:<message>`; a stop's step must exist.
pub fn needs_dismiss(tx: &mut WriteTransaction<'_>, request: NeedsDismiss) -> Result<()> {
    let project = request.project.to_string();
    if request.items.is_empty() || request.items.len() > 500 {
        return Err(invalid("set aside one to 500 items"));
    }
    for item in &request.items {
        let (kind, rest) = item.key.split_once(':').unwrap_or(("", ""));
        if !matches!(kind, "stop" | "quiet" | "ask") || rest.is_empty() || item.key.len() > 512 {
            return Err(invalid(format!("{} is not an item of Needs you", item.key)));
        }
        if !request.dismissed {
            tx.sql().execute(
                "DELETE FROM readers WHERE project_id=?1 AND identity='owner' AND stream=?2 AND thread=?3",
                params![project, NEEDS_STREAM, item.key],
            )?;
            continue;
        }
        let (cursor, generation) = if kind == "stop" {
            let found: Option<(i64, i64)> = tx
                .sql()
                .query_row(
                    "SELECT work_generation,generation FROM steps WHERE project_id=?1 AND step_id=?2",
                    params![project, rest],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((work, generation)) = found else {
                return Err(missing(format!("no step {rest}")));
            };
            (work, Some(generation))
        } else {
            (item.stamp.max(0), None)
        };
        tx.sql().execute(
            "INSERT INTO readers(project_id,identity,stream,thread,cursor,unread_alert_min,heartbeat_at) VALUES (?1,'owner',?2,?3,?4,?5,?6) ON CONFLICT(project_id,identity,stream,thread) DO UPDATE SET cursor=excluded.cursor,unread_alert_min=excluded.unread_alert_min,heartbeat_at=excluded.heartbeat_at",
            params![project, NEEDS_STREAM, item.key, cursor, generation, now()?],
        )?;
    }
    changed(tx, request.project);
    Ok(())
}
/// The items of a project's Needs you the owner set aside: a stop's only while its step is on
/// the generations it was set aside at; the rest as kept (the page holds each to its stamp).
pub fn needs_set_aside(sql: &Connection, project: ProjectId) -> Result<Vec<SetAside>> {
    let mut q = sql.prepare_cached(
        "SELECT r.thread,r.cursor,coalesce(r.heartbeat_at,'') FROM readers r WHERE r.project_id=?1 AND r.identity='owner' AND r.stream=?2 AND (substr(r.thread,1,5)<>'stop:' OR EXISTS (SELECT 1 FROM steps s WHERE s.project_id=r.project_id AND s.step_id=substr(r.thread,6) AND s.work_generation=r.cursor AND s.generation=r.unread_alert_min)) ORDER BY r.heartbeat_at DESC,r.thread",
    )?;
    let rows = q
        .query_map(params![project.to_string(), NEEDS_STREAM], |r| {
            Ok(SetAside {
                key: r.get(0)?,
                stamp: r.get(1)?,
                at: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
pub fn mark_read(tx: &mut WriteTransaction<'_>, read: MarkRead) -> Result<MessageId> {
    if read.through.0 < 0 || read.identity.trim().is_empty() {
        return Err(invalid("read identity and watermark are invalid"));
    }
    let previous = reader(
        tx.sql(),
        read.project,
        &read.identity,
        OWNER_STREAM,
        &read.thread,
    )?
    .cursor
    .0;
    // A global watermark is clamped to the last actually existing message in this thread.
    let watermark: i64 = tx.sql().query_row(
        "SELECT coalesce(max(id),0) FROM messages WHERE project_id=?1 AND thread=?2 AND id<=?3",
        params![read.project.to_string(), read.thread, read.through.0],
        |r| r.get(0),
    )?;
    tx.sql().execute("INSERT INTO readers(project_id,identity,stream,thread,cursor) VALUES (?1,?2,'owner',?3,?4) ON CONFLICT(project_id,identity,stream,thread) DO UPDATE SET cursor=max(cursor,excluded.cursor)",params![read.project.to_string(),read.identity,read.thread,watermark])?;
    // each message to the reader it takes off the unread list, stamped once with when
    tx.sql().execute("UPDATE messages SET read_at=?5 WHERE project_id=?1 AND thread=?2 AND \"to\"=?3 AND id<=?4 AND read_at IS NULL",params![read.project.to_string(),read.thread,read.identity,watermark,now()?])?;
    if watermark > previous {
        let undo = serde_json::json!({"previous": previous, "through": watermark, "at": now()?});
        tx.sql().execute("UPDATE readers SET read_undo=?4 WHERE project_id=?1 AND identity=?2 AND stream='owner' AND thread=?3", params![read.project.to_string(), read.identity, read.thread, undo.to_string()])?;
    }
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

/// A page's explicit read or Undo is atomic across its displayed threads and projects.
pub fn mark_notes(tx: &mut WriteTransaction<'_>, marks: Vec<MarkRead>, undo: bool) -> Result<()> {
    if marks.is_empty() || marks.len() > 50 {
        return Err(invalid("name between 1 and 50 threads"));
    }
    for mark in marks {
        if undo {
            undo_read(tx, mark)?;
        } else {
            mark_read(tx, mark)?;
        }
    }
    Ok(())
}

/// Restore only the last explicit read, while it still matches the page and is recent.
fn undo_read(tx: &mut WriteTransaction<'_>, read: MarkRead) -> Result<()> {
    let previous: Option<i64> = tx.sql().query_row(
        "SELECT json_extract(read_undo,'$.previous') FROM readers WHERE project_id=?1 AND identity=?2 AND stream='owner' AND thread=?3 AND cursor=?4 AND json_extract(read_undo,'$.through')=?4 AND json_extract(read_undo,'$.at')>=strftime('%Y-%m-%dT%H:%M:%f','now','-10 minutes')",
        params![read.project.to_string(), read.identity, read.thread, read.through.0], |r| r.get(0)
    ).optional()?.flatten();
    let Some(previous) = previous else {
        return Err(conflict("the read mark changed or Undo expired"));
    };
    tx.sql().execute("UPDATE readers SET cursor=?4,read_undo=NULL WHERE project_id=?1 AND identity=?2 AND stream='owner' AND thread=?3", params![read.project.to_string(),read.identity,read.thread,previous])?;
    tx.sql().execute("UPDATE messages SET read_at=NULL WHERE project_id=?1 AND thread=?2 AND \"to\"=?3 AND id>?4 AND id<=?5", params![read.project.to_string(),read.thread,read.identity,previous,read.through.0])?;
    changed(tx, read.project);
    Ok(())
}

/// History is shared across the owner's devices. An old page may advance, never regress it;
/// Undo restores the previous mark only if no other client has advanced it since.
pub fn history_seen(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    through: u64,
    undo: bool,
) -> Result<()> {
    let through = i64::try_from(through).map_err(|_| invalid("seen watermark is too large"))?;
    resolve_project(tx.sql(), &sluice_model::ids::ProjectSelector::Id(project))?;
    if undo {
        if tx.sql().execute("UPDATE projects SET history_seen=history_seen_previous,history_seen_previous=0 WHERE project_id=?1 AND history_seen=?2 AND history_seen>0", params![project.to_string(),through])? == 0 {
            return Err(conflict("the seen mark changed; reload before Undo"));
        }
    } else {
        tx.sql().execute("UPDATE projects SET history_seen_previous=history_seen,history_seen=?2 WHERE project_id=?1 AND history_seen<?2", params![project.to_string(),through])?;
    }
    tx.changed(Some(project), "history");
    Ok(())
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

/// Store-side message wake predicate for next; trailing notes remain held by the caller. A
/// question wakes a named reader when it is to it (or, stored before identities, to the
/// retired `orchestrator` or to nobody), and an unnamed one when it is to any session.
pub fn message_wakes(
    sql: &Connection,
    project: ProjectId,
    msg: &Message,
    me: Option<&str>,
) -> Result<bool> {
    if me.is_some_and(|me| msg.from == me) {
        return Ok(false);
    }
    if msg.verb == MessageVerb::Ask
        && msg.to.as_deref().is_none_or(|to| match me {
            Some(me) => to == me || to == LEGACY_ORCHESTRATOR,
            None => identity::is_session(to) || to == LEGACY_ORCHESTRATOR,
        })
    {
        return Ok(true);
    }
    if msg.verb == MessageVerb::Reply
        && let Some(id) = msg.to_message
    {
        return Ok(message(sql, project, id)?.is_question());
    }
    // An execution-limit notice from Sluice wakes the session it is addressed to.
    if msg.verb == MessageVerb::Say
        && msg.from == identity::SLUICE
        && msg
            .data
            .as_ref()
            .is_some_and(|data| data.as_value().get("execution_limit").is_some())
        && msg.to.as_deref().is_some_and(|to| match me {
            Some(me) => to == me,
            None => identity::is_session(to),
        })
    {
        return Ok(true);
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

/// An owner question whose notification is reserved and not yet claimed for dispatch.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingNotify {
    pub project: ProjectId,
    pub project_name: String,
    pub attempt: AttemptId,
    pub message: Message,
    /// The question is still open; an answered or closed one is not sent.
    pub open: bool,
}
/// Reserved notifications, oldest first.
pub fn notify_pending(sql: &Connection) -> Result<Vec<PendingNotify>> {
    let mut stmt = sql.prepare("SELECT n.project_id,p.name,n.attempt_id,n.message_id FROM notification_attempts n JOIN projects p ON p.project_id=n.project_id WHERE n.outcome='reserved' AND p.deleted_at IS NULL ORDER BY n.reserved_at,n.message_id")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    let invalid_row = |e: String| StoreError::InvalidDatabase(format!("invalid notify row: {e}"));
    rows.into_iter()
        .map(|(project, project_name, attempt, id)| {
            let project: ProjectId = project.parse().map_err(|e| invalid_row(format!("{e}")))?;
            let message = message(sql, project, MessageId(id))?;
            let open: bool = sql.query_row(
                "SELECT resolved_by IS NULL AND closed_at IS NULL FROM messages WHERE project_id=?1 AND id=?2",
                params![project.to_string(), id],
                |r| r.get(0),
            )?;
            Ok(PendingNotify {
                project,
                project_name,
                attempt: attempt.parse().map_err(|e| invalid_row(format!("{e}")))?,
                message,
                open,
            })
        })
        .collect()
}
/// Claim a reservation for dispatch before the command starts: it becomes `uncertain` with no
/// finish time until its result is recorded, so a crash in between is never replayed.
/// False when it was already claimed or settled.
pub fn notify_claim(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: MessageId,
    attempt: AttemptId,
) -> Result<bool> {
    let claimed = tx.sql().execute("UPDATE notification_attempts SET outcome='uncertain' WHERE project_id=?1 AND message_id=?2 AND attempt_id=?3 AND outcome='reserved'",params![project.to_string(),id.0,attempt.to_string()])?;
    if claimed == 1 {
        tx.changed(Some(project), "messages");
    }
    Ok(claimed == 1)
}
/// Claims an earlier coordinator left unfinished, settled as `uncertain` with `error`.
pub fn notify_abandoned(tx: &mut WriteTransaction<'_>, error: PublicError) -> Result<usize> {
    let mut stmt = tx.sql().prepare("SELECT project_id,message_id,attempt_id FROM notification_attempts WHERE outcome='uncertain' AND finished_at IS NULL")?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    let count = rows.len();
    for (project, id, attempt) in rows {
        let invalid_row =
            |e: String| StoreError::InvalidDatabase(format!("invalid notify row: {e}"));
        notify_result(
            tx,
            project.parse().map_err(|e| invalid_row(format!("{e}")))?,
            MessageId(id),
            attempt.parse().map_err(|e| invalid_row(format!("{e}")))?,
            NotificationOutcome::Uncertain,
            None,
            Some(error.clone()),
        )?;
    }
    Ok(count)
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
    // A claim (notify_claim) is uncertain without a finish time until its result lands.
    let claimed: bool = tx.sql().query_row(
        "SELECT outcome='uncertain' AND finished_at IS NULL FROM notification_attempts WHERE project_id=?1 AND message_id=?2",
        params![project.to_string(), id.0],
        |r| r.get(0),
    )?;
    if before.outcome != NotificationOutcome::Reserved && !claimed {
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
            message_wakes(tx.sql(), project, &msg, Some(identity))?
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
    let alert = Post {
        project: ProjectSelector::Id(project),
        speaker: Speaker::Sluice,
        body: format!(
            "No reader progress after record {seq}. Resnapshot status/messages and resume reading."
        ),
        verb: Verb::Ask {
            to: OWNER_STREAM.into(),
            title: Some(format!(
                "No orchestrator has read for {minutes} min (seq {seq})"
            )),
            ui: None,
            input: None,
            data: Some(JsonValue::try_from(
                serde_json::json!({"unread_record":seq,"reader":identity}),
            )?),
        },
    };
    Ok(Some(post(tx, alert, &NoPlanInputs)?.message))
}
