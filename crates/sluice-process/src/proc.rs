//! Procfs observations are snapshots, never authority to signal a numeric PID.
use crate::identity::ProcessIdentity;
use procfs::process::Process;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
};

pub fn boot_id() -> io::Result<String> {
    let id = fs::read_to_string("/proc/sys/kernel/random/boot_id")?;
    let id = id.trim();
    if id.len() != 36 || !id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid boot id",
        ));
    }
    Ok(id.into())
}

pub(crate) fn process(pid: u32) -> io::Result<Process> {
    let pid = i32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid pid"))?;
    Process::new(pid).map_err(proc_error)
}
pub(crate) fn proc_error(error: procfs::ProcError) -> io::Error {
    match error {
        procfs::ProcError::NotFound(_) => io::Error::from(io::ErrorKind::NotFound),
        procfs::ProcError::PermissionDenied(_) => io::Error::from(io::ErrorKind::PermissionDenied),
        procfs::ProcError::Io(error, _) => error,
        other => io::Error::other(other),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessObservation {
    pub identity: ProcessIdentity,
    pub parent: i32,
    pub session: i32,
    pub state: char,
    pub command: String,
    pub argv: Vec<String>,
    pub user_ticks: u64,
    pub system_ticks: u64,
    pub waited_user_ticks: i64,
    pub waited_system_ticks: i64,
}
impl ProcessObservation {
    pub fn cpu_ticks(&self) -> u64 {
        self.user_ticks
            .saturating_add(self.system_ticks)
            .saturating_add(self.waited_user_ticks.max(0) as u64)
            .saturating_add(self.waited_system_ticks.max(0) as u64)
    }
}

pub fn observe(pid: u32) -> io::Result<ProcessObservation> {
    let identity = ProcessIdentity::read(pid)?;
    let process = process(pid)?;
    let stat = process.stat().map_err(proc_error)?;
    let argv = process.cmdline().map_err(proc_error)?;
    if stat.starttime != identity.start_time || !identity.matches_current()? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process changed during observation",
        ));
    }
    Ok(ProcessObservation {
        identity,
        parent: stat.ppid,
        session: stat.session,
        state: stat.state,
        command: stat.comm,
        argv,
        user_ticks: stat.utime,
        system_ticks: stat.stime,
        waited_user_ticks: stat.cutime,
        waited_system_ticks: stat.cstime,
    })
}

/// Best-effort descendant snapshot, excluding roots and zombies. Detached work
/// must also be observed through the invocation cgroup, since it can reparent.
pub fn descendants(roots: &[ProcessIdentity]) -> io::Result<Vec<ProcessObservation>> {
    let mut seen = BTreeSet::new();
    for root in roots {
        if root.matches_current()? {
            seen.insert(root.pid);
        }
    }
    let roots = seen.clone();
    let mut children = BTreeMap::<u32, Vec<ProcessObservation>>::new();
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        match observe(pid) {
            Ok(observation) => {
                children
                    .entry(observation.parent as u32)
                    .or_default()
                    .push(observation);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) => {}
            Err(e) => return Err(e),
        }
    }
    let mut todo: Vec<_> = roots.into_iter().collect();
    let mut result = Vec::new();
    while let Some(parent) = todo.pop() {
        for child in children.remove(&parent).unwrap_or_default() {
            if seen.insert(child.identity.pid) {
                todo.push(child.identity.pid);
                if !matches!(child.state, 'Z' | 'X') {
                    result.push(child);
                }
            }
        }
    }
    Ok(result)
}
