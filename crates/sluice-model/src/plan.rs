use crate::{
    commands::{EditPreview, EditResult, PatchOperation, RuntimeApi},
    error::PublicError,
    ids::Revision,
    rpc::JsonMap,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: Revision,
    pub document: JsonMap,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanEdit {
    pub expected: Revision,
    pub ops: Vec<PatchOperation>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ValidatedPlan(JsonMap);
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreparedEdit {
    pub expected: Revision,
    pub ops: Vec<PatchOperation>,
    pub plan: ValidatedPlan,
    pub preview: EditPreview,
}

pub fn prepare_edit(_snapshot: &Snapshot, _edit: PlanEdit) -> Result<PreparedEdit, PublicError> {
    Err(PublicError::not_implemented("prepare_edit"))
}
pub async fn apply_edit(
    _api: &impl RuntimeApi,
    _edit: PreparedEdit,
) -> Result<EditResult, PublicError> {
    Err(PublicError::not_implemented("apply_edit"))
}
