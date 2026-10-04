//! message.post and message.wait: the step-facing message fns over the durable store.
//!
//! message.truth lives in sluice_store::messages: posts, take-up, first answering reply,
//! closes, claims and attachments are all store-side (p2-03). These fns only adapt a
//! run's inputs to that API and block, cancellably, on committed `messages` changes.
//! A wait never holds a writer transaction: it snapshots through the ReadPool and parks
//! on a durable-cursor subscription.
//!
//! The shared BuiltinDescriptor/FnFailure contract is p4-02's unified
//! builtins/descriptor.rs. The context here is wider than BuiltinCtx because these fns
//! write and read the home's database as the step's run.

use super::descriptor::{BuiltinDescriptor, BuiltinIcon, DEFAULT_RETRY, FnFailure};
use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::{
    commands::{Message, MessageAnswer, MessagePost, MessageView},
    error::PublicError,
    ids::{MessageId, ProjectId, ProjectSelector, RunId, StepId},
    rpc::{JsonMap, JsonValue},
    types::Type,
};
use sluice_store::{
    ChangeKey, ReadPool, Result as StoreResult, RetrySafety, StoreError, WriteTransaction, Writer,
    messages::{
        self, AskResult, InputAnswer, PlanInputSetter, QuestionState, ask, claim_answer,
        message_post,
    },
    records,
};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const DEFAULT_TIMEOUT: u64 = 300;

/// The message icons: the inbox.ask and thread.wait status glyphs, embedded like
/// descriptor.rs does.
const BUBBLE: BuiltinIcon = BuiltinIcon {
    media_type: "image/svg+xml",
    bytes: include_bytes!("../../assets/icons/bubble.svg"),
};
const ENVELOPE: BuiltinIcon = BuiltinIcon {
    media_type: "image/svg+xml",
    bytes: include_bytes!("../../assets/icons/envelope.svg"),
};

