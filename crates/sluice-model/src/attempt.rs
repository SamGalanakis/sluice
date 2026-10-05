//! What an earlier attempt at a step left behind, and whether a running step is only
//! finishing. The store derives the facts from rows it keeps anyway (runs, results,
//! submissions, records); nothing here is stored. This renders them for `step_context` and
//! the top of an agent's task.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The most characters of one string value the note quotes.
pub const VALUE_CUT: usize = 200;
/// The most characters of the submission or outputs line.
pub const LINE_CUT: usize = 1200;
/// The most uncommitted paths the note names.
pub const PATHS: usize = 20;

/// A running step whose run has stored a valid submission: its agent's work is done and the
/// run is only finishing (an older release's supervisor may still be waiting out its grace).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finishing {
    /// When the submission was stored.
    pub since: String,
    /// The `step.submit` record's seq; null once the log has trimmed it.
    pub submission_seq: Option<i64>,
    /// The run's pinned release, short.
    pub release: String,
}

/// A release id `<git-sha>-<sha256>` as its first 12 git characters; any other id (a source
/// build's fallback) as it is.
pub fn short_release(id: &str) -> String {
    match id.split_once('-') {
        Some((sha, rest))
            if sha.len() >= 12
                && !rest.is_empty()
                && sha.chars().all(|c| c.is_ascii_hexdigit()) =>
        {
            sha[..12].to_owned()
        }
        _ => id.to_owned(),
    }
}

/// The git facts an agent fn's result reports (`git` output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSeen {
    pub head: String,
    pub head_before: Option<String>,
    pub commits: Option<u64>,
    pub dirty: Option<bool>,
}
impl GitSeen {
    /// From a `git` output value; None when it is not one.
    pub fn from_output(value: &Value) -> Option<Self> {
        let head = value.get("head_after")?.as_str()?.to_owned();
        Some(Self {
            head,
            head_before: value
                .get("head_before")
                .and_then(Value::as_str)
                .map(str::to_owned),
            commits: value.get("commits").and_then(Value::as_u64),
            dirty: value.get("dirty").and_then(Value::as_bool),
        })
    }
}

/// How the previous attempt ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    Succeeded,
    Failed,
    Cancelled,
    Lost,
    /// Settled on its submission by `step_settle`.
    Settled,
    /// It finished without a recorded result.
    Unknown,
}

/// The step's previous attempt (its run before this one), as the store derives it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PreviousAttempt {
    pub run: String,
    pub started: Option<String>,
    pub finished: Option<String>,
    pub ended: Ended,
    /// Its error, `{error, message, kind?}`, strings cut.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    /// Who cancelled or settled it, and why, when the log still says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Its last submission's fields, strings cut.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitted: Option<Value>,
    /// Its outputs when it submitted nothing, strings cut (`git` is in `git`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitSeen>,
}

/// The step's `cwd` as git sees it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkTree {
    pub cwd: String,
    #[serde(flatten)]
    pub state: WorkTreeState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "git", rename_all = "snake_case")]
pub enum WorkTreeState {
    Clean,
    /// `git status --porcelain`: how many entries, and the first PATHS of them.
    Dirty {
        count: usize,
        paths: Vec<String>,
    },
    /// git could not say (absent, too slow, failed): the launch goes on without it.
    Unavailable {
        reason: String,
    },
}

/// The whole picture: which attempt this is, the one before it, and the work tree.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttemptNote {
    /// This attempt's number at the step (1 for the first).
    pub number: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous: Option<PreviousAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorkTree>,
}

