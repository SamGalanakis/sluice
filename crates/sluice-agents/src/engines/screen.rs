//! Interactive screens that block a pane-driven engine (Claude Code, Devin) before its first
//! turn, and the pane's text at a failure.
//!
//! An engine that stops on a screen sluice does not answer (first-run setup, a terms or
//! settings approval, an organization picker, …) can take no input, so the run fails at once
//! as `BlockedScreen`, naming the screen and what the owner must do, then quoting it. A screen
//! no adapter recognizes fails the same way once it has stood unchanged, before any turn, for
//! the grace (`SLUICE_AGENT_SCREEN_S`, 20 s): a normal start draws its composer within a few
//! seconds, and anything slower (a connectivity check, MCP servers connecting, a long session
//! loading) animates a spinner, which restarts the clock.
//!
//! The message has one shape: `<engine>: blocked on <screen> — <what to do>, then step_retry.
//! <Engine> showed: <the screen's rows, joined by " | ">`, with anything token-like masked.
use super::{EngineError, EngineErrorKind, account};
use std::{
    io,
    time::{Duration, Instant},
};

/// How long an unrecognized screen may stand unchanged before any turn.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(20);
/// The most of a screen's text a message quotes.
const MAX_TEXT: usize = 1000;
/// The rows of the pane at a failure a message ends with.
pub const TAIL_ROWS: usize = 6;
/// The file, in the invocation's directory, that keeps the pane at a failure.
pub const PANE_AT_FAILURE: &str = "pane-at-failure.txt";

/// A screen the engine stopped on, with what the owner must do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub engine: account::Engine,
    /// What blocks, as in `blocked on <what>`: `its first-run setup (theme picker)`.
    pub what: String,
    /// What the owner must do: `run \`claude\` once on this host …`.
    pub advice: String,
    /// The pane.
    pub pane: String,
}
impl Screen {
    pub fn error(&self) -> EngineError {
        let head = format!(
            "{}: blocked on {} — {}, then step_retry",
            self.engine.id(),
            self.what,
            self.advice
        );
        let text = quote(&self.pane);
        EngineError {
            kind: EngineErrorKind::BlockedScreen,
            message: if text.is_empty() {
                format!("{head}.")
            } else {
                format!("{head}. {} showed: {text}", self.engine.name())
            },
            retry_at: None,
        }
    }
    /// A screen no adapter recognizes, unchanged for `grace` before any turn.
    pub fn unknown(engine: account::Engine, grace: Duration, pane: &str) -> Self {
        Self {
            engine,
            what: format!(
                "a screen sluice does not recognize, unchanged for {}s before any turn",
                grace.as_secs_f64().round()
            ),
            advice: format!(
                "run `{}` once in the step's cwd on this host and answer it (or attach to the run's private pane while it waits)",
                engine.id()
            ),
            pane: pane.into(),
        }
    }
}

/// The screen's non-empty rows, trimmed and joined by ` | `, with anything token-like masked
/// and cut to 1000 characters.
pub fn quote(pane: &str) -> String {
    let text = account::redact(
        &pane
            .lines()
            .map(str::trim)
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>()
            .join(" | "),
    );
    if text.chars().count() > MAX_TEXT {
        format!("{}…", text.chars().take(MAX_TEXT).collect::<String>())
    } else {
        text
    }
}
/// Whether a row of the pane starts with `text`, past its indent and any dialog border
/// (`│`, `┃`) or selection mark (`❯`, `>`).
pub fn row(pane: &str, text: &str) -> bool {
    pane.lines().any(|row| {
        row.trim_start_matches(|c: char| c.is_whitespace() || "│┃❯>".contains(c))
            .starts_with(text)
    })
}

/// Watches for a screen that stands unchanged. The clock restarts whenever the pane changes,
/// so a spinner or a progressing startup never trips it.
#[derive(Debug, Default)]
pub struct Watch {
    seen: Option<(String, Instant)>,
}
impl Watch {
    /// Whether `pane` has stood unchanged for `grace` as of `now`.
    pub fn stands(&mut self, pane: &str, grace: Duration, now: Instant) -> bool {
        let pane = normalize(pane);
        match &self.seen {
            Some((seen, since)) if *seen == pane => now.saturating_duration_since(*since) >= grace,
            _ => {
                self.seen = Some((pane, now));
                false
            }
        }
    }
    pub fn reset(&mut self) {
        self.seen = None;
    }
}
fn normalize(pane: &str) -> String {
    pane.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
}

/// The pane's text at a failure, made safe to keep: anything token-like masked.
pub fn evidence(pane: &str) -> String {
    let text = account::redact(&normalize(pane));
    format!("{text}\n")
}
/// The last `TAIL_ROWS` non-empty rows of `evidence`, trimmed.
pub fn tail(evidence: &str) -> Vec<String> {
    let rows: Vec<_> = evidence
        .lines()
        .map(str::trim)
        .filter(|row| !row.is_empty())
        .map(|row| {
            if row.chars().count() > 200 {
                format!("{}…", row.chars().take(200).collect::<String>())
            } else {
                row.to_owned()
            }
        })
        .collect();
    rows[rows.len().saturating_sub(TAIL_ROWS)..].to_vec()
}

/// The grace from `SLUICE_AGENT_SCREEN_S` (seconds), or the default.
pub fn grace_from_env() -> io::Result<Duration> {
    match std::env::var("SLUICE_AGENT_SCREEN_S") {
        Ok(raw) => parse_grace(&raw),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_GRACE),
        Err(_) => Err(io::Error::other("SLUICE_AGENT_SCREEN_S is not UTF-8")),
    }
}
/// Parses a grace in seconds: finite and positive.
pub fn parse_grace(raw: &str) -> io::Result<Duration> {
    let invalid = || io::Error::other("SLUICE_AGENT_SCREEN_S: expected finite positive seconds");
    let seconds: f64 = raw.trim().parse().map_err(|_| invalid())?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(invalid());
    }
    Duration::try_from_secs_f64(seconds).map_err(|_| invalid())
}
