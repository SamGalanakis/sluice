use super::super::{
    DeliveryOutcome, EngineAdapter, EngineCommand, EngineContext, EngineError, EngineErrorKind,
    EngineLaunch, EngineObservation, EngineProfile, EngineStatus, InputId, SessionMetadata,
};
use super::{
    profile::{self, error},
    protocol::{Rpc, rpc_error},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    time::{Instant, sleep},
};

#[derive(Debug, Clone)]
pub struct CodexOptions {
    pub binary: PathBuf,
    pub source_home: PathBuf,
    pub sluice_home: PathBuf,
    pub search: bool,
    pub request_timeout: Duration,
}
impl CodexOptions {
    pub fn new(binary: PathBuf, source_home: PathBuf, sluice_home: PathBuf) -> Self {
        Self {
            binary,
            source_home,
            sluice_home,
            search: false,
            request_timeout: Duration::from_secs(30),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct SavedSession {
    home: PathBuf,
    cwd: PathBuf,
}

pub struct Codex {
    options: CodexOptions,
    server: Option<Child>,
    rpc: Option<Rpc>,
    socket_dir: Option<PathBuf>,
    private_home: Option<PathBuf>,
    observation: EngineObservation,
    turn: Option<String>,
    started: BTreeSet<String>,
    completed: BTreeSet<String>,
    historical: BTreeSet<String>,
    compacted: BTreeSet<String>,
    delivery: BTreeMap<InputId, DeliveryOutcome>,
    subscribed: bool,
    tui: bool,
}
impl Codex {
    pub fn new(options: CodexOptions) -> Self {
        Self {
            options,
            server: None,
            rpc: None,
            socket_dir: None,
            private_home: None,
            observation: EngineObservation::default(),
            turn: None,
            started: BTreeSet::new(),
            completed: BTreeSet::new(),
            historical: BTreeSet::new(),
            compacted: BTreeSet::new(),
            delivery: BTreeMap::new(),
            subscribed: false,
            tui: false,
        }
    }
    pub fn private_home(&self) -> Option<&Path> {
        self.private_home.as_deref()
    }
    pub fn server_pid(&self) -> Option<u32> {
        self.server.as_ref().and_then(Child::id)
    }
    /// A labelled acceptance test can insert a known transient after a completed turn.
    pub fn inject_transient(&mut self) {
        self.observation.error = Some(error(
            EngineErrorKind::Transient,
            "g3_codex injected internal transient",
        ));
    }
    fn registry(&self, session: &str) -> Result<PathBuf, EngineError> {
        validate_session(session)?;
        Ok(self
            .options
            .sluice_home
            .join("codex-native-sessions")
            .join(format!("{session}.json")))
    }
    fn saved(&self, session: &str) -> Result<Option<SavedSession>, EngineError> {
        let path = self.registry(session)?;
        if !path.exists() {
            return Ok(None);
        }
        serde_json::from_slice(&fs::read(path).map_err(local)?)
            .map(Some)
            .map_err(|_| error(EngineErrorKind::Fatal, "invalid Codex session mapping"))
    }
    fn rollout(&self, session: &str) -> Result<Option<(PathBuf, PathBuf)>, EngineError> {
        validate_session(session)?;
        let root = self.options.source_home.join("sessions");
        let mut dirs = vec![root];
        while let Some(dir) = dirs.pop() {
            if !dir.exists() {
                continue;
            }
            for entry in fs::read_dir(dir).map_err(local)? {
                let entry = entry.map_err(local)?;
                let kind = entry.file_type().map_err(local)?;
                if kind.is_dir() {
                    dirs.push(entry.path());
                } else if kind.is_file()
                    && entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with(&format!("-{session}.jsonl"))
                {
                    let text = fs::read_to_string(entry.path()).map_err(local)?;
                    for line in text.lines() {
                        let Ok(value) = serde_json::from_str::<Value>(line) else {
                            continue;
                        };
                        if value["type"] == "session_meta"
                            && let Some(cwd) = value["payload"]["cwd"].as_str()
                        {
                            return Ok(Some((entry.path(), PathBuf::from(cwd))));
                        }
                    }
                }
            }
        }
        Ok(None)
    }
    fn save_session(&self, context: &EngineContext) -> Result<(), EngineError> {
        let session = self
            .observation
            .session_id
            .as_deref()
            .ok_or_else(|| error(EngineErrorKind::Fatal, "Codex returned no session"))?;
        let home = self
            .private_home
            .clone()
            .ok_or_else(|| error(EngineErrorKind::Fatal, "Codex has no private home"))?;
        self.save_session_mapping(session, &home, &context.cwd)
    }
    /// Persist the native mapping, including when importing an interrupted session.
    pub fn save_session_mapping(
        &self,
        session: &str,
        home: &Path,
        cwd: &Path,
    ) -> Result<(), EngineError> {
        let path = self.registry(session)?;
        let parent = path
            .parent()
            .ok_or_else(|| local("invalid mapping directory"))?;
        private_dir(parent).map_err(local)?;
        let bytes = serde_json::to_vec(&SavedSession {
            home: home.into(),
            cwd: cwd.into(),
        })
        .map_err(local)?;
        atomic_private(&path, &bytes).map_err(local)
    }
    /// Return the private home recorded in the native session map.
    pub fn session_home(&self, session: &str) -> Result<Option<PathBuf>, EngineError> {
        Ok(self.saved(session)?.map(|saved| saved.home))
    }
    fn rpc(&mut self) -> Result<&mut Rpc, EngineError> {
        self.rpc
            .as_mut()
            .ok_or_else(|| error(EngineErrorKind::Fatal, "Codex adapter is not prepared"))
    }
    fn events(&mut self) -> Result<(), EngineError> {
        let events = self.rpc()?.drain()?;
        for event in events {
            self.event(&event)?;
        }
        Ok(())
    }
    fn count_start(&mut self, turn: &str) {
        if self.started.insert(turn.into()) {
            self.observation.turns_started += 1;
        }
    }
    fn event(&mut self, event: &Value) -> Result<(), EngineError> {
        let params = &event["params"];
        if let Some(thread) = params["threadId"].as_str()
            && self.observation.session_id.as_deref() != Some(thread)
        {
            return Ok(());
        }
        let turn_id = params["turn"]["id"]
            .as_str()
            .or_else(|| params["turnId"].as_str());
        if turn_id.is_some_and(|id| self.historical.contains(id)) {
            return Ok(());
        }
        // Server requests are never silently accepted. Interactive questions cannot decide parent success.
        if event.get("id").is_some() && event.get("method").is_some() {
            self.observation.status = EngineStatus::Blocked;
            self.observation.error = Some(error(
                EngineErrorKind::Fatal,
                "Codex requested unsupported interactive input",
            ));
            return Ok(());
        }
        match event["method"].as_str().unwrap_or("") {
            "thread/started" if self.observation.session_id.is_none() => {
                if let Some(id) = params["thread"]["id"].as_str() {
                    validate_session(id)?;
                    self.observation.session_id = Some(id.into());
                }
            }
            "turn/started" => {
                if let Some(id) = params["turn"]["id"].as_str() {
                    self.count_start(id);
                    if !self.completed.contains(id) {
                        self.turn = Some(id.into());
                        self.observation.status = EngineStatus::Busy;
                    }
                }
            }
            "turn/completed" | "turn/failed" => {
                if let Some(id) = params["turn"]["id"].as_str() {
                    self.count_start(id);
                    if self.completed.insert(id.into()) {
                        self.observation.turns_completed += 1;
                    }
                    if self.turn.as_deref().is_none_or(|active| active == id) {
                        self.turn = None;
                        self.observation.status = EngineStatus::Idle;
                        if !params["turn"]["error"].is_null() {
                            self.observation.error = Some(rpc_error(&params["turn"]["error"]));
                        } else if params["turn"]["status"] == "failed" {
                            self.observation.error =
                                Some(error(EngineErrorKind::Fatal, "Codex turn failed"));
                        }
                    }
                }
            }
            "item/completed" => {
                let item = &params["item"];
                match item["type"].as_str() {
                    Some("agentMessage") => {
                        if let Some(text) = item["text"].as_str() {
                            self.observation.final_text = text.into();
                        }
                    }
                    Some("contextCompaction") => {
                        let id = item["id"].as_str().map(String::from).unwrap_or_else(|| {
                            format!(
                                "{}-{}",
                                self.observation.turns_started, self.observation.compactions
                            )
                        });
                        if self.compacted.insert(id) {
                            self.observation.compactions += 1;
                        }
                    }
                    _ => {}
                }
            }
            "error" => {
                self.observation.error = Some(rpc_error(params.get("error").unwrap_or(params)));
            }
            _ => {}
        }
        self.observation.progress += 1;
        Ok(())
    }
    async fn subscribe(&mut self) -> Result<(), EngineError> {
        if self.subscribed {
            return Ok(());
        }
        let session = self
            .observation
            .session_id
            .clone()
            .ok_or_else(|| local("no Codex session"))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self
                .rpc()?
                .request("thread/resume", json!({"threadId":session}))
                .await
            {
                Ok(_) => {
                    self.subscribed = true;
                    return Ok(());
                }
                Err(e)
                    if [
                        "no rollout found",
                        "is empty",
                        "list_turns is not supported yet",
                    ]
                    .iter()
                    .any(|v| e.message.contains(v))
                        && Instant::now() < deadline =>
                {
                    sleep(Duration::from_millis(100)).await
                }
                Err(e) => return Err(e),
            }
        }
    }
    async fn deliver(
        &mut self,
        context: &EngineContext,
        id: InputId,
        text: String,
    ) -> Result<DeliveryOutcome, EngineError> {
        if let Some(previous) = self.delivery.get(&id) {
            return match previous {
                DeliveryOutcome::Acknowledged => Ok(*previous),
                DeliveryOutcome::Uncertain | DeliveryOutcome::Pending => Err(error(
                    EngineErrorKind::UnknownAcceptance,
                    "Codex input acceptance unknown; do not replay",
                )),
                DeliveryOutcome::NotAccepted => self.send_text(context, id, text).await,
            };
        }
        self.send_text(context, id, text).await
    }
    async fn send_text(
        &mut self,
        context: &EngineContext,
        id: InputId,
        text: String,
    ) -> Result<DeliveryOutcome, EngineError> {
        self.events()?;
        let session = self
            .observation
            .session_id
            .clone()
            .ok_or_else(|| local("Codex session has not started"))?;
        self.delivery.insert(id.clone(), DeliveryOutcome::Uncertain);
        self.observation.error = None;
        let input = json!([{"type":"text","text":text}]);
        if let Some(turn) = self.turn.clone() {
            match self
                .rpc()?
                .request(
                    "turn/steer",
                    json!({"threadId":session,"expectedTurnId":turn,"input":input}),
                )
                .await
            {
                Ok(reply) => {
                    if reply["turnId"].as_str() != Some(turn.as_str()) {
                        return Err(error(
                            EngineErrorKind::UnknownAcceptance,
                            "Codex steer reply has unexpected turn",
                        ));
                    }
                    return Ok(self.acknowledge(id));
                }
                Err(e)
                    if e.kind != EngineErrorKind::UnknownAcceptance
                        && (e.message.contains("no active turn")
                            || e.message.contains("already completed")
                            || e.message.contains("expectedTurnId")) =>
                {
                    // One read refresh, and only explicit idle evidence permits a new turn.
                    let refreshed = self
                        .rpc()?
                        .request(
                            "thread/read",
                            json!({"threadId":session,"includeTurns":false}),
                        )
                        .await?;
                    self.events()?;
                    if refreshed["thread"]["status"]["type"] != "idle" {
                        self.delivery
                            .insert(id.clone(), DeliveryOutcome::NotAccepted);
                        self.observation.not_accepted.push(id);
                        return Ok(DeliveryOutcome::NotAccepted);
                    }
                    self.turn = None;
                    self.observation.status = EngineStatus::Idle;
                }
                Err(e) => return Err(e),
            }
        }
        let model = self.profile().models[context.model.as_deref().unwrap_or("sol")].clone();
        let reply = self.rpc()?.request("turn/start", json!({"threadId":session,"input":input,"model":model,"effort":context.effort.as_deref().unwrap_or("high")})).await?;
        let turn = reply["turn"]["id"].as_str().ok_or_else(|| {
            error(
                EngineErrorKind::UnknownAcceptance,
                "Codex turn/start has no turn id",
            )
        })?;
        self.count_start(turn);
        self.turn = Some(turn.into());
        self.observation.status = EngineStatus::Busy;
        let outcome = self.acknowledge(id);
        // Acceptance is recorded before subscription, which may fail independently.
        if let Err(e) = self.subscribe().await {
            self.observation.error = Some(e);
        }
        if let Err(e) = self.events() {
            self.observation.error = Some(e);
        }
        self.save_session(context)?;
        Ok(outcome)
    }
    fn acknowledge(&mut self, id: InputId) -> DeliveryOutcome {
        self.delivery
            .insert(id.clone(), DeliveryOutcome::Acknowledged);
        self.observation.not_accepted.retain(|v| v != &id);
        if !self.observation.acknowledged.contains(&id) {
            self.observation.acknowledged.push(id);
        }
        self.observation.progress += 1;
        DeliveryOutcome::Acknowledged
    }
    async fn prepare_inner(
        &mut self,
        context: &EngineContext,
        session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        self.profile()
            .validate_selection(context.model.as_deref(), context.effort.as_deref())?;
        if self.server.is_some() {
            return Err(local("Codex already prepared; close before reentry"));
        }
        if !context.cwd.is_dir() {
            return Err(local("Codex cwd is missing"));
        }
        if self
            .options
            .sluice_home
            .starts_with(&self.options.source_home)
        {
            return Err(local("Codex private home overlaps owner home"));
        }
        let probe_deadline = Instant::now() + self.options.request_timeout;
        let version = loop {
            let result = tokio::time::timeout_at(
                probe_deadline,
                Command::new(&self.options.binary)
                    .arg("--version")
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| local("Codex version probe timed out"))?;
            match result {
                // A concurrent fork may temporarily retain a just-written executable descriptor.
                Err(e) if e.raw_os_error() == Some(26) && Instant::now() < probe_deadline => {
                    sleep(Duration::from_millis(10)).await
                }
                result => break result.map_err(local)?,
            }
        };
        profile::check_version(&String::from_utf8_lossy(&version.stdout))?;
        let source = match fs::read_to_string(self.options.source_home.join("config.toml")) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(local(e)),
        };
        let selected = self.profile();
        let config = profile::private_config(
            &source,
            &selected.models[context.model.as_deref().unwrap_or("sol")],
            context.effort.as_deref().unwrap_or("high"),
            self.options.search,
        )?;
        let homes = self.options.sluice_home.join("codex-native-homes");
        private_dir(&homes).map_err(local)?;
        let private = if let Some(session) = session {
            let metadata = self
                .session(session)
                .await?
                .ok_or_else(|| error(EngineErrorKind::MissingSession, "Codex session not found"))?;
            if fs::canonicalize(metadata.cwd).map_err(local)?
                != fs::canonicalize(&context.cwd).map_err(local)?
            {
                return Err(local("Codex session cwd does not match"));
            }
            let target = homes.join(session);
            if let Some(saved) = self.saved(session)? {
                if !saved.home.is_dir() {
                    return Err(error(
                        EngineErrorKind::MissingSession,
                        "Codex session home is missing",
                    ));
                }
                if saved.home != target && !target.exists() {
                    let staging = target.with_extension("clone");
                    if staging.exists() {
                        return Err(local("incomplete Codex home clone requires reconciliation"));
                    }
                    if let Err(e) = copy_tree(&saved.home, &staging) {
                        let _ = fs::remove_dir_all(&staging);
                        return Err(local(e));
                    }
                    fs::rename(staging, &target).map_err(local)?;
                }
            } else if let Some((rollout, _)) = self.rollout(session)? {
                private_dir(&target).map_err(local)?;
                let relative = rollout
                    .strip_prefix(&self.options.source_home)
                    .map_err(local)?;
                let dest = target.join(relative);
                private_dir(dest.parent().ok_or_else(|| local("invalid rollout"))?)
                    .map_err(local)?;
                atomic_private(&dest, &fs::read(rollout).map_err(local)?).map_err(local)?;
            }
            target
        } else {
            let run = context
                .run_dir
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| local("invalid Codex run directory"))?;
            homes.join(format!("pending-{run}"))
        };
        private_dir(&private).map_err(local)?;
        let auth_source = self.options.source_home.join("auth.json");
        if auth_source.exists() {
            atomic_private(
                &private.join("auth.json"),
                &fs::read(auth_source).map_err(local)?,
            )
            .map_err(local)?;
        }
        atomic_private(&private.join("config.toml"), config.as_bytes()).map_err(local)?;
        self.private_home = Some(private.clone());
        private_dir(&context.run_dir).map_err(local)?;
        let socket_dir = unique_socket_dir().map_err(local)?;
        let socket = socket_dir.join("app.sock");
        self.socket_dir = Some(socket_dir);
        let mut env = BTreeMap::from([
            ("CODEX_HOME".into(), private.to_string_lossy().into_owned()),
            (
                "SLUICE_HOME".into(),
                self.options.sluice_home.to_string_lossy().into_owned(),
            ),
            (
                "SLUICE_RUN_DIR".into(),
                context.run_dir.to_string_lossy().into_owned(),
            ),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_EDITOR".into(), "true".into()),
            ("GIT_MERGE_AUTOEDIT".into(), "no".into()),
        ]);
        if let Ok(path) = std::env::var("SLUICE_HOST_PATH") {
            env.insert("PATH".into(), path);
        }
        let log = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(context.run_dir.join("app-server.log"))
            .map_err(local)?;
        let mut command = Command::new(&self.options.binary);
        command
            .args(["app-server", "--listen"])
            .arg(format!("unix://{}", socket.display()))
            .current_dir(&context.cwd)
            .envs(&env)
            .env_remove("PYTHONPATH")
            .env_remove("VIRTUAL_ENV")
            .env_remove("CLAUDECODE")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().map_err(local)?))
            .stderr(Stdio::from(log))
            .kill_on_drop(true);
        self.server = Some(command.spawn().map_err(local)?);
        let deadline = Instant::now() + self.options.request_timeout;
        loop {
            if self
                .server
                .as_mut()
                .ok_or_else(|| local("no Codex server"))?
                .try_wait()
                .map_err(local)?
                .is_some()
            {
                return Err(local(
                    "Codex app-server exited before ready; inspect app-server.log",
                ));
            }
            if socket.exists() {
                self.rpc = Some(
                    Rpc::connect(
                        &socket,
                        &context.run_dir.join("codex-wire.jsonl"),
                        self.options.request_timeout,
                    )
                    .await?,
                );
                break;
            }
            if Instant::now() >= deadline {
                return Err(local("Codex app-server did not become ready"));
            }
            sleep(Duration::from_millis(20)).await;
        }
        self.rpc()?.request("initialize", json!({"clientInfo":{"name":"sluice","version":"0.1.0"},"capabilities":{"experimentalApi":true}})).await?;
        self.rpc()?.notify("initialized").await?;
        self.tui = context.tmux_binary.is_some();
        Ok(self.tui.then(|| EngineLaunch {
            argv: profile::tui_argv(&self.options.binary, &socket, session),
            env,
        }))
    }
}
impl EngineAdapter for Codex {
    fn profile(&self) -> EngineProfile {
        profile::profile()
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        if let Some(saved) = self.saved(session)? {
            if !saved.home.is_dir() {
                return Err(error(
                    EngineErrorKind::MissingSession,
                    "Codex session home is missing",
                ));
            }
            return Ok(Some(SessionMetadata {
                id: session.into(),
                cwd: saved.cwd,
            }));
        }
        Ok(self.rollout(session)?.map(|(_, cwd)| SessionMetadata {
            id: session.into(),
            cwd,
        }))
    }
    async fn prepare(
        &mut self,
        context: &EngineContext,
        session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        self.observation = EngineObservation::default();
        self.turn = None;
        self.started.clear();
        self.completed.clear();
        self.historical.clear();
        self.compacted.clear();
        self.delivery.clear();
        self.subscribed = false;
        let result = self.prepare_inner(context, session).await;
        if result.is_err() {
            let _ = self.close().await;
        }
        result
    }
    async fn execute(
        &mut self,
        context: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        match command {
            EngineCommand::StartFresh => {
                if self.tui {
                    let deadline = Instant::now() + self.options.request_timeout;
                    loop {
                        self.events()?;
                        if self.observation.session_id.is_some() {
                            break;
                        }
                        if Instant::now() >= deadline {
                            return Err(local("Codex TUI did not start a thread"));
                        }
                        sleep(Duration::from_millis(20)).await;
                    }
                } else {
                    let reply = self.rpc()?.request("thread/start", json!({"cwd":context.cwd,"approvalPolicy":"never","sandbox":"danger-full-access"})).await?;
                    let id = reply["thread"]["id"]
                        .as_str()
                        .ok_or_else(|| local("Codex thread/start has no thread id"))?;
                    validate_session(id)?;
                    self.observation.session_id = Some(id.into());
                    self.subscribed = false;
                }
                self.observation.status = EngineStatus::Idle;
                self.save_session(context)?;
                Ok(DeliveryOutcome::Acknowledged)
            }
            EngineCommand::Resume { session } => {
                validate_session(&session)?;
                let reply = self.rpc()?.request("thread/resume", json!({"threadId":session,"cwd":context.cwd,"approvalPolicy":"never","sandbox":"danger-full-access"})).await?;
                if reply["thread"]["id"].as_str() != Some(session.as_str()) {
                    return Err(local("Codex resumed a different thread"));
                }
                if let Some(cwd) = reply["thread"]["cwd"].as_str()
                    && fs::canonicalize(cwd).map_err(local)?
                        != fs::canonicalize(&context.cwd).map_err(local)?
                {
                    return Err(local("Codex resumed cwd does not match"));
                }
                if let Some(turns) = reply["thread"]["turns"].as_array() {
                    for turn in turns {
                        if let Some(id) = turn["id"].as_str() {
                            self.historical.insert(id.into());
                        }
                    }
                }
                self.observation.session_id = Some(session);
                self.subscribed = true;
                self.observation.status = EngineStatus::Idle;
                self.save_session(context)?;
                Ok(DeliveryOutcome::Acknowledged)
            }
            EngineCommand::DeliverText { id, text } | EngineCommand::Steer { id, text } => {
                self.deliver(context, id, text).await
            }
            EngineCommand::RequestExit => {
                if let Some(turn) = self.turn.clone() {
                    let session = self.observation.session_id.clone();
                    self.rpc()?
                        .request("turn/interrupt", json!({"threadId":session,"turnId":turn}))
                        .await?;
                }
                Ok(DeliveryOutcome::Acknowledged)
            }
        }
    }
    async fn observe(
        &mut self,
        _context: &EngineContext,
    ) -> Result<EngineObservation, EngineError> {
        if let Err(e) = self.events() {
            self.observation.error = Some(e);
            self.observation.status = EngineStatus::Exited;
        }
        if let Some(server) = self.server.as_mut()
            && server.try_wait().map_err(local)?.is_some()
        {
            self.observation.status = EngineStatus::Exited;
        }
        Ok(self.observation.clone())
    }
    async fn close(&mut self) -> io::Result<()> {
        let mut failure = None;
        if let Some(mut rpc) = self.rpc.take()
            && let Err(e) = rpc.close().await
        {
            failure = Some(e);
        }
        if let Some(mut server) = self.server.take() {
            if let Err(e) = server.start_kill() {
                failure.get_or_insert(e);
            }
            if let Err(e) = server.wait().await {
                failure.get_or_insert(e);
            }
        }
        if let Some(path) = self.socket_dir.take()
            && let Err(e) = fs::remove_dir_all(path)
        {
            failure.get_or_insert(e);
        }
        self.observation.status = EngineStatus::Exited;
        match failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
impl Drop for Codex {
    fn drop(&mut self) {
        // kill_on_drop reaps the direct child through Tokio. The guardian reaps its contained descendants.
        self.rpc.take();
        self.server.take();
        if let Some(path) = self.socket_dir.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}
fn local(e: impl std::fmt::Display) -> EngineError {
    error(EngineErrorKind::Fatal, format!("Codex: {e}"))
}
fn validate_session(session: &str) -> Result<(), EngineError> {
    if session.is_empty()
        || session.len() > 128
        || !session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(local("invalid Codex session id"));
    }
    Ok(())
}
fn private_dir(path: &Path) -> io::Result<()> {
    if path.is_symlink() {
        return Err(io::Error::other("private directory is a symlink"));
    }
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
fn atomic_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp = path.with_extension("sluice-new");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    if source.is_symlink() {
        return Err(io::Error::other("imported Codex home is a symlink"));
    }
    private_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if ["config.toml", "auth.json"].contains(&entry.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(io::Error::other(
                "imported Codex session contains a symlink",
            ));
        }
        if kind.is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        } else if kind.is_file() {
            atomic_private(&target.join(entry.file_name()), &fs::read(entry.path())?)?;
        }
    }
    Ok(())
}
fn unique_socket_dir() -> io::Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..100 {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("sluice-codex-{}-{n}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
                return Ok(path);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("cannot allocate Codex socket directory"))
}
