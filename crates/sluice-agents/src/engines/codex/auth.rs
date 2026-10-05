//! Every Codex run uses the owner's own credential file.
//!
//! Codex keeps its ChatGPT tokens in `$CODEX_HOME/auth.json` and rotates the refresh token on
//! every refresh, so a private copy per run leaves every other copy, the owner's included, with
//! a used refresh token that the server refuses as revoked. Instead each private home's
//! `auth.json` is a symlink to the owner's file. Codex 0.160.0 writes that file in place (one
//! `open(O_WRONLY|O_CREAT|O_TRUNC)` that follows the link, no rename) and reloads it before every
//! refresh and after a 401, skipping its own refresh when the file changed, so a refresh in any
//! run or in the owner's own Codex is seen by all of them.
//!
//! Homes from before this hold a regular-file copy. [`share`] converts every such copy under
//! the homes' lock: a copy for the owner's account whose `last_refresh` is newer than the
//! owner's holds the live refresh token, so the newest is written to the owner's file first,
//! and the owner's file is never replaced by an older one.

use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    os::unix::fs::{OpenOptionsExt, symlink},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// The lock file under the private homes' directory that serialises [`share`].
pub const LOCK: &str = ".auth.lock";

/// Links `private/auth.json` to `owner_home/auth.json`, after converting any private copy left
/// in a home under `homes` (`private` is one of them). Holds `homes/.auth.lock` throughout.
pub fn share(owner_home: &Path, homes: &Path, private: &Path) -> io::Result<()> {
    let owner = std::path::absolute(owner_home)?.join("auth.json");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(homes.join(LOCK))?;
    lock.lock()?;
    let mut copies = Vec::new();
    for entry in fs::read_dir(homes)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path().join("auth.json");
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            let mut file = File::open(&path)?;
            let bytes = read(&mut file)?;
            copies.push((path, file, bytes));
        }
    }
    // Adopt before relinking, so a running Codex that reloads through a new link already finds
    // the newest credentials; then read each old inode again, since a Codex that opened its copy
    // before the link replaced it writes there.
    adopt(&owner, copies.iter().map(|(_, _, bytes)| bytes.as_slice()))?;
    let mut later = Vec::new();
    for (path, mut file, bytes) in copies {
        link(&path, &owner)?;
        file.rewind()?;
        let now = read(&mut file)?;
        if now != bytes {
            tracing::info!(home = %path.display(), "Codex wrote its private credential copy while it was replaced");
            later.push(now);
        }
    }
    adopt(&owner, later.iter().map(Vec::as_slice))?;
    let target = private.join("auth.json");
    if fs::read_link(&target).ok().as_deref() != Some(owner.as_path()) {
        link(&target, &owner)?;
    }
    Ok(())
}

/// The account and the time of the last refresh of one `auth.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    account: String,
    refreshed: OffsetDateTime,
}
fn stamp(bytes: &[u8]) -> Option<Stamp> {
    let auth: Value = serde_json::from_slice(bytes).ok()?;
    Some(Stamp {
        account: auth["tokens"]["account_id"].as_str()?.to_owned(),
        refreshed: OffsetDateTime::parse(auth["last_refresh"].as_str()?, &Rfc3339).ok()?,
    })
}

/// Writes the newest of `copies` to `owner` when it is for the owner's account and newer than
/// the owner's file. An absent or unreadable owner file is left alone: the owner logged out or
/// changed accounts, and no copy decides that.
fn adopt<'a>(owner: &Path, copies: impl Iterator<Item = &'a [u8]>) -> io::Result<()> {
    let copies: Vec<_> = copies
        .filter_map(|bytes| Some((stamp(bytes)?, bytes)))
        .collect();
    if copies.is_empty() {
        return Ok(());
    }
    // Codex does not take the lock, so the owner's file may change under us: compare again
    // just before the rename, and decide again if it did.
    for _ in 0..8 {
        let current = match fs::read(owner) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let Some(theirs) = stamp(&current) else {
            return Ok(());
        };
        let Some((ours, candidate)) = copies
            .iter()
            .filter(|(stamp, _)| stamp.account == theirs.account)
            .max_by_key(|(stamp, _)| stamp.refreshed)
        else {
            return Ok(());
        };
        if ours.refreshed <= theirs.refreshed {
            return Ok(());
        }
        // Replace the file the owner's path resolves to, so an owner's own symlink stays.
        let real = fs::canonicalize(owner)?;
        let temp = temp_beside(&real);
        let result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(candidate)?;
            file.sync_all()?;
            if fs::read(owner)? != current {
                return Ok(false);
            }
            fs::rename(&temp, &real)?;
            Ok(true)
        })();
        match result {
            Ok(true) => {
                tracing::info!(
                    from = %theirs.refreshed, to = %ours.refreshed,
                    "adopted the newest Codex credentials from a private copy"
                );
                return Ok(());
            }
            Ok(false) => {
                let _ = fs::remove_file(&temp);
            }
            Err(e) => {
                let _ = fs::remove_file(&temp);
                return Err(e);
            }
        }
    }
    Err(io::Error::other(
        "the owner's Codex credentials kept changing",
    ))
}

/// Points `path` at `target` by renaming a fresh symlink over it, so `path` is never missing.
fn link(path: &Path, target: &Path) -> io::Result<()> {
    let temp = temp_beside(path);
    symlink(target, &temp)?;
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

fn temp_beside(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".sluice-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

fn read(file: &mut File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}
