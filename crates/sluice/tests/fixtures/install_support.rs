use serde_json::{Value, json};
use sluice_model::{
    commands::CommandReply,
    rpc::{RpcReply, RpcResult, decode_json, encode_frame},
};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};

pub struct Gate {
    pub root: tempfile::TempDir,
    pub home: PathBuf,
    pub install: PathBuf,
    pub children: Vec<Child>,
}
impl Gate {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let install = root.path().join("install");
        std::fs::create_dir(&home).unwrap();
        Self {
            root,
            home,
            install,
            children: Vec::new(),
        }
    }
    pub fn command(&self, binary: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(binary);
        command
            .args(args)
            .env("SLUICE_HOME", &self.home)
            .env("SLUICE_INSTALL_DIR", &self.install)
            .env("SLUICE_TEST", "1")
            .env("SLUICE_FIXTURE", "1");
        command
    }
    pub fn cli(&self, args: &[&str]) -> Output {
        self.command(Path::new(env!("CARGO_BIN_EXE_sluice")), args)
            .output()
            .unwrap()
    }
    pub fn boot(&mut self, binary: &Path, maintenance: bool) {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.path().join("coordinator.log"))
            .unwrap();
        let args = if maintenance {
            vec!["coordinator", "--maintenance"]
        } else {
            vec!["coordinator"]
        };
        let child = self
            .command(binary, &args)
            .env("PATH", "/usr/bin:/bin")
            .env_remove("PYTHONPATH")
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.children.push(child);
        wait(|| {
            if self
                .children
                .last_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_some()
            {
                panic!(
                    "coordinator exited: {}",
                    std::fs::read_to_string(self.root.path().join("coordinator.log")).unwrap()
                );
            }
            UnixStream::connect(self.home.join("coordinator.sock")).is_ok()
        });
    }
    pub fn stop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.children.clear();
    }
    pub fn rpc(&self, value: Value) -> Result<CommandReply, sluice_model::error::PublicError> {
        rpc(&self.home, value)
    }
}
pub fn send(
    stream: &mut UnixStream,
    value: Value,
) -> Result<CommandReply, sluice_model::error::PublicError> {
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    stream.write_all(&encode_frame(&json!({"protocol":1,"request_id":"install-gate","run_capability":null,"command":value})).unwrap()).unwrap();
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut bytes).unwrap();
    let reply: RpcReply = decode_json(&bytes).unwrap();
    match reply.result {
        RpcResult::Ok(v) => Ok(*v),
        RpcResult::Error(e) => Err(e),
    }
}
pub fn rpc(home: &Path, value: Value) -> Result<CommandReply, sluice_model::error::PublicError> {
    send(
        &mut UnixStream::connect(home.join("coordinator.sock")).unwrap(),
        value,
    )
}
pub fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !predicate() {
        assert!(Instant::now() < deadline, "install gate timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.stop();
        if let Ok(bytes) = std::fs::read(self.install.join("services.json"))
            && let Ok(services) = serde_json::from_slice::<Value>(&bytes)
            && let Some(records) = services.as_object()
        {
            for record in records.values() {
                if let (Some(pid), Some(start)) = (record["pid"].as_u64(), record["start"].as_str())
                    && let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    && stat.rsplit(')').next().unwrap().split_whitespace().nth(19) == Some(start)
                {
                    let _ = Command::new("/bin/kill")
                        .args(["-TERM", &pid.to_string()])
                        .status();
                }
            }
        }
        if let Ok(runs) = std::fs::read_dir(self.home.join("runs")) {
            for run in runs.flatten() {
                if run
                    .file_name()
                    .to_str()
                    .is_some_and(|s| s.parse::<sluice_model::ids::RunId>().is_ok())
                {
                    let unit = format!("sluice-test-{}.service", run.file_name().to_string_lossy());
                    let _ = Command::new("/usr/bin/systemctl")
                        .args(["--user", "stop", &unit])
                        .output();
                    let _ = Command::new("/usr/bin/systemctl")
                        .args(["--user", "reset-failed", &unit])
                        .output();
                }
            }
        }
    }
}
