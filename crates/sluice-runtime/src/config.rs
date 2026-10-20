//! The home's `config.json` (SPEC §2.3). Read on each use, so an edit takes effect without a
//! restart. A key with a value of the wrong shape is ignored with a warning and its default
//! applies; a missing or unreadable file means every default.
use serde_json::Value;
use std::{path::Path, time::Duration};

/// `log_max` when config.json does not set it.
pub const DEFAULT_LOG_MAX: usize = 10_000;
/// The port `serve` binds when neither `--port` nor config.json's `http.port` gives one.
pub const DEFAULT_PORT: u16 = 3065;
/// The host `serve` binds when neither `--host` nor config.json's `http.host` gives one.
pub const DEFAULT_HOST: &str = "127.0.0.1";
/// `notify.timeout_s` when config.json does not set it.
pub const DEFAULT_NOTIFY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct NotifyConfig {
    /// argv, run without a shell.
    pub command: Vec<String>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HomeConfig {
    /// `http.host`, when set.
    pub host: Option<String>,
    /// `http.port`, when set.
    pub port: Option<u16>,
    /// Records kept per log before a trim; a trim keeps the newest 90%.
    pub log_max: usize,
    /// Minutes an unread settlement may wait before the owner is asked about it.
    pub unread_alert_min: Option<f64>,
    pub notify: Option<NotifyConfig>,
}

impl Default for HomeConfig {
    fn default() -> Self {
        Self {
            host: None,
            port: None,
            log_max: DEFAULT_LOG_MAX,
            unread_alert_min: None,
            notify: None,
        }
    }
}

impl HomeConfig {
    pub fn load(home: &Path) -> Self {
        let path = home.join("config.json");
        let value = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(path=%path.display(), %error, "config.json is not JSON; using defaults");
                    return Self::default();
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                tracing::warn!(path=%path.display(), %error, "cannot read config.json; using defaults");
                return Self::default();
            }
        };
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Self {
        let mut config = Self::default();
        let ignored = |key: &str| tracing::warn!(key, "config.json value ignored: wrong shape");
        match value.get("http") {
            None | Some(Value::Null) => {}
            Some(Value::Object(http)) => {
                match http.get("host") {
                    None | Some(Value::Null) => {}
                    Some(Value::String(host)) if !host.trim().is_empty() => {
                        config.host = Some(host.clone())
                    }
                    Some(_) => ignored("http.host"),
                }
                match http.get("port") {
                    None | Some(Value::Null) => {}
                    Some(port) => match port.as_u64().and_then(|p| u16::try_from(p).ok()) {
                        Some(port) if port > 0 => config.port = Some(port),
                        _ => ignored("http.port"),
                    },
                }
            }
            Some(_) => ignored("http"),
        }
        match value.get("log_max") {
            None | Some(Value::Null) => {}
            Some(n) => match n.as_u64().and_then(|n| usize::try_from(n).ok()) {
                Some(n) if n > 0 => config.log_max = n,
                _ => ignored("log_max"),
            },
        }
        match value.get("unread_alert_min") {
            None | Some(Value::Null) => {}
            Some(n) => match n.as_f64() {
                Some(n) if n.is_finite() && n > 0.0 => config.unread_alert_min = Some(n),
                _ => ignored("unread_alert_min"),
            },
        }
        match value.get("notify") {
            None | Some(Value::Null) => {}
            Some(Value::Object(notify)) => {
                let command = notify
                    .get("command")
                    .and_then(Value::as_array)
                    .and_then(|argv| {
                        argv.iter()
                            .map(|a| a.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    });
                let timeout = match notify.get("timeout_s") {
                    None | Some(Value::Null) => Some(DEFAULT_NOTIFY_TIMEOUT),
                    Some(n) => n
                        .as_f64()
                        .filter(|n| n.is_finite() && *n > 0.0)
                        .and_then(|n| Duration::try_from_secs_f64(n).ok()),
                };
                match (command, timeout) {
                    (Some(command), Some(timeout))
                        if command.first().is_some_and(|c| !c.is_empty()) =>
                    {
                        config.notify = Some(NotifyConfig { command, timeout })
                    }
                    _ => ignored("notify"),
                }
            }
            Some(_) => ignored("notify"),
        }
        config
    }

    /// The log trim target: 90% of `log_max`, at least one record.
    pub fn log_keep(&self) -> usize {
        (self.log_max - self.log_max / 10).max(1)
    }
}

/// Admission by measured memory and CPU (SPEC §2.5): what must stay free for everything that
/// is not a run, the memory and CPU pressure and the load above which no run starts, what a run
/// not yet measured counts, and the workload slice's CPU weight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Headroom {
    /// `execution.host_reserve`: MemAvailable must keep this much after a new run's charge.
    pub host_reserve_bytes: u64,
    /// `execution.memory_pressure_max`: the PSI `some avg10` percentage at or above which runs
    /// are held.
    pub pressure_max: f64,
    /// `execution.admission_charge`: what a new run needs to start and counts until its
    /// warm-up is over.
    pub charge_bytes: u64,
    /// `execution.admission_warmup_s`: how long a new run counts at its charge before its
    /// measured memory.current stands for it.
    pub warmup: Duration,
    /// `execution.cpu_pressure_max`: the CPU PSI `some avg10` percentage at or above which runs
    /// are held.
    pub cpu_pressure_max: f64,
    /// `execution.load_per_cpu_max`: the 1-minute load per online CPU at or above which runs
    /// are held.
    pub load_per_cpu_max: f64,
    /// `execution.warmup_starts`: while CPU pressure or load is over half its limit, at most
    /// this many runs start within one warm-up.
    pub warmup_starts: u32,
    /// `execution.cpu_weight`: the workload slices' CPUWeight, so work outside them is
    /// scheduled first when the host is busy.
    pub cpu_weight: u16,
}
/// The shipped `memory_pressure_max`.
pub const DEFAULT_MEMORY_PRESSURE_MAX: f64 = 10.0;
/// The shipped `admission_charge`.
pub const DEFAULT_ADMISSION_CHARGE: u64 = 1 << 30;
/// The shipped `admission_warmup_s`.
pub const DEFAULT_ADMISSION_WARMUP: Duration = Duration::from_secs(30);
/// The shipped `cpu_pressure_max`.
pub const DEFAULT_CPU_PRESSURE_MAX: f64 = 30.0;
/// The shipped `load_per_cpu_max`.
pub const DEFAULT_LOAD_PER_CPU_MAX: f64 = 1.5;
/// The shipped `warmup_starts`.
pub const DEFAULT_WARMUP_STARTS: u32 = 2;
/// The shipped `cpu_weight` (systemd's default is 100).
pub const DEFAULT_CPU_WEIGHT: u16 = 20;
impl Headroom {
    /// The shipped host reserve is max(16 GiB, 15% of memory), never more than a quarter of it.
    pub fn for_host(memory_bytes: u64) -> Self {
        let share = |percent: u128| {
            u64::try_from(u128::from(memory_bytes) * percent / 100).unwrap_or(u64::MAX)
        };
        Self {
            host_reserve_bytes: (16u64 << 30).max(share(15)).min(share(25)),
            pressure_max: DEFAULT_MEMORY_PRESSURE_MAX,
            charge_bytes: DEFAULT_ADMISSION_CHARGE,
            warmup: DEFAULT_ADMISSION_WARMUP,
            cpu_pressure_max: DEFAULT_CPU_PRESSURE_MAX,
            load_per_cpu_max: DEFAULT_LOAD_PER_CPU_MAX,
            warmup_starts: DEFAULT_WARMUP_STARTS,
            cpu_weight: DEFAULT_CPU_WEIGHT,
        }
    }
}

