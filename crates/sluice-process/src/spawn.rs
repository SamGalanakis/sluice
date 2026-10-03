//! Post-exec launcher ownership. Preparation never dispatches the payload.
use crate::{
    cgroup::Cgroup,
    identity::OwnedProcess,
    launcher::{self, Grant, Hello, RunStarted},
};
use rustix::process::Signal;
use std::{
    fs::File,
    io::{self, Read},
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub struct PreparedLaunch {
    child: Option<Child>,
    process: OwnedProcess,
    socket: UnixStream,
    nonce: String,
    timeout: Duration,
}
impl PreparedLaunch {
    /// command is the same executable as the guardian, with no arguments.
    /// payload_args are passed to its dispatcher. Its stdin is reserved for the inherited socket until dispatch.
    /// This blocking boundary belongs on a dedicated thread/spawn_blocking.
    pub fn spawn(
        command: &mut Command,
        payload_args: &[std::ffi::OsString],
        timeout: Duration,
    ) -> io::Result<Self> {
        let millis = timeout.as_millis();
        if millis == 0 || millis > 60_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "barrier timeout must be 1..60000 ms",
            ));
        }
        let mut random = [0; 32];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let nonce: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let (socket, child_socket) = UnixStream::pair()?;
        if command.get_args().next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "launcher command must have no args",
            ));
        }
        command
            .args([launcher::PAYLOAD_EXEC_MODE, &nonce, &millis.to_string()])
            .args(payload_args);
        // UnixStream::pair creates CLOEXEC descriptors. std duplicates only the
        // child endpoint to fd 0; no process-global inheritance window/pre_exec.
        command.stdin(Stdio::from(OwnedFd::from(child_socket)));
        let spawned = command.spawn();
        // Drop the parent's extra child endpoint even if the Command is kept.
        command.stdin(Stdio::null());
        let mut child = spawned?;
        let process = match OwnedProcess::capture(child.id()) {
            Ok(process) => process,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        let mut prepared = Self {
            child: Some(child),
            process,
            socket,
            nonce,
            timeout,
        };
        let hello: Hello = launcher::receive(&mut prepared.socket, Instant::now() + timeout)?;
        if hello.nonce != prepared.nonce
            || hello.identity != *prepared.process.identity()
            || prepared.process.exited()?
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "launcher handshake identity mismatch",
            ));
        }
        Ok(prepared)
    }
    pub fn process(&self) -> &OwnedProcess {
        &self.process
    }
    /// Place, record durably, recheck cancellation, then grant continuation.
    /// The guardian must serialize this method with cancellation transitions.
    /// A record error or cancellation leaves a killed, reaped launcher.
    pub fn continue_in<F>(
        mut self,
        leaf: &Cgroup,
        cancel: &CancellationToken,
        record: F,
    ) -> io::Result<RunningPayload>
    where
        F: FnOnce(&crate::identity::ProcessIdentity) -> io::Result<()>,
    {
        let result = self.grant_in(leaf, cancel, record);
        let started = match result {
            Ok(started) => started,
            Err(error) => {
                // The grant may have arrived even when its acknowledgement was
                // lost. Kill the entire leaf; the guardian still must prove it
                // empty before releasing holds or reporting completion.
                let _ = leaf.kill();
                return Err(error);
            }
        };
        let child = self.child.take().expect("prepared launch owns child");
        // Keep the original pidfd: the executor may already have exited after
        // acknowledgement, and reopening by numeric PID would be unsafe.
        Ok(RunningPayload {
            child,
            started,
            owner: self,
        })
    }
    fn grant_in<F>(
        &mut self,
        leaf: &Cgroup,
        cancel: &CancellationToken,
        record: F,
    ) -> io::Result<RunStarted>
    where
        F: FnOnce(&crate::identity::ProcessIdentity) -> io::Result<()>,
    {
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        leaf.move_process(&mut self.process)?;
        record(self.process.identity())?;
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let identity = self.process.refresh()?.clone();
        if identity.cgroup != leaf.path() {
            return Err(io::Error::other("launcher left its payload leaf"));
        }
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        let deadline = Instant::now() + self.timeout;
        launcher::send(
            &mut self.socket,
            &Grant {
                nonce: self.nonce.clone(),
                identity: identity.clone(),
            },
            deadline,
        )?;
        let started: RunStarted = launcher::receive(&mut self.socket, deadline)?;
        if started.nonce != self.nonce || started.identity != identity {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "invalid RunStarted acknowledgement",
            ));
        }
        Ok(started)
    }
}
impl Drop for PreparedLaunch {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.process.signal(Signal::KILL);
            let _ = child.wait();
        }
    }
}
pub struct RunningPayload {
    child: Child,
    started: RunStarted,
    owner: PreparedLaunch,
}
impl RunningPayload {
    pub fn process(&self) -> &OwnedProcess {
        &self.owner.process
    }
    pub fn started(&self) -> &RunStarted {
        &self.started
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child.wait()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}
// The guardian owns invocation cleanup, including descendants. Dropping a root
// handle is never an emptiness proof; callers must use signals::stop_invocation.
fn cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "launch cancelled before dispatch",
    )
}
