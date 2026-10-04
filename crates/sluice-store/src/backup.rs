//! Online database backup and isolated, complete-home exports.
//!
//! These synchronous operations belong on a blocking thread. Database backup
//! permits concurrent commits. Full export requires the caller to hold admission
//! paused, and also suspend fn publication, artifact writes and cleanup for the
//! entire call. `AdmissionPaused` identifies that caller's durable maintenance
//! ownership; it is checked before and after export, rather than creating or
//! releasing maintenance mode here. Drain alone does not freeze running writers.
//!
//! Source and destination directory trees must be owned exclusively by the
//! caller during filesystem traversal. Symlinks are refused, never followed;
//! special files are refused except sockets, which are transient endpoints.
//! Coordinator endpoints, locks and SQLite sidecars are not exported/restored.
//! Restore preserves HomeId and maintenance state. Starting the restored home
//! and releasing its maintenance fence are separate caller decisions.

use crate::schema::{self, DATABASE_FILE, Result, StoreError};
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use sluice_model::error::PublicError;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const BACKUP_TIMEOUT: Duration = Duration::from_secs(30);
const INCOMPLETE: &str = ".sluice-copy-incomplete";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub bytes: u64,
}

/// An assertion that the caller holds this durable admission fence and freezes
/// filesystem publication, writes and cleanup until export returns. Mode `drain`
/// qualifies, but mode `normal`, absent/stale ownership and a changed revision do not. This function does not acquire the caller's fence.
#[derive(Debug, Clone)]
pub struct AdmissionPaused {
    pub owner: String,
    pub revision: i64,
}

/// Copy a home's database through SQLite's online backup API into a fresh file,
/// verify integrity and foreign keys, and leave one self-contained database.
/// Never overwrite an existing path, including the source, symlinks or directories.
pub fn backup(home: &Path, dst_path: &Path) -> Result<BackupInfo> {
    if !fs::symlink_metadata(home.join(DATABASE_FILE))?
        .file_type()
        .is_file()
    {
        return Err(bad("backup source database must be a regular file"));
    }
    let source = schema::open_reader(home, Duration::from_millis(50))?;
    pin_snapshot(&source)?;
    backup_connection(&source, dst_path)
}

fn pin_snapshot(source: &Connection) -> Result<()> {
    source.execute_batch("BEGIN")?;
    // BEGIN alone is deferred. Establish the snapshot before backup_init.
    source.query_row("SELECT home_id FROM home_meta WHERE singleton=1", [], |r| {
        r.get::<_, String>(0)
    })?;
    Ok(())
}

fn backup_connection(source: &Connection, dst_path: &Path) -> Result<BackupInfo> {
    let file = fresh_file(dst_path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut path = dst_path.as_os_str().to_owned();
        path.push(suffix);
        if fs::symlink_metadata(PathBuf::from(path)).is_ok() {
            fs::remove_file(dst_path)?;
            return Err(bad("destination has existing SQLite sidecars"));
        }
    }
    let mut cleanup = FileCleanup {
        path: dst_path.to_owned(),
        armed: true,
    };
    let mut destination = Connection::open_with_flags(
        dst_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    destination.busy_timeout(Duration::from_millis(50))?;
    {
        let transfer = Backup::new(source, &mut destination)?;
        let deadline = Instant::now() + BACKUP_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err(busy("online backup did not finish within 30 seconds"));
            }
            match transfer.step(128)? {
                StepResult::Done => break,
                StepResult::More => std::thread::yield_now(),
                StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                _ => {
                    return Err(StoreError::InvalidDatabase(
                        "unknown online backup result".into(),
                    ));
                }
            }
        }
    }
    integrity(&destination)?;
    // The source's WAL header may be copied. Convert only the destination to a
    // rollback journal so closing it leaves no required -wal/-shm companions.
    let journal: String = destination.query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))?;
    if !journal.eq_ignore_ascii_case("delete") {
        return Err(StoreError::InvalidDatabase(
            "backup could not leave WAL mode".into(),
        ));
    }
    destination.close().map_err(|(_, e)| e)?;
    file.sync_all()?;
    let bytes = file.metadata()?.len();
    sync_parent(dst_path)?;
    cleanup.armed = false;
    Ok(BackupInfo {
        path: dst_path.to_owned(),
        bytes,
    })
}

