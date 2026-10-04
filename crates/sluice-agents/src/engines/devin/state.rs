//! Facts from Devin hooks and private-pane input. The supervisor owns retry and completion.
use super::super::{
    DeliveryOutcome, EngineAdapter, EngineCommand, EngineContext, EngineError, EngineErrorKind,
    EngineLaunch, EngineObservation, EngineProfile, EngineStatus, InputId, SessionMetadata,
};
use super::{
    profile::{self, error},
    protocol::{self, Hook, JournalEntry},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs, io,
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    process::Command,
    time::{Instant, timeout},
};

#[derive(Debug, Clone)]
pub struct DevinOptions {
    pub binary: PathBuf,
    pub hook_binary: PathBuf,
    pub config: PathBuf,
    pub data_home: PathBuf,
    /// Explicit host and callback inputs, also used by private pane launches.
    pub environment: BTreeMap<String, String>,
    pub ready_timeout: Duration,
    pub delivery_timeout: Duration,
}
impl Default for DevinOptions {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        Self {
            binary: std::env::var_os("SLUICE_DEVIN_BIN")
                .unwrap_or_else(|| "devin".into())
                .into(),
            hook_binary: std::env::current_exe().unwrap_or_else(|_| "sluice".into()),
            config: config.join("devin/config.json"),
            data_home,
            environment: super::super::environment::host_environment(),
            ready_timeout: Duration::from_secs(180),
            delivery_timeout: Duration::from_secs(20),
        }
    }
}

struct Pending {
    id: InputId,
    text: String,
    pasted: bool,
    since: Instant,
    last_enter: Option<Instant>,
    dialog: Option<String>,
    last_escape: Option<Instant>,
}

#[derive(Clone, Copy)]
struct Restore {
    began: Instant,
    unbypassed: Option<Instant>,
    entered: Option<Instant>,
}

pub struct Devin {
    options: DevinOptions,
    observation: EngineObservation,
    prepared: Option<EngineContext>,
    resume: Option<String>,
    invocation: String,
    offset: usize,
    pending: Option<Pending>,
    queued: VecDeque<Pending>,
    compaction_events: BTreeSet<String>,
    started: BTreeSet<String>,
    stopped: BTreeSet<String>,
    active_prompt: Option<String>,
    inject_transient: bool,
    exit_requested: bool,
    exit_enter: Option<Instant>,
    restore_mode: Option<Restore>,
}
impl Devin {
    pub fn new(options: DevinOptions) -> Self {
        Self {
            options,
            observation: EngineObservation::default(),
            prepared: None,
            resume: None,
            invocation: String::new(),
            offset: 0,
            pending: None,
            queued: VecDeque::new(),
            compaction_events: BTreeSet::new(),
            started: BTreeSet::new(),
            stopped: BTreeSet::new(),
            active_prompt: None,
            inject_transient: false,
            exit_requested: false,
            exit_enter: None,
            restore_mode: None,
        }
    }
    /// One observation reports a transient without changing delivery or session evidence.
    pub fn inject_transient_once(&mut self) {
        self.inject_transient = true;
    }

