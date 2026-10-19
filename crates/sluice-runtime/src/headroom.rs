//! Admission by measured host memory (SPEC §2.5). Reservations bound what runs may hold
//! against the home total; these readings keep the host itself out of memory reclaim. A run
//! starts only while MemAvailable less the host reserve covers its reservation, memory
//! pressure (PSI `some avg10`) is under its limit, and the workload slice's memory.current
//! plus the reservation stays within the total. Runs admitted in the last [`RAMP`] count
//! with their whole reservation, since the readings do not show them yet.
use crate::{budget::bytes, config::Headroom};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

/// One reading of the host, as the execution host takes it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HostMemory {
    /// `/proc/meminfo` MemAvailable.
    pub available_bytes: u64,
    /// `/proc/pressure/memory` `some avg10`, a percentage; `None` on a kernel without PSI.
    pub pressure_some_avg10: Option<f64>,
    /// The home's workload slice `memory.current`: every live run with its guardian.
    pub workload_bytes: u64,
}

/// A run admitted this recently has not grown into its reservation yet.
pub const RAMP: Duration = Duration::from_secs(60);
/// How often a held run is checked again.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(5);
/// A published reading older than this is not shown.
const SAMPLE_FRESH: Duration = Duration::from_secs(120);
const SAMPLE_FILE: &str = "host-memory.json";

/// Why a run that needs `need` bytes cannot start now, as status says it; `pending` is the
/// memory reserved by runs admitted within [`RAMP`].
pub fn hold(
    sample: &HostMemory,
    headroom: &Headroom,
    total: Option<u64>,
    pending: u64,
    need: u64,
) -> Option<String> {
    let available = sample
        .available_bytes
        .saturating_sub(headroom.host_reserve_bytes)
        .saturating_sub(pending);
    if available < need {
        return Some(format!(
            "held: host memory headroom (available {}, need {})",
            bytes(available),
            bytes(need)
        ));
    }
    if let Some(pressure) = sample.pressure_some_avg10
        && pressure >= headroom.pressure_max
    {
        return Some(format!(
            "held: host memory pressure (some avg10 {pressure:.1}%, limit {}%)",
            headroom.pressure_max
        ));
    }
    let in_use = sample.workload_bytes.saturating_add(pending);
    if let Some(total) = total
        && in_use.saturating_add(need) > total
    {
        return Some(format!(
            "held: workload memory (in use {}, need {}, total {})",
            bytes(in_use),
            bytes(need),
            bytes(total)
        ));
    }
    None
}

/// Memory reserved by live attempts admitted within [`RAMP`].
pub fn pending(sql: &Connection) -> sluice_store::Result<u64> {
    let sum: i64 = sql.query_row(
        "SELECT coalesce(sum(json_extract(reservation,'$.memory_bytes')),0) FROM attempts
         WHERE phase<>'terminal' AND reservation IS NOT NULL
         AND julianday(created_at) >= julianday('now') - ?1/86400.0",
        [RAMP.as_secs_f64()],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(sum).unwrap_or(0))
}

/// The admission's own reading, for status and the dashboard in whichever process serves
/// them. A failed write only leaves status without the numbers.
pub fn publish(home: &Path, sample: &HostMemory) {
    let stamped = serde_json::json!({
        "sampled_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(),
        "sample": sample,
    });
    let write = || -> std::io::Result<()> {
        let temp = home.join(format!(".{SAMPLE_FILE}.{}", std::process::id()));
        std::fs::write(&temp, stamped.to_string())?;
        std::fs::rename(temp, home.join(SAMPLE_FILE))
    };
    if let Err(error) = write() {
        tracing::debug!(%error, "host memory reading not published");
    }
}

/// The reading admission last took, while it is fresh.
pub fn published(home: &Path) -> Option<HostMemory> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(SAMPLE_FILE)).ok()?).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (now.saturating_sub(value["sampled_at"].as_u64()?) <= SAMPLE_FRESH.as_secs())
        .then(|| serde_json::from_value(value["sample"].clone()).ok())
        .flatten()
}

/// The host's own reading: `/proc/meminfo`, `/proc/pressure/memory` and the slice's
/// `memory.current` (`slice_group` is its cgroup path, `None` while it does not exist).
pub fn read_host(slice_group: Option<&Path>) -> std::io::Result<HostMemory> {
    let meminfo = std::fs::read_to_string("/proc/meminfo")?;
    let available_bytes = parse_meminfo(&meminfo)
        .ok_or_else(|| std::io::Error::other("MemAvailable missing from /proc/meminfo"))?;
    let pressure_some_avg10 = match std::fs::read_to_string("/proc/pressure/memory") {
        Ok(text) => Some(
            parse_pressure(&text)
                .ok_or_else(|| std::io::Error::other("invalid /proc/pressure/memory"))?,
        ),
        // A kernel built or booted without PSI.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::Unsupported
            ) || error.raw_os_error() == Some(95) =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    let workload_bytes = match slice_group {
        None => 0,
        Some(group) => match std::fs::read_to_string(group.join("memory.current")) {
            Ok(text) => text
                .trim()
                .parse()
                .map_err(|_| std::io::Error::other("invalid memory.current"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        },
    };
    Ok(HostMemory {
        available_bytes,
        pressure_some_avg10,
        workload_bytes,
    })
}
fn parse_meminfo(text: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        line.strip_prefix("MemAvailable:")?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()?
            .checked_mul(1024)
    })
}
fn parse_pressure(text: &str) -> Option<f64> {
    text.lines()
        .find_map(|line| line.strip_prefix("some "))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("avg10="))?
        .parse()
        .ok()
}

/// What admission measured, with the settings it holds runs by.
#[derive(Debug, Clone)]
pub struct Gate {
    sample: Result<HostMemory, String>,
    headroom: Headroom,
}
impl Gate {
    /// `None` when the host has nothing to measure; a reading is published for status.
    pub fn new(
        reading: Option<std::io::Result<HostMemory>>,
        home: &Path,
        headroom: Headroom,
    ) -> Option<Self> {
        let sample = reading?.map_err(|error| error.to_string());
        if let Ok(sample) = &sample {
            publish(home, sample);
        }
        Some(Self { sample, headroom })
    }
    /// Why a run with `limits` must wait, counting runs admitted in this transaction too.
    pub fn hold(
        &self,
        sql: &Connection,
        limits: &sluice_model::RunLimits,
        policy: &sluice_model::ResourcePolicy,
    ) -> sluice_store::Result<Option<String>> {
        let need = sluice_model::limits::Reserved::of(*limits, policy.guardian_memory_bytes)
            .map_err(|e| sluice_store::StoreError::Public(crate::budget::invalid(e)))?
            .memory_bytes;
        Ok(match &self.sample {
            Ok(sample) => hold(
                sample,
                &self.headroom,
                policy.total.memory_bytes,
                pending(sql)?,
                need,
            ),
            Err(error) => Some(format!("held: host memory unreadable ({error})")),
        })
    }
    pub fn headroom(&self) -> &Headroom {
        &self.headroom
    }
}
