//! The schema-1 edit commands, copied from `sluice_model::commands` at `775d57c`: the typed
//! edit requests `prepare_edit` takes, `plan_patch`'s RFC 6902 operations and the old preview.
//! Their wire shapes are today's (`{"command": …, "args": …}`), so a test feeds the reference
//! the same arguments it feeds a typed tool. The status and report types are `sluice-model`'s.

use crate::ids::{ProjectSelector, Revision, StepId, UnitName};
use crate::rpc::{JsonMap, JsonValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use sluice_model::commands::{StepStatus, UnsupportedInput};

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
    /// Unit-name patterns (`*` any run of characters, `?` one) never removed: a selected
    /// unit whose name matches one is kept, `kept` naming the pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<Vec<String>>,
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
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum PatchOperation {
    Add { path: String, value: JsonValue },
    Remove { path: String },
    Replace { path: String, value: JsonValue },
    Move { from: String, path: String },
    Copy { from: String, path: String },
    Test { path: String, value: JsonValue },
}

/// The edit commands of today's `CommandRequest`, with their wire tags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "command",
    content = "args",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CommandRequest {
    PlanPatch(PlanPatch),
    StepAdd(StepAdd),
    UnitAdd(UnitAdd),
    StepUpdate(StepUpdate),
    StepRemove(StepRemove),
    StepPause(StepPause),
    UnitTag(UnitTag),
    PlanPrune(PlanPrune),
    StepSetInput(StepSetInput),
    EdgeAdd(EdgeEdit),
    EdgeRemove(EdgeEdit),
}
