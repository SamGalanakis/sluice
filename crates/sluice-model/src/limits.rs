//! Physical execution budgets, independent of a plan's logical resource leases.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const GUARDIAN_TASKS: u32 = 64;
pub const GUARDIAN_CPU_MILLIS: u32 = 100;
/// What a run with no memory reservation of its own reserves at admission: an opted-in
/// unlimited run still holds this much of the home total, never only the guardian's share.
pub const UNLIMITED_MEMORY_RESERVATION: u64 = 4 << 30;

/// One budget level (home total, per-run ceilings or per-run defaults). `None` is unlimited:
/// it serializes as `null` and is never gated or capped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub memory_bytes: Option<u64>,
    /// One CPU is 1000 milli-CPUs.
    pub cpu_millis: Option<u32>,
    pub tasks: Option<u32>,
}
impl Limits {
    pub const UNLIMITED: Self = Self {
        memory_bytes: None,
        cpu_millis: None,
        tasks: None,
    };
    pub fn validate(self) -> Result<Self, String> {
        if self.memory_bytes == Some(0) || self.cpu_millis == Some(0) || self.tasks == Some(0) {
            return Err("memory, cpus and tasks must be positive or unlimited".into());
        }
        Ok(self)
    }
    /// Every dimension within `ceiling`; unlimited fits only an unlimited ceiling.
    pub fn fits(self, ceiling: Self) -> bool {
        within(self.memory_bytes, ceiling.memory_bytes)
            && within(self.cpu_millis, ceiling.cpu_millis)
            && within(self.tasks, ceiling.tasks)
    }
}
fn within<T: PartialOrd>(value: Option<T>, ceiling: Option<T>) -> bool {
    match (value, ceiling) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(value), Some(ceiling)) => value <= ceiling,
    }
}

/// A run's frozen budget. `memory_bytes` is what admission reserves; `memory_max_bytes` is the
/// payload's `memory.max` (the per-run ceiling when the run was admitted). `None` is unlimited:
/// the cgroup controller set to `max`. A run resolved today always has a memory reservation;
/// an unlimited one is frozen only by releases before it, and reserves
/// [`UNLIMITED_MEMORY_RESERVATION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "RunLimitsWire")]
pub struct RunLimits {
    pub memory_bytes: Option<u64>,
    pub memory_max_bytes: Option<u64>,
    /// One CPU is 1000 milli-CPUs.
    pub cpu_millis: Option<u32>,
    pub tasks: Option<u32>,
}
/// Budgets frozen before unlimited values have no `memory_max_bytes`: their reservation was
/// also their cap.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunLimitsWire {
    memory_bytes: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    memory_max_bytes: Option<Option<u64>>,
    cpu_millis: Option<u32>,
    tasks: Option<u32>,
}
impl From<RunLimitsWire> for RunLimits {
    fn from(wire: RunLimitsWire) -> Self {
        Self {
            memory_bytes: wire.memory_bytes,
            memory_max_bytes: wire.memory_max_bytes.unwrap_or(wire.memory_bytes),
            cpu_millis: wire.cpu_millis,
            tasks: wire.tasks,
        }
    }
}
fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}
impl RunLimits {
    pub fn validate(self) -> Result<Self, String> {
        self.caps().validate()?;
        if self.memory_bytes == Some(0) {
            return Err("memory must be positive or unlimited".into());
        }
        if !within(self.memory_bytes, self.memory_max_bytes) {
            return Err("a run's memory reservation exceeds its memory cap".into());
        }
        Ok(self)
    }
    /// What the payload cgroup enforces.
    pub fn caps(self) -> Limits {
        Limits {
            memory_bytes: self.memory_max_bytes,
            cpu_millis: self.cpu_millis,
            tasks: self.tasks,
        }
    }
    /// What admission counts for the payload alone.
    pub fn reservation(self) -> Limits {
        Limits {
            memory_bytes: self.memory_bytes,
            cpu_millis: self.cpu_millis,
            tasks: self.tasks,
        }
    }
    pub fn fits(self, ceiling: Limits) -> bool {
        self.reservation().fits(ceiling) && within(self.memory_max_bytes, ceiling.memory_bytes)
    }
    /// The status form: unlimited dimensions read `"unlimited"`.
    pub fn shown(self) -> serde_json::Value {
        serde_json::json!({
            "memory_bytes": shown(self.memory_bytes),
            "memory_max_bytes": shown(self.memory_max_bytes),
            "cpu_millis": shown(self.cpu_millis),
            "tasks": shown(self.tasks),
        })
    }
}

