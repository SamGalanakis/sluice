//! sluice-model: shared pure contracts.

pub mod attempt;
pub mod commands;
pub mod doc;
pub mod edit;
pub mod error;
pub mod events;
pub mod gates;
pub mod hash;
pub mod ids;
pub mod naming;
pub mod openui;
pub mod plan;
pub mod plan_index;
pub mod plan_rows;
pub mod recipe;
pub mod rpc;
pub mod shown;
pub mod status;
pub mod types;
pub mod units;

pub use commands::RuntimeApi;
pub use edit::{EditSnapshot, PlanEdit, PreparedEdit, prepare_edit};

pub use gates::{CachedResources, DryRun, Gate, GateDecision, StateSnapshot, StepState, ValueRef};
pub use plan::{Binding, Declaration, FnSignature, Pause, Plan, SignatureProvider, Step};
pub use units::{PruneSet, RetryWalk, Unit};
