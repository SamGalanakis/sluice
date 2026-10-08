//! An engine's account problems, shared by every adapter: usage limits and auth failures.
//!
//! A hard usage cap fails the run at once as `QuotaExhausted`, an auth failure as `AuthFailed`;
//! neither is retried or replayed, since no turn can run until the owner acts. A short rate
//! limit stays `Transient` and, when the engine reported its reset, retries just after that
//! reset instead of after the fixed backoff.
//!
//! A limit is a hard cap when the engine names a usage, plan, spend or credit limit whose reset
//! is unknown, or when any limit's reset is further away than the quota threshold (15 minutes
//! by default, `SLUICE_AGENT_QUOTA_RESET_MIN`). A limit whose reset is within the threshold,
//! and a rate limit with no known reset, is a short rate limit.
//!
//! Every message has one shape: `<engine>: <what happened> — <what to do>, then step_retry.
//! <Engine> said: <the engine's text>`, with anything token-like in that text masked.
use super::{EngineError, EngineErrorKind};
use std::{
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// A limit that resets further away than this is a hard cap.
pub const DEFAULT_THRESHOLD: Duration = Duration::from_secs(15 * 60);
/// How long after a known reset a short rate limit retries.
pub const RESET_MARGIN: u64 = 2;
/// The most of an engine's own text a message carries.
const MAX_TEXT: usize = 1000;

/// The engine a problem came from, with its owner-facing names and fixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Codex,
    Claude,
    Devin,
}
impl Engine {
    /// `codex`, as the message starts.
    pub fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Devin => "devin",
        }
    }
    /// `Codex`, as in `Codex said: …`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::Devin => "Devin",
        }
    }
    fn login(self) -> &'static str {
        match self {
            Self::Codex => "run `codex login` on this host",
            Self::Claude => "run `claude auth login` on this host (or /login inside `claude`)",
            Self::Devin => "run `devin auth login` on this host (or /login inside `devin`)",
        }
    }
    fn buy(self) -> &'static str {
        match self {
            Self::Codex => "buy credits at https://chatgpt.com/codex/settings/usage",
            Self::Claude => "buy usage credits at https://claude.ai/settings/usage",
            Self::Devin => {
                "buy usage or turn on auto-reload at https://app.devin.ai/settings/usage"
            }
        }
    }
}

/// What the engine says ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    /// A usage, plan, spend or credit limit: without a known reset it is a hard cap.
    Usage,
    /// A rate limit (an HTTP 429 and the like): without a known reset it is short.
    Rate,
}
/// One limit an engine reported, before classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limit {
    pub engine: Engine,
    pub cap: Cap,
    /// What ran out, in words (`weekly usage quota exhausted`); by default `usage limit
    /// reached` or `rate limit reached`.
    pub what: Option<String>,
    /// The fix in place of the engine's default (`ask the workspace owner to add credits`).
    pub advice: Option<String>,
    /// The engine's own text.
    pub text: String,
    /// The limit's window when the engine named it, such as `weekly window`.
    pub window: Option<String>,
    /// The Unix second the limit resets, when the engine reported it.
    pub resets_at: Option<u64>,
}
impl Limit {
    pub fn new(engine: Engine, cap: Cap, text: impl Into<String>) -> Self {
        Self {
            engine,
            cap,
            what: None,
            advice: None,
            text: text.into(),
            window: None,
            resets_at: None,
        }
    }
    /// Classifies against `now` (Unix seconds) and the engine's threshold.
    pub fn classify(&self, now: u64, threshold: Duration) -> EngineError {
        let hard = match self.resets_at {
            Some(at) => at.saturating_sub(now) > threshold.as_secs(),
            None => self.cap == Cap::Usage,
        };
        let what = self.what.clone().unwrap_or_else(|| {
            match (hard, self.cap) {
                (_, Cap::Usage) => "usage limit reached",
                (true, Cap::Rate) => "rate limit reached",
                (false, Cap::Rate) => "rate limited",
            }
            .into()
        });
        let mut head = format!("{}: {what}", self.engine.id());
        if let Some(window) = &self.window {
            head.push_str(&format!(" ({window})"));
        }
        if let Some(at) = self.resets_at {
            head.push_str(&format!("; resets {} ({})", stamp(at), relative(at, now)));
        }
        if hard {
            let advice = self
                .advice
                .clone()
                .unwrap_or_else(|| self.engine.buy().into());
            let todo = if self.resets_at.is_some() {
                format!("wait for the reset or {advice}")
            } else {
                format!("{advice}, or wait for it to reset")
            };
            return EngineError {
                kind: EngineErrorKind::QuotaExhausted,
                message: said(
                    format!("{head} — {todo}, then step_retry (or run the step on another engine)"),
                    self.engine,
                    &self.text,
                ),
                retry_at: None,
            };
        }
        let retry_at = self.resets_at.map(|at| at.max(now) + RESET_MARGIN);
        let todo = if retry_at.is_some() {
            "retrying just after the reset"
        } else {
            "retrying after the standard backoff"
        };
        EngineError {
            kind: EngineErrorKind::Transient,
            message: said(format!("{head} — {todo}"), self.engine, &self.text),
            retry_at,
        }
    }
}

