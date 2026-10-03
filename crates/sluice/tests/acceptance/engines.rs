//! Executable G3 fixtures. A successful test requires public callbacks and guardian cleanup.
use crate::support::{Scratch, git, repo};
use serde_json::{Value, json};
use sluice_model::{
    commands::CommandReply,
    rpc::{
        PROTOCOL_VERSION, RequestId, RpcReply, RpcRequest, RpcResult, decode_json, encode_frame,
    },
};
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
pub fn private_write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
pub fn credentials(root: &Path, engine: &str) -> Result<BTreeMap<String, String>, String> {
    let home = root.join("owner");
    fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let owner = Path::new("/home/sam");
    match engine {
        "codex" => {
            let auth = fs::read(owner.join(".codex/auth.json"))
                .map_err(|_| "no privately copyable Codex auth.json")?;
            private_write(&home.join(".codex/auth.json"), &auth);
            let mut config = toml_edit::DocumentMut::new();
            if let Ok(text) = fs::read_to_string(owner.join(".codex/config.toml")) {
                let doc = text
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|_| "invalid source Codex config")?;
                for key in ["model_provider", "model_providers", "service_tier"] {
                    if let Some(v) = doc.get(key) {
                        config[key] = v.clone();
                    }
                }
            }
            private_write(
                &home.join(".codex/config.toml"),
                config.to_string().as_bytes(),
            );
        }
        "claude" => {
            let auth = fs::read(owner.join(".claude/.credentials.json"))
                .map_err(|_| "no privately copyable Claude credentials")?;
            private_write(&home.join(".claude/.credentials.json"), &auth);
            let config: Value = serde_json::from_slice(
                &fs::read(owner.join(".claude.json"))
                    .map_err(|_| "Claude onboarding metadata absent")?,
            )
            .map_err(|_| "invalid Claude onboarding metadata")?;
            let mut private = json!({"hasCompletedOnboarding":true,"numStartups":1,"autoUpdates":false,"bypassPermissionsModeAccepted":true});
            for key in [
                "oauthAccount",
                "lastOnboardingVersion",
                "hasAcknowledgedCostThreshold",
            ] {
                if let Some(v) = config.get(key) {
                    private[key] = v.clone();
                }
            }
            private_write(
                &home.join(".claude/.claude.json"),
                &serde_json::to_vec(&private).unwrap(),
            );
        }
        "devin" => {
            let auth = fs::read(owner.join(".local/share/devin/credentials.toml"))
                .map_err(|_| "no privately copyable Devin credentials.toml")?;
            private_write(&home.join(".local/share/devin/credentials.toml"), &auth);
            let raw = fs::read_to_string(owner.join(".config/devin/config.json"))
                .map_err(|_| "Devin source config absent")?;
            let config: Value = serde_json::from_str(
                &sluice_agents::engines::devin::protocol::strip_jsonc(&raw)
                    .map_err(|_| "invalid source Devin JSONC")?,
            )
            .map_err(|_| "invalid source Devin config")?;
            let mut private = json!({});
            for key in ["version", "devin"] {
                if let Some(v) = config.get(key) {
                    private[key] = v.clone();
                }
            }
            private_write(
                &home.join(".config/devin/config.json"),
                &serde_json::to_vec(&private).unwrap(),
            );
        }
        _ => return Err("unknown engine".into()),
    }
    for path in [
        home.join(".cache"),
        home.join(".local/state"),
        home.join(".config"),
        home.join(".local/share"),
    ] {
        fs::create_dir_all(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    Ok(BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into()),
        (
            "CODEX_HOME".into(),
            home.join(".codex").to_string_lossy().into(),
        ),
        (
            "CLAUDE_CONFIG_DIR".into(),
            home.join(".claude").to_string_lossy().into(),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            home.join(".config").to_string_lossy().into(),
        ),
        (
            "XDG_DATA_HOME".into(),
            home.join(".local/share").to_string_lossy().into(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            home.join(".cache").to_string_lossy().into(),
        ),
        (
            "XDG_STATE_HOME".into(),
            home.join(".local/state").to_string_lossy().into(),
        ),
    ]))
}
pub struct Gate {
    pub home: PathBuf,
    pub broker: Option<Child>,
    pub env: BTreeMap<String, String>,
    lease: Option<UnixStream>,
    units: Vec<String>,
}
impl Gate {
    pub fn new(root: &Path, home: PathBuf, mut env: BTreeMap<String, String>) -> Self {
        fs::create_dir_all(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let ws = workspace();
        let bin = root.join("rust-bin");
        fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(ws.join("target/debug/sluice"), bin.join("sluice")).unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        env.extend(BTreeMap::from([
            ("SLUICE_HOME".into(), home.to_string_lossy().into()),
            ("PATH".into(), path.clone()),
            ("SLUICE_HOST_PATH".into(), path),
            (
                "SLUICE_BIN".into(),
                ws.join("target/debug/sluice").to_string_lossy().into(),
            ),
            (
                "SLUICE_PYTHON_DIR".into(),
                ws.join("python").to_string_lossy().into(),
            ),
            (
                "SLUICE_UV_BIN".into(),
                absolute_tool("uv").to_string_lossy().into(),
            ),
            (
                "SLUICE_TMUX_PREFIX".into(),
                ws.join("target/private-tmux").to_string_lossy().into(),
            ),
            (
                "SLUICE_CODEX_BIN".into(),
                absolute_tool("codex").to_string_lossy().into(),
            ),
            (
                "SLUICE_CLAUDE_BIN".into(),
                absolute_tool("claude").to_string_lossy().into(),
            ),
            (
                "SLUICE_DEVIN_BIN".into(),
                absolute_tool("devin").to_string_lossy().into(),
            ),
        ]));
        Self {
            home,
            broker: None,
            env,
            lease: None,
            units: vec![],
        }
    }
    pub fn boot(&mut self) {
        let log = fs::File::create(self.home.join("broker.log")).unwrap();
        self.broker = Some(
            Command::new(workspace().join("target/debug/sluice"))
                .arg("coordinator")
                .envs(&self.env)
                .env_remove("CLAUDECODE")
                .env_remove("CLAUDE_CODE_SESSION_ID")
                .env_remove("AI_AGENT")
                .env_remove("SLUICE_FIXTURE")
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        self.wait(Duration::from_secs(15), |g| {
            UnixStream::connect(g.home.join("coordinator.sock")).is_ok()
        });
    }
    pub fn rpc(&self, value: Value) -> CommandReply {
        let command = decode_json(&serde_json::to_vec(&value).unwrap()).unwrap();
        let request = RpcRequest {
            protocol: PROTOCOL_VERSION,
            request_id: RequestId("acceptance".into()),
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
            RpcResult::Ok(r) => *r,
            RpcResult::Error(e) => panic!("acceptance RPC failed: {e:?}"),
        }
    }
    pub fn scheduling(&mut self) {
        let mut stream = UnixStream::connect(self.home.join("coordinator.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(&encode_frame(&json!({"protocol":1,"request_id":"lease","run_capability":null,"command":{"runtime":"acquire_scheduler","args":{"owner":"p5-05-acceptance"}}})).unwrap()).unwrap();
        let reply: Value = decode_json(&read_frame(&mut stream)).unwrap();
        assert!(reply["result"].get("Ok").is_some(), "{reply}");
        self.lease = Some(stream);
    }
    pub fn status(&self, selector: &Value) -> Value {
        let CommandReply::Data(v)=self.rpc(json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null}}})) else {panic!("status reply")};
        v.into_value()
    }
    pub fn wait(&mut self, timeout: Duration, mut f: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + timeout;
        while !f(self) {
            if let Some(broker) = &mut self.broker {
                assert!(
                    broker.try_wait().unwrap().is_none(),
                    "coordinator exited: {}",
                    fs::read_to_string(self.home.join("broker.log")).unwrap()
                );
            }
            assert!(Instant::now() < deadline, "acceptance condition timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    pub fn track(&mut self, run: &str) {
        assert!(run.parse::<sluice_model::ids::RunId>().is_ok());
        let unit = format!("sluice-test-{run}.service");
        if !self.units.contains(&unit) {
            self.units.push(unit);
        }
    }
    pub fn cleanup(&mut self) {
        self.lease.take();
        if let Some(mut broker) = self.broker.take() {
            let _ = broker.kill();
            let _ = broker.wait();
        }
        if let Ok(entries) = fs::read_dir(self.home.join("runs")) {
            for entry in entries.flatten() {
                if let Some(run) = entry.file_name().to_str()
                    && run.parse::<sluice_model::ids::RunId>().is_ok()
                {
                    self.track(run);
                }
            }
        }
        for unit in &self.units {
            let _ = Command::new("/usr/bin/systemctl")
                .args(["--user", "stop", unit])
                .output();
            let out = Command::new("/usr/bin/systemctl")
                .args(["--user", "show", unit, "--property=ControlGroup", "--value"])
                .output()
                .unwrap();
            let group = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !group.is_empty() {
                let path = Path::new("/sys/fs/cgroup")
                    .join(group.trim_start_matches('/'))
                    .join("cgroup.events");
                if let Ok(text) = fs::read_to_string(path) {
                    assert!(
                        text.lines().any(|l| l == "populated 0"),
                        "owned cgroup not empty"
                    );
                }
            }
            let _ = Command::new("/usr/bin/systemctl")
                .args(["--user", "reset-failed", unit])
                .output();
        }
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        self.cleanup();
    }
}
pub fn absolute_tool(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|p| p.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} absent"))
}
fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut size = [0; 4];
    stream.read_exact(&mut size).unwrap();
    let n = u32::from_be_bytes(size) as usize;
    assert!(n <= sluice_model::rpc::MAX_FRAME_BYTES);
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}
pub fn step_finished(gate: &mut Gate, selector: &Value) -> Value {
    let mut terminal = Value::Null;
    gate.wait(Duration::from_secs(240), |g| {
        let status = g.status(selector);
        let step = &status["steps"]["work"];
        if matches!(step["status"].as_str(), Some("succeeded" | "failed")) {
            terminal = step.clone();
            true
        } else {
            false
        }
    });
    assert_eq!(terminal["status"], "succeeded", "{terminal}");
    terminal
}
fn g3(engine: &str) {
    let scratch = Scratch::new();
    let env = match credentials(&scratch.0, engine) {
        Ok(e) => e,
        Err(reason) => {
            println!("g3_{engine}_fresh_submit_live_feedback_cleanup PENDING: {reason}");
            return;
        }
    };
    let cwd = scratch.0.join("work");
    fs::create_dir(&cwd).unwrap();
    repo(&cwd);
    let baseline = git(&cwd, &["rev-parse", "HEAD"]);
    let mut gate = Gate::new(&scratch.0, scratch.0.join("rust-home"), env);
    gate.boot();
    let CommandReply::Project(p)=gate.rpc(json!({"command":"project_create","args":{"name":"g3-acceptance","description":"Labelled scratch G3 engine acceptance","icon":null,"resources":{},"author":"fixture"}}))else{panic!("project reply")};
    let selector = json!({"kind":"id","value":p.project_id});
    let spec = "Labelled G3 scratch fixture. Create original.txt containing original and make exactly one commit with message 'Record the engine fixture turn.' Then wait by running sleep 8, so an addressed live message can arrive. Submit the declared word as blue using the current run's step_submit callback. Finish this turn. Do no other work.";
    let mut inputs = json!({"engine":{"default":engine},"cwd":{"default":cwd},"spec":{"default":spec},"listen":{"default":true}});
    if engine == "codex" {
        inputs["model"] = json!({"default":"sol"});
        inputs["effort"] = json!({"default":"low"});
    }
    gate.rpc(json!({"command":"plan_patch","args":{"project":selector,"rev":1,"ops":[{"op":"replace","path":"","value":{"inputs":{},"outputs":{},"steps":{"work":{"run":"agent.run","in":inputs,"outputs":{"word":"string"}}}}}],"start":true,"dry_run":false,"reason":"scratch engine gate","author":"fixture"}}));
    gate.scheduling();
    let mut run = String::new();
    gate.wait(Duration::from_secs(30), |g| {
        let status = g.status(&selector);
        if let Some(r) = status["steps"]["work"]["run_ids"][0].as_str() {
            run = r.into();
            g.track(r);
            true
        } else {
            false
        }
    });
    gate.wait(Duration::from_secs(120), |_| {
        cwd.join("original.txt").exists()
    });
    gate.rpc(json!({"command":"message_post","args":{"project":selector,"body":"Addressed live fixture input: write live.txt containing received, without another commit. Submit word=blue for this current run and finish.","thread":"step-work","to":"work","needs_reply":false,"reply_to":null,"answer":null,"title":null,"ui":null,"input":null,"data":null,"from":"fixture","run":null,"author":"fixture"}}));
    let first = step_finished(&mut gate, &selector);
    assert_eq!(first["outputs"]["word"], "blue");
    assert_eq!(
        fs::read_to_string(cwd.join("live.txt")).unwrap().trim(),
        "received"
    );
    let session = first["outputs"]["session"].as_str().unwrap().to_string();
    gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"Feedback resume: continue this same session, do not create or commit original.txt again. Submit word=green to the new current RunId using its current step_submit instructions; finish.","reason":"scratch feedback","author":"fixture"}}));
    let next = step_finished(&mut gate, &selector);
    assert_eq!(next["outputs"]["word"], "green");
    assert_eq!(next["outputs"]["session"], session);
    assert_eq!(
        git(&cwd, &["rev-list", "--count", &format!("{baseline}..HEAD")]),
        "1"
    );
    gate.cleanup();
    println!(
        "g3_{engine}_fresh_submit_live_feedback_cleanup PASS before_session={session} after_session={session} original_commits=1"
    );
}
#[test]
#[ignore = "Real Codex through public agent.run and guardian; privately copied credentials"]
fn g3_codex_fresh_submit_live_feedback_cleanup() {
    g3("codex");
}
#[test]
#[ignore = "Real Claude through public agent.run and guardian; privately copied credentials"]
fn g3_claude_fresh_submit_live_feedback_cleanup() {
    g3("claude");
}
#[test]
#[ignore = "Real Devin through public agent.run and guardian; privately copied credentials"]
fn g3_devin_fresh_submit_live_feedback_cleanup() {
    g3("devin");
}
