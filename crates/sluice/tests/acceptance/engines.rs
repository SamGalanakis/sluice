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
    let owner = &PathBuf::from(std::env::var_os("HOME").ok_or("HOME is absent")?);
    match engine {
        "codex" => {
            if !owner.join(".codex/auth.json").is_file() {
                return Err("no Codex auth.json".into());
            }
            // Linked, never copied: Codex rotates the refresh token, so a refresh in a copy
            // would revoke the owner's.
            fs::create_dir_all(home.join(".codex")).map_err(|e| e.to_string())?;
            fs::set_permissions(home.join(".codex"), fs::Permissions::from_mode(0o700)).unwrap();
            std::os::unix::fs::symlink(
                owner.join(".codex/auth.json"),
                home.join(".codex/auth.json"),
            )
            .map_err(|e| e.to_string())?;
            let out = Command::new(workspace().join(".venv/bin/python"))
                .arg(workspace().join("crates/sluice/tests/acceptance/fixtures/private_config.py"))
                .arg(owner.join(".codex/config.toml"))
                .output()
                .map_err(|_| "cannot trim Codex config")?;
            if !out.status.success() {
                return Err("invalid source Codex config".into());
            }
            private_write(&home.join(".codex/config.toml"), &out.stdout);
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
        std::os::unix::fs::symlink(Path::new(env!("CARGO_BIN_EXE_sluice")), bin.join("sluice"))
            .unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        env.extend(BTreeMap::from([
            ("SLUICE_HOME".into(), home.to_string_lossy().into()),
            ("SLUICE_AGENT_SETTLE_S".into(), "0.3".into()),
            ("SLUICE_AGENT_GRACE_MIN".into(), "0.02".into()),
            ("SLUICE_AGENT_WORK_MIN".into(), "0.02".into()),
            ("PATH".into(), path.clone()),
            ("SLUICE_HOST_PATH".into(), path),
            (
                "SLUICE_BIN".into(),
                Path::new(env!("CARGO_BIN_EXE_sluice"))
                    .to_string_lossy()
                    .into(),
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
            Command::new(env!("CARGO_BIN_EXE_sluice"))
                .arg("coordinator")
                .env_remove("CLAUDECODE")
                .env_remove("CLAUDE_CODE_SESSION_ID")
                .env_remove("AI_AGENT")
                .env_remove("SLUICE_FIXTURE")
                .envs(&self.env)
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
        match self.try_rpc(value) {
            Ok(reply) => reply,
            Err(e) => panic!("acceptance RPC failed: {e:?}"),
        }
    }
    pub fn try_rpc(&self, value: Value) -> Result<CommandReply, sluice_model::error::PublicError> {
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
            RpcResult::Ok(r) => Ok(*r),
            RpcResult::Error(e) => Err(e),
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
        let CommandReply::Data(v)=self.rpc(json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null},"all":true}})) else {panic!("status reply")};
        v.into_value()
    }
    #[track_caller]
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
            println!("p5-05 owned unit: {unit}");
            self.units.push(unit);
        }
    }
    pub fn groups_empty(&self) -> bool {
        for unit in &self.units {
            let out = Command::new("/usr/bin/systemctl")
                .args(["--user", "show", unit, "--property=ControlGroup", "--value"])
                .output()
                .unwrap();
            let group = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !group.is_empty() {
                let events = Path::new("/sys/fs/cgroup")
                    .join(group.trim_start_matches('/'))
                    .join("cgroup.events");
                match fs::read_to_string(events) {
                    Ok(text) if text.lines().any(|l| l == "populated 0") => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return false,
                }
            }
        }
        true
    }
    pub fn assert_clean(&self) {
        assert!(
            self.groups_empty(),
            "owned cgroup still populated after settlement"
        );
        if let Ok(runs) = fs::read_dir(self.home.join("runs")) {
            for run in runs.flatten() {
                let socket = run.path().join("tmux.sock");
                if socket.exists() {
                    let out = Command::new(workspace().join("target/private-tmux/bin/tmux"))
                        .arg("-S")
                        .arg(socket)
                        .arg("list-sessions")
                        .output()
                        .unwrap();
                    assert!(!out.status.success(), "private tmux survived settlement");
                }
            }
        }
    }
    pub fn cleanup(&mut self) {
        if self.broker.is_some() {
            let evidence = workspace()
                .join("target/g3-real-evidence")
                .join(self.home.parent().unwrap().file_name().unwrap());
            fs::create_dir_all(&evidence).unwrap();
            fs::set_permissions(&evidence, fs::Permissions::from_mode(0o700)).unwrap();
            if self.home.join("broker.log").is_file() {
                fs::copy(self.home.join("broker.log"), evidence.join("broker.log")).unwrap();
            }
            if let Ok(runs) = fs::read_dir(self.home.join("runs")) {
                for run in runs.flatten() {
                    let dst = evidence.join(run.file_name());
                    fs::create_dir_all(&dst).unwrap();
                    for name in ["native.json", "stderr-tail.log", "app-server.log"] {
                        if run.path().join(name).is_file() {
                            fs::copy(run.path().join(name), dst.join(name)).unwrap();
                        }
                    }
                    if let Ok(Some(cp)) = sluice_agents::supervisor::Checkpoint::read(&run.path()) {
                        println!(
                            "g3 checkpoint run={} session={:?} state={:?} internal_attempt={} live_after={} delivery={:?}",
                            cp.run,
                            cp.session,
                            cp.state,
                            cp.internal_attempt,
                            cp.live_after.0,
                            cp.delivery
                        );
                    }
                    let socket = run.path().join("tmux.sock");
                    if socket.exists() {
                        let out = Command::new(workspace().join("target/private-tmux/bin/tmux"))
                            .arg("-S")
                            .arg(socket)
                            .args(["capture-pane", "-p", "-S", "-100", "-t", "%0"])
                            .output()
                            .unwrap();
                        fs::write(dst.join("pane.txt"), out.stdout).unwrap();
                    }
                }
            }
            println!("g3 evidence: {}", evidence.display());
        }
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
        // An engine's `sluice tool` call while the broker was down auto-starts
        // the home's coordinator unit.
        let mut units = self.units.clone();
        units.push(crate::units::coordinator_unit(&self.home));
        crate::units::stop_units(&units);
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
    step_finished_within(gate, selector, Duration::from_secs(240))
}
pub fn step_finished_within(gate: &mut Gate, selector: &Value, timeout: Duration) -> Value {
    let mut terminal = Value::Null;
    gate.wait(timeout, |g| {
        let status = g.status(selector);
        let step = &status["steps"]["work"];
        if let Some(run) = step["run_ids"][0].as_str() {
            g.track(run);
        }
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
    let marker = gate.home.join("transient-after-commit");
    gate.env.insert("SLUICE_FIXTURE".into(), "1".into());
    gate.env.insert(
        "SLUICE_TEST_AGENT_TRANSIENT_MARKER".into(),
        marker.to_string_lossy().into(),
    );
    gate.env.insert("SLUICE_BACKOFF".into(), "15".into());
    gate.env.insert("SLUICE_AGENT_POLL_S".into(), "0.1".into());
    gate.env
        .insert("SLUICE_AGENT_SETTLE_S".into(), "0.2".into());
    gate.boot();
    let CommandReply::Project(p)=gate.rpc(json!({"command":"project_create","args":{"name":"g3-acceptance","description":"Labelled scratch G3 engine acceptance","icon":null,"resources":{},"author":"fixture"}}))else{panic!("project reply")};
    let selector = json!({"kind":"id","value":p.project_id});
    let spec = "Labelled G3 scratch fixture. On the FIRST task turn, create original.txt containing original and make exactly one commit with message 'Record the engine fixture turn.' Finish that turn with G3_WAITING, WITHOUT submitting word: the harness intentionally injects one transient after the completed turn. On AUTOMATIC CONTINUATION after the transient, continue this same session, never recreate or commit original.txt. Run sleep 8 to allow an addressed live message to arrive, then submit word=blue to the current run and finish. For later feedback retries, follow their new addressed message instead. Do no other work.";
    let mut inputs = json!({"engine":{"default":engine},"cwd":{"default":cwd},"spec":{"default":spec},"listen":{"default":true}});
    if engine == "codex" {
        inputs["model"] = json!({"default":{"type":"normal","model":"sol","effort":"low"}});
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
        cwd.join("original.txt").exists() && git(&cwd, &["rev-parse", "HEAD"]) != baseline
    });
    private_write(&marker, b"one completed-turn transient\n");
    let directory = gate.home.join("runs").join(&run);
    let mut retried = None;
    gate.wait(Duration::from_secs(180), |_| {
        let checkpoint = sluice_agents::supervisor::Checkpoint::read(&directory).unwrap();
        if let Some(cp) = checkpoint
            && cp.internal_attempt == 2
            && cp.state != sluice_agents::supervisor::State::Backoff
        {
            retried = Some(cp);
            true
        } else {
            false
        }
    });
    let retried = retried.unwrap();
    println!(
        "g3_{engine} transient run={run} session={:?} internal_attempt={} state={:?} baseline={:?}",
        retried.session, retried.internal_attempt, retried.state, retried.head_before
    );
    assert_eq!(retried.head_before.as_deref(), Some(baseline.as_str()));
    assert_eq!(retried.run.to_string(), run);
    assert_eq!(
        retried.live_after.0, 0,
        "new messages were required for automatic retry"
    );
    assert_eq!(
        retried
            .delivery
            .entries
            .iter()
            .filter(|e| e.id == sluice_agents::engines::InputId::Task)
            .map(|e| e.tries)
            .sum::<u32>(),
        1
    );
    gate.rpc(json!({"command":"say","args":{"project":selector,"body":"Addressed live fixture input: write live.txt containing received, without another commit. Submit word=blue for this current run and finish.","to":"work"}}));
    let first = step_finished(&mut gate, &selector);
    assert_eq!(first["outputs"]["word"], "blue");
    assert_eq!(
        fs::read_to_string(cwd.join("live.txt")).unwrap().trim(),
        "received"
    );
    let session = first["outputs"]["session"].as_str().unwrap().to_string();
    println!("g3_{engine}_fresh_submit_live_feedback_cleanup fresh session={session} run={run}");
    gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"Feedback resume: continue this same session, do not create or commit original.txt again. Submit word=green to the new current RunId using its current step_submit instructions; finish.","reason":"scratch feedback","author":"fixture"}}));
    let next = step_finished(&mut gate, &selector);
    assert_eq!(next["outputs"]["word"], "green");
    assert_eq!(next["outputs"]["session"], session);
    println!(
        "g3_{engine} feedback session={session} run={} word=green",
        next["run_ids"][0]
    );
    assert_eq!(retried.session.as_deref(), Some(session.as_str()));
    fs::remove_file(marker.with_extension("injected")).unwrap();
    private_write(&marker, b"cancel the next backoff\n");
    let feedback_run = next["run_ids"][0].as_str().unwrap().to_string();
    gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"Cancellation fixture: resume the same session, make no files or commits, and finish this turn with G3_CANCEL_WAITING WITHOUT submitting any word. The harness cancels the injected transient backoff.","reason":"scratch backoff cancellation","author":"fixture"}}));
    let mut cancel_run = String::new();
    gate.wait(Duration::from_secs(30), |g| {
        let status = g.status(&selector);
        if let Some(id) = status["steps"]["work"]["run_ids"][0].as_str()
            && id != feedback_run
        {
            cancel_run = id.into();
            g.track(id);
            true
        } else {
            false
        }
    });
    let cancel_directory = gate.home.join("runs").join(&cancel_run);
    gate.wait(Duration::from_secs(120), |_| {
        sluice_agents::supervisor::Checkpoint::read(&cancel_directory)
            .unwrap()
            .is_some_and(|c| c.state == sluice_agents::supervisor::State::Backoff)
    });
    println!("g3_{engine} cancel observed Backoff run={cancel_run} session={session}");
    gate.rpc(json!({"command":"step_cancel","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"reason":"cancel during fixture backoff","author":"fixture"}}));
    gate.wait(Duration::from_secs(30), |g| {
        g.status(&selector)["steps"]["work"]["status"] == "failed"
    });
    let cancelled = sluice_agents::supervisor::Checkpoint::read(&cancel_directory)
        .unwrap()
        .unwrap();
    assert_eq!(
        cancelled.internal_attempt, 1,
        "cancellation started another retry"
    );
    assert_eq!(cancelled.session.as_deref(), Some(session.as_str()));
    assert_eq!(
        git(&cwd, &["rev-list", "--count", &format!("{baseline}..HEAD")]),
        "1"
    );
    gate.wait(Duration::from_secs(20), |g| g.groups_empty());
    gate.assert_clean();
    gate.cleanup();
    println!(
        "g3_{engine}_fresh_submit_live_feedback_cleanup PASS before_session={session} after_session={session} original_commits=1 internal_attempt=2 cancel_attempt=1"
    );
}
#[test]
#[ignore = "real engine: labelled G3 gate"]
fn g3_codex_fresh_submit_live_feedback_cleanup() {
    g3("codex");
}
#[test]
#[ignore = "real engine: labelled G3 gate"]
fn g3_claude_fresh_submit_live_feedback_cleanup() {
    g3("claude");
}
#[test]
#[ignore = "real engine: labelled G3 gate"]
fn g3_devin_fresh_submit_live_feedback_cleanup() {
    g3("devin");
}

#[test]
fn scratch_coordinator_acceptance_transport_and_scheduler_lease() {
    let scratch = Scratch::new();
    let mut gate = Gate::new(&scratch.0, scratch.0.join("rust-home"), BTreeMap::new());
    gate.boot();
    let CommandReply::Project(project) = gate.rpc(json!({"command":"project_create","args":{"name":"transport","description":"Scratch acceptance transport","resources":{},"icon":null,"author":"fixture"}})) else {panic!("project reply")};
    let selector = json!({"kind":"id","value":project.project_id});
    assert_eq!(gate.status(&selector)["steps"], json!({}));
    gate.scheduling();
    gate.assert_clean();
    gate.cleanup();
}

#[test]
fn public_agent_run_adapter_fixtures_submit_and_feedback_resume() {
    public_adapter_fixture(&["codex", "claude", "devin"], false);
}

#[test]
fn regression_devin_guardian_addressed_input_then_feedback() {
    public_adapter_fixture(&["devin"], true);
}

fn public_adapter_fixture(engines: &[&str], addressed: bool) {
    for &engine in engines {
        let scratch = Scratch::new();
        let owner = scratch.0.join("owner");
        private_write(&owner.join(".codex/config.toml"), b"");
        private_write(&owner.join(".codex/auth.json"), b"fixture credential");
        private_write(&owner.join(".config/devin/config.json"), b"{}");
        fs::create_dir_all(owner.join(".claude")).unwrap();
        fs::create_dir_all(owner.join(".local/share")).unwrap();
        let cwd = scratch.0.join("work");
        fs::create_dir(&cwd).unwrap();
        repo(&cwd);
        let baseline = git(&cwd, &["rev-parse", "HEAD"]);
        let config = scratch.0.join("adapter-fixture.json");
        let mut turns =
            vec![json!({"busy_s":0.5,"busy_ms":500,"compact":false,"reply":"fixture done"})];
        turns.extend((0..16).map(|_| json!({"busy_s":0.5,"busy_ms":500,"reply":"fixture done"})));
        private_write(
            &config,
            &serde_json::to_vec(&json!({"turns":turns,"prompts":scratch.0.join("prompts.jsonl")}))
                .unwrap(),
        );
        let binary = scratch.0.join("adapter-fixture");
        let env_name = if engine == "claude" {
            "SLUICE_FAKE_CLAUDE"
        } else {
            "FAKE_DEVIN"
        };
        let codex_tui = if engine == "codex" {
            "export SLUICE_CODEX_FIXTURE=tui\ncase \"$1\" in -c) exec /usr/bin/sleep 600 ;; esac\n"
        } else {
            ""
        };
        crate::executable::write(
            &binary,
            format!(
                "#!/bin/sh\nset -e\n{codex_tui}export {env_name}='{}'\ncase \"$1 $2\" in '--version '*|'--help '*|'models '*|'debug '*|'app-server --help') ;; *) if [ ! -f '{}' ]; then printf fixture > original.txt; git add original.txt; git commit -qm 'Record the fake engine turn.'; touch '{}'; fi ;; esac\nexec '{}' {engine} \"$@\"\n",
                config.display(),
                scratch.0.join("committed").display(),
                scratch.0.join("committed").display(),
                Path::new(env!("CARGO_BIN_EXE_fixture")).display()
            )
            .as_bytes(),
        );
        let env = BTreeMap::from([
            ("HOME".into(), owner.to_string_lossy().into()),
            (
                "CODEX_HOME".into(),
                owner.join(".codex").to_string_lossy().into(),
            ),
            (
                "CLAUDE_CONFIG_DIR".into(),
                owner.join(".claude").to_string_lossy().into(),
            ),
            (
                "XDG_CONFIG_HOME".into(),
                owner.join(".config").to_string_lossy().into(),
            ),
            (
                "XDG_DATA_HOME".into(),
                owner.join(".local/share").to_string_lossy().into(),
            ),
        ]);
        let mut gate = Gate::new(&scratch.0, scratch.0.join("rust-home"), env);
        gate.env.insert(
            format!("SLUICE_{}_BIN", engine.to_uppercase()),
            binary.to_string_lossy().into(),
        );
        let marker = gate.home.join("transient-after-commit");
        gate.env.insert("SLUICE_FIXTURE".into(), "1".into());
        gate.env.insert(
            "SLUICE_TEST_AGENT_TRANSIENT_MARKER".into(),
            marker.to_string_lossy().into(),
        );
        gate.env.insert("SLUICE_BACKOFF".into(), "15".into());
        gate.env.insert("SLUICE_AGENT_POLL_S".into(), "0.05".into());
        gate.boot();
        let CommandReply::Project(project)=gate.rpc(json!({"command":"project_create","args":{"name":"adapter-gate","description":"Public adapter fixture","resources":{},"icon":null,"author":"fixture"}})) else {panic!("project reply")};
        let selector = json!({"kind":"id","value":project.project_id});
        gate.rpc(json!({"command":"plan_patch","args":{"project":selector,"rev":1,"ops":[{"op":"replace","path":"","value":{"inputs":{},"outputs":{},"steps":{"work":{"run":"agent.run","in":{"engine":{"default":engine},"cwd":{"default":cwd},"spec":{"default":"Complete the labelled fixture turn"},"listen":{"default":true}},"outputs":{"word":"string"}}}}}],"start":true,"dry_run":false,"reason":"public fixture","author":"fixture"}}));
        gate.scheduling();
        let mut prior = String::new();
        let mut first_session = Value::Null;
        for word in ["blue", "green"] {
            let mut run = String::new();
            gate.wait(Duration::from_secs(30), |g| {
                let status = g.status(&selector);
                if let Some(id) = status["steps"]["work"]["run_ids"][0].as_str()
                    && id != prior
                {
                    run = id.into();
                    g.track(id);
                    true
                } else {
                    false
                }
            });
            if word == "blue" {
                gate.wait(Duration::from_secs(30), |_| {
                    scratch.0.join("committed").exists()
                });
                private_write(&marker, b"fake adapter fault after first commit\n");
                let directory = gate.home.join("runs").join(&run);
                gate.wait(Duration::from_secs(30), |_| {
                    sluice_agents::supervisor::Checkpoint::read(&directory)
                        .unwrap()
                        .is_some_and(|c| {
                            c.internal_attempt == 2
                                && c.state != sluice_agents::supervisor::State::Backoff
                        })
                });
                let cp = sluice_agents::supervisor::Checkpoint::read(&directory)
                    .unwrap()
                    .unwrap();
                assert_eq!(cp.head_before.as_deref(), Some(baseline.as_str()));
                assert_eq!(cp.live_after.0, 0);
                assert_eq!(
                    cp.delivery
                        .entries
                        .iter()
                        .filter(|e| e.id == sluice_agents::engines::InputId::Task)
                        .map(|e| e.tries)
                        .sum::<u32>(),
                    1
                );
                if addressed {
                    gate.rpc(json!({"command":"say","args":{"project":selector,"body":"Addressed fixture input","to":"work"}}));
                    gate.wait(Duration::from_secs(30), |_| {
                        sluice_agents::supervisor::Checkpoint::read(&directory)
                            .unwrap()
                            .is_some_and(|c| {
                                c.live_after.0 > 0
                                    && c.delivery.entries.iter().any(|e| {
                                        matches!(
                                            e.id,
                                            sluice_agents::engines::InputId::Message { .. }
                                        ) && e.state
                                            == sluice_agents::delivery::DeliveryState::Acknowledged
                                    })
                            })
                    });
                }
            }
            gate.rpc(json!({"command":"step_submit","args":{"project":project.project_id,"step":"work","run":run,"outputs":{"word":word},"author":"fixture"}}));
            let done = step_finished_within(&mut gate, &selector, Duration::from_secs(45));
            assert_eq!(done["outputs"]["word"], word);
            if word == "blue" {
                first_session = done["outputs"]["session"].clone();
                prior = run;
                gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"Resume the same fixture session","reason":"fixture feedback","author":"fixture"}}));
            } else {
                assert_eq!(done["outputs"]["session"], first_session);
                prior = run;
            }
        }
        fs::remove_file(marker.with_extension("injected")).unwrap();
        private_write(&marker, b"cancel fake backoff\n");
        gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"Cancellation fixture","reason":"fixture cancel backoff","author":"fixture"}}));
        let mut cancel_directory = PathBuf::new();
        gate.wait(Duration::from_secs(30), |g| {
            let status = g.status(&selector);
            if let Some(run) = status["steps"]["work"]["run_ids"][0].as_str()
                && run != prior
            {
                g.track(run);
                cancel_directory = g.home.join("runs").join(run);
                true
            } else {
                false
            }
        });
        gate.wait(Duration::from_secs(30), |_| {
            sluice_agents::supervisor::Checkpoint::read(&cancel_directory)
                .unwrap()
                .is_some_and(|c| c.state == sluice_agents::supervisor::State::Backoff)
        });
        gate.rpc(json!({"command":"step_cancel","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"reason":"cancel fake transient backoff","author":"fixture"}}));
        gate.wait(Duration::from_secs(30), |g| {
            g.status(&selector)["steps"]["work"]["status"] == "failed"
        });
        let cp = sluice_agents::supervisor::Checkpoint::read(&cancel_directory)
            .unwrap()
            .unwrap();
        assert_eq!(cp.internal_attempt, 1);
        assert_eq!(cp.session.as_deref(), first_session.as_str());
        assert_eq!(
            git(&cwd, &["rev-list", "--count", &format!("{baseline}..HEAD")]),
            "1"
        );
        gate.wait(Duration::from_secs(20), |g| g.groups_empty());
        gate.assert_clean();
        gate.cleanup();
    }
}
