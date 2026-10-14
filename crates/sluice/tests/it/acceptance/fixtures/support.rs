use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = PathBuf::from("/tmp").join(format!(
            "sluice-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let evidence = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/p5-05-evidence")
                .join(self.0.file_name().unwrap());
            let _ = fs::create_dir_all(&evidence);
            if let Ok(runs) = fs::read_dir(self.0.join("rust-home/runs")) {
                for run in runs.flatten() {
                    let dst = evidence.join(run.file_name());
                    let _ = fs::create_dir_all(&dst);
                    for name in ["native.json", "stderr-tail.log", "app-server.log"] {
                        if run.path().join(name).is_file() {
                            let _ = fs::copy(run.path().join(name), dst.join(name));
                        }
                    }
                }
            }
            eprintln!("p5-05 failure evidence: {}", evidence.display());
            // Optionally retain the whole scratch tree; symlinks are copied as links.
            if let Some(keep) = std::env::var_os("SLUICE_KEEP_FAILED_SCRATCH") {
                let keep = PathBuf::from(keep).join(self.0.file_name().unwrap());
                let _ = fs::create_dir_all(keep.parent().unwrap());
                let _ = Command::new("/usr/bin/cp")
                    .args(["-a", "--no-dereference", "--"])
                    .arg(&self.0)
                    .arg(&keep)
                    .output();
                eprintln!("retained failed scratch: {}", keep.display());
            }
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
pub fn repo(cwd: &Path) {
    git(cwd, &["init", "-q"]);
    git(cwd, &["config", "user.name", "Acceptance fixture"]);
    git(cwd, &["config", "user.email", "acceptance@example.invalid"]);
    git(
        cwd,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Create the fixture baseline.",
        ],
    );
}
