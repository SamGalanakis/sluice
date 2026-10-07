use sluice_process::locks::{FileLock, LockAttempt};
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct ChildGuard(std::process::Child, sluice_process::identity::OwnedProcess);
impl ChildGuard {
    fn new(child: std::process::Child) -> Self {
        let process = sluice_process::identity::OwnedProcess::capture(child.id()).unwrap();
        Self(child, process)
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.1.signal(rustix::process::Signal::KILL);
        let _ = self.0.wait();
    }
}
#[test]
fn conflict_identifies_another_process_and_crash_releases_lock() {
    let scratch = tempfile::tempdir().unwrap();
    let mut child = ChildGuard::new(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock_worker", "--nocapture"])
            .env("SLUICE_TEST_LOCK_DIR", scratch.path())
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !scratch.path().join("ready").exists() {
        assert!(Instant::now() < deadline);
        assert!(child.0.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(10));
    }
    let path = scratch.path().join("engine.lock");
    let LockAttempt::Conflict(holder) = FileLock::try_acquire(&path, None, None).unwrap() else {
        panic!("held engine lock acquired")
    };
    assert_eq!(holder.pid, child.0.id());
    assert_eq!(holder.project.as_deref(), Some("fixture-engine"));
    assert!(holder.run.is_some());
    child.1.signal(rustix::process::Signal::KILL).unwrap();
    child.0.wait().unwrap();
    assert!(matches!(
        FileLock::try_acquire(&path, None, None).unwrap(),
        LockAttempt::Acquired(_)
    ));
}
#[test]
fn lock_worker() {
    let Some(dir) = std::env::var_os("SLUICE_TEST_LOCK_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    let lock = FileLock::try_acquire(
        &dir.join("engine.lock"),
        Some("fixture-engine".into()),
        Some(sluice_model::ids::RunId::new()),
    )
    .unwrap();
    assert!(matches!(lock, LockAttempt::Acquired(_)));
    fs::write(dir.join("ready"), b"ready").unwrap();
    loop {
        thread::sleep(Duration::from_secs(1));
        std::hint::black_box(&lock);
    }
}
#[test]
fn lock_refuses_symlink() {
    let scratch = tempfile::tempdir().unwrap();
    fs::write(scratch.path().join("target"), b"untouched").unwrap();
    std::os::unix::fs::symlink("target", scratch.path().join("lock")).unwrap();
    assert!(FileLock::try_acquire(&scratch.path().join("lock"), None, None).is_err());
    assert_eq!(
        fs::read(scratch.path().join("target")).unwrap(),
        b"untouched"
    );
}

#[test]
fn conflict_waits_for_metadata_publication_even_for_the_same_pid() {
    use fs4::FileExt;
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("home.lock");
    let LockAttempt::Acquired(old) =
        FileLock::try_acquire(&path, Some("old".into()), None).unwrap()
    else {
        panic!("new lock conflicted")
    };
    drop(old);
    // Model a second acquisition by the same live process paused before its
    // metadata write. PID/start/boot checks alone would accept the old holder.
    let publication = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scratch.path().join("home.lock.publish"))
        .unwrap();
    FileExt::lock(&publication).unwrap();
    let current = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    FileExt::lock(&current).unwrap();
    let error = FileLock::try_acquire(&path, Some("contender".into()), None).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(error.to_string().contains("publication"));
    drop(current);
    drop(publication);
    let LockAttempt::Acquired(new) =
        FileLock::try_acquire(&path, Some("new".into()), None).unwrap()
    else {
        panic!("released lock conflicted")
    };
    let LockAttempt::Conflict(holder) = FileLock::try_acquire(&path, None, None).unwrap() else {
        panic!("held lock acquired")
    };
    assert_eq!(&holder, new.holder());
    assert_eq!(holder.project.as_deref(), Some("new"));
}
