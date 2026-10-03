//! Cleanup evidence is produced only after recursive emptiness is established.
use crate::{
    cgroup::Cgroup,
    identity::{OwnedProcess, ProcessIdentity},
    systemd::TransientService,
};
use rustix::process::Signal;
use serde::Serialize;
use std::{io, time::Duration};

#[derive(Debug, Clone, Copy)]
pub struct StopPolicy {
    pub term_grace: Duration,
    pub kill_timeout: Duration,
}
impl Default for StopPolicy {
    fn default() -> Self {
        Self {
            term_grace: Duration::from_secs(5),
            kill_timeout: Duration::from_secs(5),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct EmptyProof {
    cgroup: String,
    term_sent: Vec<ProcessIdentity>,
    escalated: bool,
}
impl EmptyProof {
    pub fn cgroup(&self) -> &str {
        &self.cgroup
    }
    pub fn term_sent(&self) -> &[ProcessIdentity] {
        &self.term_sent
    }
    pub fn escalated(&self) -> bool {
        self.escalated
    }
}
/// Admission must already be closed for this invocation. Fork races during TERM
/// are handled by the recursive kill, rather than a numeric PID kill loop.
pub async fn stop_invocation(group: &Cgroup, policy: StopPolicy) -> io::Result<EmptyProof> {
    let mut sent = Vec::new();
    for pid in group.member_pids()? {
        let process = match OwnedProcess::capture(pid) {
            Ok(process) => process,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !group.contains(&process.identity().cgroup) {
            continue;
        }
        process.signal(Signal::TERM)?;
        sent.push(process.identity().clone());
    }
    let escalated = match group.wait_populated(false, policy.term_grace).await {
        Ok(()) => false,
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            group.kill()?;
            true
        }
        Err(e) => return Err(e),
    };
    group.wait_populated(false, policy.kill_timeout).await?;
    Ok(EmptyProof {
        cgroup: group.path().into(),
        term_sent: sent,
        escalated,
    })
}
/// Called outside the service by the coordinator after guardian loss or shutdown.
/// The pinned root must have been opened while the reconciled unit existed.
pub async fn stop_run(
    service: &TransientService,
    group: &Cgroup,
    timeout: Duration,
) -> io::Result<EmptyProof> {
    if !group.path().ends_with(&format!("/{}", service.name())) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cgroup does not belong to service",
        ));
    }
    let state = service.query().await?;
    if state
        .cgroup
        .as_deref()
        .is_some_and(|path| path != group.path())
    {
        return Err(io::Error::other("service cgroup changed"));
    }
    service.stop().await?;
    group.wait_populated(false, timeout).await?;
    service.reset_failed().await?;
    Ok(EmptyProof {
        cgroup: group.path().into(),
        term_sent: Vec::new(),
        escalated: true,
    })
}
/// Stable descriptors prove that every recorded executor has exited, including
/// zombies whose procfs entries have not yet been reaped.
pub async fn wait_owned_gone(processes: &[OwnedProcess], timeout: Duration) -> io::Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut alive = false;
        for process in processes {
            alive |= !process.exited()?;
        }
        if !alive {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "owned identities still live",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
