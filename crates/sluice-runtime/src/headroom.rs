//! Admission by measured memory and CPU (SPEC §2.5). A run's memory cap only kills it; what
//! admission counts is what the host and the live runs measure. A run starts only while
//! MemAvailable less the host reserve covers its admission charge, memory pressure (PSI `some
//! avg10`) is under its limit, the live runs' measured memory plus the charge stays within the
//! home total, CPU pressure and the 1-minute load per CPU are under theirs, and, while either
//! CPU reading is over half its limit, fewer than the warm-up's allowance of runs started within
//! the warm-up. A run admitted within the warm-up counts at the charge (or its reading, if
//! larger), since the readings do not show it yet: a burst of launches never passes on one
//! stale reading.
use crate::config::Headroom;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sluice_model::{
    ids::{AttemptId, RunId},
    limits::{RunLimits, RunMemory, size},
};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, SystemTime},
};

/// One reading of the host, as the execution host takes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HostReading {
    /// `/proc/meminfo` MemAvailable.
    pub available_bytes: u64,
    /// Effective memory and CPU capacity, from the same host sample.
    #[serde(default)]
    pub capacity: Option<crate::config::HostCapacity>,
    /// `/proc/pressure/memory` `some avg10`, a percentage; `None` on a kernel without PSI.
    #[serde(default)]
    pub memory_pressure_some_avg10: Option<f64>,
    /// `/proc/pressure/cpu` `some avg10`, a percentage; `None` on a kernel without PSI.
    #[serde(default)]
    pub cpu_pressure_some_avg10: Option<f64>,
    /// `/proc/loadavg`'s 1-minute load.
    #[serde(default)]
    pub load_avg1: Option<f64>,
    /// The host's online CPUs, which the load is divided by.
    #[serde(default)]
    pub cpus: Option<u32>,
}
impl HostReading {
    /// The 1-minute load per online CPU.
    pub fn load_per_cpu(&self) -> Option<f64> {
        Some(self.load_avg1? / f64::from(self.cpus.filter(|n| *n > 0)?))
    }
}

/// How often a held run is checked again.
pub const RECHECK_INTERVAL: Duration = Duration::from_secs(5);
/// A published reading older than this is not shown.
const SAMPLE_FRESH: Duration = Duration::from_secs(120);
const SAMPLE_FILE: &str = "host-reading.json";

