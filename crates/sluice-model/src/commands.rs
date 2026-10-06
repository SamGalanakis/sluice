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

/// One live project as `projects_list` reports it. `settings_rev` is what
/// `project_update` and `project_delete` take as `expected_settings_rev`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectSummary {
    pub project_id: ProjectId,
    pub name: ProjectName,
    pub description: String,
    pub rev: Revision,
    pub settings_rev: Revision,
    /// Step status to the number of the plan's steps in it.
    pub counts: std::collections::BTreeMap<String, u64>,
    pub paused: bool,
    pub archived: bool,
    /// The board's revision: 0 until a board is first set, then one more per set or clear.
    #[serde(default)]
    pub board_rev: Revision,
    /// Each declared resource: `{"capacity": n}` or `{"capacity_fn": fn}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<JsonMap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ProjectIconSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectIconSummary {
    Image {
        #[serde(rename = "type")]
        media_type: String,
    },
    Text {
        text: String,
    },
}

/// A string is a text icon, or an absolute (or `~/`) path to an image file that the
/// coordinator reads once; images otherwise cross the broker as bounded data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum IconUpload {
    Text(String),
    Image {
        media_type: String,
        bytes_base64: String,
    },
}
impl From<String> for IconUpload {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdate {
    pub project: ProjectSelector,
    pub new_name: Option<ProjectName>,
    pub description: Option<String>,
    pub icon: Option<IconUpload>,
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

/// `board_set`: set the project's board (an OpenUI Lang program, `docs("board")`) or clear it
/// with a null `program`. A stale `expected_rev` is a conflict, a program that does not check
/// is invalid (each bad line listed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardSet {
    pub project: ProjectSelector,
    /// The board program; null clears the board.
    #[schemars(required, extend("type" = ["string", "null"]))]
    pub program: Option<String>,
    pub expected_rev: Option<Revision>,
    pub reason: Option<String>,
    pub author: Option<String>,
}

/// `board_slot_set`: set one named text slot of the project's board (markdown a board's
/// `Slot(key)` draws), or clear it with an empty or null `markdown`. No revision: each slot is
/// its own value, so an orchestrator updates one per event without reading the board first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardSlotSet {
    pub project: ProjectSelector,
    /// The slot's key: a lowercase letter or digit, then up to 63 of `a-z 0-9 _ . -`.
    pub key: String,
    /// The slot's markdown (at most 16 KiB); "" or null clears the slot.
    #[schemars(required, extend("type" = ["string", "null"]))]
    pub markdown: Option<String>,
    pub author: Option<String>,
}

/// A project's board as `board_get` reads it: no program while none is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardView {
    pub project: ProjectIdentity,
    pub rev: Revision,
    pub program: Option<String>,
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

fn is_false(value: &bool) -> bool {
    !value
}

