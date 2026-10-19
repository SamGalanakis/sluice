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
/// The policy with the measured-headroom settings, from one read of config.json.
pub fn execution(
    home: &std::path::Path,
) -> Result<(ResourcePolicy, crate::config::Headroom), PublicError> {
    crate::config::load_execution(home).map_err(invalid)
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
        serde_json::json!({"run":run,"limits":limits.map(|v| serde_json::from_str::<RunLimits>(&v)).transpose()?.map(RunLimits::shown),"usage":usage.map(|v| serde_json::from_str::<serde_json::Value>(&v)).transpose()?}),
    )
}

/// CPU throttling worth telling the owner about: a minute of lost run time.
pub const CPU_THROTTLE_NOTICE_USEC: u64 = 60_000_000;
/// A ready step waiting this long on the home's execution total is reported once.
pub const QUEUE_NOTICE_MINUTES: i64 = 10;

pub(crate) fn bytes(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.1} GiB", n as f64 / f64::from(1u32 << 30))
    } else {
        format!("{} MiB", n.div_ceil(1 << 20))
    }
}

/// The notices a run's cgroup counters call for. `ended` is whether the run has finished
/// (its completion carried these counters) or is still running (sampled live).
pub fn run_notices(
    run: sluice_model::ids::RunId,
    step: Option<&sluice_model::ids::StepId>,
    limits: &RunLimits,
    usage: &sluice_model::RunUsage,
    ended: bool,
) -> Vec<(sluice_store::resources::LimitNotice, String)> {
    use sluice_model::limits::shown;
    use sluice_store::resources::LimitNotice;
    let who = step.map_or_else(
        || format!("Call {run}"),
        |s| format!("Step {s} (run {run})"),
    );
    let notice = |limit, value, observed, outcome| LimitNotice {
        run: Some(run),
        step: step.cloned(),
        limit,
        value,
        observed,
        outcome,
        since: None,
    };
    let mut out = Vec::new();
    if usage.oom_kills > 0 {
        let cap = limits.memory_max_bytes.map_or_else(
            || "the home's memory total".to_owned(),
            |n| format!("its {} memory cap", bytes(n)),
        );
        out.push((
            notice("memory", shown(limits.memory_max_bytes), serde_json::json!({"peak_bytes": usage.memory_peak_bytes, "oom_kills": usage.oom_kills}), "killed"),
            format!("{who} ran out of memory at {cap} (peak {}); the kernel killed its payload and Sluice stopped the run. Retry with less parallelism or a larger limits.memory.", bytes(usage.memory_peak_bytes)),
        ));
    } else if usage.memory_max_hits > 0
        && !ended
        && let Some(cap) = limits.memory_max_bytes
    {
        out.push((
            notice("memory", serde_json::json!(cap), serde_json::json!({"peak_bytes": usage.memory_peak_bytes, "max_events": usage.memory_max_hits}), "running"),
            format!("{who} reached its {} memory cap (peak {}); the kernel is reclaiming its memory and it may be killed. It is still running.", bytes(cap), bytes(usage.memory_peak_bytes)),
        ));
    }
    if usage.pids_limit_hits > 0 {
        let tasks = limits
            .tasks
            .map_or_else(|| "unlimited".to_owned(), |n| n.to_string());
        let state = if ended {
            "The run has finished"
        } else {
            "It is still running"
        };
        out.push((
            notice("tasks", shown(limits.tasks), serde_json::json!({"failed_forks": usage.pids_limit_hits}), if ended { "finished" } else { "running" }),
            format!("{who} hit its task limit ({tasks} tasks): {} process or thread starts failed with EAGAIN. {state}; retry with less parallelism or raise limits.tasks (null is unlimited).", usage.pids_limit_hits),
        ));
    }
    if usage.cpu_throttled_usec >= CPU_THROTTLE_NOTICE_USEC
        && let Some(cpus) = limits.cpu_millis
    {
        out.push((
            notice("cpus", serde_json::json!(f64::from(cpus) / 1000.0), serde_json::json!({"throttled_usec": usage.cpu_throttled_usec}), if ended { "finished" } else { "throttled" }),
            format!("{who} is throttled by its CPU limit ({} CPUs): {} s of run time lost so far. Raise limits.cpus (null is unlimited) if that is too slow.", f64::from(cpus) / 1000.0, usage.cpu_throttled_usec / 1_000_000),
        ));
    }
    out
}