/// What admission measured at one moment: the host, and each live run with a payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// When it was taken by the execution host's clock, in seconds since the Unix epoch.
    pub at: f64,
    pub host: HostReading,
    /// A run whose cgroup could not be read holds the error; it counts at its cap.
    pub runs: BTreeMap<RunId, Result<RunMemory, String>>,
}
impl Reading {
    /// The host's reading and each live run's, through the execution host's seams.
    /// `None` when the host has nothing to measure.
    pub async fn take<F, Fut>(
        reads: &sluice_store::ReadPool,
        host: Option<std::io::Result<HostReading>>,
        now: SystemTime,
        run_memory: F,
    ) -> Option<std::io::Result<Self>>
    where
        F: Fn(RunId, String) -> Fut,
        Fut: Future<Output = Option<std::io::Result<RunMemory>>>,
    {
        let host = match host? {
            Ok(host) => host,
            Err(error) => return Some(Err(error)),
        };
        let live = reads
            .snapshot(|sql| {
                let mut statement = sql.prepare(
                    "SELECT r.run_id,r.cgroup FROM runs r JOIN attempts a USING(attempt_id)
                     WHERE r.finished_at IS NULL AND r.cgroup IS NOT NULL",
                )?;
                let rows = statement
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .await;
        let live = match live {
            Ok(live) => live,
            Err(error) => return Some(Err(std::io::Error::other(error.to_string()))),
        };
        let mut runs = BTreeMap::new();
        for (run, cgroup) in live {
            let Ok(run) = run.parse::<RunId>() else {
                continue;
            };
            let service = cgroup
                .strip_suffix("/control")
                .unwrap_or(&cgroup)
                .to_owned();
            if let Some(memory) = run_memory(run, service).await {
                runs.insert(run, memory.map_err(|error| error.to_string()));
            }
        }
        Some(Ok(Self {
            at: unix_seconds(now),
            host,
            runs,
        }))
    }
}

/// Keep the largest working-memory sample independently of admission's latest reading.
/// The conditional update makes repeated samples cheap and survives coordinator restarts.
pub(crate) fn record_working_peak(
    tx: &mut sluice_store::WriteTransaction<'_>,
    run: RunId,
    peak: Option<u64>,
) -> sluice_store::Result<()> {
    if let Some(peak) = peak {
        let changed = tx.sql().execute(
            "UPDATE attempts SET request=json_set(request,'$.memory_working_peak_bytes',?2)
             WHERE attempt_id=(SELECT attempt_id FROM runs WHERE run_id=?1 AND finished_at IS NULL)
             AND coalesce(json_extract(request,'$.memory_working_peak_bytes'),-1) < ?2",
            (run.to_string(), i64::try_from(peak).unwrap_or(i64::MAX)),
        )?;
        if changed > 0 {
            let project: Option<String> = tx.sql().query_row(
                "SELECT project_id FROM runs WHERE run_id=?1",
                [run.to_string()],
                |row| row.get(0),
            )?;
            tx.changed(project.map(crate::calls::parse_id).transpose()?, "status");
        }
    }
    Ok(())
}

fn unix_seconds(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// What the live runs count for admission at a reading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pool {
    /// Every live run at its measured working memory, or its charge within the warm-up.
    pub measured_bytes: u64,
    /// The charges of runs still within the warm-up, which MemAvailable does not show yet.
    pub starting_bytes: u64,
    /// The runs still within the warm-up, whose CPU the load does not show yet.
    pub starting_runs: u32,
}

/// What the live attempts count at `reading`, counting attempts admitted in this transaction.
pub fn pool(
    sql: &Connection,
    reading: &Reading,
    headroom: &Headroom,
) -> sluice_store::Result<Pool> {
    let mut statement = sql.prepare(
        "SELECT r.run_id,?1-coalesce(json_extract(a.request,'$.admitted_at'),(julianday(a.created_at)-2440587.5)*86400.0),a.limits
         FROM attempts a LEFT JOIN runs r USING(attempt_id)
         WHERE a.phase<>'terminal' OR (a.reservation IS NOT NULL AND COALESCE(json_extract(a.request,'$.resource_released'),0)=0)",
    )?;
    let rows = statement
        .query_map([reading.at], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let charge = headroom.charge_bytes;
    let mut pool = Pool::default();
    for (run, age, limits) in rows {
        let measured = match run
            .and_then(|r| r.parse::<RunId>().ok())
            .and_then(|r| reading.runs.get(&r))
        {
            Some(Ok(memory)) => Some(memory.working_bytes.unwrap_or(memory.current_bytes)),
            // An unreadable run counts at its cap: it may be using all of it.
            Some(Err(_)) => Some(
                limits
                    .map(|l| serde_json::from_str::<RunLimits>(&l))
                    .transpose()?
                    .and_then(|l| l.memory_max_bytes)
                    .unwrap_or(charge)
                    .max(charge),
            ),
            None => None,
        };
        let counted = if age < headroom.warmup.as_secs_f64() {
            pool.starting_bytes = pool.starting_bytes.saturating_add(charge);
            pool.starting_runs = pool.starting_runs.saturating_add(1);
            measured.unwrap_or(0).max(charge)
        } else {
            measured.unwrap_or(charge)
        };
        pool.measured_bytes = pool.measured_bytes.saturating_add(counted);
    }
    Ok(pool)
}

/// Why a new run cannot start now: the host resource it waits for and the reason as status
/// says it, with the numbers the check used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    Memory(String),
    Cpu(String),
}
impl Hold {
    pub fn reason(&self) -> &str {
        match self {
            Self::Memory(reason) | Self::Cpu(reason) => reason,
        }
    }
    pub fn into_reason(self) -> String {
        match self {
            Self::Memory(reason) | Self::Cpu(reason) => reason,
        }
    }
    /// What a held step waits for, as status lists it.
    pub fn resource(&self) -> &'static str {
        match self {
            Self::Memory(_) => "host memory",
            Self::Cpu(_) => "host cpu",
        }
    }
    /// The owner's notice's `limit`.
    pub fn limit(&self) -> &'static str {
        match self {
            Self::Memory(_) => "host_memory",
            Self::Cpu(_) => "host_cpu",
        }
    }
}

/// Why a new run cannot start now, with the numbers the check used.
pub fn hold(
    host: &HostReading,
    headroom: &Headroom,
    total: Option<u64>,
    pool: Pool,
) -> Option<Hold> {
    memory_hold(host, headroom, total, pool)
        .map(Hold::Memory)
        .or_else(|| cpu_hold(host, headroom, pool).map(Hold::Cpu))
}

