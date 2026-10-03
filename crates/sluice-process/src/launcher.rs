//! The executable must route payload-exec here before any payload dispatch.
use crate::identity::ProcessIdentity;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    os::unix::{net::UnixStream, process::CommandExt},
    process::Command,
    time::{Duration, Instant},
};

pub const PAYLOAD_EXEC_MODE: &str = "payload-exec";
const MAX_FRAME: usize = 16 * 1024;
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Hello {
    pub nonce: String,
    pub identity: ProcessIdentity,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Grant {
    pub nonce: String,
    pub identity: ProcessIdentity,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunStarted {
    pub nonce: String,
    pub identity: ProcessIdentity,
}

pub(crate) fn send<T: Serialize>(
    socket: &mut UnixStream,
    value: &T,
    deadline: Instant,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "barrier frame too large",
        ));
    }
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend(bytes);
    while !frame.is_empty() {
        socket.set_write_timeout(Some(remaining(deadline)?))?;
        let written = match socket.write(&frame) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if written == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        frame.drain(..written);
    }
    Ok(())
}
pub(crate) fn receive<T: DeserializeOwned>(
    socket: &mut UnixStream,
    deadline: Instant,
) -> io::Result<T> {
    let mut length = [0; 4];
    read(socket, &mut length, deadline)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "barrier frame too large",
        ));
    }
    let mut data = vec![0; length];
    read(socket, &mut data, deadline)?;
    serde_json::from_slice(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
fn read(socket: &mut UnixStream, mut buffer: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !buffer.is_empty() {
        socket.set_read_timeout(Some(remaining(deadline)?))?;
        let count = match socket.read(buffer) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        buffer = &mut buffer[count..];
    }
    Ok(())
}
fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "launcher barrier timed out"))
}

/// Args exclude the payload-exec mode: nonce, timeout in milliseconds, then
/// dispatcher arguments. The inherited socketpair occupies stdin only until
/// acknowledgement. Dispatch must configure payload stdin explicitly.
///
/// The binary supplies a closure that execs uv or invokes its Rust dispatcher.
/// EOF, timeout, a wrong nonce or changed identity exits without calling it.
pub fn payload_exec_main<F>(args: Vec<OsString>, dispatch: F) -> !
where
    F: FnOnce(&[OsString]) -> io::Result<i32>,
{
    let result = barrier(&args).and_then(|()| dispatch(&args[2..]));
    let code = match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("payload-exec: {e}");
            126
        }
    };
    std::process::exit(code)
}
fn barrier(args: &[OsString]) -> io::Result<()> {
    let nonce = args
        .first()
        .and_then(|s| s.to_str())
        .filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing launcher nonce"))?;
    let timeout = args
        .get(1)
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|ms| *ms > 0 && *ms <= 60_000)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid barrier timeout"))?;
    // Duplicate with CLOEXEC, then replace stdin before dispatch. The socket
    // is never inherited by the real payload, and stdin remains a valid fd.
    let fd = rustix::io::fcntl_dupfd_cloexec(rustix::stdio::stdin(), 3)?;
    let mut socket = UnixStream::from(fd);
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let identity = ProcessIdentity::read(std::process::id())?;
    send(
        &mut socket,
        &Hello {
            nonce: nonce.into(),
            identity: identity.clone(),
        },
        deadline,
    )?;
    let grant: Grant = receive(&mut socket, deadline)?;
    let current = ProcessIdentity::read(identity.pid)?;
    if grant.nonce != nonce || !identity.same_process(&current) || grant.identity != current {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid continuation grant",
        ));
    }
    rustix::stdio::dup2_stdin(std::fs::File::open("/dev/null")?)?;
    send(
        &mut socket,
        &RunStarted {
            nonce: nonce.into(),
            identity: current,
        },
        deadline,
    )?;
    Ok(())
}
/// Successful exec never returns. Configure stdin, cwd and env in the command.
pub fn exec_payload(command: &mut Command) -> io::Result<i32> {
    Err(command.exec())
}
