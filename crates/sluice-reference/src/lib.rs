//! sluice-reference: the test-only reference the plan-rows cutover is checked against
//! (`docs/design/plan-rows.md` §12, lane H1).
//!
//! It holds, moved verbatim from `sluice-model` at `775d57c`, today's whole-plan compiler
//! (`plan`: `Plan::parse`, the RFC 6902 `Plan::patch`, `prepare_patch`), its unit derivation,
//! settlement and pruning (`units`), the gate evaluator, reconciliation and whole-plan edit
//! simulation (`gates`), recipe loading and expansion (`recipe`) and the typed edit preparation
//! (`edit`: `prepare_edit` over the schema-1 typed requests in `commands`). On top of them it
//! defines the schema-3 reference semantics of `plan_rows::PlanOp` on a document (`ops`), and
//! the differential harness (`harness`) and generators (`generate`) the incremental model
//! (lane C) is tested against.
//!
//! The crate is `publish = false` and only ever a dev-dependency: no binary links it. Its
//! leaf data types (`ids`, `types`, `hash`, `error`, `rpc`, `openui`, `commands::StepStatus`
//! and `plan_rows`) are `sluice-model`'s own, so values cross between the two without
//! conversion; everything that compiles, patches, prepares or simulates a plan is its own copy
//! and never changes with `sluice-model`.
//!
//! Use it from `sluice-model`'s integration tests (`tests/*.rs`), never from its unit tests:
//! `sluice-model`'s unit-test build is a second copy of the crate whose types do not unify with
//! the ones this crate links.

pub mod commands;
pub mod edit;
pub mod gates;
pub mod generate;
pub mod harness;
pub mod interop;
pub mod ops;
pub mod plan;
pub mod recipe;
pub mod units;

/// `sluice-model`'s leaf modules under the names the verbatim modules use, with the
/// crate-private helpers they need copied beside them.
pub mod ids {
    pub use sluice_model::ids::*;
}
pub mod error {
    pub use sluice_model::error::*;
}
pub mod hash {
    pub use sluice_model::hash::*;
}
pub mod openui {
    pub use sluice_model::openui::*;
}
pub mod rpc {
    use serde_json::Value;
    pub use sluice_model::rpc::*;

    /// Whether `JsonValue` takes this value: every integer fits i64 and every float is
    /// finite (`sluice_model::rpc::strict_value`, crate-private there).
    pub(crate) fn strict_value(v: &Value) -> bool {
        match v {
            Value::Number(n) => {
                if n.is_f64() {
                    n.as_f64().is_some_and(f64::is_finite)
                } else {
                    n.as_i64().is_some()
                }
            }
            Value::Array(a) => a.iter().all(strict_value),
            Value::Object(o) => o.values().all(strict_value),
            _ => true,
        }
    }
}
pub mod types {
    pub use sluice_model::types::*;

    fn error(path: &str, message: impl Into<String>) -> PathError {
        PathError {
            path: path.into(),
            message: message.into(),
        }
    }
    fn field_path(path: &str, field: &str) -> String {
        if path.is_empty() {
            field.into()
        } else {
            format!("{path}.{field}")
        }
    }
    /// Validate JSON numbers throughout the value, including extra record fields
    /// (`sluice_model::types::validate_json`, crate-private there).
    pub(crate) fn validate_json(
        value: &serde_json::Value,
        path: &str,
    ) -> Result<(), Vec<PathError>> {
        fn visit(value: &serde_json::Value, path: &str, depth: usize, errors: &mut Vec<PathError>) {
            use serde_json::Value;
            if depth >= 128 {
                errors.push(error(path, "JSON nesting exceeds 128"));
                return;
            }
            match value {
                Value::Number(n)
                    if !(if n.is_f64() {
                        n.as_f64().is_some_and(f64::is_finite)
                    } else {
                        n.as_i64().is_some()
                    }) =>
                {
                    errors.push(error(
                        path,
                        "JSON integers must fit i64 and floats must be finite",
                    ))
                }
                Value::Array(a) => {
                    for (i, v) in a.iter().enumerate() {
                        visit(v, &format!("{path}[{i}]"), depth + 1, errors);
                    }
                }
                Value::Object(o) => {
                    for (k, v) in o {
                        visit(v, &field_path(path, k), depth + 1, errors);
                    }
                }
                _ => {}
            }
        }
        if strict(value, 0) {
            return Ok(());
        }
        let mut errors = Vec::new();
        visit(value, path, 0, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
    /// What `validate_json` checks, without paths.
    pub(crate) fn strict(value: &serde_json::Value, depth: usize) -> bool {
        use serde_json::Value;
        if depth >= 128 {
            return false;
        }
        match value {
            Value::Number(n) if n.is_f64() => n.as_f64().is_some_and(f64::is_finite),
            Value::Number(n) => n.as_i64().is_some(),
            Value::Array(a) => a.iter().all(|v| strict(v, depth + 1)),
            Value::Object(o) => o.values().all(|v| strict(v, depth + 1)),
            _ => true,
        }
    }
    /// `validate_json` of a map's value, without building that value.
    pub(crate) fn validate_json_map(
        map: &crate::rpc::JsonMap,
        path: &str,
    ) -> Result<(), Vec<PathError>> {
        if map.0.values().all(|v| strict(v.as_value(), 1)) {
            return Ok(());
        }
        validate_json(&serde_json::to_value(map).expect("strict JSON map"), path)
    }
}

pub use edit::{EditSnapshot, PlanEdit, PreparedEdit, prepare_edit};
pub use gates::{CachedResources, DryRun, Gate, GateDecision, StateSnapshot, StepState, ValueRef};
pub use plan::{Binding, Declaration, FnSignature, Pause, Plan, SignatureProvider, Step};
pub use units::{PruneSet, RetryWalk, Unit};
