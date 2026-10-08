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
    /// The fn's Python traceback, folded under its headline.
    pub trace: String,
    /// How to resume its agent's session, as the failure says it ("To resume it, …").
    pub resume: String,
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
/// A stored error (JSON, or a bare message from an older release) is a cancel.
pub fn stored_is_cancel(stored: &str) -> bool {
    match serde_json::from_str::<PublicError>(stored) {
        Ok(error) => is_cancel(&error),
        Err(_) => cancel_reason(stored).is_some(),
    }
}
/// The project's steps that have stopped (failed or stale), each with whether it was a cancel:
/// what the Units table and anything else naming a step's state reads "cancelled" from.
pub fn stopped_steps(
    sql: &rusqlite::Connection,
    project: &str,
) -> rusqlite::Result<std::collections::BTreeMap<String, bool>> {
    let mut q = sql.prepare_cached(
        "SELECT step_id,status,error FROM steps WHERE project_id=?1 AND status IN ('failed','stale')",
    )?;
    let rows = q.query_map([project], |r| {
        let status: String = r.get(1)?;
        let error: Option<String> = r.get(2)?;
        Ok((
            r.get::<_, String>(0)?,
            status == "failed" && error.as_deref().is_some_and(stored_is_cancel),
        ))
    })?;
    rows.collect()
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
        // a fn's traceback: folded; its last exception line is the cause
        let (said, trace, cause, resume) = split_trace(&said);
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
                    "WallCap" => wall_cap(took),
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
                None => match &cause {
                    Some(cause) => ("fn_failure".into(), cause_sentence(cause, took)),
                    None => (
                        "fn_failure".into(),
                        format!("Its fn failed: {}", sentence(first_line(&said))),
                    ),
                },
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
        let quoted = (kind == "fn_failure" && cause.is_none() && !said.contains('\n')) || cancelled;
        // a captured tail (several lines) that starts mid-sentence says so
        if said.contains('\n')
            && said.starts_with(|c: char| c.is_lowercase())
            && !said
                .split_whitespace()
                .next()
                .is_some_and(|w| w.ends_with(':'))
        {
            said.insert(0, '…');
        }
        Self {
            cancelled,
            kind,
            headline,
            said: if quoted { String::new() } else { said },
            pane,
            pane_file,
            session,
            trace,
            resume,
        }
    }
    /// The resume hint with its tool call apart, so the call can be set as code: (before, call,
    /// after).
    pub fn resume_parts(&self) -> (&str, &str, &str) {
        let text = self.resume.as_str();
        match text.find("step_set_input(") {
            Some(start) => {
                let end = text[start..]
                    .find("), ")
                    .or_else(|| text[start..].rfind(')'))
                    .map_or(text.len(), |e| start + e + 1);
                (&text[..start], &text[start..end], &text[end..])
            }
            None => (text, "", ""),
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
/// A fn's message split at its Python traceback: what came before it (its `exit code` line
/// dropped), the traceback itself, its last exception's message (the cause) and any "To
/// resume …" hint after it.
fn split_trace(said: &str) -> (String, String, Option<String>, String) {
    const HEAD: &str = "Traceback (most recent call last):";
    let Some(at) = said.find(HEAD) else {
        return (said.to_owned(), String::new(), None, String::new());
    };
    let before = said[..at]
        .lines()
        .filter(|l| !(l.starts_with("exit code ") && l[10..].trim().parse::<i64>().is_ok()))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    let rest = &said[at..];
    // the exception line: unindented `Name: message` (or a bare `Name`), the last such
    let lines: Vec<&str> = rest.lines().collect();
    let found = lines.iter().rposition(|l| exception_name(l).is_some());
    let Some(found) = found else {
        return (before, rest.trim_end().to_owned(), None, String::new());
    };
    let cause = lines[found]
        .split_once(':')
        .map(|(_, m)| m.trim().to_owned());
    let trace = lines[..=found].join("\n");
    let after = lines[found + 1..].join("\n");
    let resume = after
        .find("To resume")
        .map(|i| after[i..].split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    (before, trace, cause.filter(|c| !c.is_empty()), resume)
}
/// An exception line's class (`RuntimeError: …`, `sluice.Rejected: …`): unindented, a dotted
/// name ending as Python's exceptions do.
fn exception_name(line: &str) -> Option<&str> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let name = line.split_once(':').map_or(line, |(n, _)| n).trim();
    let ident = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '_');
    let ends = [
        "Error",
        "Exception",
        "Exit",
        "Interrupt",
        "Warning",
        "Rejected",
        "Invalid",
    ]
    .iter()
    .any(|e| name.ends_with(e));
    (ident && ends).then_some(name)
}
/// A fn's exception as the step's sentence; a wall-clock cap reads as the agent's cap does.
fn cause_sentence(cause: &str, took: Option<f64>) -> String {
    if let Some(at) = cause.find("wall-clock cap of ") {
        let minutes: Option<f64> = cause[at + 18..]
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok());
        return wall_cap(minutes.map(|m| m * 60.0).or(took));
    }
    let mut chars = sentence(cause).chars().collect::<Vec<_>>();
    if let Some(c) = chars.first_mut() {
        *c = c.to_ascii_uppercase();
    }
    chars.into_iter().collect()
}
/// One wording for a run stopped at its wall-clock cap, an agent's or a fn's: "Stopped at its
/// wall-clock cap after 10h 0m." (the cap when the fn names it, else how long it ran).
fn wall_cap(seconds: Option<f64>) -> String {
    match seconds {
        Some(s) => format!(
            "Stopped at its wall-clock cap after {}.",
            super::step::short_duration(s)
        ),
        None => "Stopped at its wall-clock cap.".into(),
    }
}
/// A message as the end of a sentence: its first letter kept, a full stop when it has none (a
/// trailing colon or comma, which led into lines no longer shown, gives way to it).
fn sentence(text: &str) -> String {
    let text = first_line(text)
        .trim_end_matches([':', ';', ','])
        .trim_end();
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
    fn a_fn_traceback_reads_as_its_last_exception() {
        let stored = r#"{"error":"fn_failure","message":"exit code 1\nremains active.\ncodex: Still awaiting.\nTraceback (most recent call last):\n  File \"x.py\", line 1, in main\n    raise RuntimeError(f\"{a}\")\nRuntimeError: codex ran past the wall-clock cap of 600 min (SLUICE_AGENT_MAX_MIN)\nsession: s-9. To resume it, bind the step's session input to it and retry: step_set_input(project, step, \"session\", \"s-9\"), then step_retry."}"#;
        let f = Failure::parse(stored, None);
        assert_eq!(f.headline, "Stopped at its wall-clock cap after 10h 0m.");
        // the captured tail starts mid-sentence, and says so
        assert_eq!(f.said, "…remains active.\ncodex: Still awaiting.");
        assert!(f.trace.starts_with("Traceback") && f.trace.ends_with("(SLUICE_AGENT_MAX_MIN)"));
        assert!(f.resume.starts_with("To resume it, bind"), "{}", f.resume);
        let plain = Failure::parse(
            r#"{"error":"fn_failure","message":"Traceback (most recent call last):\n  File \"m.py\"\nValueError: no such branch: main2"}"#,
            None,
        );
        assert_eq!(plain.headline, "No such branch: main2.");
        // a first line that led into lines not shown ends as a sentence, not ":."
        let led = Failure::parse(
            r#"{"error":"fn_failure","message":"kiln clippy failed after the rebase onto 299cf4ab35:\n    = note: all struct fields"}"#,
            None,
        );
        assert_eq!(
            led.headline,
            "Its fn failed: kiln clippy failed after the rebase onto 299cf4ab35."
        );
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
