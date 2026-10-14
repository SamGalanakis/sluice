//! Who speaks and who is addressed (SPEC §8.1), in one place for MCP, the CLI and the store.
//!
//! - A session (an orchestrator's MCP connection or shell) names itself `@<name>`
//!   (`^@[a-z][a-z0-9-]{0,39}$`); the name is its identity, kept by a successor that reuses it.
//! - The person is `owner`: only the dashboard speaks as it.
//! - A worker run is its step: its step id, which never holds `@`.
//! - `sluice` is the system's own (alerts); nobody speaks as it or addresses it.
//!
//! The caller's identity resolves in order: the `me` argument, `SLUICE_ME` (the CLI), the MCP
//! session's binding (the first `me` an MCP session gives). Without one a session is anonymous:
//! it may read, but `ask`, `say` and `reply` refuse it ([`NAME_THIS_SESSION`]).
use crate::error::PublicError;
use std::fmt;

/// The person, at the dashboard.
pub const OWNER: &str = "owner";
/// The system's own sender.
pub const SLUICE: &str = "sluice";
/// The retired shared address. Runs pinned to an older release still send it until they
/// drain; from a run it means the run's unit's owner (`sluice_store::identities::route_run`).
/// A session that names it is refused.
pub const LEGACY_ORCHESTRATOR: &str = "orchestrator";
/// The CLI's identity variable.
pub const SLUICE_ME: &str = "SLUICE_ME";
/// The refusal of an anonymous session's message.
pub const NAME_THIS_SESSION: &str =
    "name this session: pass me=@name or set SLUICE_ME (for example me=\"@lash-f4\")";
/// How long since its reader's last heartbeat (`next` beats every 30 s while it waits) a
/// session still counts as reading: a message to it after that carries a delivery note.
pub const LIVE_READER_MINUTES: i64 = 10;
/// The longest session name, `@` excluded.
const NAME_MAX: usize = 40;

/// A session's identity, `@<name>`, checked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionName(String);
impl SessionName {
    /// A session name as `me` gives it: refused (`invalid`, naming `me`) unless it is `@<name>`.
    pub fn parse(text: &str) -> Result<Self, PublicError> {
        Self::parse_as("me", text)
    }
    /// As `parse`, naming `field` in the refusal.
    pub fn parse_as(field: &str, text: &str) -> Result<Self, PublicError> {
        let text = text.trim();
        if is_session(text) {
            return Ok(Self(text.to_owned()));
        }
        Err(refusal(field, &why_not_session(text)))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Whether `text` is a session identity: `@`, a lowercase letter, then at most 39 lowercase
/// letters, digits or `-`.
pub fn is_session(text: &str) -> bool {
    let Some(name) = text.strip_prefix('@') else {
        return false;
    };
    name.len() <= NAME_MAX
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// Why `text` is not a session name, saying how to name one.
fn why_not_session(text: &str) -> String {
    match text {
        "" => NAME_THIS_SESSION.into(),
        OWNER => "owner is the person at the dashboard, and only the dashboard speaks as the owner; name this session me=@name".into(),
        LEGACY_ORCHESTRATOR => "orchestrator is no longer an identity: every session names itself, me=@name or SLUICE_ME=@name (for example @lash-f4)".into(),
        SLUICE => "sluice is the system's own sender; name this session me=@name".into(),
        name if is_session(&format!("@{name}")) => {
            format!("a session name starts with @: me=@{name}")
        }
        name => format!(
            "{name:?} is not a session name: @ then a lowercase letter, then at most 39 lowercase letters, digits or -, for example @lash-f4"
        ),
    }
}

/// `invalid`, with `field: message` as its one path error.
pub fn refusal(field: &str, message: &str) -> PublicError {
    PublicError::Invalid {
        message: message.to_owned(),
        errors: vec![format!("{field}: {message}")],
    }
}

/// The caller's identity: the `me` argument, else `SLUICE_ME` (the CLI's environment), else
/// the MCP session's binding; `None` when it gave none. A given one that is not a session name
/// is refused, as is `me` naming a session other than the one this MCP session is bound to.
pub fn resolve_me(
    arg: Option<&str>,
    env: Option<&str>,
    bound: Option<&SessionName>,
) -> Result<Option<SessionName>, PublicError> {
    let given = arg.map(str::trim).filter(|s| !s.is_empty());
    if let (Some(given), Some(bound)) = (given, bound)
        && given != bound.as_str()
    {
        return Err(refusal(
            "me",
            &format!(
                "this MCP session is {bound}; a session keeps one name (start a new session to use {given})"
            ),
        ));
    }
    match given.or(env.map(str::trim).filter(|s| !s.is_empty())) {
        Some(text) => SessionName::parse(text).map(Some),
        None => Ok(bound.cloned()),
    }
}

/// Who a message is addressed to, as a session or the dashboard gives `to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    /// `to` left out (or blank).
    Omitted,
    Owner,
    Session(SessionName),
    /// `orchestrator`: from a run, its unit's owner; from anyone else, refused.
    LegacyOrchestrator,
    /// Anything else: a step id of the plan, or nothing at all (the store decides).
    Step(String),
}
impl Recipient {
    pub fn of(to: &str) -> Result<Self, PublicError> {
        let to = to.trim();
        Ok(match to {
            "" => Self::Omitted,
            OWNER => Self::Owner,
            LEGACY_ORCHESTRATOR => Self::LegacyOrchestrator,
            SLUICE => {
                return Err(refusal(
                    "to",
                    "sluice is the system's own sender and takes no messages",
                ));
            }
            to if to.starts_with('@') => Self::Session(SessionName::parse_as("to", to)?),
            to => Self::Step(to.to_owned()),
        })
    }
}