    pub fn accept_hook(&mut self, hook: Hook) -> Result<(), EngineError> {
        if let Some(sid) = &hook.session_id {
            if sid.is_empty()
                || self
                    .observation
                    .session_id
                    .as_ref()
                    .is_some_and(|old| old != sid)
            {
                return Err(error(
                    EngineErrorKind::Fatal,
                    "Devin changed session identity during invocation",
                ));
            }
            self.observation.session_id = Some(sid.clone());
        }
        self.observation.progress += 1;
        match hook.hook_event_name.as_str() {
            "UserPromptSubmit" => {
                let key = hook
                    .prompt_id
                    .clone()
                    .unwrap_or_else(|| format!("event-{}", self.observation.progress));
                if self.started.insert(key.clone()) {
                    self.observation.turns_started += 1;
                }
                self.active_prompt = Some(key);
                self.observation.status = EngineStatus::Busy;
                self.observation.error = None;
                if let Some(pending) = &self.pending
                    && pending.pasted
                    && hook
                        .prompt
                        .as_ref()
                        .is_some_and(|prompt| prompt.trim_end() == pending.text.trim_end())
                {
                    self.observation.acknowledged.push(pending.id.clone());
                    self.pending = self.queued.pop_front();
                    if let Some(p) = &mut self.pending {
                        p.since = Instant::now();
                    }
                }
            }
            "Stop" => {
                let key = hook
                    .prompt_id
                    .clone()
                    .or_else(|| self.active_prompt.clone())
                    .unwrap_or_else(|| format!("event-{}", self.observation.progress));
                if !self.stopped.insert(key.clone()) {
                    return Ok(());
                }
                self.observation.turns_completed += 1;
                if self.active_prompt.as_ref().is_none_or(|p| p == &key) {
                    self.active_prompt = None;
                    self.observation.status = EngineStatus::Idle;
                    if let Some(text) = hook.last_assistant_message {
                        self.observation.final_text = text;
                    }
                    self.observation.error = hook.error.filter(|e| !e.is_empty()).map(|e| {
                        let lower = e.to_lowercase();
                        let kind = if [
                            "capacity issues",
                            "rate limit",
                            "rate_limit",
                            "overloaded",
                            "http status 529",
                            "http status 429",
                            "http status 503",
                        ]
                        .iter()
                        .any(|s| lower.contains(s))
                        {
                            EngineErrorKind::Transient
                        } else {
                            EngineErrorKind::Fatal
                        };
                        error(kind, e)
                    });
                    self.log(&self.observation.final_text)?;
                }
            }
            "PostCompaction" => {
                let key = format!("{:?}:{:?}", hook.prompt_id, hook.summary);
                if !self.compaction_events.insert(key) {
                    return Ok(());
                }
                self.observation.compactions += 1;
                self.log("context compacted")?;
            }
            "SessionEnd" => self.observation.status = EngineStatus::Exited,
            "PreToolUse" => {
                let detail = ["command", "path", "file_path", "prompt"]
                    .iter()
                    .find_map(|k| hook.tool_input.get(k).and_then(|v| v.as_str()))
                    .unwrap_or("");
                self.log(&format!(
                    "tool {} {}",
                    hook.tool_name.as_deref().unwrap_or("tool"),
                    detail.chars().take(100).collect::<String>()
                ))?;
            }
            "PostToolUse" => {
                if let Some(e) = hook.tool_response.get("error").and_then(|v| v.as_str()) {
                    self.log(&format!("tool error {e}"))?;
                }
            }
            "SessionStart" => {
                if self.observation.status == EngineStatus::Starting {
                    self.observation.status = EngineStatus::Idle;
                }
            }
            _ => {
                return Err(error(
                    EngineErrorKind::CapabilityMismatch,
                    "unknown Devin lifecycle event",
                ));
            }
        }
        Ok(())
    }

