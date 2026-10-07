//! Claude facts and TUI commands. Completion policy belongs to the supervisor.
use super::{
    profile,
    protocol::{self, Hook, Tail, failure, io_error},
};
use crate::engines::*;
use serde_json::{Value, json};
use sluice_model::ids::RunId;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{process::Command, time::Instant};

pub const STABLE_IDLE: Duration = Duration::from_secs(10);
#[derive(Default)]
pub struct ClaudeState {
    observation: EngineObservation,
    open_turn: bool,
    ended: bool,
    idle_since: Option<Duration>,
    idle_progress: u64,
    pending: VecDeque<(InputId, String)>,
    background: Vec<Value>,
    crons: Vec<Value>,
    own_crons: BTreeSet<String>,
    tools: BTreeMap<String, String>,
    agents: BTreeSet<String>,
    wakeup: Option<f64>,
    context_tokens: u64,
    summaries: VecDeque<String>,
    /// `None` is `account::DEFAULT_THRESHOLD`.
    quota_threshold: Option<Duration>,
}
impl ClaudeState {
    /// A limit that resets further away than this fails the run as `QuotaExhausted`.
    pub fn set_quota_threshold(&mut self, threshold: Duration) {
        self.quota_threshold = Some(threshold);
    }
    /// An API error that is an account problem, classified from its category (the transcript
    /// entry's `error`, the `StopFailure` hook's `error`) first:
    /// - `authentication_failed` (`Not logged in · Please run /login`, `Login expired · …`,
    ///   `OAuth token revoked · …`) is an auth failure, except Claude's own `Authentication
    ///   error · This may be a temporary network issue, …`, which stays transient;
    ///   `oauth_org_not_allowed`, `account_on_hold`, `verification_required` and
    ///   `cloud_credential_error` bar the account the same way;
    /// - a rejected plan limit in `quotaLimits` (`rateLimitType`, `resetsAt`) is a usage limit
    ///   with that reset; `billing_error` is one with no reset; a `rate_limit` error whose text
    ///   starts with Claude's usage-limit wording is one, with the legacy epoch if any.
    ///
    /// With no category (or `invalid_request` / `unknown`), text that starts with Claude's
    /// login wording is an auth failure. `None` leaves the error to `classify`.
    fn api_account(
        &self,
        category: Option<&str>,
        limits: &Value,
        message: &str,
    ) -> Option<EngineError> {
        let barred = |problem: &str, advice: &str| {
            Some(
                account::Auth {
                    engine: account::Engine::Claude,
                    problem: format!("{problem} ({})", category.unwrap_or_default()),
                    advice: Some(advice.into()),
                    text: message.into(),
                }
                .error(),
            )
        };
        match category {
            Some("authentication_failed")
                if message
                    .trim_start()
                    .starts_with("Authentication error \u{b7} This may be a temporary") =>
            {
                return Some(failure(EngineErrorKind::Transient, message));
            }
            Some("authentication_failed") => {
                return Some(account::Auth::login(account::Engine::Claude, message).error());
            }
            Some("oauth_org_not_allowed") => {
                return barred(
                    "the organization disabled subscription access for Claude Code",
                    "use an Anthropic API key or ask the org admin to enable access",
                );
            }
            Some("account_on_hold") => {
                return barred(
                    "account on hold",
                    "resolve the hold at https://claude.ai/restricted",
                );
            }
            Some("verification_required") => {
                return barred(
                    "organization verification required",
                    "complete the verification Claude asks for",
                );
            }
            Some("cloud_credential_error") => {
                return barred(
                    "cloud credentials unavailable",
                    "check or refresh the cloud provider's credentials on this host",
                );
            }
            None | Some("invalid_request" | "unknown") if protocol::auth_text(message) => {
                return Some(account::Auth::login(account::Engine::Claude, message).error());
            }
            _ => {}
        }
        let (resets_at, kind) = if limits["status"] == "rejected" {
            (
                limits["resetsAt"].as_u64(),
                limits["rateLimitType"].as_str(),
            )
        } else if category == Some("billing_error") {
            (None, None)
        } else if matches!(category, None | Some("rate_limit")) {
            (protocol::usage_limit(message)?, None)
        } else {
            return None;
        };
        Some(
            account::Limit {
                what: kind.map(|kind| format!("{} reached", limit_name(kind))),
                window: kind.map(str::to_owned),
                resets_at,
                ..account::Limit::new(account::Engine::Claude, account::Cap::Usage, message)
            }
            .classify(
                account::now(),
                self.quota_threshold.unwrap_or(account::DEFAULT_THRESHOLD),
            ),
        )
    }
    pub fn pending(&mut self, id: InputId, text: String) -> bool {
        if self.observation.acknowledged.contains(&id) || self.pending.iter().any(|(i, _)| *i == id)
        {
            return false;
        }
        self.pending.push_back((id, text));
        true
    }
    pub fn summary_lines(&mut self) -> Vec<String> {
        self.summaries.drain(..).collect()
    }
    fn summary(&mut self, text: String, main: bool) {
        if self.summaries.len() == 256 {
            self.summaries.pop_front();
        }
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let line: String = line.chars().take(160).collect();
        let ctx = if self.context_tokens > 0 {
            format!("ctx {}k · ", (self.context_tokens + 500) / 1000)
        } else {
            String::new()
        };
        self.summaries
            .push_back(format!("{}{ctx}{line}", if main { "" } else { "  " }));
    }
    pub fn hook(&mut self, hook: &Hook) -> Result<(), EngineError> {
        if self
            .observation
            .session_id
            .as_ref()
            .is_some_and(|id| *id != hook.session_id)
        {
            return Err(failure(
                EngineErrorKind::Fatal,
                "claude: hook changed session identity",
            ));
        }
        self.observation.session_id = Some(hook.session_id.clone());
        self.observation.progress += 1;
        self.idle_since = None;
        match hook.hook_event_name.as_str() {
            "SessionStart" => {
                if hook.source.as_deref() == Some("compact") {
                    self.observation.compactions += 1;
                }
            }
            "UserPromptSubmit" if hook.agent_id.is_none() => {
                self.observation.turns_started += 1;
                self.open_turn = true;
                self.ended = false;
                self.observation.error = None;
                self.observation.status = EngineStatus::Busy;
                if let Some(prompt) = &hook.prompt {
                    let prompt = prompt.trim_matches(['\r', '\n']);
                    if let Some(at) = self
                        .pending
                        .iter()
                        .position(|(_, text)| text.trim_matches(['\r', '\n']) == prompt)
                    {
                        let (id, _) = self.pending.remove(at).expect("position exists");
                        self.observation.acknowledged.push(id);
                    }
                }
            }
            "Stop" | "StopFailure" if hook.agent_id.is_none() => {
                if !self.ended {
                    self.observation.turns_completed += 1;
                    self.ended = true;
                }
                self.open_turn = false;
                self.observation.status = EngineStatus::Idle;
                if let Some(text) = &hook.last_assistant_message {
                    self.observation.final_text = text.clone();
                }
                self.background = hook.background_tasks.clone();
                self.crons = hook.session_crons.clone();
                if hook.hook_event_name == "StopFailure" {
                    let text = hook
                        .last_assistant_message
                        .as_deref()
                        .or(hook.error_details.as_deref())
                        .unwrap_or_default();
                    match self.api_account(hook.error.as_deref(), &Value::Null, text) {
                        // The transcript's entry for this error, read first, carries its
                        // reset; the hook's text does not.
                        Some(_) if self.observation.error.is_some() => {}
                        Some(limit) => self.observation.error = Some(limit),
                        None => {
                            self.observation.error = Some(classify(&format!(
                                "{} {} {}",
                                hook.error.as_deref().unwrap_or_default(),
                                hook.error_details.as_deref().unwrap_or_default(),
                                hook.last_assistant_message.as_deref().unwrap_or_default()
                            )))
                        }
                    }
                }
            }
            "SubagentStart" => {
                if let Some(id) = &hook.agent_id {
                    self.agents.insert(id.clone());
                }
            }
            "SubagentStop" => {
                if let Some(id) = &hook.agent_id {
                    self.agents.remove(id);
                }
            }
            "SessionEnd" => self.observation.status = EngineStatus::Exited,
            "PreToolUse" | "PostToolUse" | "PostToolUseFailure" => {
                if let Some(tool) = &hook.tool_name {
                    self.summary(format!("tool {tool}"), hook.agent_id.is_none());
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub fn transcript(&mut self, entry: &Value, main: bool) {
        self.observation.progress += 1;
        let main = main && entry.get("isSidechain").and_then(Value::as_bool) != Some(true);
        let kind = entry["type"].as_str().unwrap_or_default();
        let content = &entry["message"]["content"];
        if kind == "assistant" {
            if entry["isApiErrorMessage"].as_bool() == Some(true) {
                let message = text_of(content);
                self.summary(format!("error {message}"), main);
                if main {
                    self.observation.error = Some(
                        self.api_account(entry["error"].as_str(), &entry["quotaLimits"], &message)
                            .unwrap_or_else(|| classify(&message)),
                    );
                }
                return;
            }
            if main {
                let usage = &entry["message"]["usage"];
                let tokens: u64 = [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                ]
                .iter()
                .filter_map(|key| usage[*key].as_u64())
                .sum();
                if tokens > 0 {
                    self.context_tokens = tokens;
                }
            }
            for block in content.as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => {
                        if let Some(text) = block["text"].as_str().filter(|s| !s.trim().is_empty())
                        {
                            self.summary(text.into(), main);
                            if main {
                                self.observation.final_text = text.into();
                            }
                        }
                    }
                    Some("tool_use") => {
                        let name = block["name"].as_str().unwrap_or_default();
                        if main && let Some(id) = block["id"].as_str() {
                            self.tools.insert(id.into(), name.into());
                        }
                        let input = &block["input"];
                        let arg = [
                            "file_path",
                            "path",
                            "notebook_path",
                            "command",
                            "pattern",
                            "url",
                            "query",
                            "description",
                            "prompt",
                        ]
                        .iter()
                        .find_map(|key| input[*key].as_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| input.to_string());
                        let arg: String = arg
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .chars()
                            .take(100)
                            .collect();
                        self.summary(format!("tool {name} {arg}"), main);
                    }
                    _ => {}
                }
            }
        } else if kind == "user" {
            if entry["origin"]["kind"] == "task-notification" {
                self.summary(format!("task notification: {}", text_of(content)), main);
            }
            for block in content.as_array().into_iter().flatten() {
                if block["is_error"].as_bool() == Some(true) {
                    self.summary(format!("tool error {}", text_of(&block["content"])), main);
                }
                if !main {
                    continue;
                }
                let name = block["tool_use_id"]
                    .as_str()
                    .and_then(|id| self.tools.get(id));
                let result = &entry["toolUseResult"];
                match name.map(String::as_str) {
                    Some("ScheduleWakeup") => {
                        if let Some(at) = result["scheduledFor"].as_f64() {
                            self.wakeup = Some(at / 1000.0);
                        }
                    }
                    Some("CronCreate") if result["recurring"].as_bool() == Some(false) => {
                        if let Some(id) = result["id"].as_str() {
                            self.own_crons.insert(id.into());
                        }
                    }
                    _ => {}
                }
            }
        } else if kind == "system" && entry["subtype"] == "scheduled_task_fire" {
            if main {
                self.wakeup = None;
            }
            self.summary(format!("wakeup: {}", text_of(&entry["content"])), main);
        }
    }
    pub fn snapshot(
        &mut self,
        status: Option<&str>,
        composer: bool,
        dead: bool,
        now: Duration,
        epoch: f64,
    ) -> EngineObservation {
        let mut work = vec![];
        if status == Some("shell") {
            work.push("a background shell is running".into());
        }
        for task in &self.background {
            if matches!(
                task["status"].as_str(),
                Some("completed" | "failed" | "stopped" | "killed")
            ) {
                continue;
            }
            if status.is_some() && task["type"] == "shell" {
                continue;
            }
            work.push(format!(
                "background task: {}",
                task["description"]
                    .as_str()
                    .or(task["type"].as_str())
                    .unwrap_or("unknown")
            ));
        }
        work.extend(
            self.agents
                .iter()
                .map(|id| format!("background agent: {id}")),
        );
        if self.wakeup.is_some_and(|at| epoch < at + 120.0) {
            work.push(format!("a wakeup at {:.0}", self.wakeup.unwrap()));
        }
        if self.crons.iter().any(|c| {
            c["id"]
                .as_str()
                .is_some_and(|id| self.own_crons.contains(id))
        }) {
            work.push("a scheduled job".into());
        }
        self.observation.background_work = work;
        self.observation.waiting = self.observation.background_work.first().cloned();
        if dead || self.observation.status == EngineStatus::Exited {
            self.observation.status = EngineStatus::Exited;
            self.idle_since = None;
        } else if status == Some("waiting") || (!composer && status != Some("busy")) {
            self.observation.status = EngineStatus::Blocked;
            self.idle_since = None;
        } else if status == Some("busy") {
            self.observation.status = EngineStatus::Busy;
            self.idle_since = None;
        } else if matches!(status, Some("idle" | "shell")) || self.ended {
            if self.open_turn {
                let verified = status == Some("idle")
                    && composer
                    && self.observation.background_work.is_empty();
                if !verified || self.idle_progress != self.observation.progress {
                    self.idle_since = None;
                }
                self.idle_progress = self.observation.progress;
                if verified {
                    let since = *self.idle_since.get_or_insert(now);
                    if now.saturating_sub(since) >= STABLE_IDLE {
                        self.observation.turns_completed += 1;
                        self.open_turn = false;
                        self.ended = true;
                    }
                }
            }
            self.observation.status = if self.open_turn {
                EngineStatus::Busy
            } else {
                EngineStatus::Idle
            };
        } else {
            self.observation.status = if self.open_turn {
                EngineStatus::Busy
            } else {
                EngineStatus::Starting
            };
            self.idle_since = None;
        }
        self.observation.clone()
    }
}
fn blocked_screen(what: String, advice: String, pane: &str) -> EngineError {
    screen::Screen {
        engine: account::Engine::Claude,
        what,
        advice,
        pane: pane.into(),
    }
    .error()
}
/// Names Claude's `rateLimitType`: `weekly limit`.
fn limit_name(kind: &str) -> &'static str {
    match kind {
        "five_hour" => "5-hour session limit",
        "seven_day_opus" => "weekly Opus limit",
        "seven_day_sonnet" => "weekly Sonnet limit",
        "overage" | "extra_usage" => "usage credit limit",
        k if k.starts_with("seven_day") => "weekly limit",
        _ => "plan limit",
    }
}
fn text_of(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn classify(text: &str) -> EngineError {
    let lower = text.to_lowercase();
    let kind = if lower.contains("no conversation found") || lower.contains("session not found") {
        EngineErrorKind::MissingSession
    } else if [
        "rate_limit",
        "rate limit",
        "429",
        "529",
        "overloaded",
        "usage limit",
        "hit your limit",
        "temporarily unavailable",
        "connection error",
        "timeout",
        "timed out",
        "500",
        "502",
        "503",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        EngineErrorKind::Transient
    } else {
        EngineErrorKind::Fatal
    };
    failure(kind, text.chars().take(2048).collect::<String>())
}

#[derive(Clone, Copy)]
enum PastePhase {
    Ready,
    Draft,
    Submitted,
}
struct Delivery {
    id: InputId,
    text: String,
    phase: PastePhase,
    since: Instant,
    retry_at: Instant,
    retry: Duration,
    covered: bool,
}
pub struct Claude {
    binary: PathBuf,
    home: PathBuf,
    /// Whether Claude is told `CLAUDE_CONFIG_DIR`. Only when the host chose the directory: set to
    /// the default `~/.claude`, Claude would read `~/.claude/.claude.json` instead of the owner's
    /// `~/.claude.json` and open on its first-run setup, so no turn ever starts.
    pass_config_dir: bool,
    hook_binary: PathBuf,
    run: RunId,
    mcp: Option<String>,
    context: Option<EngineContext>,
    pane: Option<String>,
    state: ClaudeState,
    transcript: Option<PathBuf>,
    tails: BTreeMap<PathBuf, Tail>,
    seen: BTreeMap<PathBuf, u64>,
    began: Instant,
    started: bool,
    transient_fault: bool,
    outgoing: VecDeque<Delivery>,
    prepared_session: Option<String>,
    environment: BTreeMap<String, String>,
    /// How long a screen sluice does not recognize may stand unchanged before any turn.
    screen_grace: Duration,
    watch: screen::Watch,
}
impl Claude {
    pub fn new(binary: PathBuf, home: PathBuf, hook_binary: PathBuf, run: RunId) -> Self {
        Self {
            binary,
            home,
            pass_config_dir: true,
            hook_binary,
            run,
            mcp: None,
            context: None,
            pane: None,
            state: ClaudeState::default(),
            transcript: None,
            tails: BTreeMap::new(),
            seen: BTreeMap::new(),
            began: Instant::now(),
            started: false,
            transient_fault: false,
            outgoing: VecDeque::new(),
            prepared_session: None,
            environment: super::super::environment::host_environment(),
            screen_grace: screen::DEFAULT_GRACE,
            watch: screen::Watch::default(),
        }
    }
    pub fn from_environment() -> Result<Self, EngineError> {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let configured = var("CLAUDE_CONFIG_DIR");
        let pass_config_dir = configured.is_some();
        let home = configured
            .or_else(|| var("HOME").map(|p| p.join(".claude")))
            .ok_or_else(|| failure(EngineErrorKind::Fatal, "claude: missing home"))?;
        let hook_binary = var("SLUICE_BIN")
            .ok_or_else(|| failure(EngineErrorKind::Fatal, "claude: missing pinned SLUICE_BIN"))?;
        let run = std::env::var("SLUICE_RUN_ID")
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| failure(EngineErrorKind::Fatal, "claude: missing RunId"))?;
        Ok(Self::new(
            var("SLUICE_CLAUDE_BIN").unwrap_or_else(|| "claude".into()),
            home,
            hook_binary,
            run,
        )
        .with_mcp(std::env::var("SLUICE_CLAUDE_MCP_CONFIG").ok())
        .with_config_dir_passed(pass_config_dir))
    }
    /// See `pass_config_dir`.
    pub fn with_config_dir_passed(mut self, pass: bool) -> Self {
        self.pass_config_dir = pass;
        self
    }
    pub fn with_mcp(mut self, config: Option<String>) -> Self {
        self.mcp = config;
        self
    }
    pub fn with_environment(mut self, environment: BTreeMap<String, String>) -> Self {
        self.environment = environment;
        self
    }
    /// A limit that resets further away than this fails the run as `QuotaExhausted`.
    pub fn with_quota_threshold(mut self, threshold: Duration) -> Self {
        self.state.set_quota_threshold(threshold);
        self
    }
    /// How long a screen sluice does not recognize may stand unchanged before any turn
    /// (`screen::DEFAULT_GRACE`).
    pub fn with_screen_grace(mut self, grace: Duration) -> Self {
        self.screen_grace = grace;
        self
    }
    /// Before any turn, the screen Claude stopped on instead of its composer: a known one at
    /// once, any other once it has stood unchanged for the grace. The dialogs the adapter
    /// answers, a blank pane and the composer are none.
    fn blocked(&mut self, pane: &str) -> Option<EngineError> {
        if let Some((what, advice)) = protocol::blocking_screen(pane) {
            return Some(blocked_screen(what, advice, pane));
        }
        if pane.trim().is_empty()
            || protocol::composer_ready(pane)
            || protocol::answered_dialog(pane)
        {
            self.watch.reset();
            return None;
        }
        self.watch
            .stands(pane, self.screen_grace, std::time::Instant::now())
            .then(|| {
                screen::Screen::unknown(account::Engine::Claude, self.screen_grace, pane).error()
            })
    }
    /// One injected transient after a completed turn, for the labelled acceptance gate.
    pub fn inject_transient(&mut self) {
        self.transient_fault = true;
    }
    pub fn progress_lines(&mut self) -> Vec<String> {
        self.state.summary_lines()
    }
    pub fn handle_hook(&mut self, event: &str, payload: Value) -> Result<Value, EngineError> {
        let hook = protocol::decode_hook(event, &payload)?;
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| failure(EngineErrorKind::Fatal, "claude: hook before prepare"))?;
        if fs::canonicalize(&hook.cwd).map_err(io_error)? != context.cwd {
            return Err(failure(EngineErrorKind::Fatal, "claude: hook cwd mismatch"));
        }
        let path = sluice_process::host::resolve_path(&hook.transcript_path)
            .map_err(|e| failure(EngineErrorKind::Fatal, e.to_string()))?;
        let projects = sluice_process::host::resolve_path(&self.home.join("projects"))
            .map_err(|e| failure(EngineErrorKind::Fatal, e.to_string()))?;
        if !path.starts_with(projects) {
            return Err(failure(
                EngineErrorKind::Fatal,
                "claude: transcript escapes engine home",
            ));
        }
        self.state.hook(&hook)?;
        if !path.components().any(|c| c.as_os_str() == "subagents") {
            self.transcript = Some(path);
        }
        if event == "SessionStart" {
            let text = bounded_text(&context.run_dir.join("me.md"))
                .or_else(|_| bounded_text(&context.run_dir.join("task.md")))
                .unwrap_or_else(|_| {
                    format!(
                        "Your current task is in {}; read it fully, then do it.",
                        context.run_dir.join("task.md").display()
                    )
                });
            return Ok(
                json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":text}}),
            );
        }
        if event == "PreToolUse" && hook.tool_name.as_deref() == Some("AskUserQuestion") {
            return Ok(
                json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Ask the orchestrator with sluice tool ask, as your task says. Interactive questions have no reader."}}),
            );
        }
        Ok(json!({}))
    }
    async fn output(mut command: Command) -> Result<Vec<u8>, EngineError> {
        command.kill_on_drop(true).stdin(Stdio::null());
        let output = tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .map_err(|_| failure(EngineErrorKind::Fatal, "claude: command probe timed out"))?
            .map_err(io_error)?;
        if !output.status.success() {
            return Err(classify(&String::from_utf8_lossy(&output.stderr)));
        }
        Ok(output.stdout)
    }
    async fn tmux(&self, args: &[&str]) -> Result<String, EngineError> {
        let context = self
            .context
            .as_ref()
            .ok_or_else(|| failure(EngineErrorKind::Fatal, "claude: not prepared"))?;
        let binary = context.tmux_binary.as_ref().ok_or_else(|| {
            failure(
                EngineErrorKind::CapabilityMismatch,
                "claude: approved private tmux required",
            )
        })?;
        let mut command = Command::new(binary);
        command
            .current_dir(&context.run_dir)
            .args(["-S", "tmux.sock", "-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        Ok(String::from_utf8_lossy(&Self::output(command).await?).into_owned())
    }
    async fn capture(&self) -> Result<String, EngineError> {
        self.tmux(&[
            "capture-pane",
            "-p",
            "-t",
            self.pane
                .as_deref()
                .ok_or_else(|| failure(EngineErrorKind::Fatal, "claude: no private pane"))?,
        ])
        .await
    }
    async fn key(&self, key: &str) -> Result<(), EngineError> {
        self.tmux(&[
            "send-keys",
            "-t",
            self.pane.as_deref().unwrap_or_default(),
            key,
        ])
        .await
        .map(|_| ())
    }
    fn enqueue(&mut self, id: InputId, text: String) -> DeliveryOutcome {
        if self.state.observation.acknowledged.contains(&id) {
            return DeliveryOutcome::Acknowledged;
        }
        if !self.state.pending(id.clone(), text.clone()) {
            return DeliveryOutcome::Pending;
        }
        self.state.observation.not_accepted.retain(|i| *i != id);
        self.outgoing.push_back(Delivery {
            id,
            text,
            phase: PastePhase::Ready,
            since: Instant::now(),
            retry_at: Instant::now(),
            retry: Duration::from_secs(1),
            covered: false,
        });
        DeliveryOutcome::Pending
    }
    async fn advance_delivery(&mut self, pane: &str) -> Result<(), EngineError> {
        while self
            .outgoing
            .front()
            .is_some_and(|d| self.state.observation.acknowledged.contains(&d.id))
        {
            self.outgoing.pop_front();
        }
        let Some(front) = self.outgoing.front() else {
            return Ok(());
        };
        let phase = front.phase;
        let text = front.text.clone();
        let needle = text
            .lines()
            .find(|s| !s.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .chars()
            .take(24)
            .collect::<String>();
        match phase {
            PastePhase::Ready => {
                if !protocol::composer_ready(pane) {
                    if front.since.elapsed() > Duration::from_secs(180) {
                        let delivery = self.outgoing.pop_front().unwrap();
                        self.state.pending.retain(|(id, _)| *id != delivery.id);
                        self.state.observation.not_accepted.push(delivery.id);
                        return Ok(());
                    }
                    if front.retry_at > Instant::now() {
                        return Ok(());
                    }
                    if pane.contains("Yes, I trust this folder") {
                        let selected = pane.lines().find(|s| s.contains('❯')).unwrap_or_default();
                        self.key(if selected.contains("Yes, I trust") {
                            "Enter"
                        } else {
                            "Down"
                        })
                        .await?;
                    } else if pane
                        .contains("WARNING: Claude Code running in Bypass Permissions mode")
                        && pane.contains("Yes, I accept")
                    {
                        let selected = pane.lines().find(|s| s.contains('❯')).unwrap_or_default();
                        self.key(if selected.contains("Yes, I accept") {
                            "Enter"
                        } else {
                            "Down"
                        })
                        .await?;
                    } else if protocol::occupied(pane) && front.covered {
                        self.key("Escape").await?;
                    }
                    let front = self.outgoing.front_mut().unwrap();
                    front.covered = protocol::occupied(pane);
                    front.retry_at = Instant::now() + Duration::from_millis(750);
                    return Ok(());
                }
                let path = self.context.as_ref().unwrap().run_dir.join("claude-paste");
                private_write(&path, &protocol::paste_payload(&(text + "\n"))).map_err(io_error)?;
                self.key("C-a").await?;
                self.key("C-k").await?;
                self.tmux(&[
                    "load-buffer",
                    "-b",
                    "sluice-claude",
                    &path.to_string_lossy(),
                ])
                .await?;
                // Set the phase before paste so cancellation cannot replay its bytes.
                let front = self.outgoing.front_mut().unwrap();
                front.phase = PastePhase::Draft;
                front.since = Instant::now();
                self.tmux(&[
                    "paste-buffer",
                    "-d",
                    "-p",
                    "-b",
                    "sluice-claude",
                    "-t",
                    self.pane.as_deref().unwrap(),
                ])
                .await?;
            }
            PastePhase::Draft => {
                if protocol::draft_visible(pane, &needle)
                    && front.since.elapsed() >= Duration::from_millis(100)
                {
                    let front = self.outgoing.front_mut().unwrap();
                    front.phase = PastePhase::Submitted;
                    front.since = Instant::now();
                    front.retry_at = Instant::now() + Duration::from_secs(1);
                    self.key("Enter").await?;
                } else if front.since.elapsed() >= Duration::from_secs(5) {
                    return Err(failure(
                        EngineErrorKind::UnknownAcceptance,
                        "claude: pasted draft could not be verified",
                    ));
                }
            }
            PastePhase::Submitted => {
                if protocol::draft_visible(pane, &needle) {
                    if front.since.elapsed() >= Duration::from_secs(20) {
                        return Err(failure(
                            EngineErrorKind::UnknownAcceptance,
                            "claude: draft never left the composer",
                        ));
                    }
                    if front.retry_at <= Instant::now() {
                        let front = self.outgoing.front_mut().unwrap();
                        front.retry = (front.retry * 2).min(Duration::from_secs(8));
                        front.retry_at = Instant::now() + front.retry;
                        self.key("Enter").await?;
                    }
                }
                // Absence of a draft is not acceptance. Wait for UserPromptSubmit.
            }
        }
        Ok(())
    }
    fn transcripts(&mut self) -> Result<(), EngineError> {
        let Some(main) = &self.transcript else {
            return Ok(());
        };
        let mut paths = vec![main.clone()];
        let sub = main.with_extension("").join("subagents");
        if let Ok(entries) = fs::read_dir(sub) {
            for entry in entries {
                let path = entry.map_err(io_error)?.path();
                if path.extension().is_some_and(|s| s == "jsonl") {
                    paths.push(path);
                }
            }
        }
        for path in paths {
            let is_main = path == *main;
            let offset = self.seen.get(&path).copied().unwrap_or(0);
            let tail = self
                .tails
                .entry(path.clone())
                .or_insert_with(|| Tail::new(path.clone(), offset));
            let skipped = tail.skipped();
            for entry in tail.read()? {
                self.state.transcript(&entry, is_main);
            }
            // An oversized record is skipped, not fatal; the run's log says so. A turn
            // still ends on its Stop hook (which carries the final message) or a stable idle
            // pane, neither of which reads the transcript.
            let skipped = tail.skipped() - skipped;
            if skipped > 0 {
                eprintln!(
                    "claude: skipped {skipped} transcript record(s) over 1 MiB in {} ({} so far)",
                    path.display(),
                    tail.skipped()
                );
            }
        }
        Ok(())
    }
    fn find_session(
        &self,
        session: &str,
    ) -> Result<Option<(PathBuf, SessionMetadata)>, EngineError> {
        self.find_recorded_session(session)?
            .map(|(path, mut metadata)| {
                metadata.cwd = fs::canonicalize(metadata.cwd).map_err(io_error)?;
                Ok((path, metadata))
            })
            .transpose()
    }
    /// Read the recorded cwd even if it has disappeared, for paused import diagnostics.
    pub fn recorded_session(&self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        Ok(self
            .find_recorded_session(session)?
            .map(|(_, metadata)| metadata))
    }
    fn find_recorded_session(
        &self,
        session: &str,
    ) -> Result<Option<(PathBuf, SessionMetadata)>, EngineError> {
        protocol::session_id(session)?;
        let projects = self.home.join("projects");
        let entries = match fs::read_dir(&projects) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(e)),
        };
        for entry in entries {
            let path = entry
                .map_err(io_error)?
                .path()
                .join(format!("{session}.jsonl"));
            if !path.is_file() {
                continue;
            }
            let resolved = fs::canonicalize(&path).map_err(io_error)?;
            if !resolved.starts_with(fs::canonicalize(&projects).map_err(io_error)?) {
                return Err(failure(
                    EngineErrorKind::Fatal,
                    "claude: session transcript escapes home",
                ));
            }
            let mut tail = Tail::new(path.clone(), 0);
            loop {
                let before = tail.offset();
                for record in tail.read()? {
                    if let Some(cwd) = record["cwd"].as_str() {
                        let cwd = PathBuf::from(cwd);
                        return Ok(Some((
                            path,
                            SessionMetadata {
                                id: session.into(),
                                cwd,
                            },
                        )));
                    }
                }
                if tail.offset() == before {
                    break;
                }
            }
            return Err(failure(
                EngineErrorKind::Fatal,
                "claude: session has no cwd metadata",
            ));
        }
        Ok(None)
    }
}
fn bounded_text(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut bytes = vec![];
    file.take((protocol::MAX_EVENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > protocol::MAX_EVENT_BYTES {
        return Err(io::Error::other("Claude context exceeds 1 MiB"));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}
pub fn private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()
}
impl EngineAdapter for Claude {
    fn on_hook(&mut self, hook: HookEvent) -> Result<HookReply, EngineError> {
        let stdout = self.handle_hook(&hook.event, hook.payload)?;
        Ok(HookReply {
            stdout: Some(stdout),
            exit_code: 0,
        })
    }
    fn profile(&self) -> EngineProfile {
        profile::profile()
    }
    async fn models(&mut self) -> Result<Vec<String>, EngineError> {
        Ok(profile::models())
    }
    async fn session(&mut self, session: &str) -> Result<Option<SessionMetadata>, EngineError> {
        Ok(self.find_session(session)?.map(|(_, meta)| meta))
    }
    async fn prepare(
        &mut self,
        context: &EngineContext,
        session: Option<&str>,
    ) -> Result<Option<EngineLaunch>, EngineError> {
        let model = crate::model::launch_model("claude", context.model.as_ref())?;
        let mut command = Command::new(&self.binary);
        command.env_clear().envs(&self.environment);
        for name in profile::SCRUB_ENV {
            command.env_remove(name);
        }
        if self.pass_config_dir {
            command.env("CLAUDE_CONFIG_DIR", &self.home);
        }
        command.arg("--version").env("DISABLE_AUTOUPDATER", "1");
        profile::validate_version(&String::from_utf8_lossy(&Self::output(command).await?))?;
        let mut context = context.clone();
        context.cwd = fs::canonicalize(&context.cwd).map_err(io_error)?;
        context.run_dir = fs::canonicalize(&context.run_dir).map_err(io_error)?;
        let threshold = self.state.quota_threshold;
        self.state = ClaudeState {
            quota_threshold: threshold,
            ..ClaudeState::default()
        };
        self.pane = None;
        self.tails.clear();
        self.seen.clear();
        self.transcript = None;
        self.started = false;
        self.outgoing.clear();
        self.prepared_session = session.map(str::to_owned);
        self.began = Instant::now();
        self.watch.reset();
        if let Some(session) = session {
            let (path, meta) = self.find_session(session)?.ok_or_else(|| {
                failure(EngineErrorKind::MissingSession, "claude: session not found")
            })?;
            if meta.cwd != context.cwd {
                return Err(failure(
                    EngineErrorKind::Fatal,
                    format!(
                        "claude: session was started in {}, not {}; cannot resume a session from another directory",
                        meta.cwd.display(),
                        context.cwd.display()
                    ),
                ));
            }
            self.seen
                .insert(path.clone(), fs::metadata(&path).map_err(io_error)?.len());
            if let Ok(entries) = fs::read_dir(path.with_extension("").join("subagents")) {
                for entry in entries {
                    let path = entry.map_err(io_error)?.path();
                    if path.extension().is_some_and(|s| s == "jsonl") {
                        self.seen
                            .insert(path.clone(), fs::metadata(&path).map_err(io_error)?.len());
                    }
                }
            }
            self.state.observation.session_id = Some(session.into());
            self.transcript = Some(path);
        }
        let mut hooks = serde_json::Map::new();
        for event in profile::HOOKS {
            let command = format!(
                "{} agent-hook claude {}",
                protocol::shell_quote(&self.hook_binary.to_string_lossy()),
                protocol::shell_quote(event)
            );
            hooks.insert(
                (*event).into(),
                json!([{"hooks":[{"type":"command","command":command,"timeout":15}]}]),
            );
        }
        let settings = context.run_dir.join("claude-settings.json");
        private_write(
            &settings,
            &serde_json::to_vec(&json!({"hooks":hooks}))
                .map_err(|e| failure(EngineErrorKind::Fatal, e.to_string()))?,
        )
        .map_err(io_error)?;
        let argv = profile::argv(
            &self.binary,
            &model,
            &settings,
            session,
            self.mcp.as_deref(),
        );
        self.context = Some(context);
        let mut env = self.environment.clone();
        if self.pass_config_dir {
            env.insert(
                "CLAUDE_CONFIG_DIR".into(),
                self.home.to_string_lossy().into_owned(),
            );
        } else {
            env.remove("CLAUDE_CONFIG_DIR");
        }
        env.extend(BTreeMap::from([
            ("SLUICE_RUN_ID".into(), self.run.to_string()),
            (
                "SLUICE_RUN_DIR".into(),
                self.context
                    .as_ref()
                    .unwrap()
                    .run_dir
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "SLUICE_BIN".into(),
                self.hook_binary.to_string_lossy().into_owned(),
            ),
            ("DISABLE_AUTOUPDATER".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_EDITOR".into(), "true".into()),
            ("GIT_MERGE_AUTOEDIT".into(), "no".into()),
        ]));
        Ok(Some(EngineLaunch { argv, env }))
    }
    async fn execute(
        &mut self,
        _context: &EngineContext,
        command: EngineCommand,
    ) -> Result<DeliveryOutcome, EngineError> {
        if let EngineCommand::Resume { session } = &command
            && self.prepared_session.as_deref() != Some(session.as_str())
        {
            return Err(failure(
                EngineErrorKind::Fatal,
                "claude: resume command differs from prepared session",
            ));
        }
        if matches!(command, EngineCommand::StartFresh) && self.prepared_session.is_some() {
            return Err(failure(
                EngineErrorKind::Fatal,
                "claude: fresh command for a resumed launch",
            ));
        }
        match command {
            EngineCommand::StartFresh | EngineCommand::Resume { .. } => {
                if self.started {
                    return Ok(DeliveryOutcome::Acknowledged);
                }
                let panes = self.tmux(&["list-panes", "-a", "-F", "#{pane_id}"]).await?;
                let panes: Vec<_> = panes.lines().collect();
                if panes.len() != 1 {
                    return Err(failure(
                        EngineErrorKind::Fatal,
                        "claude: expected exactly one private pane",
                    ));
                }
                self.pane = Some(panes[0].into());
                self.started = true;
                Ok(DeliveryOutcome::Acknowledged)
            }
            EngineCommand::DeliverText { id, text } | EngineCommand::Steer { id, text } => {
                Ok(self.enqueue(id, text))
            }
            EngineCommand::RequestExit => {
                Ok(self.enqueue(InputId::Continue { attempt: u32::MAX }, "/exit".into()))
            }
        }
    }
    async fn observe(
        &mut self,
        _context: &EngineContext,
    ) -> Result<EngineObservation, EngineError> {
        let pane = self
            .pane
            .as_deref()
            .ok_or_else(|| failure(EngineErrorKind::Fatal, "claude: not started"))?;
        let display = self
            .tmux(&[
                "display-message",
                "-p",
                "-t",
                pane,
                "#{pane_pid} #{pane_dead} #{pane_dead_status}",
            ])
            .await?;
        let mut parts = display.split_whitespace();
        let pid = parts.next().unwrap_or_default();
        let dead = parts.next() == Some("1");
        let exit = parts.next();
        let status = protocol::read_json(&self.home.join("sessions").join(format!("{pid}.json")));
        let status = status.as_ref().filter(|v| {
            v["kind"] == "interactive"
                && self
                    .state
                    .observation
                    .session_id
                    .as_ref()
                    .is_none_or(|id| v["sessionId"].as_str() == Some(id))
        });
        self.transcripts()?;
        let capture = self.capture().await?;
        // Logged out, Claude opens on its login screen instead of the composer: no input can
        // be accepted until the owner signs in.
        if !dead && self.state.observation.error.is_none() && protocol::login_screen(&capture) {
            self.state.observation.error = Some(
                account::Auth {
                    problem: "not logged in (Claude shows its login screen)".into(),
                    ..account::Auth::login(account::Engine::Claude, protocol::login_text(&capture))
                }
                .error(),
            );
        }
        if !dead
            && self.state.observation.error.is_none()
            && self.state.observation.turns_started == 0
            && let Some(error) = self.blocked(&capture)
        {
            self.state.observation.error = Some(error);
        }
        if !dead {
            self.advance_delivery(&capture).await?;
        }
        let mut observation = self.state.snapshot(
            status.and_then(|v| v["status"].as_str()),
            protocol::composer_ready(&capture),
            dead,
            self.began.elapsed(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        );
        if dead && observation.error.is_none() && exit != Some("0") {
            // Claude prints a required update as it exits. tmux scrolls the dead pane's top
            // row away to print `Pane is dead`, so the rows above the screen count too.
            let target = self.pane.clone().unwrap_or_default();
            let last = self
                .tmux(&["capture-pane", "-p", "-S", "-50", "-t", &target])
                .await
                .unwrap_or_default();
            observation.error = Some(match protocol::blocking_screen(&last) {
                Some((what, advice)) => blocked_screen(what, advice, &last),
                None => classify(&capture),
            });
        }
        if self.transient_fault && observation.turns_completed > 0 {
            self.transient_fault = false;
            observation.error = Some(failure(
                EngineErrorKind::Transient,
                "claude: injected acceptance-gate transient",
            ));
        }
        Ok(observation)
    }
    async fn close(&mut self) -> io::Result<()> {
        // The guardian owns the pane, server and descendant subtree. No auxiliary
        // engine processes are spawned by this adapter; probes are kill-on-drop.
        self.started = false;
        self.pane = None;
        Ok(())
    }
}