fn memory_hold(
    host: &HostReading,
    headroom: &Headroom,
    total: Option<u64>,
    pool: Pool,
) -> Option<String> {
    let need = headroom.charge_bytes;
    let room = host
        .available_bytes
        .saturating_sub(headroom.host_reserve_bytes)
        .saturating_sub(pool.starting_bytes);
    if room < need {
        let starting = if pool.starting_bytes > 0 {
            format!(", starting {}", size(pool.starting_bytes))
        } else {
            String::new()
        };
        return Some(format!(
            "held: host memory (available {}, reserve {}{starting}, need {})",
            size(host.available_bytes),
            size(headroom.host_reserve_bytes),
            size(need)
        ));
    }
    if let Some(pressure) = host.memory_pressure_some_avg10
        && pressure >= headroom.pressure_max
    {
        return Some(format!(
            "held: memory pressure (some avg10 {pressure:.1}%, limit {}%)",
            headroom.pressure_max
        ));
    }
    if let Some(total) = total
        && pool.measured_bytes.saturating_add(need) > total
    {
        return Some(format!(
            "held: pool memory (measured {} + {} > {})",
            size(pool.measured_bytes),
            size(need),
            size(total)
        ));
    }
    None
}

fn cpu_hold(host: &HostReading, headroom: &Headroom, pool: Pool) -> Option<String> {
    let pressure = host.cpu_pressure_some_avg10;
    if let Some(pressure) = pressure
        && pressure >= headroom.cpu_pressure_max
    {
        return Some(format!(
            "held: cpu pressure (some avg10 {pressure:.1}%, limit {}%)",
            headroom.cpu_pressure_max
        ));
    }
    let load = host.load_per_cpu();
    if let Some(load) = load
        && load >= headroom.load_per_cpu_max
    {
        return Some(format!(
            "held: cpu load (1-min load {:.1} on {} CPUs, {load:.2} per CPU, limit {})",
            host.load_avg1.unwrap_or_default(),
            host.cpus.unwrap_or_default(),
            headroom.load_per_cpu_max
        ));
    }
    // Over half a limit, a burst waits for the warm-up: the readings do not show its runs yet.
    if pool.starting_runs >= headroom.warmup_starts {
        let busy = if let Some(pressure) = pressure
            && pressure >= headroom.cpu_pressure_max / 2.0
        {
            Some(format!(
                "cpu pressure {pressure:.1}%, over half of {}%",
                headroom.cpu_pressure_max
            ))
        } else if let Some(load) = load
            && load >= headroom.load_per_cpu_max / 2.0
        {
            Some(format!(
                "load {load:.2} per CPU, over half of {}",
                headroom.load_per_cpu_max
            ))
        } else {
            None
        };
        if let Some(busy) = busy {
            return Some(format!(
                "held: cpu warm-up ({} runs started within {} s, limit {} while {busy})",
                pool.starting_runs,
                headroom.warmup.as_secs_f64(),
                headroom.warmup_starts
            ));
        }
    }
    None
}

/// The admission's own reading, for status and the dashboard in whichever process serves
/// them. A failed write only leaves status without the numbers.
pub fn publish(home: &Path, reading: &Reading) {
    let stamped = serde_json::json!({
        "sampled_at": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs(),
        "reading": reading,
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
pub fn published(home: &Path) -> Option<Reading> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(SAMPLE_FILE)).ok()?).ok()?;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    (now.saturating_sub(value["sampled_at"].as_u64()?) <= SAMPLE_FRESH.as_secs())
        .then(|| serde_json::from_value(value["reading"].clone()).ok())
        .flatten()
}

