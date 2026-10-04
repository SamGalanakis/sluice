//! Fixture executables written so a parallel test cannot exec-race them.
//!
//! A test binary runs its tests on threads of one process. While this process
//! holds a file open for writing, any sibling thread that forks hands the open
//! descriptor to its child until that child execs, and an exec of the file in
//! that window fails with ETXTBSY ("Text file busy"). Writing a temporary and
//! renaming it does not help: the renamed inode is the one the descriptor names.
//! So the bytes are copied into place by a short-lived `cp` child, whose
//! descriptors no sibling fork can inherit; this process only stages a data file
//! it never executes, then marks the copy executable and renames it into place.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

pub fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let name = path.file_name().unwrap().to_string_lossy();
    let unique = format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let staged = path.with_file_name(format!(".{name}.{unique}.data"));
    let copied = path.with_file_name(format!(".{name}.{unique}.exec"));
    fs::write(&staged, contents).unwrap();
    let status = Command::new("/usr/bin/cp")
        .arg("--")
        .arg(&staged)
        .arg(&copied)
        .status()
        .unwrap();
    assert!(status.success(), "cp {} failed", staged.display());
    fs::remove_file(&staged).unwrap();
    fs::set_permissions(&copied, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(&copied, path).unwrap();
}
