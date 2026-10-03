//! Quiet detection observes work, and produces notes without changing supervision decisions.
use crate::git::GitSample;
use sluice_process::{
    identity::ProcessIdentity,
    proc::{ProcessObservation, descendants},
};
use std::{collections::BTreeMap, io, path::Path, time::Duration};

pub type CpuSample = BTreeMap<(u32, u64, String), u64>;
pub fn descendant_cpu(roots: &[ProcessIdentity]) -> io::Result<CpuSample> {
    Ok(cpu_sample(&descendants(roots)?))
}
pub fn cpu_sample(processes: &[ProcessObservation]) -> CpuSample {
    processes
        .iter()
        .filter(|p| p.state != 'Z' && !control_process(&p.argv))
        .map(|p| {
            (
                (
                    p.identity.pid,
                    p.identity.start_time,
                    p.identity.boot_id.clone(),
                ),
                p.cpu_ticks(),
            )
        })
        .collect()
}
pub fn control_process(argv: &[String]) -> bool {
    let names: Vec<_> = argv
        .iter()
        .take(2)
        .map(|a| {
            Path::new(a)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
        })
        .collect();
    let main = names.first().map(String::as_str).unwrap_or("");
    if matches!(main, "tmux" | "codex-code-mode-host")
        || (main == "codex" && argv.iter().any(|a| a == "app-server"))
    {
        return true;
    }
    let module = argv.get(1).filter(|a| *a == "-m").and_then(|_| argv.get(2));
    let is_mcp = |s: &str| {
        s == "mcp"
            || s.starts_with("mcp-")
            || s.starts_with("mcp_")
            || s.starts_with("mcp.")
            || s.ends_with("-mcp")
    };
    names
        .iter()
        .any(|s| s == "mcp" || s.ends_with("-mcp") || s.starts_with("mcp-server"))
        || module.is_some_and(|s| is_mcp(s))
        || argv.get(1).is_some_and(|s| {
            Path::new(s).parent().is_some_and(|parent| {
                parent.components().any(|p| {
                    let name = p.as_os_str().to_string_lossy().to_lowercase();
                    name == "mcp" || name.ends_with("-mcp") || name == "mcp-server"
                })
            })
        })
}
#[derive(Default)]
pub struct QuietMonitor {
    mark: Option<GitSample>,
    cpu: CpuSample,
    changed: Option<Duration>,
    noted: Option<Duration>,
    sampled: Option<Duration>,
}
impl QuietMonitor {
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn due(&self, now: Duration, period: Duration) -> bool {
        self.sampled.is_none_or(|last| {
            now.saturating_sub(last) >= (period / 10).min(Duration::from_secs(180))
        })
    }
    pub fn observe(
        &mut self,
        now: Duration,
        period: Duration,
        mark: Option<GitSample>,
        cpu: CpuSample,
    ) -> Option<String> {
        self.sampled = Some(now);
        let active = cpu
            .iter()
            .any(|(id, ticks)| *ticks > self.cpu.get(id).copied().unwrap_or(0));
        self.cpu = cpu;
        if self.changed.is_none() || mark != self.mark || active {
            self.mark = mark;
            self.changed = Some(now);
            self.noted = None;
            return None;
        }
        let mark = self.mark.as_ref()?;
        let changed = self.changed?;
        if now.saturating_sub(self.noted.unwrap_or(changed)) < period {
            return None;
        }
        self.noted = Some(now);
        Some(format!(
            "busy {:.0} min with no change to the worktree (HEAD {}, {})",
            now.saturating_sub(changed).as_secs_f64() / 60.0,
            &mark.head[..mark.head.len().min(7)],
            if mark.tracked.is_empty() {
                "no diff"
            } else {
                "uncommitted diff unchanged"
            }
        ))
    }
}