/// Load execution policy separately from display preferences: malformed policy or an
/// unreadable configuration must stop admission, never disable resource enforcement.
pub fn load_resource_policy(home: &Path) -> Result<sluice_model::ResourcePolicy, String> {
    load_execution(home).map(|(policy, _)| policy)
}
/// The execution policy and the measured-headroom settings, from one read of config.json.
pub fn load_execution(home: &Path) -> Result<(sluice_model::ResourcePolicy, Headroom), String> {
    let value = match std::fs::read(home.join("config.json")) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| format!("cannot parse config.json: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(format!("cannot read config.json: {error}")),
    };
    execution_from_value(&value, &HostCapacity::read()?)
}

pub fn resource_policy_from_value(
    value: &Value,
    memory_bytes: u64,
    cpu_millis: u32,
) -> Result<sluice_model::ResourcePolicy, String> {
    execution_from_value(value, &HostCapacity::plain(memory_bytes, cpu_millis))
        .map(|(policy, _)| policy)
}

pub fn execution_from_value(
    value: &Value,
    host: &HostCapacity,
) -> Result<(sluice_model::ResourcePolicy, Headroom), String> {
    let (memory_bytes, cpu_millis) = (host.memory_bytes, host.cpu_millis);
    use sluice_model::{LimitsOverride, ResourcePolicy};
    #[derive(serde::Deserialize, Default)]
    #[serde(deny_unknown_fields)]
    struct ExecutionConfig {
        #[serde(default)]
        defaults: LimitsOverride,
        #[serde(default)]
        ceilings: LimitsOverride,
        #[serde(default)]
        total: LimitsOverride,
        guardian_memory: Option<sluice_model::limits::MemoryBytes>,
        #[serde(default)]
        allow_unlimited_memory: bool,
        host_reserve: Option<sluice_model::limits::MemoryBytes>,
        memory_pressure_max: Option<f64>,
        admission_charge: Option<sluice_model::limits::MemoryBytes>,
        admission_warmup_s: Option<f64>,
        cpu_pressure_max: Option<f64>,
        load_per_cpu_max: Option<f64>,
        warmup_starts: Option<u32>,
        cpu_weight: Option<u16>,
    }
    if !value.is_object() {
        return Err("config.json must contain an object".into());
    }
    let config: ExecutionConfig = match value.get("execution") {
        None => ExecutionConfig::default(),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid execution policy: {error}"))?,
    };
    // Unlimited memory is never an accident: a null at any level needs the opt-in.
    if !config.allow_unlimited_memory {
        for (level, layer) in [
            ("defaults", &config.defaults),
            ("ceilings", &config.ceilings),
            ("total", &config.total),
        ] {
            if layer.memory == Some(None) {
                return Err(format!(
                    "execution.{level}.memory is unlimited (null), so runs would have no memory cap; set it to a size such as \"{}\", or set execution.allow_unlimited_memory to true to accept that",
                    match level {
                        "defaults" => "6GiB",
                        "ceilings" => "10GiB",
                        _ => "70GiB",
                    }
                ));
            }
        }
    }
    let mut headroom = Headroom::for_host(memory_bytes);
    if let Some(reserve) = config.host_reserve {
        headroom.host_reserve_bytes = reserve.0;
    }
    if let Some(pressure) = config.memory_pressure_max {
        if !(pressure.is_finite() && pressure > 0.0 && pressure <= 100.0) {
            return Err(
                "execution.memory_pressure_max must be a percentage above 0 and at most 100".into(),
            );
        }
        headroom.pressure_max = pressure;
    }
    if let Some(charge) = config.admission_charge {
        headroom.charge_bytes = charge.0;
    }
    if let Some(warmup) = config.admission_warmup_s {
        headroom.warmup = Duration::try_from_secs_f64(warmup)
            .map_err(|_| "execution.admission_warmup_s must be a number of seconds, 0 or more")?;
    }
    if let Some(pressure) = config.cpu_pressure_max {
        if !(pressure.is_finite() && pressure > 0.0 && pressure <= 100.0) {
            return Err(
                "execution.cpu_pressure_max must be a percentage above 0 and at most 100".into(),
            );
        }
        headroom.cpu_pressure_max = pressure;
    }
    if let Some(load) = config.load_per_cpu_max {
        if !(load.is_finite() && load > 0.0) {
            return Err("execution.load_per_cpu_max must be a number above 0".into());
        }
        headroom.load_per_cpu_max = load;
    }
    if let Some(starts) = config.warmup_starts {
        if starts == 0 {
            return Err("execution.warmup_starts must be at least 1".into());
        }
        headroom.warmup_starts = starts;
    }
    if let Some(weight) = config.cpu_weight {
        if !(1..=10_000).contains(&weight) {
            return Err("execution.cpu_weight must be from 1 to 10000".into());
        }
        headroom.cpu_weight = weight;
    }
    let mut policy = ResourcePolicy::for_host(memory_bytes, cpu_millis)?;
    policy.total = config.total.apply(policy.total)?;
    use sluice_model::limits::size;
    if let Some(total) = policy.total.memory_bytes.filter(|n| *n > memory_bytes) {
        return Err(match &host.memory_cgroup {
            Some(group) => format!(
                "execution.total.memory ({}) exceeds the host's effective memory: cgroup {group} has memory.max {}; set execution.total.memory to at most {} or raise that cgroup's limit",
                size(total),
                size(memory_bytes),
                size(memory_bytes)
            ),
            None => format!(
                "execution.total.memory ({}) exceeds the host's memory ({} MemTotal)",
                size(total),
                size(memory_bytes)
            ),
        });
    }
    if let Some(total) = policy.total.cpu_millis.filter(|n| *n > cpu_millis) {
        let cpus = |n: u32| f64::from(n) / 1000.0;
        return Err(match &host.cpu_cgroup {
            Some(group) => format!(
                "execution.total.cpus ({}) exceeds the host's effective CPUs: cgroup {group} has cpu.max of {} CPUs; lower execution.total.cpus or raise that cgroup's limit",
                cpus(total),
                cpus(cpu_millis)
            ),
            None => format!(
                "execution.total.cpus ({}) exceeds the host's {} CPUs",
                cpus(total),
                cpus(cpu_millis)
            ),
        });
    }
    if let Some(memory) = config.guardian_memory {
        policy.guardian_memory_bytes = memory.0;
    }
    // Inherited ceilings shrink to fit a smaller finite total; explicit ones are checked.
    let room = |total: Option<u64>, guardian: u64| total.map(|n| n.saturating_sub(guardian));
    let clamp = |inherited: Option<u64>, bound: Option<u64>| match (inherited, bound) {
        (Some(n), Some(bound)) => Some(n.min(bound)),
        (n, _) => n,
    };
    let narrow = |n: Option<u64>| n.map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    policy.ceilings.memory_bytes = clamp(
        policy.ceilings.memory_bytes,
        room(policy.total.memory_bytes, policy.guardian_memory_bytes),
    );
    policy.ceilings.cpu_millis = narrow(clamp(
        policy.ceilings.cpu_millis.map(u64::from),
        room(
            policy.total.cpu_millis.map(u64::from),
            u64::from(sluice_model::limits::GUARDIAN_CPU_MILLIS),
        ),
    ));
    policy.ceilings.tasks = narrow(clamp(
        policy.ceilings.tasks.map(u64::from),
        room(
            policy.total.tasks.map(u64::from),
            u64::from(sluice_model::limits::GUARDIAN_TASKS),
        ),
    ));
    policy.ceilings = config.ceilings.apply(policy.ceilings)?;
    // Inherited defaults shrink to the ceilings; an unlimited default under a finite
    // ceiling becomes that ceiling.
    let fit = |default: Option<u64>, ceiling: Option<u64>| match (default, ceiling) {
        (_, None) => default,
        (None, ceiling) => ceiling,
        (Some(n), Some(ceiling)) => Some(n.min(ceiling)),
    };
    policy.defaults.memory_bytes = fit(policy.defaults.memory_bytes, policy.ceilings.memory_bytes);
    policy.defaults.cpu_millis = narrow(fit(
        policy.defaults.cpu_millis.map(u64::from),
        policy.ceilings.cpu_millis.map(u64::from),
    ));
    policy.defaults.tasks = narrow(fit(
        policy.defaults.tasks.map(u64::from),
        policy.ceilings.tasks.map(u64::from),
    ));
    policy.defaults = config.defaults.apply(policy.defaults)?;
    // The shipped charge shrinks to fit a small total; an explicit one is checked.
    if config.admission_charge.is_none()
        && let Some(total) = policy.total.memory_bytes
    {
        headroom.charge_bytes = headroom.charge_bytes.min(total);
    }
    if policy
        .total
        .memory_bytes
        .is_some_and(|total| headroom.charge_bytes > total)
    {
        return Err(
            "execution.admission_charge exceeds the home memory total, so no run could start"
                .into(),
        );
    }
    Ok((policy.validate()?, headroom))
}