/// Capacity a live attempt holds against the home total: its payload's finite limits plus the
/// guardian's share. Unlimited CPU and tasks reserve only the guardian's share; unlimited
/// memory reserves [`UNLIMITED_MEMORY_RESERVATION`] besides it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reserved {
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub tasks: u32,
}
impl Reserved {
    pub fn of(limits: RunLimits, guardian_memory_bytes: u64) -> Result<Self, String> {
        let overflow = || "run and guardian reservation overflows".to_string();
        Ok(Self {
            memory_bytes: limits
                .memory_bytes
                .unwrap_or(UNLIMITED_MEMORY_RESERVATION)
                .checked_add(guardian_memory_bytes)
                .ok_or_else(overflow)?,
            cpu_millis: limits
                .cpu_millis
                .unwrap_or(0)
                .checked_add(GUARDIAN_CPU_MILLIS)
                .ok_or_else(overflow)?,
            tasks: limits
                .tasks
                .unwrap_or(0)
                .checked_add(GUARDIAN_TASKS)
                .ok_or_else(overflow)?,
        })
    }
    /// Every dimension within `total`; an unlimited total holds anything.
    pub fn fits(self, total: Limits) -> bool {
        total.memory_bytes.is_none_or(|n| self.memory_bytes <= n)
            && total.cpu_millis.is_none_or(|n| self.cpu_millis <= n)
            && total.tasks.is_none_or(|n| self.tasks <= n)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunUsage {
    pub memory_peak_bytes: u64,
    pub oom_kills: u64,
    pub pids_limit_hits: u64,
    pub cpu_throttled_usec: u64,
    /// `memory.events` `max`: times usage reached `memory.max` and the kernel reclaimed.
    #[serde(default)]
    pub memory_max_hits: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustedResource {
    Memory,
    Tasks,
}

/// Bytes, written as a positive integer or an integer with a KiB, MiB, GiB or TiB suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
#[schemars(with = "MemoryInput")]
pub struct MemoryBytes(pub u64);
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum MemoryInput {
    Bytes(u64),
    Unit(String),
}
impl<'de> Deserialize<'de> for MemoryBytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = match MemoryInput::deserialize(deserializer)? {
            MemoryInput::Bytes(bytes) => bytes,
            MemoryInput::Unit(text) => {
                let (digits, multiplier) = [("KiB", 1u64 << 10), ("MiB", 1 << 20), ("GiB", 1 << 30), ("TiB", 1 << 40)]
                    .into_iter()
                    .find_map(|(suffix, multiplier)| text.strip_suffix(suffix).map(|digits| (digits, multiplier)))
                    .ok_or_else(|| serde::de::Error::custom("memory requires integer bytes or an integer with KiB, MiB, GiB or TiB suffix"))?;
                digits
                    .parse::<u64>()
                    .ok()
                    .and_then(|n| n.checked_mul(multiplier))
                    .ok_or_else(|| {
                        serde::de::Error::custom("memory size is invalid or overflows bytes")
                    })?
            }
        };
        if bytes == 0 {
            return Err(serde::de::Error::custom("memory must be positive"));
        }
        Ok(Self(bytes))
    }
}

/// CPU count represented internally as milli-CPUs, with no rounding at the wire boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[schemars(with = "f64")]
pub struct CpuMillis(pub u32);
impl Serialize for CpuMillis {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(f64::from(self.0) / 1000.0)
    }
}
impl<'de> Deserialize<'de> for CpuMillis {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let cpus = f64::deserialize(deserializer)?;
        let millis = cpus * 1000.0;
        if !millis.is_finite()
            || millis < 1.0
            || millis > f64::from(u32::MAX)
            || (millis - millis.round()).abs() > 1e-6
        {
            return Err(serde::de::Error::custom(
                "cpus must be positive, with at most three decimal places",
            ));
        }
        Ok(Self(millis.round() as u32))
    }
}

