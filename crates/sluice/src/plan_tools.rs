//! Plan tools share MCP's flat decoder, defaults and supplied-shape refusals.
use serde_json::{Map, Value};
use sluice_model::{commands::CommandRequest, error::PublicError};

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        "plan_get"
            | "plan_read"
            | "step_get"
            | "unit_get"
            | "plan_history"
            | "plan_view"
            | "plan_edit"
            | "unit_update"
            | "unit_remove"
            | "step_add"
            | "step_update"
            | "step_remove"
            | "step_pause"
            | "unit_add"
            | "unit_tag"
            | "edge_add"
            | "edge_remove"
            | "step_set_input"
            | "plan_prune"
            | "plan_set_input"
    )
}

pub fn decode(
    name: &str,
    args: Map<String, Value>,
    author: &str,
) -> Result<CommandRequest, PublicError> {
    sluice_web::mcp::decode_tool(name, args, Some(author))
}
