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
    Message(Box<Message>),
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
