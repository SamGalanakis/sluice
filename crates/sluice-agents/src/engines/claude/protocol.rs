//! Bounded decoding of Claude hooks, transcript lines and the framed TUI composer.
use crate::engines::{EngineError, EngineErrorKind};
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

pub const MAX_EVENT_BYTES: usize = 1024 * 1024;
pub fn failure(kind: EngineErrorKind, message: impl Into<String>) -> EngineError {
    EngineError {
        kind,
        message: message.into(),
        retry_at: None,
    }
}
pub fn io_error(error: io::Error) -> EngineError {
    failure(EngineErrorKind::Fatal, error.to_string())
}
pub fn session_id(value: &str) -> Result<(), EngineError> {
    if !value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        || value.len() > 128
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(failure(
            EngineErrorKind::Fatal,
            "claude: invalid session id",
        ));
    }
    Ok(())
}
#[derive(Debug, Deserialize)]
pub struct Hook {
    pub hook_event_name: String,
    pub session_id: String,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub error_details: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub background_tasks: Vec<Value>,
    #[serde(default)]
    pub session_crons: Vec<Value>,
}
pub fn decode_hook(event: &str, payload: &Value) -> Result<Hook, EngineError> {
    if serde_json::to_vec(payload)
        .map_err(|e| failure(EngineErrorKind::Fatal, e.to_string()))?
        .len()
        > MAX_EVENT_BYTES
    {
        return Err(failure(
            EngineErrorKind::Fatal,
            "claude: hook payload exceeds 1 MiB",
        ));
    }
    let hook: Hook = serde_json::from_value(payload.clone())
        .map_err(|_| failure(EngineErrorKind::Fatal, "claude: invalid hook payload"))?;
    session_id(&hook.session_id)?;
    if !super::profile::HOOKS.contains(&event)
        || hook.hook_event_name != event
        || !hook.cwd.is_absolute()
        || !hook.transcript_path.is_absolute()
    {
        return Err(failure(
            EngineErrorKind::Fatal,
            "claude: hook identity/event/path mismatch",
        ));
    }
    Ok(hook)
}

/// Follows a JSONL transcript Claude Code appends to. A record (one line) over
/// `MAX_EVENT_BYTES` is skipped and counted, never fatal: Claude Code writes such lines for
/// a large tool result or file read. A line still incomplete past the limit is dropped as it
/// grows (the tail is in discard mode until its newline), so memory stays bounded by the
/// limit plus one read.
#[derive(Debug)]
pub struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    /// Dropping an oversized record's bytes until its newline.
    discarding: bool,
    skipped: u64,
}
impl Tail {
    pub fn new(path: PathBuf, offset: u64) -> Self {
        Self {
            path,
            offset,
            partial: vec![],
            discarding: false,
            skipped: 0,
        }
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    /// How many oversized records this tail has skipped.
    pub fn skipped(&self) -> u64 {
        self.skipped
    }
    pub fn read(&mut self) -> Result<Vec<Value>, EngineError> {
        let mut file = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(io_error(e)),
        };
        let size = file.metadata().map_err(io_error)?.len();
        if size < self.offset {
            self.offset = 0;
            self.partial.clear();
            self.discarding = false;
        }
        file.seek(SeekFrom::Start(self.offset)).map_err(io_error)?;
        let mut data = vec![];
        file.take((8 * MAX_EVENT_BYTES) as u64)
            .read_to_end(&mut data)
            .map_err(io_error)?;
        self.offset += data.len() as u64;
        Ok(self.lines(&data))
    }
    /// The complete records `data` (the bytes after the last read) ends, decoded.
    fn lines(&mut self, mut data: &[u8]) -> Vec<Value> {
        if self.discarding {
            let Some(end) = data.iter().position(|b| *b == b'\n') else {
                return vec![];
            };
            data = &data[end + 1..];
            self.discarding = false;
        }
        self.partial.extend_from_slice(data);
        let mut output = vec![];
        let mut consumed = 0;
        for (index, byte) in self.partial.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            let line = &self.partial[consumed..index];
            consumed = index + 1;
            if line.len() > MAX_EVENT_BYTES {
                self.skipped += 1;
                continue;
            }
            if let Ok(value) = serde_json::from_slice::<Value>(line)
                && value.is_object()
            {
                output.push(value);
            }
        }
        self.partial.drain(..consumed);
        if self.partial.len() > MAX_EVENT_BYTES {
            self.partial = vec![];
            self.discarding = true;
            self.skipped += 1;
        }
        output
    }
}

