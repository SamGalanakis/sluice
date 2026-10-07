//! Stable process identity. All signals use an owned pidfd.
use crate::proc::{boot_id, proc_error, process};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fd::{AsFd, BorrowedFd, OwnedFd},
    process::{Pid, PidfdFlags, pidfd_open, pidfd_send_signal},
};
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
    pub boot_id: String,
    pub cgroup: String,
}
impl ProcessIdentity {
    pub fn read(pid: u32) -> io::Result<Self> {
        let process = process(pid)?;
        let before = process.stat().map_err(proc_error)?;
        let cgroup = process
            .cgroups()
            .map_err(proc_error)?
            .0
            .into_iter()
            .find(|group| group.hierarchy == 0)
            .ok_or_else(|| io::Error::other("no cgroup v2 membership"))?
            .pathname;
        let after = process.stat().map_err(proc_error)?;
        if before.starttime != after.starttime {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "pid reused during identity read",
            ));
        }
        Ok(Self {
            pid,
            start_time: after.starttime,
            boot_id: boot_id()?,
            cgroup,
        })
    }
    /// Membership can change during placement; generation cannot.
    pub fn same_process(&self, other: &Self) -> bool {
        self.pid == other.pid
            && self.start_time == other.start_time
            && self.boot_id == other.boot_id
    }
    pub fn matches_current(&self) -> io::Result<bool> {
        match Self::read(self.pid) {
            Ok(current) => Ok(self.same_process(&current)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

#[derive(Debug)]
pub struct OwnedProcess {
    identity: ProcessIdentity,
    pidfd: OwnedFd,
}
impl OwnedProcess {
    pub fn open(expected: &ProcessIdentity) -> io::Result<Self> {
        if !expected.matches_current()? {
            return Err(stale());
        }
        let pid =
            Pid::from_raw(i32::try_from(expected.pid).map_err(|_| stale())?).ok_or_else(stale)?;
        let pidfd = open_pidfd(pid)?;
        let current = ProcessIdentity::read(expected.pid).map_err(vanished)?;
        let owned = Self {
            identity: current,
            pidfd,
        };
        if owned.identity != *expected || owned.exited()? {
            return Err(stale());
        }
        Ok(owned)
    }
    pub fn capture(pid: u32) -> io::Result<Self> {
        Self::open(&ProcessIdentity::read(pid)?)
    }
    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }
    pub fn pidfd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }
    pub fn signal(&self, signal: Signal) -> io::Result<()> {
        match pidfd_send_signal(&self.pidfd, signal) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn exited(&self) -> io::Result<bool> {
        let mut fds = [PollFd::new(&self.pidfd, PollFlags::IN)];
        let zero = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            match poll(&mut fds, Some(&zero)) {
                Ok(_) => return Ok(fds[0].revents().intersects(PollFlags::IN | PollFlags::HUP)),
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn refresh(&mut self) -> io::Result<&ProcessIdentity> {
        let current = ProcessIdentity::read(self.identity.pid)?;
        if self.exited()? || !self.identity.same_process(&current) {
            return Err(stale());
        }
        self.identity = current;
        Ok(&self.identity)
    }
}
fn stale() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "stale process identity")
}
/// A process can exit after its identity was checked; that is a stale identity, not a failure.
fn vanished(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) {
        stale()
    } else {
        error
    }
}
fn open_pidfd(pid: Pid) -> io::Result<OwnedFd> {
    pidfd_open(pid, PidfdFlags::empty()).map_err(|e| vanished(e.into()))
}

pub use rustix::process::Signal;
