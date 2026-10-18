//! Physical execution budgets, independent of a plan's logical resource leases.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const GUARDIAN_TASKS: u32 = 64;
pub const GUARDIAN_CPU_MILLIS: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    pub memory_bytes: u64,
    /// One CPU is 1000 milli-CPUs.
    pub cpu_millis: u32,
    pub tasks: u32,
}
impl RunLimits {
    pub fn validate(self) -> Result<Self, String> {
        if self.memory_bytes == 0 || self.cpu_millis == 0 || self.tasks == 0 {
            return Err("memory, cpus and tasks must all be positive".into());
        }
        Ok(self)
    }
    pub fn fits(self, ceiling: Self) -> bool {
        self.memory_bytes <= ceiling.memory_bytes
            && self.cpu_millis <= ceiling.cpu_millis
            && self.tasks <= ceiling.tasks
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunUsage {
    pub memory_peak_bytes: u64,
    pub oom_kills: u64,
    pub pids_limit_hits: u64,
    pub cpu_throttled_usec: u64,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitsOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryBytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<CpuMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(deserialize_with = "positive_tasks")]
    pub tasks: Option<u32>,
}
impl LimitsOverride {
    pub fn apply(&self, base: RunLimits) -> Result<RunLimits, String> {
        RunLimits {
            memory_bytes: self.memory.map_or(base.memory_bytes, |n| n.0),
            cpu_millis: self.cpus.map_or(base.cpu_millis, |n| n.0),
            tasks: self.tasks.unwrap_or(base.tasks),
        }
        .validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicy {
    pub defaults: RunLimits,
    pub ceilings: RunLimits,
    pub total: RunLimits,
    pub guardian_memory_bytes: u64,
}
impl ResourcePolicy {
    pub fn for_host(memory_bytes: u64, cpu_millis: u32) -> Result<Self, String> {
        let total_memory = memory_bytes / 4 * 3;
        let guardian_memory_bytes = (256 << 20).min(total_memory / 8);
        let total_cpu = cpu_millis / 4 * 3;
        let ceilings = RunLimits {
            memory_bytes: total_memory.saturating_sub(guardian_memory_bytes),
            cpu_millis: total_cpu.saturating_sub(GUARDIAN_CPU_MILLIS),
            tasks: 4096,
        };
        Self {
            defaults: RunLimits {
                memory_bytes: (8 << 30).min(ceilings.memory_bytes),
                cpu_millis: 2000.min(ceilings.cpu_millis),
                tasks: 512,
            },
            ceilings,
            total: RunLimits {
                memory_bytes: total_memory,
                cpu_millis: total_cpu,
                tasks: 8192,
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
        if !self.ceilings.fits(self.total)
            || self
                .ceilings
                .memory_bytes
                .checked_add(self.guardian_memory_bytes)
                .is_none_or(|n| n > self.total.memory_bytes)
            || self
                .ceilings
                .tasks
                .checked_add(GUARDIAN_TASKS)
                .is_none_or(|n| n > self.total.tasks)
            || self
                .ceilings
                .cpu_millis
                .checked_add(GUARDIAN_CPU_MILLIS)
                .is_none_or(|n| n > self.total.cpu_millis)
        {
            return Err(
                "execution ceilings plus guardian reservation exceed the home budget".into(),
            );
        }
        Ok(self)
    }
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
        Ok(limits)
    }
}

fn positive_tasks<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u32>, D::Error> {
    let tasks = Option::<u32>::deserialize(deserializer)?;
    if tasks == Some(0) {
        return Err(serde::de::Error::custom("tasks must be positive"));
    }
    Ok(tasks)
}