fn default_start() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanPatch {
    pub project: ProjectSelector,
    pub rev: Revision,
    pub ops: Vec<PatchOperation>,
    #[serde(default = "default_start")]
    pub start: bool,
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
    #[serde(default = "default_start")]
    pub start: bool,
    pub edit: EditOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitAdd {
    pub project: ProjectSelector,
    pub recipe: String,
    pub unit: UnitName,
    pub params: JsonMap,
    #[serde(default = "default_start")]
    pub start: bool,
    #[schemars(with = "std::collections::BTreeMap<String, Vec<String>>")]
    pub after: indexmap::IndexMap<String, Vec<String>>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    #[schemars(with = "std::collections::BTreeMap<String, JsonMap>")]
    pub inputs: indexmap::IndexMap<String, JsonMap>,
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
    /// Also every step downstream of the selection: those reading from or gated on
    /// a selected step, transitively.
    #[serde(default, skip_serializing_if = "is_false")]
    pub subtree: bool,
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
    pub tags: Option<Vec<String>>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_rev: Option<Revision>,
    pub project: ProjectSelector,
    pub selection: StepSelection,
    pub message: Option<String>,
    pub reason: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepCancel {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_rev: Option<Revision>,
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

/// Settle a running step whose run has submitted (it is finishing) on that submission: its
/// agent is stopped as a cancel stops it, and the step succeeds with the outputs the done
/// signal would have given it. Only a bare agent fn's step; anything else is refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSettle {
    pub project: ProjectSelector,
    pub step: StepId,
    #[serde(default)]
    pub reason: String,
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageAnswer {
    pub action: String,
    pub params: Option<JsonMap>,
    pub values: Option<JsonMap>,
}

/// A question that needs a reply. `to` is a step of the project's current plan,
/// `orchestrator` or `owner`; the thread and sender are derived, never given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    pub project: ProjectSelector,
    /// Required; a missing or unknown recipient is refused as `invalid`, never stored.
    #[serde(default)]
    pub to: String,
    pub body: String,
    pub title: Option<String>,
    pub ui: Option<String>,
    pub input: Option<String>,
    pub data: Option<JsonValue>,
    /// The asking run: its step is the sender. Without it the caller is the orchestrator.
    pub run: Option<RunId>,
    /// The dashboard speaks as the owner.
    #[serde(default, skip_serializing_if = "is_false")]
    pub owner: bool,
}

/// A note; no reply is expected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Say {
    pub project: ProjectSelector,
    /// Required; a missing or unknown recipient is refused as `invalid`, never stored.
    #[serde(default)]
    pub to: String,
    pub body: String,
    pub data: Option<JsonValue>,
    pub run: Option<RunId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub owner: bool,
}

/// A reply to one message: to its sender, on its thread. It resolves an open question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub project: ProjectSelector,
    pub to_message: MessageId,
    #[serde(default)]
    pub body: String,
    pub answer: Option<MessageAnswer>,
    pub run: Option<RunId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub owner: bool,
}

/// The retired `message_post` wire shape. Only runs started before ask, say and reply
/// send it; the coordinator translates it and refuses it without a run identity.
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
    /// Read as the owner (the dashboard's identity) instead of the orchestrator.
    #[serde(default, skip_serializing_if = "is_false")]
    pub owner: bool,
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

/// A page of the log. Every filter narrows only the records it speaks of (an empty list is no
/// filter): `kinds` every record, `threads` and `recipients` message records, `statuses`
/// step.status records; a record is kept when every filter that applies to it keeps it.
/// `threads` without `kinds` also keeps messages only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogRead {
    pub project: Option<ProjectSelector>,
    pub since_seq: Option<RecordSeq>,
    pub kinds: Option<Vec<String>>,
    pub threads: Option<Vec<String>>,
    /// Only step.status records whose `to` is one of these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statuses: Option<Vec<StepStatus>>,
    /// Only message records whose `to` is one of these (a step id, `orchestrator`, `owner`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipients: Option<Vec<String>>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogWait {
    pub read: LogRead,
    pub timeout_seconds: u64,
    pub questions_only: bool,
}

/// `step_wait`: wait until every selected step's status meets `until`, or `timeout_seconds`
/// pass. Exactly one of `steps` and `tags` selects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepWait {
    pub project: ProjectSelector,
    pub selection: StepSelection,
    /// `"succeeded"`, `"settled"` or `{"any_of": [statuses]}`. The coordinator checks it and
    /// refuses a bad one as `invalid`.
    #[schemars(with = "StepWaitUntil")]
    pub until: JsonValue,
    pub timeout_seconds: u64,
}

