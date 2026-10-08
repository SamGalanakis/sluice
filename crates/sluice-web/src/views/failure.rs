//! What a failed step's stored error says, read for a person: whether the owner cancelled it,
//! one plain sentence from its failure kind, what it said, and the pane its agent left. The
//! shared status helper the plan, the index and the step page all read "cancelled" from: a
//! cancel is a failed step to the store (no record kind or failure variant of its own), told
//! apart here by its stored error.
use serde::Serialize;
use sluice_model::error::PublicError;

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Failure {
    /// The owner (or an agent acting for them) cancelled it: not a failure of the work.
    pub cancelled: bool,
    /// The stored kind: `WallCap`, `fn_failure`, `cancelled`, …
    pub kind: String,
    /// One plain sentence: "Stopped at its wall-clock cap after 10h 0m."
    pub headline: String,
    /// What it said, without the pane dump; empty when the headline already says it all.
    pub said: String,
    /// The last rows of its agent's pane when it failed.
    pub pane: String,
    /// Where the whole screen was written, as the error names it.
    pub pane_file: String,
    pub session: String,
}

/// A stored error (the step's or a run's `error` JSON) is a cancel: its kind `cancelled`, an
/// agent's `Cancelled`, or a fn's message in the cancel's own words (`cancelled: <reason>`,
/// which `step_cancel` documents).
pub fn is_cancel(error: &PublicError) -> bool {
    match error {
        PublicError::Cancelled { .. } => true,
        PublicError::AgentFailure { kind, .. } => kind == "Cancelled",
        PublicError::FnFailure { message } => cancel_reason(message).is_some(),
        _ => false,
    }
}
fn cancel_reason(message: &str) -> Option<&str> {
    message
        .strip_prefix("cancelled: ")
        .or_else(|| (message == "cancelled").then_some(""))
}

