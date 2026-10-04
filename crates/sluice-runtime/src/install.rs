//! Installation selection and fencing, independent of every selected home.
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use sluice_model::error::PublicError;
use sluice_store::{RetrySafety, WriteTransaction, Writer};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub generation: u64,
    pub release_path: PathBuf,
    pub home_path: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Fence {
    pub generation: u64,
    pub reason: String,
    pub since: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub generation: u64,
    pub selection: Option<Selection>,
    pub fence: Option<Fence>,
}
#[derive(Debug, Clone)]
pub struct Installation {
    pub dir: PathBuf,
}
/// Closing this file releases the flock, including on process death.
pub struct Guard {
    _file: File,
    pub generation: u64,
}
fn storage(e: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: e.to_string(),
    }
}
fn maintenance(message: impl Into<String>) -> PublicError {
    PublicError::Busy {
        message: message.into(),
        retryable: false,
    }
}
fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, PublicError> {
    match fs::read(path) {
        Ok(bytes) => sluice_model::rpc::decode_json(&bytes).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(storage(e)),
    }
}
impl Installation {
    pub fn configured() -> Result<Self, PublicError> {
        let dir = if let Some(dir) = std::env::var_os("SLUICE_INSTALL_DIR") {
            PathBuf::from(dir)
        } else {
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| storage("HOME is absent"))?)
                .join(".local/share/sluice/install")
        };
        let installation = Self::at(dir)?;
        if let Some(home) = std::env::var_os("SLUICE_HOME")
            && installation
                .dir
                .starts_with(sluice_process::host::resolve_path(Path::new(&home)).map_err(storage)?)
        {
            return Err(storage(
                "installation control directory must be outside SLUICE_HOME",
            ));
        }
        Ok(installation)
    }
    pub fn at(dir: PathBuf) -> Result<Self, PublicError> {
        let dir = sluice_process::host::resolve_path(&dir).map_err(storage)?;
        Ok(Self { dir })
    }
    /// Development binaries use a scratch counterpart outside their home.
    pub fn for_home(home: &Path) -> Result<Self, PublicError> {
        if std::env::var_os("SLUICE_INSTALL_DIR").is_some() || current_release().is_some() {
            return Self::configured();
        }
        Self::at(home.with_extension("sluice-install"))
    }
    fn lock(&self, exclusive: bool) -> Result<File, PublicError> {
        fs::create_dir_all(&self.dir).map_err(storage)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("install.lock"))
            .map_err(storage)?;
        if exclusive {
            FileExt::lock(&file)
        } else {
            FileExt::lock_shared(&file)
        }
        .map_err(storage)?;
        Ok(file)
    }
    fn unlocked_status(&self) -> Result<Status, PublicError> {
        let selection: Option<Selection> = read(&self.dir.join("selection.json"))?;
        let fence: Option<Fence> = read(&self.dir.join("fence.json"))?;
        let sequence: Option<u64> = read(&self.dir.join("generation.json"))?;
        let generation = sequence
            .unwrap_or(0)
            .max(selection.as_ref().map_or(0, |s| s.generation))
            .max(fence.as_ref().map_or(0, |f| f.generation));
        Ok(Status {
            generation,
            selection,
            fence,
        })
    }
    /// The home this installation selects, read without the lock (selection.json is
    /// replaced atomically) so naming a default home never creates installation files.
    pub fn selected_home(&self) -> Result<Option<PathBuf>, PublicError> {
        Ok(read::<Selection>(&self.dir.join("selection.json"))?.map(|s| s.home_path))
    }
    pub fn status(&self) -> Result<Status, PublicError> {
        let _lock = self.lock(false)?;
        self.unlocked_status()
    }
    fn next(&self) -> Result<u64, PublicError> {
        self.unlocked_status()?
            .generation
            .checked_add(1)
            .ok_or_else(|| storage("installation generation exhausted"))
    }
    fn publish(&self, name: &str, value: &impl Serialize) -> Result<(), PublicError> {
        let temp = self.dir.join(format!(
            ".{name}.{}",
            sluice_model::ids::InvocationId::new()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(storage)?;
        let result = (|| {
            file.write_all(&serde_json::to_vec_pretty(value).map_err(storage)?)
                .map_err(storage)?;
            file.write_all(b"\n").map_err(storage)?;
            file.sync_all().map_err(storage)?;
            fs::rename(&temp, self.dir.join(name)).map_err(storage)?;
            File::open(&self.dir)
                .and_then(|f| f.sync_all())
                .map_err(storage)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    pub fn fence(&self, reason: String) -> Result<Status, PublicError> {
        if reason.trim().is_empty() {
            return Err(PublicError::BadRequest {
                message: "fence reason must not be blank".into(),
            });
        }
        let _lock = self.lock(true)?;
        let generation = self.next()?;
        let fence = Fence {
            generation,
            reason,
            since: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(storage)?
                .as_secs()
                .to_string(),
        };
        self.publish("fence.json", &fence)?;
        self.publish("generation.json", &generation)?;
        self.unlocked_status()
    }
    pub fn unfence(&self) -> Result<Status, PublicError> {
        self.unfence_checked(|_| Ok(()))
    }
    /// Remove the fence after `verify` accepts the selected release, both under the
    /// exclusive lock, so the release that admission will run is the one checked.
    pub fn unfence_checked<F>(&self, verify: F) -> Result<Status, PublicError>
    where
        F: FnOnce(&Path) -> Result<(), PublicError>,
    {
        let _lock = self.lock(true)?;
        if let Some(selection) = self.unlocked_status()?.selection {
            verify(&selection.release_path)?;
        }
        let generation = self.next()?;
        self.publish("generation.json", &generation)?;
        match fs::remove_file(self.dir.join("fence.json")) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(storage(e)),
        }
        File::open(&self.dir)
            .and_then(|f| f.sync_all())
            .map_err(storage)?;
        self.unlocked_status()
    }
    /// Selects an immutable release directory and its home in one atomic JSON rename.
    /// Python rollback artifacts may use the same bin/sluice layout without a Rust manifest.
    pub fn select(&self, release: &Path, home: &Path) -> Result<Status, PublicError> {
        let _lock = self.lock(true)?;
        if self.unlocked_status()?.fence.is_none() {
            return Err(maintenance("select requires the installation fence"));
        }
        let release_path = fs::canonicalize(release).map_err(storage)?;
        if !release_path.join("bin/sluice").is_file() {
            return Err(storage("release lacks bin/sluice"));
        }
        let home_path = sluice_process::host::resolve_path(home).map_err(storage)?;
        if self.dir.starts_with(&home_path) {
            return Err(storage(
                "installation control directory must be outside the selected home",
            ));
        }
        let generation = self.next()?;
        // Keep a Rust descriptor reader for rollback targets whose executable
        // does not understand the internal entry prefix. JSON is the authority.
        if release_path.join("manifest.json").is_file() || !self.dir.join("entry").exists() {
            let dispatcher = if release_path.join("manifest.json").is_file() {
                release_path.join("bin/sluice")
            } else {
                std::env::current_exe().map_err(storage)?
            };
            let temp = self
                .dir
                .join(format!(".entry.{}", sluice_model::ids::InvocationId::new()));
            std::os::unix::fs::symlink(dispatcher, &temp).map_err(storage)?;
            fs::rename(&temp, self.dir.join("entry")).map_err(storage)?;
        }
        self.publish(
            "selection.json",
            &Selection {
                generation,
                release_path,
                home_path,
            },
        )?;
        self.publish("generation.json", &generation)?;
        self.unlocked_status()
    }
    pub fn admission_guard(
        &self,
        home: &Path,
        maintenance_boot: bool,
    ) -> Result<Guard, PublicError> {
        self.checked_guard(home, maintenance_boot, true)
    }
    /// Existing runs stay pinned to their own executable across a release switch.
    pub fn payload_guard(&self, home: &Path) -> Result<Guard, PublicError> {
        self.checked_guard(home, false, false)
    }
    fn checked_guard(
        &self,
        home: &Path,
        maintenance_boot: bool,
        check_release: bool,
    ) -> Result<Guard, PublicError> {
        let file = self.lock(false)?;
        let status = self.unlocked_status()?;
        if !maintenance_boot && let Some(fence) = status.fence {
            return Err(maintenance(format!(
                "maintenance: {} (generation {})",
                fence.reason, fence.generation
            )));
        }
        if let Some(selection) = status.selection {
            let home = sluice_process::host::resolve_path(home).map_err(storage)?;
            if home != selection.home_path {
                return Err(maintenance("maintenance: stale selected home"));
            }
            if check_release
                && let Some(release) = current_release()
                && release != selection.release_path
            {
                return Err(maintenance("maintenance: stale selected release"));
            }
        }
        Ok(Guard {
            _file: file,
            generation: status.generation,
        })
    }
}
pub fn current_release() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let root = exe.parent()?.parent()?;
    root.join("manifest.json")
        .is_file()
        .then(|| root.to_path_buf())
}
pub fn admission_guard(home: &Path) -> Result<Guard, PublicError> {
    Installation::for_home(home)?.admission_guard(home, false)
}
/// Acquire inside the actor job, return the guard with its result, then release after
/// commit. A dropped RPC future cannot release the guard of an enqueued write.
pub async fn admission_write<T, F>(
    writer: &Writer,
    safety: RetrySafety,
    operation: F,
) -> Result<T, PublicError>
where
    T: Send + 'static,
    F: FnOnce(&mut WriteTransaction<'_>) -> sluice_store::Result<T> + Send + 'static,
{
    let home = writer.home().to_path_buf();
    let (value, _guard) = writer
        .write(safety, move |tx| {
            let guard = admission_guard(&home)?;
            let value = operation(tx)?;
            Ok((value, guard))
        })
        .await?;
    Ok(value)
}

/// Guardian executable identity is independent of the function bundle identity.
pub fn release_id(fallback: &str) -> String {
    current_release()
        .and_then(|root| {
            root.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| fallback.into())
}