    fn log(&self, text: &str) -> Result<(), EngineError> {
        if let Some(context) = &self.prepared {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(context.run_dir.join("devin.log"))
                .map_err(fatal)?;
            writeln!(
                f,
                "{}",
                text.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(160)
                    .collect::<String>()
            )
            .map_err(fatal)?;
        }
        Ok(())
    }
    fn read_hooks(&mut self) -> Result<(), EngineError> {
        let Some(context) = &self.prepared else {
            return Ok(());
        };
        use std::io::{Read, Seek, SeekFrom};
        let mut file = fs::File::open(context.run_dir.join("devin-hooks.jsonl")).map_err(fatal)?;
        if file.metadata().map_err(fatal)?.len() < self.offset as u64 {
            return Err(fatal("Devin hook journal truncated"));
        }
        file.seek(SeekFrom::Start(self.offset as u64))
            .map_err(fatal)?;
        let mut bytes = Vec::new();
        file.take(protocol::MAX_HOOK_BYTES as u64 + 4096)
            .read_to_end(&mut bytes)
            .map_err(fatal)?;
        let mut consumed = 0;
        while let Some(n) = bytes[consumed..].iter().position(|b| *b == b'\n') {
            let end = consumed + n;
            if n > protocol::MAX_HOOK_BYTES {
                return Err(fatal("oversized journal event"));
            }
            let entry: JournalEntry =
                serde_json::from_slice(&bytes[consumed..end]).map_err(fatal)?;
            consumed = end + 1;
            self.offset += n + 1;
            if entry.invocation == self.invocation {
                self.accept_hook(entry.hook)?;
            }
        }
        if bytes.len() - consumed > protocol::MAX_HOOK_BYTES {
            return Err(fatal("oversized partial journal event"));
        }
        Ok(())
    }
    fn read_export(&mut self) -> Result<(), EngineError> {
        let Some(context) = &self.prepared else {
            return Ok(());
        };
        let path = context.run_dir.join("devin.json");
        let Ok(bytes) = fs::read(path) else {
            return Ok(());
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Ok(());
        };
        if let Some(sid) = value
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            if self
                .observation
                .session_id
                .as_ref()
                .is_some_and(|s| s != sid)
            {
                return Err(error(
                    EngineErrorKind::Fatal,
                    "Devin export session differs from hooks",
                ));
            }
            self.observation.session_id = Some(sid.into());
        }
        if self.observation.final_text.is_empty()
            && let Some(steps) = value.get("steps").and_then(|v| v.as_array())
        {
            for step in steps.iter().rev() {
                if matches!(
                    step.get("source").and_then(|v| v.as_str()),
                    Some("agent" | "assistant")
                ) && let Some(msg) = step.get("message").filter(|v| !v.is_null())
                {
                    self.observation.final_text = msg
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| msg.to_string());
                    break;
                }
            }
        }

        Ok(())
    }
    async fn tmux(&self, context: &EngineContext, args: &[&str]) -> Result<String, EngineError> {
        let binary = context.tmux_binary.as_ref().ok_or_else(|| {
            error(
                EngineErrorKind::CapabilityMismatch,
                "Devin needs the supervisor's approved private tmux",
            )
        })?;
        let mut command = Command::new(binary);
        command
            .current_dir(&context.run_dir)
            .args(["-S", "tmux.sock", "-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        output(command).await
    }
    async fn capture(&self, context: &EngineContext) -> Result<String, EngineError> {
        self.tmux(context, &["capture-pane", "-p", "-t", "%0"])
            .await
    }
    async fn restore_permission_mode(
        &mut self,
        context: &EngineContext,
    ) -> Result<bool, EngineError> {
        let Some(mut restore) = self.restore_mode else {
            return Ok(true);
        };
        let pane = self.capture(context).await?;
        let mode = protocol::permission_mode(&pane);
        if mode == protocol::PermissionMode::Bypass {
            self.restore_mode = None;
            return Ok(true);
        }
        if restore.began.elapsed() >= self.options.ready_timeout {
            return Err(error(
                EngineErrorKind::CapabilityMismatch,
                "Devin resumed permission mode could not be verified as bypass",
            ));
        }
        // The release restores the session's mode after drawing the composer, so only a
        // settled non-bypass verdict sends /bypass, at most once per invocation.
        restore.unbypassed = (mode == protocol::PermissionMode::NotBypass)
            .then(|| restore.unbypassed.unwrap_or_else(Instant::now));
        if let Some(entered) = restore.entered {
            if entered.elapsed() >= Duration::from_secs(1)
                && protocol::draft_visible(&pane, "/bypass")
            {
                restore.entered = Some(Instant::now());
                self.restore_mode = Some(restore);
                self.tmux(context, &["send-keys", "-t", "%0", "Enter"])
                    .await?;
            }
        } else if restore
            .unbypassed
            .is_some_and(|t| t.elapsed() >= Duration::from_secs(1))
        {
            restore.entered = Some(Instant::now());
            self.restore_mode = Some(restore);
            self.tmux(context, &["send-keys", "-t", "%0", "C-a", "C-k"])
                .await?;
            self.tmux(context, &["send-keys", "-t", "%0", "-l", "/bypass"])
                .await?;
            self.tmux(context, &["send-keys", "-t", "%0", "Enter"])
                .await?;
        }
        self.restore_mode = Some(restore);
        Ok(false)
    }
    async fn pane_alive(&self, context: &EngineContext) -> Result<bool, EngineError> {
        let panes = match self
            .tmux(
                context,
                &["list-panes", "-a", "-F", "#{pane_id} #{pane_dead}"],
            )
            .await
        {
            Ok(panes) => panes,
            Err(error)
                if error.message.contains("no current target")
                    || error.message.contains("no server running")
                    || error.message.contains("no sessions") =>
            {
                String::new()
            }
            Err(error) => return Err(error),
        };
        Ok(panes.lines().any(|line| line.trim() == "%0 0"))
    }
    /// Hooks are journaled before Devin exits, so ones written during the pane probe or a
    /// failed client command still belong to this observation.
    fn exited(&mut self) -> Result<EngineObservation, EngineError> {
        self.read_hooks()?;
        self.observation.status = EngineStatus::Exited;
        self.read_export()?;
        Ok(self.observation.clone())
    }
    async fn drive(&mut self, context: &EngineContext) -> Result<EngineObservation, EngineError> {
        if !self.restore_permission_mode(context).await? {
            let mut observation = self.observation.clone();
            observation.status = EngineStatus::Starting;
            return Ok(observation);
        }
        if self.observation.status == EngineStatus::Starting
            && !self.exit_requested
            && context.tmux_binary.is_some()
            && protocol::composer_ready(&self.capture(context).await?)
        {
            self.observation.status = EngineStatus::Idle;
        }
        self.advance_delivery(context).await?;
        if self.exit_requested
            && self.observation.status != EngineStatus::Exited
            && self
                .exit_enter
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(200))
        {
            let pane = self.capture(context).await?;
            if protocol::draft_visible(&pane, "/exit") {
                self.tmux(context, &["send-keys", "-t", "%0", "Enter"])
                    .await?;
                self.exit_enter = Some(Instant::now());
            }
        }
        self.read_export()?;
        let mut observation = self.observation.clone();
        if self.inject_transient {
            self.inject_transient = false;
            observation.error = Some(error(
                EngineErrorKind::Transient,
                "injected Devin transient",
            ));
        }
        Ok(observation)
    }
    async fn advance_delivery(&mut self, context: &EngineContext) -> Result<(), EngineError> {
        let Some(pending) = &self.pending else {
            return Ok(());
        };
        if self.observation.error.is_some() {
            return Ok(());
        }
        let pasted = pending.pasted;
        let expired = pending.since.elapsed()
            >= if pasted {
                self.options.delivery_timeout
            } else {
                self.options.ready_timeout
            };
        if expired {
            if pasted {
                self.observation.error = Some(error(
                    EngineErrorKind::UnknownAcceptance,
                    "Devin prompt acceptance unknown; reconcile before replay",
                ));
            } else {
                self.observation.not_accepted.push(pending.id.clone());
                self.pending = self.queued.pop_front();
                if let Some(p) = &mut self.pending {
                    p.since = Instant::now();
                }
            }
            return Ok(());
        }
        let pane = self.capture(context).await?;
        if !pasted {
            if !protocol::composer_ready(&pane) {
                if ["Select a menu item", "Select model", "Search sessions"]
                    .iter()
                    .any(|m| pane.contains(m))
                {
                    let pending = self.pending.as_mut().unwrap();
                    let confirmed = pending.dialog.as_ref() == Some(&pane);
                    pending.dialog = Some(pane);
                    if confirmed
                        && pending
                            .last_escape
                            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
                    {
                        self.tmux(context, &["send-keys", "-t", "%0", "Escape"])
                            .await?;
                        self.pending.as_mut().unwrap().last_escape = Some(Instant::now());
                    }
                }
                return Ok(());
            }
            self.tmux(context, &["send-keys", "-t", "%0", "C-a", "C-k"])
                .await?;
            let pending = self.pending.as_ref().unwrap();
            protocol::private_write(
                &context.run_dir.join("devin-input"),
                pending.text.replace('\n', "\r").as_bytes(),
            )
            .map_err(fatal)?;
            self.tmux(
                context,
                &["load-buffer", "-b", "sluice-devin", "devin-input"],
            )
            .await?;
            // Record uncertainty before the external paste, including cancellation of its future.
            let pending = self.pending.as_mut().unwrap();
            pending.pasted = true;
            pending.since = Instant::now();
            self.tmux(
                context,
                &["paste-buffer", "-p", "-d", "-b", "sluice-devin", "-t", "%0"],
            )
            .await?;
        } else {
            let pending = self.pending.as_ref().unwrap();
            let draft = protocol::draft_visible(&pane, &protocol::needle(&pending.text))
                || protocol::composer_region(&pane).contains("[Pasted text #");
            let queued =
                pane.to_lowercase().contains("queued") && pane.to_lowercase().contains("send now");
            if (draft || queued)
                && pending
                    .last_enter
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
            {
                self.tmux(context, &["send-keys", "-t", "%0", "Enter"])
                    .await?;
                self.pending.as_mut().unwrap().last_enter = Some(Instant::now());
            }
        }
        Ok(())
    }
}
impl Default for Devin {
    fn default() -> Self {
        Self::new(DevinOptions::default())
    }
}
fn fatal(e: impl std::fmt::Display) -> EngineError {
    error(EngineErrorKind::Fatal, e.to_string())
}
async fn output(mut command: Command) -> Result<String, EngineError> {
    command.kill_on_drop(true).stdin(Stdio::null());
    let out = timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(fatal)?
        .map_err(fatal)?;
    if !out.status.success() {
        return Err(fatal(format!(
            "Devin probe/client exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    String::from_utf8(out.stdout).map_err(fatal)
}

impl EngineAdapter for Devin {
    fn on_hook(
        &mut self,
        event: super::super::HookEvent,
    ) -> Result<super::super::HookReply, EngineError> {
        let bytes = serde_json::to_vec(&event.payload).map_err(fatal)?;
        let hook = protocol::decode_hook(&bytes, &event.event).map_err(fatal)?;
        let context = self
            .prepared
            .as_ref()
            .ok_or_else(|| fatal("Devin hook before prepare"))?;
        protocol::append_hook(
            &context.run_dir.join("devin-hooks.jsonl"),
            &self.invocation,
            hook,
        )
        .map_err(fatal)?;
        self.read_hooks()?;
        Ok(super::super::HookReply {
            stdout: None,
            exit_code: 0,
        })
    }

    fn profile(&self) -> EngineProfile {
        profile::profile()
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        let alias = PathBuf::from(format!("{session}.session"));
        let session = if alias.is_file() {
            fs::read_to_string(alias).map_err(fatal)?.trim().to_string()
        } else {
            session.into()
        };
        if session.is_empty() {
            return Ok(None);
        }
        let db = self.options.data_home.join("devin/cli/sessions.db");
        if !db.exists() {
            return Ok(None);
        }
        let hex: String = session.bytes().map(|b| format!("{b:02x}")).collect();
        let query = format!(
            "SELECT (SELECT user_version FROM pragma_user_version) AS version, (SELECT group_concat(name || ':' || type, ',') FROM pragma_table_info('sessions')) AS columns, id, working_directory FROM (SELECT 1) LEFT JOIN sessions ON id=CAST(X'{hex}' AS TEXT) LIMIT 2;"
        );
        let mut command = Command::new("/usr/bin/sqlite3");
        command.args(["-readonly", "-json"]).arg(db).arg(query);
        let raw = output(command).await?;
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let rows: Vec<serde_json::Value> = serde_json::from_str(&raw).map_err(fatal)?;
        if rows.len() != 1 {
            return Err(fatal("ambiguous Devin session metadata"));
        }
        let row = &rows[0];
        let columns = row["columns"].as_str().unwrap_or("");
        if row["version"] != 0
            || !["id:TEXT", "working_directory:TEXT"]
                .iter()
                .all(|c| columns.split(',').any(|s| s == *c))
        {
            return Err(error(
                EngineErrorKind::CapabilityMismatch,
                "unsupported Devin sessions.db schema; expected user_version 0 and TEXT id/working_directory",
            ));
        }
        if row["id"].is_null() {
            return Ok(None);
        }
        let cwd = row["working_directory"]
            .as_str()
            .ok_or_else(|| fatal("missing Devin session cwd"))?;
        Ok(Some(SessionMetadata {
            id: session,
            cwd: cwd.into(),
        }))
    }
    async fn prepare(
        &mut self,
        context: &EngineContext,
        session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        self.profile()
            .validate_selection(context.model.as_deref(), context.effort.as_deref())?;
        let mut version = Command::new(&self.options.binary);
        version
            .arg("--version")
            .env_clear()
            .envs(&self.options.environment);
        let version = output(version).await?;
        let mut help = Command::new(&self.options.binary);
        help.arg("--help")
            .env_clear()
            .envs(&self.options.environment);
        profile::validate_cli(&version, &output(help).await?)?;
        self.resume = match session {
            Some(session) => {
                let meta = self.session(session).await?.ok_or_else(|| {
                    error(
                        EngineErrorKind::MissingSession,
                        "Devin session does not exist",
                    )
                })?;
                if fs::canonicalize(&meta.cwd).map_err(fatal)?
                    != fs::canonicalize(&context.cwd).map_err(fatal)?
                {
                    return Err(error(
                        EngineErrorKind::CapabilityMismatch,
                        "Devin resume cwd does not match session",
                    ));
                }
                Some(meta.id)
            }
            None => None,
        };
        sluice_process::host::guard_scratch_home(&context.run_dir).map_err(fatal)?;
        fs::create_dir_all(&context.run_dir).map_err(fatal)?;
        self.invocation = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(fatal)?
                .as_nanos()
        );
        self.observation = EngineObservation {
            session_id: self.resume.clone(),
            ..EngineObservation::default()
        };
        self.offset = 0;
        self.pending = None;
        self.queued.clear();
        self.exit_requested = false;
        self.exit_enter = None;
        self.compaction_events.clear();
        self.started.clear();
        self.stopped.clear();
        self.active_prompt = None;
        self.prepared = Some(context.clone());
        self.restore_mode = session
            .filter(|_| context.tmux_binary.is_some())
            .map(|_| Restore {
                began: Instant::now(),
                unbypassed: None,
                entered: None,
            });
        let journal = context.run_dir.join("devin-hooks.jsonl");
        protocol::private_write(&journal, b"").map_err(fatal)?;
        protocol::private_write(&context.run_dir.join("devin.log"), b"").map_err(fatal)?;
        match fs::remove_file(context.run_dir.join("devin.json")) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(fatal(e)),
        }
        let raw = match fs::read_to_string(&self.options.config) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => "{}".into(),
            Err(e) => return Err(fatal(e)),
        };
        let model = self.profile().models[context.model.as_deref().unwrap_or("swe-2-high")].clone();
        let cfg = protocol::config(
            &raw,
            &model,
            &self.options.hook_binary,
            &journal,
            &self.invocation,
        )
        .map_err(fatal)?;
        let config = context.run_dir.join("devin-config.json");
        protocol::private_write(&config, &serde_json::to_vec_pretty(&cfg).map_err(fatal)?)
            .map_err(fatal)?;
        let mut argv: Vec<String> = vec!["/usr/bin/env".into()];
        for key in [
            "CLAUDECODE",
            "TMUX",
            "TMUX_PANE",
            "PYTHONPATH",
            "VIRTUAL_ENV",
        ] {
            argv.extend(["-u".into(), key.into()]);
        }
        argv.push(self.options.binary.to_string_lossy().into_owned());
        argv.extend([
            "--config".into(),
            config.to_string_lossy().into_owned(),
            "--export".into(),
            context
                .run_dir
                .join("devin.json")
                .to_string_lossy()
                .into_owned(),
            "--model".into(),
            model,
            "--permission-mode".into(),
            "dangerous".into(),
            "--respect-workspace-trust".into(),
            "false".into(),
        ]);
        if let Some(sid) = &self.resume {
            argv.extend(["--resume".into(), sid.clone()]);
        }
        let mut env = self.options.environment.clone();
        env.insert(
            "SLUICE_RUN_DIR".into(),
            context.run_dir.to_string_lossy().into_owned(),
        );
        if let Ok(run) = std::env::var("SLUICE_RUN_ID") {
            env.insert("SLUICE_RUN_ID".into(), run);
        }
        if let Ok(path) = std::env::var("SLUICE_HOST_PATH") {
            env.insert("PATH".into(), path);
        }
        env.extend(BTreeMap::from([
            ("DEVIN_PERMISSION_MODE".into(), "dangerous".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_EDITOR".into(), "true".into()),
            ("GIT_MERGE_AUTOEDIT".into(), "no".into()),
        ]));
        Ok(Some(EngineLaunch { argv, env }))
    }
    async fn execute(
        &mut self,
        context: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        if self
            .prepared
            .as_ref()
            .is_none_or(|c| c.run_dir != context.run_dir || c.cwd != context.cwd)
        {
            return Err(fatal("Devin command outside prepared invocation"));
        }
        match command {
            EngineCommand::StartFresh if self.resume.is_none() => Ok(DeliveryOutcome::Acknowledged),
            EngineCommand::Resume { session } if self.resume.as_ref() == Some(&session) => {
                Ok(DeliveryOutcome::Acknowledged)
            }
            EngineCommand::StartFresh | EngineCommand::Resume { .. } => Err(error(
                EngineErrorKind::MissingSession,
                "Devin launch does not match start/resume command",
            )),
            EngineCommand::DeliverText { id, text } | EngineCommand::Steer { id, text } => {
                self.read_hooks()?;
                if self.observation.acknowledged.contains(&id) {
                    return Ok(DeliveryOutcome::Acknowledged);
                }
                if self.pending.as_ref().is_some_and(|p| p.id == id)
                    || self.queued.iter().any(|p| p.id == id)
                {
                    return Ok(DeliveryOutcome::Pending);
                }
                if self.queued.len() >= 16384 || text.len() > protocol::MAX_HOOK_BYTES {
                    return Err(fatal("Devin input queue bound exceeded"));
                }
                self.observation.not_accepted.retain(|old| old != &id);
                let pending = Pending {
                    id,
                    text: protocol::delivered_text(&text),
                    pasted: false,
                    since: Instant::now(),
                    last_enter: None,
                    dialog: None,
                    last_escape: None,
                };
                if self.pending.is_some() {
                    self.queued.push_back(pending);
                } else {
                    self.pending = Some(pending);
                }
                Ok(DeliveryOutcome::Pending)
            }
            EngineCommand::RequestExit => {
                self.exit_requested = true;
                self.exit_enter = Some(Instant::now());
                self.pending = None;
                self.queued.clear();
                self.tmux(context, &["send-keys", "-t", "%0", "C-a", "C-k"])
                    .await?;
                self.tmux(context, &["send-keys", "-t", "%0", "-l", "/exit"])
                    .await?;
                self.tmux(context, &["send-keys", "-t", "%0", "Enter"])
                    .await?;
                Ok(DeliveryOutcome::Pending)
            }
        }
    }
    async fn observe(&mut self, context: &EngineContext) -> Result<EngineObservation, EngineError> {
        self.read_hooks()?;
        if context.tmux_binary.is_some() && !self.pane_alive(context).await? {
            return self.exited();
        }
        match self.drive(context).await {
            // The pane can exit between the probe and a later client command.
            Err(_) if context.tmux_binary.is_some() && !self.pane_alive(context).await? => {
                self.exited()
            }
            result => result,
        }
    }
    async fn close(&mut self) -> io::Result<()> {
        self.read_hooks().map_err(io::Error::other)?;
        self.read_export().map_err(io::Error::other)?;
        if let Some(context) = &self.prepared {
            let export = context.run_dir.join("devin.json");
            if export.exists() {
                protocol::private_write(
                    &context.run_dir.join("devin.log.json"),
                    &fs::read(export)?,
                )?;
            }
            protocol::private_write(
                &context.run_dir.join("devin.log.final"),
                self.observation.final_text.as_bytes(),
            )?;
            protocol::private_write(
                &context.run_dir.join("devin.log.session"),
                format!("{}\n", self.observation.session_id.as_deref().unwrap_or("")).as_bytes(),
            )?;
        }
        Ok(())
    }
}