fn integrity(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    let mut rows = statement.query([])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        let verdict: String = row.get(0)?;
        if verdict != "ok" {
            return Err(StoreError::InvalidDatabase(format!(
                "backup integrity_check: {verdict}"
            )));
        }
        count += 1;
    }
    if count != 1 {
        return Err(StoreError::InvalidDatabase(
            "backup integrity_check returned no single verdict".into(),
        ));
    }
    if connection
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(StoreError::InvalidDatabase(
            "backup foreign_key_check failed".into(),
        ));
    }
    Ok(())
}

/// Export the online database plus every persistent home file, including global
/// and project fn generations, recipes, pinned releases and run/session artifacts.
/// The destination must not exist. An ownership/revision check detects a released
/// or replaced maintenance fence; an incomplete export is removed on error.
pub fn export_home(home: &Path, dst_dir: &Path, paused: &AdmissionPaused) -> Result<BackupInfo> {
    if !fs::symlink_metadata(home)?.file_type().is_dir()
        || !fs::symlink_metadata(home.join(DATABASE_FILE))?
            .file_type()
            .is_file()
    {
        return Err(bad(
            "export source must be a regular home directory and database",
        ));
    }
    reject_nested(home, dst_dir)?;
    let source = schema::open_reader(home, Duration::from_millis(50))?;
    check_paused(&source, paused)?;
    pin_snapshot(&source)?;
    let mut destination = FreshDirectory::create(dst_dir, false)?;
    let info = backup_connection(&source, &dst_dir.join(DATABASE_FILE))?;
    copy_home_files(home, dst_dir)?;
    // Check on a NEW connection, not the earlier pinned snapshot.
    check_paused(
        &schema::open_reader(home, Duration::from_millis(50))?,
        paused,
    )?;
    destination.finish()?;
    Ok(info)
}

fn check_paused(connection: &Connection, paused: &AdmissionPaused) -> Result<()> {
    let (mode, owner, revision): (String, Option<String>, i64) = connection.query_row(
        "SELECT mode,owner,revision FROM maintenance WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if mode != "drain"
        || paused.owner.is_empty()
        || owner.as_deref() != Some(paused.owner.as_str())
        || revision != paused.revision
    {
        return Err(busy(
            "export requires the caller's active admission pause with unchanged maintenance owner and revision",
        ));
    }
    Ok(())
}

/// Restore a standalone database backup or a complete exported directory into
/// an absent/empty isolated home. Existing contents are always refused. The
/// database uses the online backup API even on restore, then verifies the Rust
/// schema and integrity. No coordinator, guardian or fn is started.
pub fn restore_into_fresh_home(src: &Path, dst: &Path) -> Result<BackupInfo> {
    let metadata = fs::symlink_metadata(src)?;
    if metadata.file_type().is_symlink() {
        return Err(bad("restore source must not be a symlink"));
    }
    let is_home = metadata.is_dir();
    let database = if is_home {
        reject_nested(src, dst)?;
        src.join(DATABASE_FILE)
    } else {
        src.to_owned()
    };
    if !fs::symlink_metadata(&database)?.file_type().is_file() {
        return Err(bad("restore source database must be a regular file"));
    }
    let source = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    source.busy_timeout(Duration::from_millis(50))?;
    source.pragma_update(None, "query_only", true)?;
    integrity(&source)?;
    pin_snapshot(&source)?;
    let mut destination = FreshDirectory::create(dst, true)?;
    let info = backup_connection(&source, &dst.join(DATABASE_FILE))?;
    // A backup of an older schema comes forward here, as its writer would bring it.
    schema::upgrade_copy(&dst.join(DATABASE_FILE))?;
    // This verifies HomeId, format, schema, application_id and all 23 tables.
    schema::open_reader(dst, Duration::from_millis(50))?;
    if is_home {
        copy_home_files(src, dst)?;
    }
    destination.finish()?;
    Ok(info)
}

fn copy_home_files(src: &Path, dst: &Path) -> Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(
            name.to_str(),
            Some(
                DATABASE_FILE
                    | "sluice.db-wal"
                    | "sluice.db-shm"
                    | "sluice.db-journal"
                    | "coordinator.sock"
                    | "coordinator.lock"
                    | "locks"
            )
        ) {
            continue;
        }
        if name == INCOMPLETE {
            return Err(bad("source home is an incomplete export or restore"));
        }
        copy_entry(&entry.path(), &dst.join(name))?;
    }
    Ok(())
}

