//! Devin JSONC, lifecycle journal and composer protocol for CLI 3000.11.3.
use super::profile::HOOKS;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub const MAX_HOOK_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub hook_event_name: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Value,
    #[serde(default)]
    pub tool_response: Value,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub invocation: String,
    pub hook: Hook,
}

pub fn decode_hook(bytes: &[u8], event: &str) -> io::Result<Hook> {
    if bytes.len() > MAX_HOOK_BYTES || !HOOKS.contains(&event) {
        return Err(io::Error::other("unsupported or oversized Devin hook"));
    }
    let hook: Hook = sluice_model::rpc::decode_json(bytes).map_err(io::Error::other)?;
    if hook.hook_event_name != event {
        return Err(io::Error::other("Devin hook event does not match callback"));
    }
    Ok(hook)
}

/// Cross-process locking includes both the JSON and newline, followed by a durable flush.
pub fn append_hook(path: &Path, invocation: &str, hook: Hook) -> io::Result<()> {
    if invocation.is_empty() {
        return Err(io::Error::other("missing invocation"));
    }
    let mut file = OpenOptions::new()
        .append(true)
        .read(true)
        .custom_flags(0x20000 /* O_NOFOLLOW on Linux */)
        .open(path)?;
    file.lock()?;
    let mut bytes = serde_json::to_vec(&JournalEntry {
        invocation: invocation.into(),
        hook,
    })
    .map_err(io::Error::other)?;
    bytes.push(b'\n');
    file.write_all(&bytes)?;
    file.sync_data()
}

/// Called only by the Devin branch of `sluice agent hook`.
pub fn hook_main(event: &str) -> io::Result<()> {
    let path = std::env::var_os("SLUICE_DEVIN_JOURNAL")
        .ok_or_else(|| io::Error::other("missing Devin journal"))?;
    let path = std::path::PathBuf::from(path);
    sluice_process::host::guard_scratch_home(&path).map_err(io::Error::other)?;
    let invocation = std::env::var("SLUICE_DEVIN_INVOCATION").map_err(io::Error::other)?;
    let mut bytes = Vec::new();
    io::stdin()
        .take(MAX_HOOK_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    append_hook(&path, &invocation, decode_hook(&bytes, event)?)
}

pub fn private_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(0x20000)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_data()
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn config(
    raw: &str,
    model: &str,
    binary: &Path,
    journal: &Path,
    invocation: &str,
) -> io::Result<Value> {
    let mut cfg: Value = serde_json::from_str(&strip_jsonc(raw)?).map_err(io::Error::other)?;
    let object = cfg
        .as_object_mut()
        .ok_or_else(|| io::Error::other("Devin config must be an object"))?;
    let agent = object
        .entry("agent")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| io::Error::other("Devin agent config must be an object"))?;
    agent.insert("model".into(), model.into());
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| io::Error::other("Devin hooks must be an object"))?;
    for event in HOOKS {
        let command = format!(
            "SLUICE_DEVIN_JOURNAL={} SLUICE_DEVIN_INVOCATION={} {} agent-hook devin {}",
            shell_quote(&journal.to_string_lossy()),
            shell_quote(invocation),
            shell_quote(&binary.to_string_lossy()),
            event
        );
        hooks.entry(event).or_insert_with(|| json!([])).as_array_mut()
            .ok_or_else(|| io::Error::other("Devin hook groups must be arrays"))?
            .push(json!({"matcher":"", "hooks":[{"type":"command", "command":command, "timeout":30}]}));
    }
    Ok(cfg)
}

pub fn strip_jsonc(raw: &str) -> io::Result<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let (mut i, mut string) = (0, false);
    while i < bytes.len() {
        let b = bytes[i];
        if string {
            out.push(b);
            if b == b'\\' && i + 1 < bytes.len() {
                i += 1;
                out.push(bytes[i]);
            } else if b == b'"' {
                string = false;
            }
        } else if b == b'"' {
            string = true;
            out.push(b);
        } else if bytes.get(i..i + 2) == Some(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(b'\n');
            continue;
        } else if bytes.get(i..i + 2) == Some(b"/*") {
            i += 2;
            while i + 1 < bytes.len() && &bytes[i..i + 2] != b"*/" {
                i += 1;
            }
            if i + 1 >= bytes.len() {
                return Err(io::Error::other("unterminated JSONC comment"));
            }
            i += 2;
            out.push(b' ');
            continue;
        } else {
            out.push(b);
        }
        i += 1;
    }
    String::from_utf8(out).map_err(io::Error::other)
}