/// A layer's override. A field left out inherits; `null` (or the string `"unlimited"`) makes
/// that dimension unlimited; a value sets it. Unlimited is stored and shown as `null`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitsOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "setting")]
    #[schemars(with = "Option<MemoryInput>")]
    pub memory: Option<Option<MemoryBytes>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "setting")]
    #[schemars(with = "Option<CpuInput>")]
    pub cpus: Option<Option<CpuMillis>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "setting")]
    #[schemars(with = "Option<TasksInput>")]
    pub tasks: Option<Option<Tasks>>,
}
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
enum CpuInput {
    Cpus(f64),
    Unlimited(String),
}
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code)]
enum TasksInput {
    Tasks(u32),
    Unlimited(String),
}
/// A positive task count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Tasks(pub u32);
impl<'de> Deserialize<'de> for Tasks {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u32::deserialize(deserializer)? {
            0 => Err(serde::de::Error::custom("tasks must be positive")),
            n => Ok(Self(n)),
        }
    }
}
/// The spelling of unlimited besides `null`.
pub const UNLIMITED: &str = "unlimited";
fn setting<'de, D: serde::Deserializer<'de>, T: serde::de::DeserializeOwned>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() || value.as_str() == Some(UNLIMITED) {
        return Ok(Some(None));
    }
    serde_json::from_value(value)
        .map(|value| Some(Some(value)))
        .map_err(serde::de::Error::custom)
}
impl LimitsOverride {
    /// Shorthand for an override that sets every dimension.
    pub fn all(memory: Option<u64>, cpu_millis: Option<u32>, tasks: Option<u32>) -> Self {
        Self {
            memory: Some(memory.map(MemoryBytes)),
            cpus: Some(cpu_millis.map(CpuMillis)),
            tasks: Some(tasks.map(Tasks)),
        }
    }
    pub fn apply(&self, base: Limits) -> Result<Limits, String> {
        Limits {
            memory_bytes: self.memory.map_or(base.memory_bytes, |n| n.map(|n| n.0)),
            cpu_millis: self.cpus.map_or(base.cpu_millis, |n| n.map(|n| n.0)),
            tasks: self.tasks.map_or(base.tasks, |n| n.map(|n| n.0)),
        }
        .validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicy {
    pub defaults: Limits,
    pub ceilings: Limits,
    pub total: Limits,
    pub guardian_memory_bytes: u64,
}
/// The shipped home total of tasks: a fork storm stops inside the home's slice, well before the
/// user's own session runs out of process IDs.
pub const DEFAULT_TOTAL_TASKS: u32 = 65536;
/// The shipped home memory total is the smaller of this and [`DEFAULT_TOTAL_MEMORY_PERCENT`] of
/// the host's memory: the rest is the agents', the editor's and the services' own.
pub const DEFAULT_TOTAL_MEMORY: u64 = 70 << 30;
pub const DEFAULT_TOTAL_MEMORY_PERCENT: u64 = 55;
/// The shipped per-run memory cap (`memory.max`) and reservation.
pub const DEFAULT_RUN_MEMORY_MAX: u64 = 10 << 30;
pub const DEFAULT_RUN_MEMORY: u64 = 6 << 30;
impl ResourcePolicy {
    /// The shipped policy: the home total is min(70 GiB, 55% of memory) with a task cap and no
    /// CPU quota; a run reserves 6 GiB for admission and is capped at 10 GiB; CPU and tasks
    /// are unlimited per run. Every value shrinks to fit a small host.
    pub fn for_host(memory_bytes: u64, _cpu_millis: u32) -> Result<Self, String> {
        let total_memory = DEFAULT_TOTAL_MEMORY.min(
            u64::try_from(
                u128::from(memory_bytes) * u128::from(DEFAULT_TOTAL_MEMORY_PERCENT) / 100,
            )
            .unwrap_or(u64::MAX),
        );
        let guardian_memory_bytes = (256 << 20).min(total_memory / 8);
        let ceiling_memory =
            DEFAULT_RUN_MEMORY_MAX.min(total_memory.saturating_sub(guardian_memory_bytes));
        Self {
            defaults: Limits {
                memory_bytes: Some(DEFAULT_RUN_MEMORY.min(ceiling_memory)),
                cpu_millis: None,
                tasks: None,
            },
            ceilings: Limits {
                memory_bytes: Some(ceiling_memory),
                cpu_millis: None,
                tasks: None,
            },
            total: Limits {
                memory_bytes: Some(total_memory),
                cpu_millis: None,
                tasks: Some(DEFAULT_TOTAL_TASKS),
            },
            guardian_memory_bytes,
        }
        .validate()
    }
    pub fn validate(self) -> Result<Self, String> {
        self.defaults.validate()?;
        self.ceilings.validate()?;
        self.total.validate()?;
        if self.guardian_memory_bytes == 0 {
            return Err("guardian_memory must be positive".into());
        }
        if !self.defaults.fits(self.ceilings) {
            return Err("execution defaults exceed the per-run ceilings".into());
        }
        // A finite ceiling plus the guardian's share must fit a finite total; an unlimited
        // ceiling under a finite total is bounded by the total (the home's slice) alone.
        let over = |ceiling: Option<u64>, guardian: u64, total: Option<u64>| match total {
            None => false,
            Some(total) => ceiling
                .unwrap_or(0)
                .checked_add(guardian)
                .is_none_or(|n| n > total),
        };
        if over(
            self.ceilings.memory_bytes,
            self.guardian_memory_bytes,
            self.total.memory_bytes,
        ) || over(
            self.ceilings.cpu_millis.map(u64::from),
            u64::from(GUARDIAN_CPU_MILLIS),
            self.total.cpu_millis.map(u64::from),
        ) || over(
            self.ceilings.tasks.map(u64::from),
            u64::from(GUARDIAN_TASKS),
            self.total.tasks.map(u64::from),
        ) {
            return Err(
                "execution ceilings plus guardian reservation exceed the home budget".into(),
            );
        }
        Ok(self)
    }
    /// Home defaults, then project defaults, then the step's or call's override, within the
    /// per-run ceilings. The run's memory cap is the memory ceiling. A run whose memory came
    /// out unlimited (only under an unlimited ceiling) reserves the home default, or
    /// [`UNLIMITED_MEMORY_RESERVATION`] when that is unlimited too, within the total's room.
    pub fn resolve(
        &self,
        project: Option<&LimitsOverride>,
        run: Option<&LimitsOverride>,
    ) -> Result<RunLimits, String> {
        let mut limits = self.defaults;
        for overrides in [project, run].into_iter().flatten() {
            limits = overrides.apply(limits)?;
            if !limits.fits(self.ceilings) {
                return Err("execution limits exceed the installation's per-run ceilings".into());
            }
        }
        let floor = self
            .defaults
            .memory_bytes
            .unwrap_or(UNLIMITED_MEMORY_RESERVATION.min(
                self.total.memory_bytes.map_or(u64::MAX, |n| {
                    n.saturating_sub(self.guardian_memory_bytes).max(1)
                }),
            ));
        RunLimits {
            memory_bytes: Some(limits.memory_bytes.unwrap_or(floor)),
            memory_max_bytes: self.ceilings.memory_bytes,
            cpu_millis: limits.cpu_millis,
            tasks: limits.tasks,
        }
        .validate()
    }
}

/// `null` for unlimited in stored and tool JSON; status shows the word.
pub fn shown<T: Serialize>(value: Option<T>) -> serde_json::Value {
    value.map_or_else(
        || serde_json::Value::String(UNLIMITED.into()),
        |n| serde_json::json!(n),
    )
}
