pub mod core;
pub mod descriptor;
pub mod gh;
pub mod git;
pub mod jev;
pub mod messages;

use sluice_model::rpc::JsonMap;

pub use descriptor::{BuiltinCtx, BuiltinDescriptor, FnFailure, RetryBudget, catalog, find};

/// Run one compiled builtin by name. jev's four are built (P4.05); the rest
/// fail `NotBuilt` until their units land their dispatch bodies, and a name
/// the catalog does not know is a terminal error.
pub async fn dispatch(
    name: &str,
    inputs: &JsonMap,
    ctx: &BuiltinCtx,
) -> Result<JsonMap, FnFailure> {
    match descriptor::find(name) {
        Some(d) if d.name.starts_with("jev.") => jev::dispatch(name, inputs, ctx).await,
        Some(_) => Err(FnFailure::NotBuilt(name.to_string())),
        None => Err(FnFailure::Terminal(format!("unknown builtin {name}"))),
    }
}