/// The engine cannot authenticate: logged out, a token expired or revoked, or the account
/// barred. Nothing works until the owner signs in again on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Auth {
    pub engine: Engine,
    /// What happened (`not logged in (token revoked)`); see `login_problem`.
    pub problem: String,
    /// The fix in place of the engine's login command.
    pub advice: Option<String>,
    /// The engine's own text.
    pub text: String,
}
impl Auth {
    /// A login problem, named from the engine's text.
    pub fn login(engine: Engine, text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            engine,
            problem: login_problem(&text),
            advice: None,
            text,
        }
    }
    pub fn error(&self) -> EngineError {
        let advice = self
            .advice
            .clone()
            .unwrap_or_else(|| self.engine.login().into());
        EngineError {
            kind: EngineErrorKind::AuthFailed,
            message: said(
                format!(
                    "{}: {} — {advice}, then step_retry",
                    self.engine.id(),
                    self.problem
                ),
                self.engine,
                &self.text,
            ),
            retry_at: None,
        }
    }
}
/// `not logged in (<why>)`, the why read from the engine's own words.
pub fn login_problem(text: &str) -> String {
    let lower = text.to_lowercase();
    let why = [
        ("revoked", "token revoked"),
        ("invalidated", "token revoked"),
        (
            "signed in to another account",
            "signed out or switched account elsewhere",
        ),
        ("session has ended", "session ended"),
        ("expired", "token expired"),
        ("could not be refreshed", "token could not be refreshed"),
        ("no longer authenticated", "session no longer authenticated"),
        ("invalid api key", "invalid API key"),
        ("missing bearer", "no credentials"),
        ("not logged in", "no credentials"),
        ("not signed in", "no credentials"),
        ("please log in", "no credentials"),
        ("401", "401 Unauthorized"),
    ]
    .iter()
    .find(|(needle, _)| lower.contains(needle))
    .map_or("unauthorized", |(_, why)| why);
    format!("not logged in ({why})")
}
fn said(head: String, engine: Engine, text: &str) -> String {
    let text = redact(text.trim());
    if text.is_empty() {
        return format!("{head}.");
    }
    let text = if text.chars().count() > MAX_TEXT {
        format!("{}…", text.chars().take(MAX_TEXT).collect::<String>())
    } else {
        text
    };
    format!("{head}. {} said: {text}", engine.name())
}