pub fn composer_region(pane: &str) -> String {
    let lines: Vec<_> = pane.lines().collect();
    let rules: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, s)| s.contains("────"))
        .map(|(i, _)| i)
        .collect();
    if rules.len() >= 2 {
        lines[rules[rules.len() - 2] + 1..rules[rules.len() - 1]].join("\n")
    } else {
        lines[lines.len().saturating_sub(8)..].join("\n")
    }
}
fn plain(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}
pub fn composer_ready(pane: &str) -> bool {
    let pane = pane.split_whitespace().collect::<Vec<_>>().join(" ");
    [
        "Ask Devin to build features, fix bugs, or work on your code",
        "Guide Devin while it works",
    ]
    .iter()
    .any(|s| pane.contains(s))
}
/// Devin is mid-turn: 3000.11.3 draws a spinner line ending `(esc twice to interrupt)` (`(esc
/// again to interrupt)` after one Escape) above the composer and the `Guide Devin while it works`
/// placeholder in it, also while a Fusion lead waits on its sidekick. Only the bottom rows count,
/// so a quoted indicator in the transcript cannot match.
pub fn working(pane: &str) -> bool {
    let pane = strip_ansi(pane);
    let rows: Vec<_> = pane.lines().filter(|l| !l.trim().is_empty()).collect();
    let bottom = rows[rows.len().saturating_sub(10)..]
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    ["to interrupt)", "Guide Devin while it works"]
        .iter()
        .any(|s| bottom.contains(s))
}
/// Input submitted while Devin works waits in its queue, shown with `queued` and `send now`
/// until the turn ends; Enter on the empty composer sends it at once by interrupting the turn.
pub fn input_queued(pane: &str) -> bool {
    let pane = pane.to_lowercase();
    pane.contains("queued") && pane.contains("send now")
}
/// Permission state shown around an empty composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Bypass,
    /// Another indicator, or the release's indicator-free Normal mode under a rendered footer.
    NotBypass,
    Unknown,
}
/// 3000.11.3 prints `(bypass permissions on)` above the composer and no indicator in Normal
/// mode. Only rows near the last placeholder count, so resumed history cannot match.
pub fn permission_mode(pane: &str) -> PermissionMode {
    let pane = strip_ansi(pane);
    let lines: Vec<_> = pane.lines().collect();
    let Some(composer) = lines.iter().rposition(|line| composer_ready(line)) else {
        return PermissionMode::Unknown;
    };
    let region = lines[composer.saturating_sub(4)..]
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if ["bypass permissions on", "bypass mode", "dangerous mode"]
        .iter()
        .any(|s| region.contains(s))
    {
        PermissionMode::Bypass
    } else if [
        "accept edits on",
        "smart mode on",
        "bash mode",
        "handoff mode",
        "autonomous",
        "plan mode",
        "normal mode",
    ]
    .iter()
    .any(|s| region.contains(s))
        || lines[composer + 1..]
            .iter()
            .any(|line| line.contains("Context:"))
    {
        PermissionMode::NotBypass
    } else {
        PermissionMode::Unknown
    }
}
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}
pub fn draft_visible(pane: &str, needle: &str) -> bool {
    let region = plain(&composer_region(pane));
    !needle.is_empty() && region.contains(&plain(needle))
}
pub fn paste_payload(text: &str) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = Vec::new();
    for c in normalized.chars() {
        if c == '\n' {
            out.push(b'\r');
        } else if c == '\t' || !c.is_control() {
            out.extend(c.to_string().as_bytes());
        }
    }
    out.push(b'\r');
    out
}
pub fn delivered_text(text: &str) -> String {
    String::from_utf8_lossy(&paste_payload(text)).replace('\r', "\n")
}
pub fn needle(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .chars()
        .take_while(|c| !c.is_control())
        .take(24)
        .collect()
}
