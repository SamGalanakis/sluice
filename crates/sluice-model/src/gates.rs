use crate::ids::{StepId, UnitName};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ValueRef(pub String);
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Gate {
    Step { id: StepId, accept_skip: bool },
    Bool { reference: ValueRef, negate: bool },
    Unit { name: UnitName, accept_skip: bool },
}
