use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
pub fn scratch() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("sluice-test-p7-")
        .tempdir_in("/tmp")
        .unwrap()
}
pub fn binary() -> PathBuf {
    let path = match std::env::var_os("SLUICE_G7_RELEASE") {
        Some(release) => {
            let release = PathBuf::from(release).canonicalize().unwrap();
            assert!(release.starts_with(workspace().join("target")));
            assert!(release.join("manifest.json").is_file());
            release.join("bin/sluice")
        }
        None => workspace().join("target/release/sluice"),
    };
    assert!(
        path.is_file(),
        "build the release before the ignored G7 tests"
    );
    path
}
pub fn evidence(name: &str, value: &Value) {
    let dir = workspace().join("target/p7-03-evidence");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(dir.join(format!("{name}.json")), value.to_string()).unwrap();
    println!("{name} {value}");
}
pub fn checked(command: &mut Command) -> std::process::Output {
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{command:?}: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
pub fn snapshot(root: &Path) -> Value {
    let out = checked(
        Command::new("/home/sam/.local/bin/uv")
            .args(["run", "--project"])
            .arg(workspace())
            .args(["--no-sync", "python"])
            .arg(workspace().join("crates/sluice/tests/rehearsal/fixtures/snapshot.py"))
            .arg(root)
            .env("SLUICE_HOME", root.join("unused-home"))
            .env("SLUICE_INSTALL_DIR", root.join("install")),
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
pub fn import(root: &Path, destination: &Path) -> Value {
    let out = checked(
        Command::new(binary())
            .arg("import-python-home")
            .arg(root.join("source"))
            .arg(destination)
            .arg("--staging")
            .arg(root.join("staging"))
            .env("SLUICE_HOME", destination)
            .env("SLUICE_INSTALL_DIR", root.join("install")),
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
pub fn readonly(home: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        home.join("sluice.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}
pub fn live_stamp() -> (std::time::SystemTime, u64) {
    let m = fs::metadata("/home/sam/.sluice/sluice.db").unwrap();
    (m.modified().unwrap(), m.len())
}
pub fn tree(root: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    checked(
        Command::new("/usr/bin/cp")
            .args(["-a", "--"])
            .arg(root.join("."))
            .arg(dst),
    );
}

pub struct Process(pub Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
pub fn wait(mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < end, "fixture timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}
pub fn boot(binary: &Path, home: &Path, env: &BTreeMap<String, String>) -> Process {
    assert!(home.ancestors().any(|p| {
        p.parent() == Some(Path::new("/tmp"))
            && p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("sluice-test-")
    }));
    assert_ne!(
        home.canonicalize().unwrap(),
        PathBuf::from("/home/sam/.sluice")
    );
    let log = fs::File::create(home.join("rehearsal-coordinator.log")).unwrap();
    let mut child = Process(
        Command::new(binary)
            .args(["coordinator", "--maintenance"])
            .envs(env)
            .env("SLUICE_HOME", home)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    wait(|| {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "{}",
            fs::read_to_string(home.join("rehearsal-coordinator.log")).unwrap()
        );
        UnixStream::connect(home.join("coordinator.sock")).is_ok()
    });
    child
}
pub fn rpc(home: &Path, command: Value) -> Value {
    let mut socket = UnixStream::connect(home.join("coordinator.sock")).unwrap();
    rpc_stream(&mut socket, command)
}
pub fn rpc_stream(socket: &mut UnixStream, command: Value) -> Value {
    socket
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let raw =
        json!({"protocol":1,"request_id":"p7-rehearsal","run_capability":null,"command":command});
    socket
        .write_all(&sluice_model::rpc::encode_frame(&raw).unwrap())
        .unwrap();
    let mut header = [0; 4];
    socket.read_exact(&mut header).unwrap();
    let mut raw = vec![0; u32::from_be_bytes(header) as usize];
    socket.read_exact(&mut raw).unwrap();
    serde_json::from_slice(&raw).unwrap()
}
pub fn data(home: &Path, command: Value) -> Value {
    let reply = rpc(home, command);
    assert_eq!(reply["result"]["status"], "ok", "{reply}");
    reply["result"]["value"]["data"].clone()
}
pub fn install(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(binary())
        .arg("install")
        .args(args)
        .env("SLUICE_INSTALL_DIR", root.join("install"))
        .env("SLUICE_HOME", root.join("rust-home"))
        .output()
        .unwrap()
}
pub fn install_available() -> bool {
    let scratch = scratch();
    let out = Command::new(binary())
        .args(["install", "--help"])
        .env("SLUICE_HOME", scratch.path().join("home"))
        .env("SLUICE_INSTALL_DIR", scratch.path().join("install"))
        .output()
        .unwrap();
    out.status.success()
}

pub fn ok(home: &Path, command: Value) -> Value {
    let reply = rpc(home, command);
    assert_eq!(reply["result"]["status"], "ok", "{reply}");
    reply["result"]["value"].clone()
}
pub fn scheduler(home: &Path) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("coordinator.sock")).unwrap();
    stream.write_all(&sluice_model::rpc::encode_frame(&json!({"protocol":1,"request_id":"p7-scheduler","run_capability":null,"command":{"runtime":"acquire_scheduler","args":{"owner":"p7-rehearsal"}}})).unwrap()).unwrap();
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let mut body = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut body).unwrap();
    let reply: Value = serde_json::from_slice(&body).unwrap();
    assert!(reply["result"].get("Ok").is_some(), "{reply}");
    stream
}
pub struct OwnedHome(pub PathBuf);
impl OwnedHome {
    pub fn units(&self) -> Vec<String> {
        fs::read_dir(self.0.join("runs"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|id| id.parse::<sluice_model::ids::RunId>().ok())
                    .map(|id| format!("sluice-test-{id}.service"))
            })
            .collect()
    }
    pub fn assert_empty(&self) {
        for unit in self.units() {
            let out = Command::new("/usr/bin/systemctl")
                .args([
                    "--user",
                    "show",
                    "--property=ControlGroup",
                    "--value",
                    &unit,
                ])
                .output()
                .unwrap();
            let cgroup = String::from_utf8(out.stdout).unwrap();
            if !cgroup.trim().is_empty() {
                let path = Path::new("/sys/fs/cgroup")
                    .join(cgroup.trim().trim_start_matches('/'))
                    .join("cgroup.events");
                if path.exists() {
                    assert!(
                        fs::read_to_string(path).unwrap().contains("populated 0"),
                        "owned work remains in {unit}"
                    );
                }
            }
        }
    }
}
impl Drop for OwnedHome {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let dest = workspace().join("target/p7-03-evidence/lane-failure");
            let _ = fs::create_dir_all(&dest);
            let _ = fs::copy(
                self.0.join("rehearsal-coordinator.log"),
                dest.join("coordinator.log"),
            );
            if let Ok(runs) = fs::read_dir(self.0.join("runs")) {
                for run in runs.flatten() {
                    let dir = dest.join(run.file_name());
                    let _ = fs::create_dir_all(&dir);
                    for name in ["native.json", "stderr-tail.log", "stderr.log", "stdout.log"] {
                        let _ = fs::copy(run.path().join(name), dir.join(name));
                    }
                }
            }
            let _ = fs::copy(self.0.join("sluice.db"), dest.join("sluice.db"));
        }
        for unit in self.units() {
            let _ = Command::new("/usr/bin/systemctl")
                .args(["--user", "stop", &unit])
                .output();
            let _ = Command::new("/usr/bin/systemctl")
                .args(["--user", "reset-failed", &unit])
                .output();
        }
    }
}

pub fn release_fixture(root: &Path) -> PathBuf {
    let binary = binary();
    let source = binary.parent().unwrap().parent().unwrap();
    assert!(
        source.join("manifest.json").is_file(),
        "p7-02: build a scratch package and set SLUICE_G7_RELEASE before exercising selection"
    );
    let release = root.join("releases").join(source.file_name().unwrap());
    tree(source, &release);
    release
}