/// `value` with every string longer than `limit` characters cut, saying how much more there
/// was.
pub fn cut_strings(value: &Value, limit: usize) -> Value {
    match value {
        Value::String(s) if s.chars().count() > limit => Value::String(format!(
            "{}… [{} more characters]",
            s.chars().take(limit).collect::<String>(),
            s.chars().count() - limit
        )),
        Value::Array(items) => Value::Array(items.iter().map(|v| cut_strings(v, limit)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), cut_strings(v, limit)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn cut_line(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_owned();
    }
    format!(
        "{}… [{} more characters]",
        text.chars().take(limit).collect::<String>(),
        count - limit
    )
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

impl AttemptNote {
    /// The markdown section an agent's task starts with, and `step_context`'s `note`.
    pub fn text(&self) -> String {
        let mut lines = vec!["## Previous attempt".to_owned()];
        match &self.previous {
            None => lines.push("None: this is the first attempt at this step.".into()),
            Some(previous) => {
                let mut ended = match previous.ended {
                    Ended::Succeeded => "succeeded".to_owned(),
                    Ended::Failed => "failed".to_owned(),
                    Ended::Cancelled => "was cancelled".to_owned(),
                    Ended::Lost => "was lost".to_owned(),
                    Ended::Settled => "was settled on its submission".to_owned(),
                    Ended::Unknown => "ended without a recorded result".to_owned(),
                };
                if let Some(by) = &previous.by {
                    ended.push_str(&format!(" by {by}"));
                }
                if let Some(reason) = previous.reason.as_deref().filter(|r| !r.is_empty()) {
                    ended.push_str(&format!(" ({})", cut_line(reason, VALUE_CUT)));
                }
                if let Some(error) = &previous.error
                    && !matches!(previous.ended, Ended::Cancelled | Ended::Settled)
                {
                    let kind = error
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("error");
                    let sub = error
                        .get("kind")
                        .and_then(Value::as_str)
                        .map(|k| format!(" {k}"))
                        .unwrap_or_default();
                    let message = error.get("message").and_then(Value::as_str).unwrap_or("");
                    ended.push_str(&format!(
                        ": {kind}{sub}: {}",
                        cut_line(message.trim(), VALUE_CUT)
                    ));
                }
                let when = match (&previous.started, &previous.finished) {
                    (Some(started), Some(finished)) => format!(", {started} to {finished}"),
                    (None, Some(finished)) => format!(", ended {finished}"),
                    _ => String::new(),
                };
                lines.push(format!(
                    "This is attempt {} at this step. The previous one (run {}{when}) {ended}.",
                    self.number, previous.run
                ));
                if let Some(submitted) = &previous.submitted {
                    lines.push(format!(
                        "It submitted: {}",
                        cut_line(&submitted.to_string(), LINE_CUT)
                    ));
                } else if let Some(outputs) = &previous.outputs {
                    lines.push(format!(
                        "It submitted nothing; its outputs: {}",
                        cut_line(&outputs.to_string(), LINE_CUT)
                    ));
                } else {
                    lines.push("It submitted nothing and left no outputs.".into());
                }
                if let Some(git) = &previous.git {
                    let mut text = format!("Its git: head {}", short_sha(&git.head));
                    if let (Some(commits), Some(before)) = (git.commits, &git.head_before) {
                        text.push_str(&format!(
                            ", {commits} commit{} since {}",
                            if commits == 1 { "" } else { "s" },
                            short_sha(before)
                        ));
                    }
                    match git.dirty {
                        Some(true) => text.push_str(", uncommitted changes when it ended"),
                        Some(false) => text.push_str(", clean when it ended"),
                        None => {}
                    }
                    lines.push(format!("{text}."));
                }
            }
        }
        if let Some(tree) = &self.worktree {
            lines.push(match &tree.state {
                WorkTreeState::Clean => {
                    format!("The working directory {} is clean now.", tree.cwd)
                }
                WorkTreeState::Dirty { count, paths } => {
                    let more = count.saturating_sub(paths.len());
                    format!(
                        "The working directory {} has {count} uncommitted path{} now: {}{}.",
                        tree.cwd,
                        if *count == 1 { "" } else { "s" },
                        paths.join(", "),
                        if more > 0 {
                            format!(" and {more} more")
                        } else {
                            String::new()
                        }
                    )
                }
                WorkTreeState::Unavailable { reason } => format!(
                    "git status of the working directory {} is unavailable: {reason}.",
                    tree.cwd
                ),
            });
        }
        if self.previous.is_some() {
            lines.push(
                "What it left may still be there: look before you redo or undo its work.".into(),
            );
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn releases_shorten_to_their_git_sha() {
        assert_eq!(
            short_release(
                "0123456789abcdef0123456789abcdef01234567-89abcdef0123456789abcdef0123456789abcdef0123456789abcdef01234567"
            ),
            "0123456789ab"
        );
        assert_eq!(short_release("runtime-v1"), "runtime-v1");
    }

    #[test]
    fn cuts_long_strings_at_any_depth() {
        let long = "x".repeat(205);
        assert_eq!(
            cut_strings(&json!({"a":[long],"b":1}), 200),
            json!({"a":[format!("{}… [5 more characters]", "x".repeat(200))],"b":1})
        );
    }
}