/// What a message fn sees of its run: the durable home it posts and waits in.
/// `run`/`step` are the executing run's identity — `message.post` needs them to own
/// its question (attachments, take-up and claims); `message.wait` needs neither.
pub type MutationGuard = Arc<dyn Fn(&WriteTransaction<'_>) -> StoreResult<()> + Send + Sync>;
#[derive(Clone)]
pub struct MessageCtx {
    pub project: ProjectId,
    pub step: Option<StepId>,
    pub run: Option<RunId>,
    pub writer: Writer,
    pub reads: ReadPool,
    pub plan_inputs: Arc<dyn PlanInputSetter + Send + Sync>,
    /// Fires when the run is cancelled; every wait polls it alongside the subscription.
    pub cancel: CancellationToken,
    pub mutation_guard: Option<MutationGuard>,
}

/// Adapts the context's trait object to the store's `&impl PlanInputSetter` generics.
struct ArcPlanInputs(Arc<dyn PlanInputSetter + Send + Sync>);
impl PlanInputSetter for ArcPlanInputs {
    fn set_input(&self, tx: &mut WriteTransaction<'_>, update: InputAnswer<'_>) -> StoreResult<()> {
        self.0.set_input(tx, update)
    }
}

/// Descriptors for the two message fns, matching the message_post tool plus `wait`.
pub fn descriptors() -> [BuiltinDescriptor; 2] {
    [
        BuiltinDescriptor {
            name: "message.post",
            doc: "Post a message to a thread of the project. needs_reply (default true for \
a new thread, false for a reply) makes it a question; false marks a note. wait: true \
blocks until the first answering reply, which it returns, and fails when the question is \
closed.",
            inputs: ports(&[
                ("body", "string"),
                ("thread", "string?"),
                ("to", "string?"),
                ("needs_reply", "boolean?"),
                ("reply_to", "int?"),
                ("answer", "answer?"),
                ("title", "string?"),
                ("ui", "string?"),
                ("input", "string?"),
                ("data", "Any?"),
                ("from", "string?"),
                ("wait", "boolean?"),
            ]),
            outputs: ports(&[("id", "int"), ("reply", "Any?")]),
            open: false,
            submits: vec![],
            icon: Some(BUBBLE),
            retry: DEFAULT_RETRY,
        },
        BuiltinDescriptor {
            name: "message.wait",
            doc: "Wait for messages on a thread after since (with `to`: those addressed to \
it or to nobody). Returns as soon as there is one, or with none after timeout seconds \
(default 300). wake \"questions\": notes (needs_reply false) do not end the wait; they \
come back with the next question, or at the timeout.",
            inputs: ports(&[
                ("thread", "string"),
                ("since", "int?"),
                ("to", "string?"),
                ("timeout", "int?"),
                ("wake", "string?"),
            ]),
            outputs: ports(&[("messages", "Any[]"), ("last_seq", "int")]),
            open: false,
            submits: vec![],
            icon: Some(ENVELOPE),
            retry: DEFAULT_RETRY,
        },
    ]
}

pub async fn dispatch(
    name: &str,
    inputs: &JsonMap,
    ctx: &MessageCtx,
) -> Result<JsonMap, FnFailure> {
    match name {
        "message.post" => post(inputs, ctx).await,
        "message.wait" => wait(inputs, ctx).await,
        _ => Err(terminal(format!("unknown message builtin: {name}"))),
    }
}

/// message.post: one durable post through the writer, or the waiting ask when `wait`.
async fn post(inputs: &JsonMap, ctx: &MessageCtx) -> Result<JsonMap, FnFailure> {
    let waiting = flag(inputs, "wait")?;
    if waiting && opt_bool(inputs, "needs_reply")? == Some(false) {
        return Err(terminal(
            "a waiting post is a question: needs_reply cannot be false",
        ));
    }
    let post = MessagePost {
        project: ProjectSelector::Id(ctx.project),
        body: required_string(inputs, "body")?,
        thread: opt_string(inputs, "thread")?,
        to: opt_string(inputs, "to")?,
        needs_reply: opt_bool(inputs, "needs_reply")?,
        reply_to: opt_id(inputs, "reply_to")?.map(MessageId),
        answer: opt_answer(inputs)?,
        title: opt_string(inputs, "title")?,
        ui: opt_string(inputs, "ui")?,
        input: opt_string(inputs, "input")?,
        data: opt_json(inputs, "data"),
        from: opt_string(inputs, "from")?,
        run: ctx.run,
        // A step's posts are authored by the step; the store falls back to it for `from`.
        author: ctx.step.as_ref().map(ToString::to_string),
    };
    if !waiting {
        let inputs_setter = ArcPlanInputs(Arc::clone(&ctx.plan_inputs));
        let guard = ctx.mutation_guard.clone();
        let posted = ctx
            .writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                if let Some(guard) = guard {
                    guard(tx)?;
                }
                message_post(tx, post, &inputs_setter)
            })
            .await
            .map_err(store_failure)?;
        return output([("id", json!(posted.id.0)), ("reply", Value::Null)]);
    }
    let run = ctx
        .run
        .ok_or_else(|| terminal("waiting ask requires a run"))?;
    let inputs_setter = ArcPlanInputs(Arc::clone(&ctx.plan_inputs));
    let guard = ctx.mutation_guard.clone();
    let result = ctx
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            if let Some(guard) = guard {
                guard(tx)?;
            }
            ask(tx, post, &inputs_setter)
        })
        .await
        .map_err(store_failure)?;
    let (question, reply) = match result {
        AskResult::Answered { question, reply } => (question, *reply),
        AskResult::Closed(_) => return Err(terminal("question closed")),
        AskResult::Waiting(question) => {
            let reply = wait_for_answer(ctx, question.id, run).await?;
            (question, reply)
        }
    };
    output([
        ("id", json!(question.id.0)),
        (
            "reply",
            serde_json::to_value(&reply).map_err(|e| terminal(e.to_string()))?,
        ),
    ])
}

