//! Stopped, trusted-owner snapshots only. Concurrent malicious same-uid namespace
//! replacement is outside this temporary command's boundary. Links are inspected
//! and refused unless an engine's external credential ownership is explicit.
use anyhow::{Context, Result, bail, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

pub fn directory(path: &Path) -> Result<()> {
    if path.exists() {
        ensure!(
            !fs::symlink_metadata(path)?.file_type().is_symlink(),
            "directory is a symlink: {}",
            path.display()
        );
        ensure!(path.is_dir(), "not a directory: {}", path.display());
    } else {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    Ok(())
}

pub fn read(path: &Path) -> Result<Vec<u8>> {
    for ancestor in path
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
    {
        ensure!(
            !fs::symlink_metadata(ancestor)?.file_type().is_symlink(),
            "file ancestor is a symlink: {} -> {}",
            ancestor.display(),
            fs::read_link(ancestor).unwrap_or_default().display()
        );
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(0x20000 | 0x800)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let before = file.metadata()?;
    ensure!(before.is_file(), "not a regular file: {}", path.display());
    let mut bytes = Vec::new();
    (&file)
        .take(512 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 512 * 1024 * 1024,
        "file exceeds import limit: {}",
        path.display()
    );
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "snapshot changed: {}",
        path.display()
    );
    Ok(bytes)
}

pub fn write(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    directory(path.parent().context("file has no parent")?)?;
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    (&file).write_all(bytes)?;
    file.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
    file.sync_all()?;
    Ok(())
}

pub fn atomic_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let tmp = path.with_extension("new");
    if tmp.exists() {
        fs::remove_file(&tmp)?;
    }
    write(&tmp, &serde_json::to_vec_pretty(value)?, 0o600)?;
    fs::rename(&tmp, path)?;
    File::open(path.parent().context("ledger has no parent")?)?.sync_all()?;
    Ok(())
}

pub fn json(path: &Path) -> Result<serde_json::Value> {
    Ok(sluice_model::rpc::decode_json::<sluice_model::rpc::JsonValue>(&read(path)?)?.into_value())
}

pub fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    let mut out = fs::read_dir(path)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(out.len() <= 100_000, "too many import files");
    out.sort();
    Ok(out)
}

pub fn tree(source: &Path, dest: &Path, omit: &[&str], depth: usize) -> Result<()> {
    ensure!(depth <= 32, "import directory too deep");
    directory(dest)?;
    for path in entries(source)? {
        let name = path.file_name().context("file name missing")?;
        if omit.iter().any(|s| name == *s) {
            continue;
        }
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            bail!(
                "symlink needs explicit review: {} -> {}",
                path.display(),
                fs::read_link(&path)?.display()
            );
        } else if meta.is_dir() {
            tree(&path, &dest.join(name), omit, depth + 1)?;
        } else if meta.is_file() {
            write(&dest.join(name), &read(&path)?, meta.permissions().mode())?;
        } else {
            bail!("special file refused: {}", path.display());
        }
    }
    Ok(())
}

pub fn sync_tree(path: &Path) -> Result<()> {
    for file in entries(path)? {
        if fs::symlink_metadata(&file)?.is_dir() {
            sync_tree(&file)?;
        } else if fs::symlink_metadata(&file)?.is_file() {
            File::open(file)?.sync_all()?;
        }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}

pub fn safe_component(text: &str) -> Result<()> {
    ensure!(
        !text.is_empty()
            && Path::new(text).components().count() == 1
            && matches!(
                Path::new(text).components().next(),
                Some(Component::Normal(_))
            ),
        "invalid source id"
    );
    Ok(())
}

pub fn identity_files(
    root: &Path,
    prefix: &str,
    out: &mut Vec<serde_json::Value>,
    depth: usize,
) -> Result<()> {
    ensure!(depth <= 32, "manifest directory too deep");
    for path in entries(root)? {
        let name = path
            .file_name()
            .context("missing name")?
            .to_str()
            .context("non-UTF8 import filename")?;
        let key = format!("{prefix}/{name}");
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            bail!("manifest contains symlink: {}", path.display());
        } else if meta.is_dir() {
            identity_files(&path, &key, out, depth + 1)?;
        } else {
            out.push(serde_json::json!({"path":key,"sha256":sluice_store::artifacts::fingerprint(&read(&path)?),"mode":meta.permissions().mode() & 0o777}));
        }
    }
    Ok(())
}