// Composer recognition follows the Apache-2.0 Omnigent bridge and the Python
// port, Copyright 2026 Databricks, Inc. Rules and wrapped drafts are retained.
fn rule(line: &str) -> bool {
    let s = line.trim();
    let glyphs = "─━╭╮╰╯│┃╌╍";
    if s.chars().count() < 3 {
        return false;
    }
    if s.chars().all(|ch| glyphs.contains(ch)) {
        return true;
    }
    let edge = "─━╭╮╰╯╌╍";
    let label = s.trim_matches(|ch| edge.contains(ch));
    label.len() < s.len()
        && s.chars().count() >= 20
        && label.starts_with(' ')
        && label.ends_with(' ')
        && !label.trim().is_empty()
        && !label.chars().any(|ch| glyphs.contains(ch))
}
fn rows(pane: &str) -> Vec<&str> {
    pane.lines().filter(|s| !s.trim().is_empty()).collect()
}
fn composer_row(pane: &str) -> Option<&str> {
    let lines = rows(pane);
    let rules: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, s)| rule(s))
        .map(|(i, _)| i)
        .collect();
    let mut candidates = vec![];
    if rules.len() >= 2 {
        candidates.push(rules[rules.len() - 2] + 1);
    }
    if let Some(last) = rules.last() {
        candidates.push(last + 1);
    }
    candidates.into_iter().find_map(|i| {
        lines
            .get(i)
            .copied()
            .filter(|s| s.trim().starts_with(['❯', '!']))
    })
}
fn history_search(pane: &str) -> bool {
    let lines = rows(pane);
    let at = lines
        .iter()
        .rposition(|s| rule(s))
        .map(|i| i + 1)
        .unwrap_or(lines.len().saturating_sub(8));
    let footer = lines[at..]
        .iter()
        .map(|s| s.trim().split("  ").next().unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    footer.starts_with("search prompts:") || footer.starts_with("no matching prompt:")
}
pub fn composer_ready(pane: &str) -> bool {
    !history_search(pane) && composer_row(pane).is_some_and(|s| s.trim().starts_with('❯'))
}
pub fn occupied(pane: &str) -> bool {
    !pane.trim().is_empty() && !composer_ready(pane)
}
pub fn draft_visible(pane: &str, needle: &str) -> bool {
    let lines = rows(pane);
    let Some(at) = lines.iter().rposition(|s| s.contains('❯')) else {
        return false;
    };
    let mut text = lines[at].rsplit('❯').next().unwrap_or_default().to_owned();
    for line in lines.iter().skip(at + 1).take(7) {
        if rule(line) {
            break;
        }
        text.push_str(line);
    }
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let needle: String = needle.chars().filter(|c| !c.is_whitespace()).collect();
    text.contains("[Pastedtext") || (!needle.is_empty() && text.contains(&needle))
}
pub fn paste_payload(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .map(|c| if c == '\n' { '\r' } else { c })
        .collect::<String>()
        .into_bytes()
}
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
/// Claude Code's own wording for a hard usage limit, the error-level prefixes 2.1.284 keeps
/// for its usage-limit notices (`You've hit your weekly limit · resets Sep 22, 1am
/// (Europe/Berlin)`, `You're out of usage credits. …`), and the older
/// `Claude AI usage limit reached|<epoch>`. Returns the reset when the text carries one. Only
/// an API error's own text is read (an `isApiErrorMessage` entry or a `StopFailure`), and only
/// its start, so tool output and the agent's prose cannot match. The fast-mode cooldown
/// (`You've hit your fast limit`) is short and not a usage limit.
pub fn usage_limit(message: &str) -> Option<Option<u64>> {
    const LIMITS: [&str; 12] = [
        "You've hit your",
        "You've reached your",
        "You're out of usage credits",
        "Your org is out of usage",
        "Your seat type doesn't include usage credits",
        "Your seat type doesn't include usage",
        "Your usage allocation has been disabled by your admin",
        "Your group's usage limit is set to $0",
        "You're out of extra usage",
        "Your seat type doesn't include extra usage",
        "This service is disabled for your org",
        "Claude AI usage limit reached",
    ];
    let text = message.trim_start().replace('\u{2019}', "'");
    if let Some(rest) = text.strip_prefix("Claude AI usage limit reached") {
        return Some(rest.strip_prefix('|').and_then(|at| at.trim().parse().ok()));
    }
    let fable = text.starts_with("Fable")
        && text
            .split('.')
            .next()
            .is_some_and(|s| s.len() <= 80 && s.ends_with(" requires usage credits"));
    ((LIMITS.iter().any(|limit| text.starts_with(limit))
        && !text.starts_with("You've hit your fast limit"))
        || fable)
        .then_some(None)
}
/// Claude Code's login wording at the start of an API error's text (2.1.284: `Not logged in ·
/// Please run /login`, `Login expired · Please run /login`, `OAuth token revoked · Please run
/// /login`, `Failed to authenticate: OAuth session expired and could not be refreshed`,
/// `Invalid API key · Fix external API key`, `Please run /login · API Error: 403 …`).
pub fn auth_text(message: &str) -> bool {
    const LOGIN: [&str; 10] = [
        "Not logged in",
        "Login expired",
        "OAuth token revoked",
        "OAuth token has expired",
        "Failed to authenticate",
        "Invalid API key",
        "Invalid auth token",
        "Please run /login",
        "Authentication required \u{b7} Sign in again",
        "Your account does not have access to Claude",
    ];
    let text = message.trim_start();
    LOGIN.iter().any(|login| text.starts_with(login))
}
/// Claude Code logged out opens on its login screen: a `Select login method:` row with the
/// `Claude account with subscription` choice, and no composer.
pub fn login_screen(pane: &str) -> bool {
    !composer_ready(pane)
        && pane.lines().any(|l| l.trim() == "Select login method:")
        && pane.contains("Claude account with subscription")
}
/// The login screen's own rows, for the failure's message.
pub fn login_text(pane: &str) -> String {
    pane.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join(" ")
}
/// The two dialogs the adapter answers itself, before the composer: the folder-trust prompt
/// (`Yes, I trust this folder`) and the bypass-permissions warning (`WARNING: Claude Code
/// running in Bypass Permissions mode`, `Yes, I accept`).
pub fn answered_dialog(pane: &str) -> bool {
    pane.contains("Yes, I trust this folder")
        || (pane.contains("WARNING: Claude Code running in Bypass Permissions mode")
            && pane.contains("Yes, I accept"))
}
/// A screen Claude Code 2.1.284 stops on before its composer and sluice does not answer, with
/// what the owner must do; `None` without one, with the composer showing, or on the dialogs
/// the adapter answers. The rows are Claude's own (from its bundle):
/// - its first-run setup (`Onboarding`): the theme picker (`Let's get started.`, `Choose the
///   text style that looks best with your terminal`), `Security notes:` (`Claude can make
///   mistakes.`) and `Use Claude Code's terminal setup?`;
/// - setup's connectivity check failing (`Unable to connect to Anthropic services`);
/// - the API-key prompt (`Detected a custom API key in your environment`, `Do you want to use
///   this API key?`);
/// - the updated-terms notice (`Updates to Consumer Terms and Policies`, `We've updated our
///   Consumer Terms and Privacy Policy.`);
/// - `Managed settings require approval`;
/// - a project's MCP servers (`New MCP server found in this project: …`, `… new MCP servers
///   found in this project`);
/// - `Allow external CLAUDE.md file imports?`;
/// - a required update (`It looks like your version of Claude Code (…) needs an update.`,
///   `Claude Code … is older than the minimum version required by your organization`), which
///   Claude prints as it exits.
///
/// Its login screen (`Select login method:`) is an auth failure instead (`login_screen`).
pub fn blocking_screen(pane: &str) -> Option<(String, String)> {
    use crate::engines::screen::row;
    if composer_ready(pane) || answered_dialog(pane) {
        return None;
    }
    let setup = "Claude Code's first-run setup is unfinished for the account it runs as: run `claude` once on this host as that user and finish it (and if the host sets CLAUDE_CONFIG_DIR, check that it is the configured one)";
    let (what, advice) = if row(
        pane,
        "Choose the text style that looks best with your terminal",
    ) {
        ("its first-run setup (theme picker)", setup)
    } else if row(pane, "Security notes:") && pane.contains("Claude can make mistakes.") {
        ("its first-run setup (security notes)", setup)
    } else if row(pane, "Use Claude Code's terminal setup?") {
        ("its first-run setup (terminal setup)", setup)
    } else if row(pane, "Unable to connect to Anthropic services") {
        (
            "its first-run connectivity check (it cannot reach Anthropic)",
            "check this host's network and proxy to Anthropic (https://code.claude.com/docs/en/network-config), then run `claude` once on this host and finish its setup",
        )
    } else if row(pane, "Do you want to use this API key?") {
        (
            "its API-key prompt (ANTHROPIC_API_KEY is set)",
            "run `claude` once on this host and answer whether to use the key, or remove ANTHROPIC_API_KEY from the environment the run inherits",
        )
    } else if row(pane, "Updates to Consumer Terms and Policies")
        || row(pane, "We've updated our Consumer Terms and Privacy Policy")
    {
        (
            "Anthropic's updated terms",
            "run `claude` once on this host and review and accept the updated terms",
        )
    } else if row(pane, "Managed settings require approval") {
        (
            "its approval of the organization's managed settings",
            "run `claude` once on this host and approve the managed settings",
        )
    } else if pane.contains("New MCP server found in this project")
        || pane.contains("new MCP servers found in this project")
    {
        (
            "its approval of the project's MCP servers (.mcp.json)",
            "run `claude` once in the step's cwd on this host and approve or reject the servers (or set enabledMcpjsonServers or enableAllProjectMcpServers in the project's settings)",
        )
    } else if row(pane, "Allow external CLAUDE.md file imports?") {
        (
            "its approval of CLAUDE.md imports from outside the directory",
            "run `claude` once in the step's cwd on this host and answer it",
        )
    } else if pane.contains(") needs an update.")
        || pane.contains("is older than the minimum version required by your organization")
    {
        (
            "a required update (this Claude Code is older than the version it now requires)",
            "update Claude Code on this host (`claude update`) together with sluice's pinned Claude profile",
        )
    } else {
        return None;
    };
    Some((what.into(), advice.into()))
}
pub fn read_json(path: &Path) -> Option<Value> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_EVENT_BYTES as u64 {
        return None;
    }
    serde_json::from_reader(file).ok()
}

