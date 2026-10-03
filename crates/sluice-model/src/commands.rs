use crate::{
    error::PublicError,
    events::{ChangeBatch, ChangeCursor, Record},
    ids::*,
    rpc::{JsonMap, JsonValue},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectIdentity {
    pub project_id: ProjectId,
    pub name: ProjectName,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdate {
    pub project: ProjectSelector,
    pub new_name: Option<ProjectName>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub resources: Option<JsonMap>,
    pub paused: Option<bool>,
    pub archived: Option<bool>,
    pub expected_settings_rev: Option<Revision>,
    pub reason: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectDelete {
    pub project: ProjectSelector,
    pub confirm_name: ProjectName,
    pub expected_settings_rev: Revision,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSelection {
    pub steps: Option<Vec<StepId>>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditOptions {
    pub expected: Option<Revision>,
    pub dry_run: bool,
    pub reason: String,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanPatch {
    pub project: ProjectSelector,
    pub rev: Revision,
    pub ops: Vec<PatchOperation>,
    pub dry_run: bool,
    pub reason: String,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepAdd {
    pub project: ProjectSelector,
    pub step: StepId,
    pub spec: JsonMap,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitAdd {
    pub project: ProjectSelector,
    pub recipe: String,
    pub unit: UnitName,
    pub params: JsonMap,
    pub start: bool,
    #[schemars(with = "std::collections::BTreeMap<String, Vec<String>>")]
    pub after: indexmap::IndexMap<String, Vec<String>>,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EdgeEdit {
    pub project: ProjectSelector,
    pub step: String,
    pub after: Vec<String>,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepUpdate {
    pub project: ProjectSelector,
    pub step: StepId,
    pub changes: JsonMap,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepRemove {
    pub project: ProjectSelector,
    pub selection: StepSelection,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepPause {
    pub project: ProjectSelector,
    pub selection: StepSelection,
    pub paused: bool,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitTag {
    pub project: ProjectSelector,
    pub unit: UnitName,
    pub add: Vec<String>,
    pub remove: Vec<String>,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanPrune {
    pub project: ProjectSelector,
    pub units: Option<Vec<UnitName>>,
    pub older_than_seconds: u64,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanSetInput {
    pub project: ProjectSelector,
    pub name: String,
    pub value: JsonValue,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSetInput {
    pub project: ProjectSelector,
    pub selection: StepSelection,
    pub inputs: JsonMap,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSetOutput {
    pub project: ProjectSelector,
    pub step: StepId,
    pub outputs: JsonMap,
    pub force: bool,
    pub reason: String,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepRetry {
    pub project: ProjectSelector,
    pub selection: StepSelection,
    pub message: Option<String>,
    pub reason: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepCancel {
    pub project: ProjectSelector,
    pub selection: StepSelection,
    pub reason: String,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSubmit {
    pub project: ProjectId,
    pub step: StepId,
    pub run: RunId,
    pub outputs: JsonMap,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageAnswer {
    pub action: String,
    pub params: Option<JsonMap>,
    pub values: Option<JsonMap>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessagePost {
    pub project: ProjectSelector,
    pub body: String,
    pub thread: Option<String>,
    pub to: Option<String>,
    pub needs_reply: Option<bool>,
    pub reply_to: Option<MessageId>,
    pub answer: Option<MessageAnswer>,
    pub title: Option<String>,
    pub ui: Option<String>,
    pub input: Option<String>,
    pub data: Option<JsonValue>,
    pub from: Option<String>,
    pub run: Option<RunId>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Messages {
    pub project: ProjectSelector,
    pub view: MessageView,
    pub thread: Option<String>,
    pub since: Option<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkRead {
    pub project: ProjectId,
    pub identity: String,
    pub thread: String,
    pub through: MessageId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FnCall {
    pub name: String,
    pub inputs: JsonMap,
    pub project: Option<ProjectSelector>,
    pub wait_seconds: Option<u64>,
    pub direct: bool,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogRead {
    pub project: Option<ProjectSelector>,
    pub since_seq: Option<RecordSeq>,
    pub kinds: Option<Vec<String>>,
    pub threads: Option<Vec<String>>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogWait {
    pub read: LogRead,
    pub timeout_seconds: u64,
    pub questions_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Next {
    pub projects: Vec<ProjectSelector>,
    pub since_seq: RecordSeq,
    pub me: String,
    pub timeout_seconds: u64,
    pub all: bool,
    pub settle_seconds: u64,
    pub settle_max_seconds: u64,
    pub settles: Settles,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub sql: String,
    pub params: Vec<JsonValue>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AcquireLease {
    pub run: RunId,
    pub resource: String,
    pub amount: u64,
    pub priority: i64,
    pub request_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLease {
    pub lease: LeaseId,
    pub run: RunId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompletionActionTarget {
    pub step: StepId,
    pub generation: StepGeneration,
    pub work: WorkGeneration,
    pub result: ResultId,
    pub result_attempt: Option<AttemptId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterCompletionAction {
    pub project: ProjectId,
    pub run: RunId,
    pub target: CompletionActionTarget,
    pub message: String,
    pub author: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditPreview {
    pub ops: Vec<PatchOperation>,
    pub would_start: Vec<StepId>,
    pub would_queue: Vec<StepId>,
    pub would_skip: Vec<StepId>,
    pub would_stale: Vec<StepId>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditResult {
    pub project: ProjectIdentity,
    pub rev: Revision,
    pub preview: EditPreview,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryResult {
    pub project: ProjectIdentity,
    pub steps: Vec<StepId>,
    pub rearmed: Vec<StepId>,
    pub stopped_at: Vec<StepId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputEditResult {
    pub edit: EditResult,
    pub changed: Vec<StepId>,
    pub running: Vec<StepId>,
    pub unsupported: Vec<UnsupportedInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnsupportedInput {
    pub step: StepId,
    pub inputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: MessageId,
    pub thread: String,
    pub from: String,
    pub to: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub needs_reply: bool,
    pub reply_to: Option<MessageId>,
    pub answer: Option<MessageAnswer>,
    pub ui: Option<String>,
    pub input: Option<String>,
    pub data: Option<JsonValue>,
    pub run: Option<RunId>,
    pub at: String,
    pub claimed_by: Option<RunId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessagePage {
    pub project: ProjectIdentity,
    pub messages: Vec<Message>,
    pub last_id: Option<MessageId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RecordPage {
    pub records: Vec<Record>,
    pub last_seq: RecordSeq,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NextResult {
    pub records: Vec<Record>,
    pub notes: Vec<Record>,
    pub last_seq: RecordSeq,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompletionActionConflict {
    pub expected: CompletionActionTarget,
    pub current: Option<CompletionActionTarget>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransientRetry {
    pub run: RunId,
    pub attempt: AttemptId,
    pub invocation: InvocationId,
    pub internal_attempt: u32,
    pub backoff_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileBinding {
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum PatchOperation {
    Add { path: String, value: JsonValue },
    Remove { path: String },
    Replace { path: String, value: JsonValue },
    Move { from: String, path: String },
    Copy { from: String, path: String },
    Test { path: String, value: JsonValue },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageView {
    Inbox,
    Questions,
    History,
    Thread,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Settles {
    Short,
    Full,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Stale,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Reserved,
    Claimed,
    Executing,
    Completing,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    Waiting,
    Held,
    Released,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "outcome",
    content = "details",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CompletionActionOutcome {
    Applied(RetryResult),
    Conflict(CompletionActionConflict),
    Discarded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "command",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[non_exhaustive]
pub enum CommandRequest {
    ProjectUpdate(ProjectUpdate),
    ProjectDelete(ProjectDelete),
    PlanPatch(PlanPatch),
    StepAdd(StepAdd),
    UnitAdd(UnitAdd),
    StepUpdate(StepUpdate),
    StepRemove(StepRemove),
    StepPause(StepPause),
    UnitTag(UnitTag),
    PlanPrune(PlanPrune),
    PlanSetInput(PlanSetInput),
    StepSetInput(StepSetInput),
    StepSetOutput(StepSetOutput),
    StepRetry(StepRetry),
    StepCancel(StepCancel),
    StepSubmit(StepSubmit),
    MessagePost(MessagePost),
    Messages(Messages),
    MarkRead(MarkRead),
    FnCall(FnCall),
    LogRead(LogRead),
    LogWait(LogWait),
    Next(Next),
    Query(Query),
    AcquireLease(AcquireLease),
    ReleaseLease(ReleaseLease),
    RegisterCompletionAction(RegisterCompletionAction),
    EdgeAdd(EdgeEdit),
    EdgeRemove(EdgeEdit),
    ProjectsList,
    ProjectCreate {
        name: ProjectName,
        description: String,
        icon: Option<String>,
        resources: JsonMap,
        author: Option<String>,
    },
    PlanGet {
        project: ProjectSelector,
    },
    PlanHistory {
        project: ProjectSelector,
        since_rev: Option<Revision>,
    },
    RecipeList {
        project: ProjectSelector,
    },
    FnList {
        project: Option<ProjectSelector>,
    },
    FnGet {
        name: String,
        project: Option<ProjectSelector>,
    },
    FnSave {
        manifest: JsonMap,
        main_py: String,
        project: Option<ProjectSelector>,
    },
    CallStatus {
        call: RunId,
        project: Option<ProjectSelector>,
    },
    StepContext {
        project: ProjectSelector,
        step: StepId,
    },
    Status {
        project: ProjectSelector,
        selection: StepSelection,
    },
    PlanView {
        project: ProjectSelector,
        format: PlanViewFormat,
    },
    Verify {
        project: Option<ProjectSelector>,
    },
    Drain {
        projects: Option<Vec<ProjectSelector>>,
        author: Option<String>,
    },
    Release {
        author: Option<String>,
    },
    Backup {
        destination: String,
    },
    Docs {
        topic: Option<String>,
    },
    Builtin {
        invocation: crate::rpc::FnInvocation,
    },
    Submission {
        run: RunId,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanViewFormat {
    Mermaid,
    Html,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "reply",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
#[non_exhaustive]
pub enum CommandReply {
    Project(ProjectIdentity),
    Projects(Vec<ProjectIdentity>),
    Deleted {
        project_id: ProjectId,
        name: ProjectName,
        deleted: bool,
    },
    Edit(EditResult),
    Preview(EditPreview),
    Retry(RetryResult),
    Inputs(InputEditResult),
    Posted {
        id: MessageId,
    },
    Messages(MessagePage),
    Records(RecordPage),
    Next(NextResult),
    Lease {
        lease: LeaseId,
        state: LeaseState,
    },
    CompletionAction(CompletionActionOutcome),
    Data(JsonValue),
    Ack,
}

pub trait RuntimeApi: Send + Sync {
    fn command(
        &self,
        request: CommandRequest,
    ) -> impl std::future::Future<Output = Result<CommandReply, PublicError>> + Send;
    fn changes(
        &self,
        cursor: ChangeCursor,
    ) -> impl std::future::Future<Output = Result<ChangeBatch, PublicError>> + Send;
}