/// Park on committed `messages` changes until the question resolves, then claim its
/// answer for this run. A close or a stopped asking run ends the wait with an error.
async fn wait_for_answer(
    ctx: &MessageCtx,
    id: MessageId,
    run: RunId,
) -> Result<Message, FnFailure> {
    let project = ctx.project;
    // "messages" for the answer arriving, "status" for the asking run stopping —
    // a stop commits no message record, so messages alone would never wake us.
    let mut subscription = ctx
        .reads
        .subscribe(
            &ctx.writer,
            vec![
                ChangeKey::new(Some(project), "messages"),
                ChangeKey::new(Some(project), "status"),
            ],
        )
        .await
        .map_err(read_failure)?;
    loop {
        let question = ctx
            .reads
            .snapshot(move |sql| messages::question(sql, project, id))
            .await
            .map_err(read_failure)?;
        match question.state {
            QuestionState::Answered => {
                let guard = ctx.mutation_guard.clone();
                return ctx
                    .writer
                    .write(RetrySafety::Idempotent, move |tx| {
                        if let Some(guard) = guard {
                            guard(tx)?;
                        }
                        claim_answer(tx, project, id, run)
                    })
                    .await
                    .map_err(store_failure);
            }
            QuestionState::Closed => return Err(terminal("question closed")),
            QuestionState::Open => {
                if let Some(stopped) = question.stopped {
                    return Err(terminal(stopped));
                }
                tokio::select! {
                    _ = ctx.cancel.cancelled() => return Err(terminal("cancelled")),
                    result = subscription.wait() => { result.map_err(read_failure)?; }
                }
            }
        }
    }
}

/// message.wait: the thread's messages after `since`, returning on the first waking one
/// (any message, or only questions under `wake: "questions"`), on the timeout, or on
/// cancellation. Notes keep riding with the next question, as the Python fn held them.
async fn wait(inputs: &JsonMap, ctx: &MessageCtx) -> Result<JsonMap, FnFailure> {
    let thread = required_string(inputs, "thread")?;
    let since = opt_id(inputs, "since")?.unwrap_or(0);
    let to = opt_string(inputs, "to")?;
    let timeout = opt_id(inputs, "timeout")?.unwrap_or(DEFAULT_TIMEOUT as i64);
    let timeout =
        u64::try_from(timeout).map_err(|_| terminal("input \"timeout\" must be nonnegative"))?;
    let wake = opt_string(inputs, "wake")?.unwrap_or_else(|| "any".into());
    if !matches!(wake.as_str(), "any" | "questions") {
        return Err(terminal(format!(
            "wake: expected one of any, questions, got {wake:?}"
        )));
    }
    let questions = wake == "questions";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    let project = ctx.project;
    let mut subscription = ctx
        .reads
        .subscribe(&ctx.writer, vec![ChangeKey::new(Some(project), "messages")])
        .await
        .map_err(read_failure)?;
    loop {
        let found_thread = thread.clone();
        let (found, last_seq) = ctx
            .reads
            .snapshot(move |sql| {
                let found = messages::messages(
                    sql,
                    project,
                    MessageView::Thread,
                    Some(found_thread.as_str()),
                    Some(MessageId(since)),
                    "message.wait",
                )?;
                let last = records::bounds(sql, Some(project))?.1;
                Ok((found, last))
            })
            .await
            .map_err(read_failure)?;
        let found: Vec<Message> = found
            .into_iter()
            .filter(|msg| {
                to.as_ref()
                    .is_none_or(|t| msg.to.as_ref().is_none_or(|msg_to| msg_to == t))
            })
            .collect();
        let waking = if questions {
            found.iter().any(|msg| msg.needs_reply)
        } else {
            !found.is_empty()
        };
        if waking || tokio::time::Instant::now() >= deadline {
            return output([
                (
                    "messages",
                    serde_json::to_value(&found).map_err(|e| terminal(e.to_string()))?,
                ),
                ("last_seq", json!(last_seq.0)),
            ]);
        }
        tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(terminal("cancelled")),
            _ = tokio::time::sleep_until(deadline) => {}
            result = subscription.wait() => { result.map_err(read_failure)?; }
        }
    }
}