fn copy_entry(src: &Path, dst: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(src)?;
    if metadata.file_type().is_symlink() {
        return Err(bad("home export/restore refuses symlinks"));
    }
    if metadata.is_dir() {
        fs::create_dir(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_entry(&entry.path(), &dst.join(entry.file_name()))?;
        }
        fs::set_permissions(dst, metadata.permissions())?;
        File::open(dst)?.sync_all()?;
    } else if metadata.is_file() {
        let mut source = File::open(src)?;
        let mut destination = fresh_file(dst)?;
        std::io::copy(&mut source, &mut destination)?;
        destination.set_permissions(metadata.permissions())?;
        destination.sync_all()?;
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if metadata.file_type().is_socket() {
                return Ok(());
            }
        }
        return Err(bad("home export/restore refuses special files"));
    }
    Ok(())
}

fn reject_nested(src: &Path, dst: &Path) -> Result<()> {
    let src = fs::canonicalize(src)?;
    let dst = if dst.exists() {
        fs::canonicalize(dst)?
    } else {
        let parent = parent_dir(dst);
        fs::canonicalize(parent)?.join(
            dst.file_name()
                .ok_or_else(|| bad("destination needs a file name"))?,
        )
    };
    if dst.starts_with(&src) || src.starts_with(&dst) {
        return Err(bad(
            "source and destination homes must be separate directory trees",
        ));
    }
    Ok(())
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
fn sync_parent(path: &Path) -> Result<()> {
    File::open(parent_dir(path))?.sync_all()?;
    Ok(())
}
fn fresh_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            bad("destination already exists; backup never overwrites")
        } else {
            e.into()
        }
    })
}
fn bad(message: &str) -> StoreError {
    PublicError::BadRequest {
        message: message.into(),
    }
    .into()
}
fn busy(message: &str) -> StoreError {
    PublicError::Busy {
        message: message.into(),
        retryable: false,
    }
    .into()
}

struct FileCleanup {
    path: PathBuf,
    armed: bool,
}
impl Drop for FileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
            // Only sidecars of our newly reserved destination are removed.
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut path = self.path.as_os_str().to_owned();
                path.push(suffix);
                let _ = fs::remove_file(PathBuf::from(path));
            }
        }
    }
}
struct FreshDirectory {
    path: PathBuf,
    created: bool,
    armed: bool,
}
impl FreshDirectory {
    fn create(path: &Path, allow_empty: bool) -> Result<Self> {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        let created = match builder.create(path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && allow_empty => {
                if !fs::symlink_metadata(path)?.file_type().is_dir()
                    || fs::read_dir(path)?.next().is_some()
                {
                    return Err(bad("restore destination must be an empty directory"));
                }
                false
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(bad("export destination already exists"));
            }
            Err(e) => return Err(e.into()),
        };
        let marker_path = path.join(INCOMPLETE);
        let marker = match fresh_file(&marker_path) {
            Ok(marker) => marker,
            Err(error) => {
                // No reservation was acquired: never remove another copy's files.
                if created {
                    let _ = fs::remove_dir(path);
                }
                return Err(error);
            }
        };
        // Another restore may have completed after our emptiness check but
        // before marker creation. Only our marker is ours until this recheck.
        let untouched = fs::read_dir(path).and_then(|entries| {
            for entry in entries {
                if entry?.file_name() != INCOMPLETE {
                    return Ok(false);
                }
            }
            Ok(true)
        });
        if !matches!(untouched, Ok(true)) {
            let _ = fs::remove_file(&marker_path);
            return Err(bad("destination changed before copy reservation"));
        }
        let directory = Self {
            path: path.to_owned(),
            created,
            armed: true,
        };
        marker.sync_all()?;
        Ok(directory)
    }
    fn finish(&mut self) -> Result<()> {
        fs::remove_file(self.path.join(INCOMPLETE))?;
        File::open(&self.path)?.sync_all()?;
        sync_parent(&self.path)?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for FreshDirectory {
    fn drop(&mut self) {
        if self.armed {
            if self.created {
                let _ = fs::remove_dir_all(&self.path);
            } else if let Ok(entries) = fs::read_dir(&self.path) {
                // Keep the caller's originally empty directory and permissions.
                for entry in entries.flatten() {
                    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                        let _ = fs::remove_dir_all(entry.path());
                    } else {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}
