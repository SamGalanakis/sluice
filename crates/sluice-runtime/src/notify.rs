//! Owner notification (SPEC §8): config.json's `notify.command` runs once for each new open
//! question to `owner`, with the question as JSON on stdin.
//!
//! Posting the question reserves the attempt in the same transaction. The coordinator claims
//! the reservation durably before the command starts and records the result after it ends,
//! in tasks of their own, so a slow command never holds the scheduler or the writer. A command
//! that exits non-zero or cannot start is tried again, at most [`TRIES`] times in all; one
//! that times out is not, because it may have delivered. A claim left by a stopped coordinator
//! is settled as `uncertain` and never replayed: a crash may lose a notification, never repeat
//! one.
use crate::config::{HomeConfig, NotifyConfig};
use serde_json::{Value, json};
use sluice_model::{error::PublicError, events::NotificationOutcome};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    messages::{self, PendingNotify},
};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinSet,
};

/// Runs of the command per question, the first included.
pub const TRIES: usize = 3;
/// The pause before each further try.
const BACKOFF: [Duration; TRIES - 1] = [Duration::from_secs(1), Duration::from_secs(2)];
/// The stderr tail kept with the result.
const STDERR_TAIL: usize = 2048;

/// The coordinator's dispatcher. One per home: the coordinator flock makes it the only one.
pub struct Notifier {
    home: PathBuf,
    writer: Writer,
    reads: ReadPool,
    tasks: JoinSet<()>,
    recovered: bool,
}

impl Notifier {
    pub fn new(home: PathBuf, writer: Writer, reads: ReadPool) -> Self {
        Self {
            home,
            writer,
            reads,
            tasks: JoinSet::new(),
            recovered: false,
        }
    }

    /// Settle what an earlier coordinator left in flight, then claim every reservation and
    /// start its dispatch. Returns at once; results land from the dispatch tasks.
    pub async fn tick(&mut self) -> Result<(), PublicError> {
        while let Some(done) = self.tasks.try_join_next() {
            if let Err(error) = done {
                tracing::error!(%error, "notify task failed");
            }
        }
        if !self.recovered {
            let error = PublicError::ProcessLost {
                message: "the coordinator stopped while the notify command ran".into(),
            };
            self.writer
                .write(RetrySafety::Idempotent, move |tx| {
                    messages::notify_abandoned(tx, error.clone())
                })
                .await?;
            self.recovered = true;
        }
        let Some(config) = HomeConfig::load(&self.home).notify else {
            return Ok(());
        };
        let pending = self
            .reads
            .snapshot(messages::notify_pending)
            .await
            .map_err(|e| e.into_public(true))?;
        for item in pending {
            let (project, id, attempt) = (item.project, item.message.id, item.attempt);
            if !item.open {
                let error = PublicError::Cancelled {
                    message: "question is no longer open; not sent".into(),
                };
                self.writer
                    .write(RetrySafety::Idempotent, move |tx| {
                        messages::notify_result(
                            tx,
                            project,
                            id,
                            attempt,
                            NotificationOutcome::Failed,
                            None,
                            Some(error.clone()),
                        )
                    })
                    .await?;
                continue;
            }
            let claimed = self
                .writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    messages::notify_claim(tx, project, id, attempt)
                })
                .await?;
            if !claimed {
                continue;
            }
            let (writer, home, config) = (self.writer.clone(), self.home.clone(), config.clone());
            self.tasks.spawn(async move {
                let (outcome, stderr, error) = dispatch(&config, &home, &item).await;
                let result = writer
                    .write(RetrySafety::Idempotent, move |tx| {
                        messages::notify_result(
                            tx,
                            project,
                            id,
                            attempt,
                            outcome.clone(),
                            stderr.clone(),
                            error.clone(),
                        )
                    })
                    .await;
                if let Err(error) = result {
                    tracing::error!(%project, message = id.0, %error, "notify result not recorded");
                }
            });
        }
        Ok(())
    }

    /// Wait for every dispatch started so far (tests, and an orderly stop).
    pub async fn settle(&mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}

/// The JSON on the command's stdin: the message's own fields plus `project` (its name at
/// dispatch) and `project_id`.
pub fn payload(item: &PendingNotify) -> Value {
    let mut value = serde_json::to_value(&item.message).unwrap_or(Value::Null);
    if let Some(fields) = value.as_object_mut() {
        fields.insert("project".into(), json!(item.project_name));
        fields.insert("project_id".into(), json!(item.project));
    }
    value
}

enum Try {
    Done,
    Failed(String),
    TimedOut,
}

async fn dispatch(
    config: &NotifyConfig,
    home: &Path,
    item: &PendingNotify,
) -> (NotificationOutcome, Option<String>, Option<PublicError>) {
    let input = serde_json::to_vec(&payload(item)).unwrap_or_default();
    let env = crate::dotenv::run_environment(home, Some(item.project));
    let mut tail = String::new();
    let mut reason = String::new();
    for attempt in 0..TRIES {
        if attempt > 0 {
            tokio::time::sleep(BACKOFF[attempt - 1]).await;
        }
        let (result, stderr) = run_once(config, home, &env, &input).await;
        tail = stderr;
        match result {
            Try::Done => return (NotificationOutcome::Dispatched, nonempty(tail), None),
            Try::TimedOut => {
                return (
                    NotificationOutcome::Uncertain,
                    nonempty(tail),
                    Some(PublicError::FnFailure {
                        message: format!(
                            "notify command timed out after {}s; not retried",
                            config.timeout.as_secs_f64()
                        ),
                    }),
                );
            }
            Try::Failed(why) => {
                tracing::warn!(project = %item.project, message = item.message.id.0, try_ = attempt + 1, why = %why, "notify command failed");
                reason = why;
            }
        }
    }
    (
        NotificationOutcome::Failed,
        nonempty(tail),
        Some(PublicError::FnFailure {
            message: format!("notify command {reason} ({TRIES} tries)"),
        }),
    )
}

fn nonempty(text: String) -> Option<String> {
    (!text.is_empty()).then_some(text)
}

async fn run_once(
    config: &NotifyConfig,
    home: &Path,
    env: &std::collections::BTreeMap<String, String>,
    input: &[u8],
) -> (Try, String) {
    let mut command = tokio::process::Command::new(&config.command[0]);
    command
        .args(&config.command[1..])
        .envs(env)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return (Try::Failed(format!("cannot start: {e}")), String::new()),
    };
    let mut stdin = child.stdin.take();
    let mut stderr = child.stderr.take();
    let run = async {
        if let Some(mut stdin) = stdin.take() {
            // A command that does not read its stdin is fine.
            let _ = stdin.write_all(input).await;
        }
        let mut kept = Vec::new();
        if let Some(stderr) = stderr.as_mut() {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                kept.extend_from_slice(&chunk[..n]);
                if kept.len() > STDERR_TAIL {
                    kept.drain(..kept.len() - STDERR_TAIL);
                }
            }
        }
        (child.wait().await, kept)
    };
    let outcome = tokio::time::timeout(config.timeout, run).await;
    match outcome {
        Err(_) => {
            let _ = child.kill().await;
            (Try::TimedOut, String::new())
        }
        Ok((status, kept)) => {
            let tail = String::from_utf8_lossy(&kept).into_owned();
            match status {
                Ok(status) if status.success() => (Try::Done, tail),
                Ok(status) => (Try::Failed(format!("exited with {status}")), tail),
                Err(e) => (Try::Failed(format!("lost: {e}")), tail),
            }
        }
    }
}