/// What `step_wait` waits for: every selected step succeeded, every one settled (§7.1), or
/// every one in one of the listed statuses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
pub enum StepWaitUntil {
    Named(StepWaitTarget),
    AnyOf { any_of: Vec<StepStatus> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepWaitTarget {
    Succeeded,
    Settled,
}

impl StepWaitUntil {
    /// Check a raw `until`, naming every problem with its path.
    pub fn parse(value: &serde_json::Value) -> Result<Self, Vec<String>> {
        const SHAPE: &str = r#"until: must be "succeeded", "settled" or {"any_of": [statuses]}"#;
        let status =
            |value: &serde_json::Value| serde_json::from_value::<StepStatus>(value.clone()).ok();
        match value {
            serde_json::Value::String(name) => match name.as_str() {
                "succeeded" => Ok(Self::Named(StepWaitTarget::Succeeded)),
                "settled" => Ok(Self::Named(StepWaitTarget::Settled)),
                _ => Err(vec![format!("{SHAPE}, not {value}")]),
            },
            serde_json::Value::Object(object) => {
                let mut errors: Vec<String> = object
                    .keys()
                    .filter(|key| *key != "any_of")
                    .map(|key| format!("until.{key}: unknown field; {SHAPE}"))
                    .collect();
                let mut statuses = vec![];
                match object.get("any_of") {
                    None => errors.push(format!("until.any_of: missing; {SHAPE}")),
                    Some(serde_json::Value::Array(items)) if items.is_empty() => {
                        errors.push("until.any_of: name at least one status".into())
                    }
                    Some(serde_json::Value::Array(items)) => {
                        for (i, item) in items.iter().enumerate() {
                            match status(item) {
                                Some(status) => statuses.push(status),
                                None => errors.push(format!(
                                    "until.any_of[{i}]: {item} is not a step status (pending, \
                                     running, succeeded, failed, stale, skipped)"
                                )),
                            }
                        }
                    }
                    Some(other) => errors.push(format!(
                        "until.any_of: must be a list of step statuses, not {other}"
                    )),
                }
                if errors.is_empty() {
                    Ok(Self::AnyOf { any_of: statuses })
                } else {
                    Err(errors)
                }
            }
            _ => Err(vec![format!("{SHAPE}, not {value}")]),
        }
    }
    /// Whether one step meets the condition; `settled` is the step's settledness (§7.1).
    pub fn met(&self, status: &StepStatus, settled: bool) -> bool {
        match self {
            Self::Named(StepWaitTarget::Succeeded) => *status == StepStatus::Succeeded,
            Self::Named(StepWaitTarget::Settled) => settled,
            Self::AnyOf { any_of } => any_of.contains(status),
        }
    }
}

/// `step_wait`'s reply: whether the condition held, each selected step's status (plan order)
/// and the project log's last seq at that reading, for a `log_read` from there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepWaitResult {
    pub met: bool,
    #[schemars(with = "std::collections::BTreeMap<String, StepStatus>")]
    pub steps: indexmap::IndexMap<StepId, StepStatus>,
    pub seq: RecordSeq,
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

/// `status` (SPEC §8): the steps view, or with `view: units` one compact row per unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusQuery {
    pub project: ProjectSelector,
    pub selection: StepSelection,
    /// Cut every string over 200 characters in inputs and outputs (steps view only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub brief: bool,
    /// Include the done units, which are otherwise left out and counted in `done_units`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub all: bool,
    #[serde(default, skip_serializing_if = "StatusView::is_steps")]
    pub view: StatusView,
    /// Keep only the units in these states (units view only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<Vec<UnitState>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusView {
    #[default]
    Steps,
    Units,
}
impl StatusView {
    fn is_steps(&self) -> bool {
        *self == Self::Steps
    }
}

/// A unit's state in the units view, checked in this order: a step running, a step failed
/// or stale, every step succeeded or skipped, nothing startable and something held, a step
/// queued on resources, else pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnitState {
    Running,
    Failed,
    Settled,
    Blocked,
    Queued,
    Pending,
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
    /// Ready executable candidates, including those also listed in `would_queue`.
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
    /// The new revision, or the current one when the edit changed nothing (its
    /// `preview.ops` is then empty and nothing was committed).
    pub rev: Revision,
    pub preview: EditPreview,
    /// The steps the edit was about: `unit_add`'s new steps, `unit_tag`'s unit,
    /// `step_pause`'s selection (subtree included), `plan_prune`'s removed steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<StepId>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryResult {
    pub project: ProjectIdentity,
    pub steps: Vec<StepId>,
    pub rearmed: Vec<StepId>,
    pub stopped_at: Vec<StepId>,
}

