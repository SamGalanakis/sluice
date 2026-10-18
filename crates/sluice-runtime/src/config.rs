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

/// Load execution policy separately from display preferences: malformed policy or an
/// unreadable configuration must stop admission, never disable resource enforcement.
pub fn load_resource_policy(home: &Path) -> Result<sluice_model::ResourcePolicy, String> {
    let value = match std::fs::read(home.join("config.json")) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| format!("cannot parse config.json: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(error) => return Err(format!("cannot read config.json: {error}")),
    };
    let (memory, cpus) = effective_host_capacity()?;
    resource_policy_from_value(&value, memory, cpus)
}

pub fn resource_policy_from_value(
    value: &Value,
    memory_bytes: u64,
    cpu_millis: u32,
) -> Result<sluice_model::ResourcePolicy, String> {
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
    }
    if !value.is_object() {
        return Err("config.json must contain an object".into());
    }
    let config: ExecutionConfig = match value.get("execution") {
        None => ExecutionConfig::default(),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| format!("invalid execution policy: {error}"))?,
    };
    let mut policy = ResourcePolicy::for_host(memory_bytes, cpu_millis)?;
    policy.total = config.total.apply(policy.total)?;
    if policy.total.memory_bytes > memory_bytes || policy.total.cpu_millis > cpu_millis {
        return Err("execution total exceeds the host's effective memory or CPU capacity".into());
    }
    if let Some(memory) = config.guardian_memory {
        policy.guardian_memory_bytes = memory.0;
    }
    policy.ceilings.memory_bytes = policy.ceilings.memory_bytes.min(
        policy
            .total
            .memory_bytes
            .saturating_sub(policy.guardian_memory_bytes),
    );
    policy.ceilings.cpu_millis = policy.ceilings.cpu_millis.min(
        policy
            .total
            .cpu_millis
            .saturating_sub(sluice_model::limits::GUARDIAN_CPU_MILLIS),
    );
    policy.ceilings.tasks = policy.ceilings.tasks.min(
        policy
            .total
            .tasks
            .saturating_sub(sluice_model::limits::GUARDIAN_TASKS),
    );
    policy.ceilings = config.ceilings.apply(policy.ceilings)?;
    policy.defaults.memory_bytes = policy
        .defaults
        .memory_bytes
        .min(policy.ceilings.memory_bytes);
    policy.defaults.cpu_millis = policy.defaults.cpu_millis.min(policy.ceilings.cpu_millis);
    policy.defaults.tasks = policy.defaults.tasks.min(policy.ceilings.tasks);
    policy.defaults = config.defaults.apply(policy.defaults)?;
    policy.validate()
}

fn effective_host_capacity() -> Result<(u64, u32), String> {
    let meminfo = std::fs::read_to_string("/proc/meminfo")
        .map_err(|error| format!("cannot inspect host memory: {error}"))?;
    let mut memory = meminfo
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
    let mut cpus = u32::try_from(
        std::thread::available_parallelism()
            .map_err(|error| format!("cannot inspect CPU capacity: {error}"))?
            .get(),
    )
    .ok()
    .and_then(|n| n.checked_mul(1000))
    .ok_or("CPU capacity overflows")?;
    let membership = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|error| format!("cannot inspect cgroup membership: {error}"))?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("cgroup v2 membership is unavailable")?;
    if relative.split('/').any(|part| part == "..") {
        return Err("cgroup membership is outside the visible hierarchy".into());
    }
    let root = Path::new("/sys/fs/cgroup");
    let mut current = root.join(relative.trim_start_matches('/'));
    loop {
        for (name, is_memory) in [("memory.max", true), ("cpu.max", false)] {
            match std::fs::read_to_string(current.join(name)) {
                Ok(raw) => {
                    let fields: Vec<_> = raw.split_whitespace().collect();
                    if fields.first() == Some(&"max") {
                        continue;
                    }
                    if is_memory {
                        memory = memory.min(
                            fields
                                .first()
                                .ok_or("empty memory.max")?
                                .parse::<u64>()
                                .map_err(|_| "invalid memory.max")?,
                        );
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
                        cpus = cpus.min(u32::try_from(milli).unwrap_or(u32::MAX));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "cannot read {}: {error}",
                        current.join(name).display()
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
    Ok((memory, cpus))
}