#[cfg(test)]
mod tail_tests {
    use super::*;

    fn record(n: usize) -> String {
        format!("{{\"type\":\"assistant\",\"n\":{n}}}\n")
    }
    fn numbers(values: &[Value]) -> Vec<u64> {
        values.iter().map(|v| v["n"].as_u64().unwrap()).collect()
    }

    #[test]
    fn an_oversized_tail_still_being_written_is_dropped_until_its_newline() {
        let mut tail = Tail::new(PathBuf::new(), 0);
        assert_eq!(numbers(&tail.lines(record(1).as_bytes())), [1]);
        // The record grows past the limit with no newline yet: it is dropped as it comes.
        let blob = format!("{{\"blob\":\"{}", "y".repeat(MAX_EVENT_BYTES));
        assert!(tail.lines(blob.as_bytes()).is_empty());
        assert_eq!(tail.skipped(), 1);
        assert!(tail.partial.is_empty());
        assert!(
            tail.lines("y".repeat(MAX_EVENT_BYTES).as_bytes())
                .is_empty()
        );
        assert!(tail.partial.is_empty());
        // Its end, then more records in the same read.
        let rest = format!("yy\"}}\n{}{}", record(2), record(3));
        assert_eq!(numbers(&tail.lines(rest.as_bytes())), [2, 3]);
        assert_eq!(tail.skipped(), 1);
        // A record split across reads still parses.
        let next = record(4);
        let (a, b) = next.split_at(5);
        assert!(tail.lines(a.as_bytes()).is_empty());
        assert_eq!(numbers(&tail.lines(b.as_bytes())), [4]);
    }
}
