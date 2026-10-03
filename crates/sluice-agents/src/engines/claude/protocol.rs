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

#[derive(Debug)]
pub struct Tail {
    path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}
impl Tail {
    pub fn new(path: PathBuf, offset: u64) -> Self {
        Self {
            path,
            offset,
            partial: vec![],
        }
    }
    pub fn offset(&self) -> u64 {
        self.offset
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
        }
        file.seek(SeekFrom::Start(self.offset)).map_err(io_error)?;
        let mut data = vec![];
        file.take((8 * MAX_EVENT_BYTES) as u64)
            .read_to_end(&mut data)
            .map_err(io_error)?;
        self.offset += data.len() as u64;
        self.partial.extend(data);
        let mut output = vec![];
        let mut consumed = 0;
        for (index, byte) in self.partial.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            let line = &self.partial[consumed..index];
            if line.len() > MAX_EVENT_BYTES {
                return Err(failure(
                    EngineErrorKind::Fatal,
                    "claude: transcript record exceeds 1 MiB",
                ));
            }
            if let Ok(value) = serde_json::from_slice::<Value>(line)
                && value.is_object()
            {
                output.push(value);
            }
            consumed = index + 1;
        }
        self.partial.drain(..consumed);
        if self.partial.len() > MAX_EVENT_BYTES {
            return Err(failure(
                EngineErrorKind::Fatal,
                "claude: incomplete transcript record exceeds 1 MiB",
            ));
        }
        Ok(output)
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
pub fn read_json(path: &Path) -> Option<Value> {
    let file = File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_EVENT_BYTES as u64 {
        return None;
    }
    serde_json::from_reader(file).ok()
}
