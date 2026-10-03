use crate::{
    ids::{StepId, UnitName},
    rpc::JsonValue,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "of",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Type {
    String,
    Int,
    Float,
    Boolean,
    Any,
    Optional(Box<Type>),
    List(Box<Type>),
    Enum(Vec<String>),
    Record(
        #[schemars(with = "std::collections::BTreeMap<String, Type>")]
        indexmap::IndexMap<String, Type>,
    ),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum BoundValue {
    Ready(JsonValue),
    Waiting,
    Skipped(SkipReason),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SkipReason {
    Boolean {
        reference: String,
        value: Option<bool>,
        negate: bool,
    },
    Step {
        step: StepId,
    },
    Unit {
        unit: UnitName,
        exit: StepId,
    },
}
