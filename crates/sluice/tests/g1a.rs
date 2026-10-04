//! G1a uses only this binary, fresh homes and guarded sluice-test services.
#[allow(dead_code)]
#[path = "../../../tests/support/units.rs"]
mod units;
use serde_json::{Value, json};
use sluice_model::{
    commands::CommandReply,
    ids::ProjectId,
    rpc::{
        PROTOCOL_VERSION, RequestId, RpcReply, RpcRequest, RpcResult, decode_json, encode_frame,
    },
};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Gate {
    home: PathBuf,
    broker: Option<Child>,
    caller: Option<Child>,
    units: Vec<String>,
}
impl Gate {
    fn boot(&mut self) {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.home.join("broker.log"))
            .unwrap();
        self.broker = Some(
            Command::new(env!("CARGO_BIN_EXE_sluice"))
                .env("SLUICE_HOME", &self.home)
                .env("SLUICE_FIXTURE", "1")
                .arg("coordinator")
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        wait("coordinator socket accepts", || {
            if self.broker.as_mut().unwrap().try_wait().unwrap().is_some() {
                panic!(
                    "broker exited: {}",
                    std::fs::read_to_string(self.home.join("broker.log")).unwrap()
                );
            }
            UnixStream::connect(self.home.join("coordinator.sock")).is_ok()
        });
    }
    fn kill_broker(&mut self) {
        let mut child = self.broker.take().unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
    fn rpc(&self, value: Value) -> CommandReply {
        let command = decode_json(&serde_json::to_vec(&value).unwrap()).unwrap();
        let request = RpcRequest {
            protocol: PROTOCOL_VERSION,
            request_id: RequestId("gate".into()),
            run_capability: None,
            command,
        };
        let mut stream = UnixStream::connect(self.home.join("coordinator.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream.write_all(&encode_frame(&request).unwrap()).unwrap();
        let reply: RpcReply = decode_json(&read_frame(&mut stream)).unwrap();
        assert_eq!(reply.request_id, request.request_id);
        match reply.result {
            RpcResult::Ok(reply) => *reply,
            RpcResult::Error(error) => panic!("RPC: {error:?}"),
        }
    }
    fn status(&self, p: ProjectId) -> Value {
        let CommandReply::Data(value)=self.rpc(json!({"command":"status","args":{"project":{"kind":"id","value":p},"selection":{"steps":null,"tags":null}}}))else{panic!("status")};
        value.into_value()
    }
    fn lease(&self, owner: &str) -> Result<UnixStream, Value> {
        let mut stream = UnixStream::connect(self.home.join("coordinator.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(&encode_frame(&json!({"protocol":1,"request_id":owner,"run_capability":null,"command":{"runtime":"acquire_scheduler","args":{"owner":owner}}})).unwrap()).unwrap();
        let reply: Value = decode_json(&read_frame(&mut stream)).unwrap();
        if reply["result"].get("Ok").is_some() {
            Ok(stream)
        } else {
            Err(reply)
        }
    }
    fn track(&mut self, run: &str) {
        let name = format!("sluice-test-{run}.service");
        if !self.units.contains(&name) {
            self.units.push(name);
        }
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        if let Some(mut child) = self.broker.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.caller.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Run units recovered from this gate's scratch home cover those created
        // before the test could record their returned run IDs.
        let mut owned = units::home_units(&self.home);
        owned.extend(self.units.iter().cloned());
        owned.sort();
        owned.dedup();
        units::stop_units(&owned);
    }
}
fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!(length <= sluice_model::rpc::MAX_FRAME_BYTES);
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}
fn wait(label: &str, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "gate condition timed out: {label}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn dispatched(dir: &Path) -> bool {
    dir.join("dispatch-count").exists()
}
#[test]
#[ignore = "G1a real binary and delegated user services; scratch home only"]
fn boot_callbacks_restart_adoption_lease_and_direct_caller_death() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("h");
    std::fs::create_dir(&home).unwrap();
    sluice_process::host::guard_scratch_home(&home).unwrap();
    let mut gate = Gate {
        home,
        broker: None,
        caller: None,
        units: vec![],
    };
    gate.boot();
    let CommandReply::Project(p)=gate.rpc(json!({"command":"project_create","args":{"name":"fixture","description":"G1a","icon":null,"resources":{"cpu":1},"author":"gate"}}))else{panic!("create")};
    let project = p.project_id;
    let selector = json!({"kind":"id","value":project});
    gate.rpc(json!({"command":"plan_patch","args":{"project":selector,"rev":1,"ops":[{"op":"replace","path":"","value":{"inputs":{},"outputs":{"answer":{"source":"last/value"}},"steps":{"seed":{"run":"core.echo","in":{"value":{"default":7}}},"submit":{"run":"fixture.submit","in":{"value":{"source":"seed/value"}},"outputs":{"submitted":"boolean"}},"long":{"run":"fixture.wait","in":{"value":{"source":"submit/submitted"}},"needs":{"cpu":1}},"last":{"run":"core.echo","in":{"value":{"source":"long/value"}}}}}}],"start":true,"dry_run":false,"reason":"fixture","author":"gate"}}));
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        gate.status(project)["steps"]["seed"]["status"],
        "pending",
        "coordinator has no scheduler"
    );
    let lease = gate.lease("first").unwrap();
    assert!(gate.lease("conflict").is_err());
    let mut long_run = String::new();
    wait("long step dispatched", || {
        let status = gate.status(project);
        if let Some(run) = status["steps"]["long"]["run_ids"][0].as_str() {
            long_run = run.into();
            dispatched(&gate.home.join("runs").join(run))
        } else {
            false
        }
    });
    let status = gate.status(project);
    assert_eq!(status["steps"]["submit"]["status"], "succeeded");
    assert_eq!(status["steps"]["submit"]["outputs"]["submitted"], true);
    assert_eq!(status["resources"]["cpu"]["held"], 1);
    gate.track(status["steps"]["submit"]["run_ids"][0].as_str().unwrap());
    gate.track(&long_run);
    let run_dir = gate.home.join("runs").join(&long_run);
    let frozen = std::fs::read(run_dir.join("admitted.json")).unwrap();
    gate.kill_broker();
    drop(lease);
    assert_eq!(
        std::fs::read_to_string(run_dir.join("dispatch-count")).unwrap(),
        "dispatch\n"
    );
    gate.boot();
    assert_eq!(
        std::fs::read(run_dir.join("admitted.json")).unwrap(),
        frozen
    );
    assert_eq!(gate.status(project)["resources"]["cpu"]["held"], 1);
    let lease = gate.lease("replacement").unwrap();
    std::fs::write(run_dir.join("finish"), b"").unwrap();
    wait("last step succeeded after adoption", || {
        gate.status(project)["steps"]["last"]["status"] == "succeeded"
    });
    assert_eq!(gate.status(project)["outputs"]["answer"], true);
    assert_eq!(gate.status(project)["resources"]["cpu"]["held"], 0);
    assert_eq!(
        std::fs::read_to_string(run_dir.join("dispatch-count")).unwrap(),
        "dispatch\n"
    );
    drop(lease);
    // Real direct CLI caller waits for a guardian. Killing only that caller leaves
    // execution owned by systemd and the calls row owned by the broker.
    let args=json!({"command":"fn_call","args":{"name":"fixture.wait","inputs":{"value":99},"project":selector,"wait_seconds":60,"direct":true,"author":"gate"}}).to_string();
    gate.caller = Some(
        Command::new(env!("CARGO_BIN_EXE_sluice"))
            .env("SLUICE_HOME", &gate.home)
            .args(["tool", "rpc", &args])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut direct_dir = PathBuf::new();
    wait("direct call dispatched", || {
        for entry in std::fs::read_dir(gate.home.join("runs")).unwrap() {
            let path = entry.unwrap().path();
            if path == run_dir || !dispatched(&path) {
                continue;
            }
            let invocation: Value =
                serde_json::from_slice(&std::fs::read(path.join("invocation.json")).unwrap())
                    .unwrap();
            if invocation["step"].is_null() {
                direct_dir = path;
                return true;
            }
        }
        false
    });
    let direct = direct_dir
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    gate.track(&direct);
    let mut caller = gate.caller.take().unwrap();
    caller.kill().unwrap();
    caller.wait().unwrap();
    std::fs::write(direct_dir.join("finish"), b"").unwrap();
    wait("direct call succeeded after caller death", || {
        let CommandReply::Data(status) =
            gate.rpc(json!({"command":"call_status","args":{"call":direct,"project":selector}}))
        else {
            panic!("call status")
        };
        status.as_value()["status"] == "succeeded"
    });
    assert_eq!(
        std::fs::read_to_string(direct_dir.join("dispatch-count")).unwrap(),
        "dispatch\n"
    );
    // The direct completion also lands with no scheduler lease.
    let CommandReply::Data(status) =
        gate.rpc(json!({"command":"call_status","args":{"call":direct,"project":selector}}))
    else {
        panic!("call status")
    };
    assert_eq!(status.as_value()["outputs"]["value"], 99);
    // A new CLI request activates only the broker after its predecessor dies.
    gate.kill_broker();
    let output = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", &gate.home)
        .env("SLUICE_FIXTURE", "1")
        .args(["tool", "rpc", "{\"command\":\"projects_list\"}"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    gate.rpc(json!({"command":"plan_patch","args":{"project":selector,"rev":2,"ops":[{"op":"add","path":"/steps/late","value":{"run":"core.echo","in":{"value":{"default":123}}}}],"start":true,"dry_run":false,"reason":"auto-start has no scheduler","author":"gate"}}));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(gate.status(project)["steps"]["late"]["status"], "pending");
}
