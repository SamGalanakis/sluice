//! Observe live jobs and quiet worktrees without interrupting work.
use crate::git::GitSample;
use sluice_process::{
    cgroup::Cgroup,
    identity::ProcessIdentity,
    proc::{ProcessObservation, descendants},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    time::Duration,
};

/// Live work throughout the run's payload, including detached or reparented jobs. A quiet
/// child still protects its job: CPU use and transcript progress are irrelevant here.
pub fn background_work(
    supervisor: &ProcessIdentity,
    control_scripts: &[PathBuf],
) -> io::Result<Vec<String>> {
    let (service, _) = supervisor
        .cgroup
        .split_once("/payload/")
        .ok_or_else(|| io::Error::other("agent is outside an admitted payload leaf"))?;
    let payload = format!("{service}/payload/");
    let mut processes = Vec::new();
    for pid in Cgroup::open_service(service)?.member_pids()? {
        match sluice_process::proc::observe(pid) {
            Ok(process)
                if process.identity.cgroup.starts_with(&payload)
                    && !matches!(process.state, 'Z' | 'X') =>
            {
                processes.push(process)
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    // A composed agent has an outer dispatcher and a Python fn waiting for its reply.
    // Their exact published script paths identify control work without exempting arbitrary
    // Python jobs. Parent dispatchers, too, remain in the run's payload while waiting.
    let mut controls = BTreeSet::from([supervisor.pid]);
    let mut parent = supervisor.pid;
    while let Some(process) = processes.iter().find(|p| p.identity.pid == parent) {
        parent = process.parent as u32;
        if !controls.insert(parent) {
            break;
        }
    }
    controls.extend(
        processes
            .iter()
            .filter(|p| {
                p.argv.iter().any(|arg| {
                    control_scripts
                        .iter()
                        .any(|script| Path::new(arg) == script)
                })
            })
            .map(|p| p.identity.pid),
    );
    // The one main pane is control whatever executable the engine launch chose. Only the
    // supervisor's private tmux counts here; a job's own tmux is still background work.
    let tmux: BTreeSet<_> = processes
        .iter()
        .filter(|p| {
            p.parent as u32 == supervisor.pid
                && (p.argv.first().is_some_and(|a| basename(a) == "tmux")
                    || p.command.starts_with("tmux:"))
        })
        .map(|p| p.identity.pid)
        .collect();
    let mut agent_parents = BTreeSet::from([supervisor.pid]);
    for process in processes
        .iter()
        .filter(|p| tmux.contains(&(p.parent as u32)))
    {
        controls.insert(process.identity.pid);
        if agent_launcher(&process.argv) {
            agent_parents.insert(process.identity.pid);
        }
    }
    let mut agents = BTreeSet::new();
    loop {
        let before = agents.len();
        for p in &processes {
            if agent_parents.contains(&(p.parent as u32)) && agent_cli(&p.argv) {
                agents.insert(p.identity.pid);
                if agent_launcher(&p.argv) {
                    agent_parents.insert(p.identity.pid);
                }
            }
        }
        if agents.len() == before {
            break;
        }
    }
    Ok(processes
        .iter()
        .filter(|p| {
            !controls.contains(&p.identity.pid)
                && !agents.contains(&p.identity.pid)
                && (agent_cli(&p.argv) || !control_process(&p.argv))
                && !p.command.starts_with("tmux:")
        })
        .map(|p| format!("pid {} ({})", p.identity.pid, p.command))
        .collect())
}
fn basename(arg: &str) -> &str {
    Path::new(arg)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
}
fn agent_launcher(argv: &[String]) -> bool {
    argv.first()
        .is_some_and(|arg| matches!(basename(arg), "node" | "bun" | "python" | "python3"))
}
fn agent_cli(argv: &[String]) -> bool {
    let Some(main) = argv.first().map(|arg| basename(arg)) else {
        return false;
    };
    matches!(main, "codex" | "claude" | "devin")
        || (matches!(main, "node" | "bun" | "python" | "python3" | "fixture")
            && argv.get(1).is_some_and(|arg| {
                matches!(
                    basename(arg),
                    "codex" | "claude" | "devin" | "codex.js" | "claude.js"
                )
            }))
}

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
            "busy {:.0} min with no change to the worktree (HEAD {}, {}); if you are waiting on a job, keep waiting",
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