impl Failure {
    /// A stored error's JSON; a message that is not JSON reads as a fn's message.
    pub fn parse(stored: &str, took: Option<f64>) -> Self {
        match serde_json::from_str::<PublicError>(stored) {
            Ok(error) => Self::new(&error, took),
            Err(_) => Self::new(
                &PublicError::FnFailure {
                    message: stored.to_owned(),
                },
                took,
            ),
        }
    }
    /// `took`: how long its run ran, for the caps' sentences.
    pub fn new(error: &PublicError, took: Option<f64>) -> Self {
        let (message, session) = match error {
            PublicError::AgentFailure {
                message, session, ..
            }
            | PublicError::ExitedWithoutSubmit { message, session } => {
                (message.as_str(), session.clone().unwrap_or_default())
            }
            other => (message_of(other), String::new()),
        };
        let (said, pane, pane_file) = split_pane(message);
        let after = took
            .map(|s| format!(" after {}", super::step::short_duration(s)))
            .unwrap_or_default();
        let (kind, headline): (String, String) = match error {
            PublicError::Cancelled { message } => (
                "cancelled".into(),
                if message == "cancel requested" || message.is_empty() {
                    "Cancelled while it ran.".into()
                } else {
                    format!("Cancelled: {}", sentence(message))
                },
            ),
            PublicError::AgentFailure { kind, .. } => {
                let headline = match kind.as_str() {
                    "Cancelled" => "Cancelled while it ran.".into(),
                    "WallCap" => format!("Stopped at its wall-clock cap{after}."),
                    "StallCap" => format!("Stopped: its agent wrote nothing for too long{after}."),
                    "ReadyTimeout" => "Its engine never became ready.".into(),
                    "TurnStartTimeout" => {
                        "Its engine did not start a turn after its input was delivered.".into()
                    }
                    "ExitedWithoutSubmit" => {
                        "Its agent stopped without submitting its outputs.".into()
                    }
                    "QuotaExhausted" => "Its engine hit a usage cap.".into(),
                    "AuthFailed" => "Its engine could not sign in.".into(),
                    "BlockedScreen" => "Its engine stopped on a screen it cannot pass.".into(),
                    "EngineExited" => format!("Its engine exited{after}."),
                    "Transient" => "A passing failure outlasted its retries.".into(),
                    "MissingSession" => "Its engine's session could not be found.".into(),
                    "LockConflict" => "Another run held its session's lock.".into(),
                    "SessionCwd" => "Its session belongs to another working directory.".into(),
                    "CapabilityMismatch" => "Its engine lacks something sluice needs.".into(),
                    "UnknownAcceptance" => {
                        "Sluice could not confirm that its engine took its input.".into()
                    }
                    "Cleanup" => "Its run could not be cleaned up.".into(),
                    "Invalid" => "Its run's request was invalid.".into(),
                    _ => format!("Its agent failed{after}."),
                };
                (kind.clone(), headline)
            }
            PublicError::ExitedWithoutSubmit { .. } => (
                "exited_without_submit".into(),
                "Its agent stopped without submitting its outputs.".into(),
            ),
            PublicError::FnFailure { message } => match cancel_reason(message) {
                Some("") => ("cancelled".into(), "Cancelled while it ran.".into()),
                Some(reason) => (
                    "cancelled".into(),
                    format!("Cancelled: {}", sentence(reason)),
                ),
                None => (
                    "fn_failure".into(),
                    format!("Its fn failed: {}", sentence(first_line(&said))),
                ),
            },
            PublicError::ProcessLost { .. } => {
                ("process_lost".into(), "Its process was lost.".into())
            }
            PublicError::Rejected { .. } => {
                ("rejected".into(), "Its fn refused its inputs.".into())
            }
            PublicError::Invalid { .. } => (
                "invalid".into(),
                "Its outputs did not match their declarations.".into(),
            ),
            PublicError::Transient { .. } => (
                "transient".into(),
                "A passing failure outlasted its retries.".into(),
            ),
            _ => ("failed".into(), "It failed.".into()),
        };
        let mut said = said;
        if let PublicError::Invalid { errors, .. } = error {
            for e in errors {
                said.push('\n');
                said.push_str(e);
            }
        }
        // what the headline already quotes is not said twice
        let cancelled = is_cancel(error);
        let quoted = (kind == "fn_failure" && !said.contains('\n')) || cancelled;
        Self {
            cancelled,
            kind,
            headline,
            said: if quoted { String::new() } else { said },
            pane,
            pane_file,
            session,
        }
    }
    /// A cancel's sentence under its "Cancelled" head: the reason, without the word again.
    pub fn cancel_words(&self) -> &str {
        self.headline
            .strip_prefix("Cancelled: ")
            .unwrap_or(&self.headline)
    }
}
fn message_of(error: &PublicError) -> &str {
    match error {
        PublicError::BadRequest { message }
        | PublicError::NotFound { message }
        | PublicError::Conflict { message, .. }
        | PublicError::Invalid { message, .. }
        | PublicError::Busy { message, .. }
        | PublicError::Storage { message }
        | PublicError::CursorExpired { message }
        | PublicError::ProcessLost { message }
        | PublicError::Cancelled { message }
        | PublicError::FnFailure { message }
        | PublicError::Transient { message }
        | PublicError::Rejected { message }
        | PublicError::AgentFailure { message, .. }
        | PublicError::ExitedWithoutSubmit { message, .. } => message,
        _ => "",
    }
}
fn first_line(text: &str) -> &str {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}
/// A message as the end of a sentence: its first letter kept, a full stop when it has none.
fn sentence(text: &str) -> String {
    let text = first_line(text);
    if text.ends_with(['.', '!', '?', ')']) || text.is_empty() {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}
/// The message, and the pane the supervisor appended to it: "\npane at failure (last rows;
/// whole screen: <path>):\n  <row>\n  <row>".
fn split_pane(message: &str) -> (String, String, String) {
    const HEAD: &str = "\npane at failure";
    let Some(at) = message.find(HEAD) else {
        return (message.trim_end().to_owned(), String::new(), String::new());
    };
    let said = message[..at].trim_end().to_owned();
    let rest = &message[at + 1..];
    let (header, rows) = rest.split_once('\n').unwrap_or((rest, ""));
    let file = header
        .split_once("whole screen: ")
        .map(|(_, p)| p.trim_end_matches("):").trim_end_matches(')').to_owned())
        .unwrap_or_default();
    let pane = rows
        .lines()
        .map(|l| l.strip_prefix("  ").unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned();
    (said, pane, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_wall_cap_leads_with_its_sentence_and_folds_its_pane() {
        let stored = r#"{"error":"agent_failure","kind":"WallCap","message":"engine operation deadline exceeded\npane at failure (last rows; whole screen: /h/runs/r/invocations/i/pane-at-failure.txt):\n  ✻ Brewed for 2m\n  > done","session":"s-1"}"#;
        let f = Failure::parse(stored, Some(36_000.0));
        assert!(!f.cancelled);
        assert_eq!(f.headline, "Stopped at its wall-clock cap after 10h 0m.");
        assert_eq!(f.said, "engine operation deadline exceeded");
        assert_eq!(f.pane, "✻ Brewed for 2m\n> done");
        assert_eq!(f.pane_file, "/h/runs/r/invocations/i/pane-at-failure.txt");
        assert_eq!(f.session, "s-1");
    }
    #[test]
    fn every_cancel_reads_as_a_cancel() {
        for stored in [
            r#"{"error":"cancelled","message":"cancel requested"}"#,
            r#"{"error":"agent_failure","kind":"Cancelled","message":"cancelled during transient backoff"}"#,
            r#"{"error":"fn_failure","message":"cancelled: pivot: audit instead (Sam)"}"#,
        ] {
            let f = Failure::parse(stored, None);
            assert!(f.cancelled, "{stored}");
            assert!(f.headline.starts_with("Cancelled"), "{}", f.headline);
        }
        let f = Failure::parse(
            r#"{"error":"fn_failure","message":"cancelled: pivot: audit instead (Sam)"}"#,
            None,
        );
        assert_eq!(f.headline, "Cancelled: pivot: audit instead (Sam)");
        assert!(
            !Failure::parse(r#"{"error":"fn_failure","message":"exit code 1"}"#, None).cancelled
        );
    }
}
