//! Flock lifetime is the descriptor lifetime. Lock files must never be unlinked.
use crate::{identity::ProcessIdentity, proc::boot_id};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use sluice_model::ids::RunId;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockHolder {
    pub project: Option<String>,
    pub run: Option<RunId>,
    pub pid: u32,
    pub start_time: u64,
    pub boot_id: String,
}
impl LockHolder {
    pub fn current(project: Option<String>, run: Option<RunId>) -> io::Result<Self> {
        let identity = ProcessIdentity::read(std::process::id())?;
        Ok(Self {
            project,
            run,
            pid: identity.pid,
            start_time: identity.start_time,
            boot_id: identity.boot_id,
        })
    }
}
#[derive(Debug)]
pub enum LockAttempt {
    Acquired(FileLock),
    Conflict(LockHolder),
}
#[derive(Debug)]
pub struct FileLock {
    file: File,
    holder: LockHolder,
}
impl FileLock {
    /// The parent directory is caller-owned, and all contenders retain the same
    /// inode. Metadata is read only while flock is held by the identified owner.
    pub fn try_acquire(
        path: &Path,
        project: Option<String>,
        run: Option<RunId>,
    ) -> io::Result<LockAttempt> {
        let holder = LockHolder::current(project, run)?;
        let bytes = serde_json::to_vec(&holder).map_err(io::Error::other)?;
        if bytes.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "lock holder metadata is too large",
            ));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)?;
        let deadline = Instant::now() + Duration::from_secs(1);
        // Serialize acquisition + metadata publication with conflict reads.
        // Checking /proc/locks alone cannot distinguish two successive locks
        // held by the same PID/start time before the new metadata is written.
        let mut publication_path = path.as_os_str().to_owned();
        publication_path.push(".publish");
        let publication = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(publication_path)?;
        while !try_exclusive(&publication)? {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "lock holder metadata publication in progress",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
        loop {
            let acquired = try_exclusive(&file)?;
            if acquired {
                file.set_len(0)?;
                file.seek(SeekFrom::Start(0))?;
                file.write_all(&bytes)?;
                file.sync_data()?;
                return Ok(LockAttempt::Acquired(Self { file, holder }));
            }
            // Flock can be visible before metadata is written. Check the kernel
            // owner on both sides of the read; stale metadata is never a holder.
            let owner = flock_owner(&file)?;
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(16 * 1024)
                .read_to_end(&mut bytes)?;
            if let Ok(metadata) = serde_json::from_slice::<LockHolder>(&bytes)
                && Some(metadata.pid) == owner
                && flock_owner(&file)? == owner
                && metadata.boot_id == boot_id()?
                && ProcessIdentity::read(metadata.pid).is_ok_and(|id| {
                    id.start_time == metadata.start_time && id.boot_id == metadata.boot_id
                })
            {
                return Ok(LockAttempt::Conflict(metadata));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "lock held but holder metadata is not yet verifiable",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn holder(&self) -> &LockHolder {
        &self.holder
    }
    pub fn file(&self) -> &File {
        &self.file
    }
}
fn flock_owner(file: &File) -> io::Result<Option<u32>> {
    let metadata = file.metadata()?;
    let major = rustix::fs::major(metadata.dev());
    let minor = rustix::fs::minor(metadata.dev());
    for line in fs::read_to_string("/proc/locks")?.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() < 6 || fields[1] != "FLOCK" || fields[3] != "WRITE" {
            continue;
        }
        let id: Vec<_> = fields[5].split(':').collect();
        if id.len() == 3
            && u32::from_str_radix(id[0], 16).ok() == Some(major)
            && u32::from_str_radix(id[1], 16).ok() == Some(minor)
            && id[2].parse::<u64>().ok() == Some(metadata.ino())
        {
            return fields[4].parse().map(Some).map_err(io::Error::other);
        }
    }
    Ok(None)
}

fn try_exclusive(file: &File) -> io::Result<bool> {
    match FileExt::try_lock(file) {
        Ok(()) => Ok(true),
        Err(fs4::TryLockError::WouldBlock) => Ok(false),
        Err(fs4::TryLockError::Error(error)) => Err(error),
    }
}