fn required_string(inputs: &JsonMap, name: &str) -> Result<String, FnFailure> {
    match inputs.0.get(name).map(JsonValue::as_value) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(terminal(format!("input \"{name}\" must be a string"))),
    }
}
fn opt_string(inputs: &JsonMap, name: &str) -> Result<Option<String>, FnFailure> {
    match inputs.0.get(name).map(JsonValue::as_value) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(terminal(format!("input \"{name}\" must be a string"))),
    }
}
fn opt_bool(inputs: &JsonMap, name: &str) -> Result<Option<bool>, FnFailure> {
    match inputs.0.get(name).map(JsonValue::as_value) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        _ => Err(terminal(format!("input \"{name}\" must be a boolean"))),
    }
}
fn flag(inputs: &JsonMap, name: &str) -> Result<bool, FnFailure> {
    Ok(opt_bool(inputs, name)?.unwrap_or(false))
}
fn opt_id(inputs: &JsonMap, name: &str) -> Result<Option<i64>, FnFailure> {
    match inputs.0.get(name).map(JsonValue::as_value) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| terminal(format!("input \"{name}\" must be an integer"))),
    }
}
fn opt_json(inputs: &JsonMap, name: &str) -> Option<JsonValue> {
    inputs
        .0
        .get(name)
        .filter(|value| !value.as_value().is_null())
        .cloned()
}
fn opt_answer(inputs: &JsonMap) -> Result<Option<MessageAnswer>, FnFailure> {
    match inputs.0.get("answer").map(JsonValue::as_value) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|e| {
                terminal(format!(
                    "input \"answer\" must be {{action, params?, values?}}: {e}"
                ))
            }),
    }
}
fn output<const N: usize>(pairs: [(&str, Value); N]) -> Result<JsonMap, FnFailure> {
    let mut map = IndexMap::with_capacity(N);
    for (name, value) in pairs {
        map.insert(
            name.to_string(),
            JsonValue::try_from(value).map_err(|e| terminal(e.to_string()))?,
        );
    }
    Ok(JsonMap(map))
}
fn store_failure(error: PublicError) -> FnFailure {
    match error {
        PublicError::Busy {
            message,
            retryable: true,
        } => FnFailure::Transient(message),
        other => FnFailure::Terminal(other.to_string()),
    }
}
/// Read-pool calls surface StoreError; route them through the public taxonomy first.
/// Reads are idempotent, so a busy snapshot is a transient failure.
fn read_failure(error: StoreError) -> FnFailure {
    store_failure(error.into_public(true))
}
fn terminal(message: impl Into<String>) -> FnFailure {
    FnFailure::Terminal(message.into())
}

/// The fn.json type grammar: names, `?` optional and `[]` list suffixes, plus the `answer`
/// record. A bare `answer` form expands to the reply-action record shape.
fn ty(form: &str) -> Type {
    if let Some(inner) = form.strip_suffix('?') {
        return Type::Optional(Box::new(ty(inner)));
    }
    if let Some(inner) = form.strip_suffix("[]") {
        return Type::List(Box::new(ty(inner)));
    }
    match form {
        "string" => Type::String,
        "int" => Type::Int,
        "float" => Type::Float,
        "boolean" => Type::Boolean,
        "Any" => Type::Any,
        "answer" => Type::Record(IndexMap::from([
            ("action".to_string(), Type::String),
            ("params".to_string(), Type::Optional(Box::new(Type::Any))),
            ("values".to_string(), Type::Optional(Box::new(Type::Any))),
        ])),
        other => panic!("unknown fn type {other:?}"),
    }
}
fn ports(forms: &[(&'static str, &'static str)]) -> Vec<(&'static str, Type)> {
    forms.iter().map(|(name, form)| (*name, ty(form))).collect()
}
