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

type ResourceRow = (Option<String>, Option<String>, bool, Option<i64>);

/// Frozen limits and final usage belong to the attempt, not today's configuration. `memory`
/// includes live working memory and file cache, the sampled working peak, and the kernel's
/// cache-inclusive peak. Unknown historic working peaks remain null.
pub fn run_resources(
    sql: &Connection,
    run: sluice_model::ids::RunId,
) -> sluice_store::Result<serde_json::Value> {
    use rusqlite::OptionalExtension;
    let row: Option<ResourceRow> = sql.query_row(
        "SELECT a.limits,json_extract(a.request,'$.resource_usage'),r.finished_at IS NULL,json_extract(a.request,'$.memory_working_peak_bytes') FROM attempts a JOIN runs r USING(attempt_id) WHERE r.run_id=?1",
        [run.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional()?;
    let (limits, usage, live, sampled_peak) = row.unwrap_or_default();
    let sampled_peak = sampled_peak.and_then(|n| u64::try_from(n).ok());
    let limits = limits
        .map(|v| serde_json::from_str::<RunLimits>(&v))
        .transpose()?;
    let usage = usage
        .map(|v| serde_json::from_str::<serde_json::Value>(&v))
        .transpose()?;
    let measured = live
        .then(|| {
            sql.path()
                .and_then(|p| std::path::Path::new(p).parent())
                .and_then(|home| crate::headroom::published_run(home, run))
        })
        .flatten();
    let memory = serde_json::json!({
        "cap_bytes": limits.map(|l| sluice_model::limits::shown(l.memory_max_bytes)),
        "current_bytes": measured.map(|m| m.current_bytes),
        "working_bytes": measured.and_then(|m| m.working_bytes),
        "file_cache_bytes": measured.and_then(|m| m.file_cache_bytes),
        "pressure_some_avg10": measured.and_then(|m| m.pressure_some_avg10),
        "working_peak_bytes": sampled_peak.max(usage.as_ref().and_then(|u| u["memory_working_peak_bytes"].as_u64())),
        "peak_bytes": measured.map(|m| m.peak_bytes).or_else(|| usage.as_ref().and_then(|u| u["memory_peak_bytes"].as_u64())),
    });
    Ok(
        serde_json::json!({"run":run,"limits":limits.map(RunLimits::shown),"usage":usage,"memory":memory}),
    )
}

/// CPU throttling worth telling the owner about: a minute of lost run time.
pub const CPU_THROTTLE_NOTICE_USEC: u64 = 60_000_000;

/// The notices a run's cgroup counters call for. `ended` is whether the run has finished
/// (its completion carried these counters) or is still running (sampled live).
pub fn run_notices(
    run: sluice_model::ids::RunId,
    step: Option<&sluice_model::ids::StepId>,
    limits: &RunLimits,
    usage: &sluice_model::RunUsage,
    ended: bool,
    memory: Option<(
        sluice_model::limits::RunMemory,
        crate::config::MemoryNoticePolicy,
    )>,
) -> Vec<(sluice_store::resources::LimitNotice, String)> {
    use sluice_model::limits::{shown, size};
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
            |n| format!("its {} memory cap", size(n)),
        );
        out.push((
            notice("memory", shown(limits.memory_max_bytes), serde_json::json!({"peak_bytes": usage.memory_peak_bytes, "working_peak_bytes": usage.memory_working_peak_bytes, "oom_kills": usage.oom_kills}), "killed"),
            format!("{who} ran out of memory at {cap} ({}); the kernel killed its payload and Sluice stopped the run. Retry with less parallelism, or ask for more explicitly with a larger limits.memory.", usage.memory_summary()),
        ));
    } else if !ended && let Some((memory, policy)) = memory {
        let working_high = memory
            .working_bytes
            .zip(limits.memory_max_bytes)
            .is_some_and(|(working, cap)| {
                working as f64 / cap as f64 * 100.0 >= policy.working_percent
            });
        let stalled = memory
            .pressure_some_avg10
            .is_some_and(|pressure| pressure >= policy.pressure_percent);
        if memory.oom_events > 0 || working_high || stalled {
            let cap = limits
                .memory_max_bytes
                .map_or_else(|| "unlimited".to_owned(), size);
            let working = memory
                .working_bytes
                .map_or_else(|| "unknown".to_owned(), size);
            let cache = memory
                .file_cache_bytes
                .map_or_else(|| "unknown".to_owned(), size);
            let pressure = memory
                .pressure_some_avg10
                .map_or_else(|| "unknown".to_owned(), |v| format!("{v:.1}%"));
            out.push((
                notice("memory", shown(limits.memory_max_bytes), serde_json::json!({
                    "peak_bytes": usage.memory_peak_bytes, "working_peak_bytes": usage.memory_working_peak_bytes, "current_bytes": memory.current_bytes,
                    "working_bytes": memory.working_bytes, "file_cache_bytes": memory.file_cache_bytes,
                    "pressure_some_avg10": memory.pressure_some_avg10, "oom_events": memory.oom_events,
                    "max_events": usage.memory_max_hits,
                    "working_percent": policy.working_percent, "pressure_percent": policy.pressure_percent,
                }), "running"),
                format!("{who} is under memory pressure (working {working}, cap {cap}, file cache {cache}, {}, PSI some avg10 {pressure}, OOM events {}). It is still running. Reduce parallelism or ask for a larger limits.memory if pressure persists.", usage.memory_summary(), memory.oom_events),
            ));
        }
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
