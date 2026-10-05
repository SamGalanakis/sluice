use crate::{
    commands::*,
    error::PublicError,
    ids::*,
    rpc::{JsonMap, JsonValue},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", deny_unknown_fields)]
#[non_exhaustive]
pub enum Event {
    #[serde(rename = "plan.edit")]
    PlanEdit {
        rev: Revision,
        author: String,
        reason: String,
        ops: Vec<PatchOperation>,
    },
    #[serde(rename = "plan.input")]
    PlanInput {
        rev: Revision,
        author: String,
        reason: String,
        name: String,
        value: JsonValue,
    },
    #[serde(rename = "step.output")]
    StepOutput {
        rev: Revision,
        author: String,
        reason: String,
        step: StepId,
        outputs: JsonMap,
        force: bool,
    },
    #[serde(rename = "step.retry")]
    StepRetry {
        rev: Revision,
        author: String,
        reason: String,
        step: StepId,
        work: WorkGeneration,
    },
    #[serde(rename = "step.cancel")]
    StepCancel {
        step: StepId,
        author: String,
        reason: String,
    },
    #[serde(rename = "step.submit")]
    StepSubmit {
        step: StepId,
        run: RunId,
        outputs: JsonMap,
        author: Option<String>,
    },
    #[serde(rename = "step.settle")]
    StepSettle {
        step: StepId,
        run: RunId,
        author: String,
        reason: String,
    },
    #[serde(rename = "step.status")]
    StepStatus {
        step: StepId,
        from: Option<StepStatus>,
        to: StepStatus,
        error: Option<PublicError>,
        run_ids: Vec<RunId>,
        needs: JsonMap,
    },
    #[serde(rename = "step.lease")]
    StepLease {
        step: Option<StepId>,
        run: RunId,
        lease: LeaseId,
        resource: String,
        amount: u64,
        state: LeaseState,
        reason: Option<String>,
    },
    #[serde(rename = "step.queued")]
    StepQueued {
        step: StepId,
        needs: JsonMap,
        resources: JsonMap,
        reason: String,
    },
    #[serde(rename = "call")]
    Call {
        call: RunId,
        #[serde(rename = "fn")]
        name: String,
        status: StepStatus,
        inputs: Option<JsonMap>,
        outputs: Option<JsonMap>,
        error: Option<PublicError>,
        direct: bool,
        author: Option<String>,
    },
    #[serde(rename = "message")]
    Message(
        #[serde(with = "message_event")]
        #[schemars(with = "message_event::Fields<'static>")]
        Box<Message>,
    ),
    #[serde(rename = "project.pause")]
    ProjectPause {
        paused: bool,
        reason: Option<String>,
        author: String,
    },
    #[serde(rename = "project.archive")]
    ProjectArchive {
        archived: bool,
        reason: Option<String>,
        author: String,
    },
    #[serde(rename = "project.update")]
    ProjectUpdate {
        fields: Vec<String>,
        reason: Option<String>,
        author: String,
    },
    /// A board set or cleared; the program itself is not recorded.
    #[serde(rename = "project.board")]
    ProjectBoard {
        rev: Revision,
        cleared: bool,
        reason: Option<String>,
        author: String,
    },
    #[serde(rename = "project.rename")]
    ProjectRename {
        old_name: ProjectName,
        new_name: ProjectName,
        author: String,
    },
    #[serde(rename = "project.delete")]
    ProjectDelete {
        project_id: ProjectId,
        name: ProjectName,
        author: String,
    },
    #[serde(rename = "project.capacity")]
    ProjectCapacity {
        resource: String,
        #[serde(rename = "fn")]
        name: String,
        capacity: Option<u64>,
        error: Option<PublicError>,
    },
    #[serde(rename = "project.notify")]
    ProjectNotify {
        message: MessageId,
        outcome: NotificationOutcome,
        error: Option<PublicError>,
    },
    #[serde(rename = "run.adopt")]
    RunAdopt {
        run: RunId,
        step: Option<StepId>,
        call: Option<RunId>,
        outcome: AdoptionOutcome,
    },
    #[serde(rename = "run.orphan")]
    RunOrphan { run: RunId },
    #[serde(rename = "run.completion_action.register")]
    RunCompletionActionRegistered {
        run: RunId,
        target: CompletionActionTarget,
        message: String,
        author: String,
    },
    #[serde(rename = "run.completion_action")]
    RunCompletionAction {
        run: RunId,
        outcome: CompletionActionOutcome,
        author: String,
    },
    #[serde(rename = "unit.settled")]
    UnitSettled {
        unit: UnitName,
        work: WorkGeneration,
        steps: Vec<UnitStep>,
    },
}

mod message_event {
    use super::*;

    /// The message record: a log record (kind "message") flattens these fields, so
    /// the message's own time is `posted_at`. A question's state is never recorded.
    #[derive(Serialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub(super) struct Fields<'a> {
        id: MessageId,
        verb: MessageVerb,
        from: &'a str,
        to: Option<&'a str>,
        thread: &'a str,
        body: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ui: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<&'a JsonValue>,
        #[serde(skip_serializing_if = "Option::is_none")]
        run: Option<RunId>,
        posted_at: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        to_message: Option<MessageId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        answer: Option<&'a MessageAnswer>,
    }

    /// Records written before the verbs carry needs_reply, reply_to and claimed_by
    /// instead; they read as the one current shape.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Stored {
        id: MessageId,
        verb: Option<MessageVerb>,
        thread: String,
        from: String,
        to: Option<String>,
        title: Option<String>,
        body: String,
        needs_reply: Option<bool>,
        reply_to: Option<MessageId>,
        to_message: Option<MessageId>,
        answer: Option<MessageAnswer>,
        ui: Option<String>,
        input: Option<String>,
        data: Option<JsonValue>,
        run: Option<RunId>,
        #[serde(rename = "posted_at", alias = "at")]
        at: String,
        #[allow(dead_code)]
        claimed_by: Option<RunId>,
    }

    pub(super) fn serialize<S: serde::Serializer>(
        message: &Message,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Fields {
            id: message.id,
            verb: message.verb,
            from: &message.from,
            to: message.to.as_deref(),
            thread: &message.thread,
            body: &message.body,
            title: message.title.as_deref(),
            ui: message.ui.as_deref(),
            input: message.input.as_deref(),
            data: message.data.as_ref(),
            run: message.run,
            posted_at: &message.at,
            to_message: message.to_message,
            answer: message.answer.as_ref(),
        }
        .serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Box<Message>, D::Error> {
        let stored = Stored::deserialize(deserializer)?;
        let to_message = stored.to_message.or(stored.reply_to);
        let verb = stored.verb.unwrap_or(if to_message.is_some() {
            MessageVerb::Reply
        } else if stored.needs_reply.unwrap_or(false) {
            MessageVerb::Ask
        } else {
            MessageVerb::Say
        });
        Ok(Box::new(Message {
            id: stored.id,
            verb,
            from: stored.from,
            to: stored.to,
            thread: stored.thread,
            body: stored.body,
            title: stored.title,
            ui: stored.ui,
            input: stored.input,
            data: stored.data,
            run: stored.run,
            at: stored.at,
            to_message,
            answer: stored.answer,
            state: None,
            answered_by: None,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdoptionOutcome {
    Watching,
    Finished,
    Lost,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotificationOutcome {
    Reserved,
    Dispatched,
    Failed,
    Uncertain,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Record {
    pub seq: RecordSeq,
    pub at: String,
    pub project: Option<ProjectId>,
    #[serde(flatten)]
    pub event: Event,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitStep {
    pub id: StepId,
    pub status: StepStatus,
    pub held: bool,
    pub outputs: Option<JsonMap>,
    pub omitted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeCursor {
    pub after: RecordSeq,
    pub projects: Vec<ProjectId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangeBatch {
    pub records: Vec<Record>,
    pub cursor: ChangeCursor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamBatch {
    pub html: Vec<HtmlPatch>,
    pub version: StreamVersion,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HtmlPatch {
    pub target: String,
    pub html: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamVersion {
    pub view: String,
    pub version: u64,
    pub cursor: RecordSeq,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "event",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StreamEvent {
    Elements(HtmlPatch),
    Version(StreamVersion),
}