/// What the host gives this process: MemTotal and the CPU count, lowered by the nearest
/// `memory.max` and `cpu.max` among its cgroup and their ancestors. Each names the cgroup whose
/// limit set it, so a total above it can say where the limit comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCapacity {
    pub memory_bytes: u64,
    /// The cgroup (as `/user.slice`) whose memory.max is `memory_bytes`; `None` for MemTotal.
    pub memory_cgroup: Option<String>,
    pub cpu_millis: u32,
    /// The cgroup whose cpu.max is `cpu_millis`; `None` for the CPU count.
    pub cpu_cgroup: Option<String>,
}
impl HostCapacity {
    /// A host with no cgroup limits.
    pub fn plain(memory_bytes: u64, cpu_millis: u32) -> Self {
        Self {
            memory_bytes,
            memory_cgroup: None,
            cpu_millis,
            cpu_cgroup: None,
        }
    }
    /// This process's capacity, from /proc and /sys/fs/cgroup.
    pub fn read() -> Result<Self, String> {
        let meminfo = std::fs::read_to_string("/proc/meminfo")
            .map_err(|error| format!("cannot inspect host memory: {error}"))?;
        let cpus = u32::try_from(
            std::thread::available_parallelism()
                .map_err(|error| format!("cannot inspect CPU capacity: {error}"))?
                .get(),
        )
        .ok()
        .and_then(|n| n.checked_mul(1000))
        .ok_or("CPU capacity overflows")?;
        let membership = std::fs::read_to_string("/proc/self/cgroup")
            .map_err(|error| format!("cannot inspect cgroup membership: {error}"))?;
        Self::from_tree(Path::new("/sys/fs/cgroup"), &membership, &meminfo, cpus)
    }
    /// The capacity under the cgroup v2 tree at `root`, for a process with `/proc/self/cgroup`
    /// `membership`, `/proc/meminfo` text `meminfo` and `cpu_millis` CPUs.
    pub fn from_tree(
        root: &Path,
        membership: &str,
        meminfo: &str,
        cpu_millis: u32,
    ) -> Result<Self, String> {
        let memory = meminfo
            .lines()
            .find_map(|line| {
                line.strip_prefix("MemTotal:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()?
                    .checked_mul(1024)
            })
            .ok_or("host memory total is unavailable")?;
        let mut capacity = Self::plain(memory, cpu_millis);
        let relative = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or("cgroup v2 membership is unavailable")?;
        if relative.split('/').any(|part| part == "..") {
            return Err("cgroup membership is outside the visible hierarchy".into());
        }
        let mut current = root.join(relative.trim_start_matches('/'));
        loop {
            let name = || {
                format!(
                    "/{}",
                    current.strip_prefix(root).unwrap_or(&current).display()
                )
            };
            for (file, is_memory) in [("memory.max", true), ("cpu.max", false)] {
                match std::fs::read_to_string(current.join(file)) {
                    Ok(raw) => {
                        let fields: Vec<_> = raw.split_whitespace().collect();
                        if fields.first() == Some(&"max") {
                            continue;
                        }
                        if is_memory {
                            let limit = fields
                                .first()
                                .ok_or("empty memory.max")?
                                .parse::<u64>()
                                .map_err(|_| "invalid memory.max")?;
                            if limit < capacity.memory_bytes {
                                capacity.memory_bytes = limit;
                                capacity.memory_cgroup = Some(name());
                            }
                        } else {
                            let quota = fields
                                .first()
                                .ok_or("empty cpu.max")?
                                .parse::<u64>()
                                .map_err(|_| "invalid CPU quota")?;
                            let period = fields
                                .get(1)
                                .ok_or("missing CPU period")?
                                .parse::<u64>()
                                .map_err(|_| "invalid CPU period")?;
                            let milli = quota
                                .checked_mul(1000)
                                .and_then(|n| n.checked_div(period))
                                .ok_or("invalid CPU quota ratio")?;
                            let milli = u32::try_from(milli).unwrap_or(u32::MAX);
                            if milli < capacity.cpu_millis {
                                capacity.cpu_millis = milli;
                                capacity.cpu_cgroup = Some(name());
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "cannot read {}: {error}",
                            current.join(file).display()
                        ));
                    }
                }
            }
            if current == root {
                break;
            }
            if !current.pop() {
                return Err("invalid cgroup ancestry".into());
            }
        }
        Ok(capacity)
    }
}