/// `step_set_input`'s reply: the edit result plus what happened to each selected step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InputEditResult {
    #[serde(flatten)]
    pub edit: EditResult,
    pub changed: Vec<StepId>,
    /// Selected but running, so left unchanged.
    pub running: Vec<StepId>,
    /// Selected but lacking an input.
    pub unsupported: Vec<UnsupportedInput>,
}

/// `plan_prune`'s reply: the edit result (its `steps` are the removed steps), the
/// removed units, and each candidate unit kept with what holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PruneResult {
    #[serde(flatten)]
    pub edit: EditResult,
    pub units: Vec<UnitName>,
    pub kept: Vec<KeptUnit>,
}

/// A unit prune kept: held by a step outside the pruned set, or by a plan output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeptUnit {
    pub unit: UnitName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<StepId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnsupportedInput {
    pub step: StepId,
    pub inputs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageVerb {
    Ask,
    Say,
    Reply,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionState {
    Open,
    Answered,
    Closed,
}

/// One message. `to_message` and `answer` are a reply's; `state` and `answered_by`
/// are a question's as read now (never in the log record). Rows stored before the
/// verbs existed read with the verb derived from them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: MessageId,
    pub verb: MessageVerb,
    pub from: String,
    pub to: Option<String>,
    pub thread: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_message: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<MessageAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<QuestionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<MessageId>,
}
impl Message {
    /// Asks, and replies stored as questions before the verbs existed.
    pub fn is_question(&self) -> bool {
        self.verb == MessageVerb::Ask || self.state.is_some()
    }
}

/// How a message reached its recipient when it was posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    /// A live listening run of the step was handed it (or an orchestrator/owner inbox).
    Delivered,
    /// The step will run, and its next run is assigned it.
    Queued,
    /// The step has no live or upcoming run; a later run (e.g. a retry) gets it.
    NoLiveRun,
}

/// What ask, say and reply return.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MessageReceipt {
    pub id: MessageId,
    pub to: String,
    pub thread: String,
    pub delivery: Delivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<RunId>,
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
    BoardSet(BoardSet),
    BoardSlotSet(BoardSlotSet),
    BoardGet {
        project: ProjectSelector,
    },
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
    StepSettle(StepSettle),
    Ask(Ask),
    Say(Say),
    Reply(Reply),
    /// Retired; accepted only from runs started on an older release.
    MessagePost(MessagePost),
    Messages(Messages),
    MarkRead(MarkRead),
    FnCall(FnCall),
    LogRead(LogRead),
    LogWait(LogWait),
    StepWait(StepWait),
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
        icon: Option<IconUpload>,
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
    Status(StatusQuery),
    PlanView {
        project: ProjectSelector,
        format: PlanViewFormat,
        #[serde(default, skip_serializing_if = "is_false")]
        all: bool,
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
    Projects(Vec<ProjectSummary>),
    Deleted {
        project_id: ProjectId,
        name: ProjectName,
        deleted: bool,
    },
    Edit(EditResult),
    Preview(EditPreview),
    Retry(RetryResult),
    Inputs(InputEditResult),
    Pruned(PruneResult),
    /// The retired message_post's reply, kept for the binaries that sent it.
    Posted {
        id: MessageId,
    },
    Receipt(MessageReceipt),
    /// `board_set`'s reply: the board's new revision.
    BoardRev {
        rev: Revision,
    },
    Board(BoardView),
    Messages(MessagePage),
    Records(RecordPage),
    Next(NextResult),
    StepWait(StepWaitResult),
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