/// Masks anything token-like: a run of token characters with a credential prefix (`sk-`,
/// `eyJ`, `ghp_`, …), one of 40 or more with letters and digits, or one with a digit right
/// after a credential word (`Bearer`, `token=`, `"api_key": "…"`). Ids such as a 32-hex trace
/// id, a `req_…` request id or a UUID stay.
pub fn redact(text: &str) -> String {
    const PREFIXES: [&str; 17] = [
        "sk-",
        "sk_",
        "rk_",
        "pk_",
        "ghp_",
        "gho_",
        "ghs_",
        "ghu_",
        "github_pat_",
        "glpat-",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "ASIA",
        "eyJ",
        "ya29.",
        "sess-",
    ];
    const WORDS: [&str; 13] = [
        "bearer",
        "token",
        "api_key",
        "apikey",
        "api-key",
        "secret",
        "password",
        "cookie",
        "authorization",
        "access_token",
        "refresh_token",
        "id_token",
        "session_token",
    ];
    let token = |c: char| c.is_ascii_alphanumeric() || "-_.~+".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(token) {
        out.push_str(&rest[..start]);
        let run_len = rest[start..]
            .find(|c| !token(c))
            .unwrap_or(rest.len() - start);
        let run = &rest[start..start + run_len];
        let core = run.trim_end_matches('.');
        let digit = core.bytes().any(|b| b.is_ascii_digit());
        let alpha = core.bytes().any(|b| b.is_ascii_alphabetic());
        // what comes just before it (a credential word, then its separators): the output's
        // last few characters, never the whole of it again, so a long text masks in one pass
        let mut from = out.len().saturating_sub(64);
        while !out.is_char_boundary(from) {
            from += 1;
        }
        let before = out[from..]
            .trim_end_matches([' ', ':', '=', '"', '\''])
            .to_ascii_lowercase();
        let secret = (core.len() >= 16 && PREFIXES.iter().any(|p| core.starts_with(p)))
            || (core.len() >= 40 && digit && alpha)
            || (core.len() >= 12 && digit && WORDS.iter().any(|w| before.ends_with(w)));
        if secret {
            out.push_str("[redacted]");
            out.push_str(&run[core.len()..]);
        } else {
            out.push_str(run);
        }
        rest = &rest[start + run_len..];
    }
    out.push_str(rest);
    out
}

/// The current Unix second.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// The quota threshold from `SLUICE_AGENT_QUOTA_RESET_MIN` (minutes), or the default.
pub fn threshold_from_env() -> io::Result<Duration> {
    match std::env::var("SLUICE_AGENT_QUOTA_RESET_MIN") {
        Ok(raw) => parse_threshold(&raw),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_THRESHOLD),
        Err(_) => Err(io::Error::other(
            "SLUICE_AGENT_QUOTA_RESET_MIN is not UTF-8",
        )),
    }
}
/// Parses a threshold in minutes: finite and not negative (0 makes every known reset a hard
/// cap).
pub fn parse_threshold(raw: &str) -> io::Result<Duration> {
    let invalid =
        || io::Error::other("SLUICE_AGENT_QUOTA_RESET_MIN: expected finite minutes, at least 0");
    let minutes: f64 = raw.trim().parse().map_err(|_| invalid())?;
    if !minutes.is_finite() || minutes < 0.0 {
        return Err(invalid());
    }
    Duration::try_from_secs_f64(minutes * 60.0).map_err(|_| invalid())
}
/// Names a window from its length in minutes: `weekly window`, `5-hour window`.
pub fn window(minutes: u64) -> String {
    let name = match minutes {
        10080 => "weekly".into(),
        1440 => "daily".into(),
        m if m > 0 && m % 1440 == 0 => format!("{}-day", m / 1440),
        m if m > 0 && m % 60 == 0 => format!("{}-hour", m / 60),
        m => format!("{m}-minute"),
    };
    format!("{name} window")
}
/// `2026-10-12T15:04Z`, in UTC to the minute.
pub fn stamp(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|at| time::OffsetDateTime::from_unix_timestamp(at).ok())
        .map(|t| {
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}Z",
                t.year(),
                u8::from(t.month()),
                t.day(),
                t.hour(),
                t.minute()
            )
        })
        .unwrap_or_else(|| format!("unix {at}"))
}
/// `in 6d 23h`, `in 3h`, `in 2h 5m`, `in 12m`, `in 40s` or `now`.
pub fn relative(at: u64, now: u64) -> String {
    let left = at.saturating_sub(now);
    let (d, h, m, s) = (
        left / 86400,
        left % 86400 / 3600,
        left % 3600 / 60,
        left % 60,
    );
    match left {
        0 => "now".into(),
        1..60 => format!("in {s}s"),
        60..3600 => format!("in {m}m"),
        3600..86400 if m == 0 => format!("in {h}h"),
        3600..86400 => format!("in {h}h {m}m"),
        _ if h == 0 => format!("in {d}d"),
        _ => format!("in {d}d {h}h"),
    }
}
