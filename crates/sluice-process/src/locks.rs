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

/// Serializes run launch against adoption and final capacity release. This lock
/// is inherited by the start helper so dropping an async caller cannot make a
/// still-pending systemd start look absent.
#[derive(Debug)]
pub struct RunFence {
    file: File,
    directory: std::path::PathBuf,
}
impl RunFence {
    pub async fn acquire(home: &Path, run: RunId) -> io::Result<Self> {
        loop {
            if let Some(fence) = Self::try_acquire(home, run)? {
                return Ok(fence);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    pub fn try_acquire(home: &Path, run: RunId) -> io::Result<Option<Self>> {
        let directory = home.join("runs").join(run.to_string());
        fs::create_dir_all(&directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(directory.join("launch.lock"))?;
        if !try_exclusive(&file)? {
            return Ok(None);
        }
        Ok(Some(Self { file, directory }))
    }
    pub fn file(&self) -> &File {
        &self.file
    }
    pub(crate) fn prepare_start(&self) -> io::Result<std::path::PathBuf> {
        crate::journal::atomic_json(&self.directory, "launch.pending", &boot_id()?)?;
        Ok(self.directory.join("launch.pending"))
    }
    /// Start a manager-request helper that retains this fence until its child
    /// exits. Dropping the returned child detaches it; failure leaves durable
    /// pending evidence, while success records the manager acknowledgement.
    pub fn spawn_start_helper(
        &self,
        command: std::process::Command,
    ) -> io::Result<tokio::process::Child> {
        use std::process::Stdio;
        let marker = self.prepare_start()?;
        let inherited = match self.file.try_clone() {
            Ok(file) => file,
            Err(error) => {
                self.clear_pending()?;
                return Err(error);
            }
        };
        let mut helper = tokio::process::Command::new("/bin/sh");
        helper
            .args(["-c", "marker=$1; shift; \"$@\" </dev/null; status=$?; if [ \"$status\" -eq 0 ]; then /bin/rm -f -- \"$marker\"; fi; exit \"$status\"", "sluice-start"])
            .arg(&marker)
            .arg(command.get_program())
            .args(command.get_args())
            .stdin(Stdio::from(inherited))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        for (key, value) in command.get_envs() {
            if let Some(value) = value {
                helper.env(key, value);
            } else {
                helper.env_remove(key);
            }
        }
        if let Some(directory) = command.get_current_dir() {
            helper.current_dir(directory);
        }
        match helper.spawn() {
            Ok(child) => Ok(child),
            Err(error) => {
                self.clear_pending()?;
                Err(error)
            }
        }
    }
    fn clear_pending(&self) -> io::Result<()> {
        match fs::remove_file(self.directory.join("launch.pending")) {
            Ok(()) => File::open(&self.directory)?.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    /// A previous boot cannot retain a request in this boot's user manager.
    pub fn start_pending(home: &Path, run: RunId) -> io::Result<bool> {
        let path = home
            .join("runs")
            .join(run.to_string())
            .join("launch.pending");
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let launched_boot: String = serde_json::from_slice(&contents).map_err(io::Error::other)?;
        if launched_boot == boot_id()? {
            return Ok(true);
        }
        Self::acknowledge_start(home, run)?;
        Ok(false)
    }
    /// Call only after observing the service, or proving no helper was spawned.
    pub fn acknowledge_start(home: &Path, run: RunId) -> io::Result<()> {
        let directory = home.join("runs").join(run.to_string());
        match fs::remove_file(directory.join("launch.pending")) {
            Ok(()) => File::open(directory)?.sync_all(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
