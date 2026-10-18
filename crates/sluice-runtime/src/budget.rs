//! Resolve physical budgets at admission; logical resources keep their own leases.
use rusqlite::Connection;
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, ProjectSelector},
    limits::{LimitsOverride, ResourcePolicy, RunLimits},
};

pub fn policy(home: &std::path::Path) -> Result<ResourcePolicy, PublicError> {
    crate::config::load_resource_policy(home).map_err(invalid)
}
pub fn invalid(message: impl Into<String>) -> PublicError {
    PublicError::Invalid {
        message: message.into(),
        errors: vec![],
    }
}
pub fn resolve(
    sql: &Connection,
    project: Option<ProjectId>,
    overrides: Option<&LimitsOverride>,
    policy: &ResourcePolicy,
) -> sluice_store::Result<RunLimits> {
    let project = project
        .map(|id| sluice_store::projects::resolve(sql, &ProjectSelector::Id(id)))
        .transpose()?;
    policy
        .resolve(project.as_ref().and_then(|p| p.limits.as_ref()), overrides)
        .map_err(|e| invalid(e).into())
}

/// Frozen limits and final usage belong to the attempt, not today's configuration.
pub fn run_resources(
    sql: &Connection,
    run: sluice_model::ids::RunId,
) -> sluice_store::Result<serde_json::Value> {
    use rusqlite::OptionalExtension;
    let row: Option<(Option<String>, Option<String>)> = sql.query_row(
        "SELECT a.limits,json_extract(a.request,'$.resource_usage') FROM attempts a JOIN runs r USING(attempt_id) WHERE r.run_id=?1",
        [run.to_string()], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let (limits, usage) = row.unwrap_or_default();
    Ok(
        serde_json::json!({"run":run,"limits":limits.map(|v| serde_json::from_str::<serde_json::Value>(&v)).transpose()?,"usage":usage.map(|v| serde_json::from_str::<serde_json::Value>(&v)).transpose()?}),
    )
}