/// The host's own reading: `/proc/meminfo`, `/proc/pressure/{memory,cpu}`, `/proc/loadavg`
/// and the online CPUs in `/proc/stat`.
pub fn read_host() -> std::io::Result<HostReading> {
    let meminfo = std::fs::read_to_string("/proc/meminfo")?;
    let available_bytes = parse_meminfo(&meminfo)
        .ok_or_else(|| std::io::Error::other("MemAvailable missing from /proc/meminfo"))?;
    let loadavg = std::fs::read_to_string("/proc/loadavg")?;
    let load_avg1 = loadavg
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<f64>().ok())
        .ok_or_else(|| std::io::Error::other("invalid /proc/loadavg"))?;
    let stat = std::fs::read_to_string("/proc/stat")?;
    let cpus = stat
        .lines()
        .filter(|line| {
            line.strip_prefix("cpu")
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .count();
    Ok(HostReading {
        available_bytes,
        capacity: Some(crate::config::HostCapacity::read().map_err(std::io::Error::other)?),
        memory_pressure_some_avg10: read_pressure("/proc/pressure/memory")?,
        cpu_pressure_some_avg10: read_pressure("/proc/pressure/cpu")?,
        load_avg1: Some(load_avg1),
        cpus: u32::try_from(cpus).ok().filter(|n| *n > 0),
    })
}
/// A PSI file's `some avg10`; `None` on a kernel built or booted without PSI.
fn read_pressure(path: &str) -> std::io::Result<Option<f64>> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            Ok(Some(parse_pressure(&text).ok_or_else(|| {
                std::io::Error::other(format!("invalid {path}"))
            })?))
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::Unsupported
            ) || error.raw_os_error() == Some(95) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
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
    reading: Result<Reading, String>,
    headroom: Headroom,
}
impl Gate {
    /// `None` when the host has nothing to measure. Admission publishes the sample before
    /// loading policy, so test host capacity and the pressure gate share that same reading.
    pub fn new(reading: Option<std::io::Result<Reading>>, headroom: Headroom) -> Option<Self> {
        let reading = reading?.map_err(|error| error.to_string());
        Some(Self { reading, headroom })
    }
    /// Retain each admission sample even when no new run can start.
    pub async fn record_peaks(
        &self,
        writer: &sluice_store::Writer,
    ) -> Result<(), sluice_model::error::PublicError> {
        if let Ok(reading) = &self.reading {
            let peaks: Vec<_> = reading
                .runs
                .iter()
                .filter_map(|(run, memory)| Some((*run, memory.as_ref().ok()?.working_bytes?)))
                .collect();
            if !peaks.is_empty() {
                writer
                    .write(sluice_store::RetrySafety::Idempotent, move |tx| {
                        for (run, peak) in &peaks {
                            record_working_peak(tx, *run, Some(*peak))?;
                        }
                        Ok(())
                    })
                    .await?;
            }
        }
        Ok(())
    }
    /// Why a new run must wait, counting runs admitted in this transaction too.
    pub fn hold(
        &self,
        sql: &Connection,
        policy: &sluice_model::ResourcePolicy,
    ) -> sluice_store::Result<Option<Hold>> {
        Ok(match &self.reading {
            Ok(reading) => hold(
                &reading.host,
                &self.headroom,
                policy.total.memory_bytes,
                pool(sql, reading, &self.headroom)?,
            ),
            Err(error) => Some(Hold::Memory(format!("held: host unreadable ({error})"))),
        })
    }
    /// Stamp an attempt this gate admitted with the reading's time, so its warm-up runs on the
    /// same clock as the readings.
    pub fn admitted(&self, sql: &Connection, attempt: AttemptId) -> sluice_store::Result<()> {
        if let Ok(reading) = &self.reading {
            sql.execute(
                "UPDATE attempts SET request=json_insert(request,'$.admitted_at',?2) WHERE attempt_id=?1",
                (attempt.to_string(), reading.at),
            )?;
        }
        Ok(())
    }
    pub fn headroom(&self) -> &Headroom {
        &self.headroom
    }
}

/// The status form of memory admission: the settings, and the numbers of the reading admission
/// last published (`null` while there is none).
pub fn shown(
    sql: &Connection,
    home: &Path,
    policy: &sluice_model::ResourcePolicy,
    headroom: &Headroom,
) -> sluice_store::Result<serde_json::Value> {
    let reading = published(home);
    let pool = reading
        .as_ref()
        .map(|reading| pool(sql, reading, headroom))
        .transpose()?;
    Ok(serde_json::json!({
        "total_bytes": sluice_model::limits::shown(policy.total.memory_bytes),
        "measured_bytes": pool.map(|p| p.measured_bytes),
        "starting_bytes": pool.map(|p| p.starting_bytes),
        "available_bytes": reading.as_ref().map(|r| r.host.available_bytes),
        "pressure_some_avg10": reading.as_ref().and_then(|r| r.host.memory_pressure_some_avg10),
        "host_reserve_bytes": headroom.host_reserve_bytes,
        "pressure_max": headroom.pressure_max,
        "admission_charge_bytes": headroom.charge_bytes,
        "admission_warmup_s": headroom.warmup.as_secs_f64(),
    }))
}

/// The status form of CPU admission: the settings, and the numbers of the reading admission
/// last published (`null` while there is none).
pub fn shown_cpu(
    sql: &Connection,
    home: &Path,
    headroom: &Headroom,
) -> sluice_store::Result<serde_json::Value> {
    let reading = published(home);
    let pool = reading
        .as_ref()
        .map(|reading| pool(sql, reading, headroom))
        .transpose()?;
    let host = reading.as_ref().map(|r| &r.host);
    Ok(serde_json::json!({
        "pressure_some_avg10": host.and_then(|h| h.cpu_pressure_some_avg10),
        "load_avg1": host.and_then(|h| h.load_avg1),
        "cpus": host.and_then(|h| h.cpus),
        "load_per_cpu": host.and_then(|h| h.load_per_cpu()),
        "starting_runs": pool.map(|p| p.starting_runs),
        "pressure_max": headroom.cpu_pressure_max,
        "load_per_cpu_max": headroom.load_per_cpu_max,
        "warmup_starts": headroom.warmup_starts,
        "cpu_weight": headroom.cpu_weight,
    }))
}

/// A live run's measured memory and peak, from the reading admission last published.
pub fn published_run(home: &Path, run: RunId) -> Option<RunMemory> {
    published(home)?.runs.get(&run)?.clone().ok()
}
