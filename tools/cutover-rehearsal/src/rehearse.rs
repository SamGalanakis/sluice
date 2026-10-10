//! The deadline, played on a private copy of a home with the old release's own code
//! (`docs/design/plan-rows.md` §10.10 step 2, the cutover's steps 3 to 7): drain (author
//! `cutover`), one `step_cancel` per (project, step) with live attempts and the cutover's
//! reason, the intent check, one adoption pass through `RehearsalHost`, and the zero-blocker
//! check. Nothing here deletes or rewrites a row: only the old release's cancel, completion
//! and adoption code ends live work on the copy. A refused cancel, an attempt left without
//! intent or a blocker left over fails the rehearsal and is reported as the cutover would.

use crate::{
    host::RehearsalHost,
    report::{CancelRefusal, CutoverReport, RunOutcome, StopRequest, StoppedRun, advice},
};
use serde::Serialize;
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandRequest, StepStatus},
    error::PublicError,
    ids::{AttemptId, ProjectId, RunId, StepId},
    rpc::decode_json,
};
use sluice_runtime::{coordinator::Coordinator, dispatch::Catalog};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// The author every cutover write carries.
pub const AUTHOR: &str = "cutover";

/// The cancel's reason, naming the cutover and its deadline.
pub fn reason(deadline: &str) -> String {
    format!("schema-3 cutover at {deadline}: stopped at the deadline; retry it after the cutover")
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The cutover deadline, as the notice gives it (RFC 3339).
    pub deadline: String,
    /// The candidate release's commit, for the report.
    pub sha: String,
}

/// Live work left after the play: what the drain counts, and calls still running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blocker {
    pub kind: String,
    pub identity: String,
    pub project: Option<String>,
    pub resource: Option<String>,
}

/// The rehearsal's outcome: the report the cutover would write for this copy, and whatever
/// live work is left.
#[derive(Debug, Clone, Serialize)]
pub struct Rehearsal {
    pub report: CutoverReport,
    pub blockers: Vec<Blocker>,
    /// Runs the adoption pass reconciled through the rehearsal host.
    pub reconciled: usize,
}
impl Rehearsal {
    /// Zero blockers and no refused cancel: the copy may be converted.
    pub fn ok(&self) -> bool {
        self.report.refused.is_empty() && self.blockers.is_empty()
    }
}

fn storage(error: impl std::fmt::Display) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}
fn refuse(message: String) -> PublicError {
    PublicError::BadRequest { message }
}

/// The account's home, from the password database (as the old release's own guard reads it).
fn account_home() -> Option<PathBuf> {
    let uid = std::fs::metadata("/proc/self").ok().map(|m| {
        use std::os::unix::fs::MetadataExt;
        m.uid().to_string()
    })?;
    std::fs::read_to_string("/etc/passwd")
        .ok()?
        .lines()
        .find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            (fields.len() >= 7 && fields[2] == uid).then(|| PathBuf::from(fields[5]))
        })
}

/// Refuse anything but a private copy: the home the live installation selects (or one under
/// it), a home a coordinator may be serving, a copy holding a `.env`, and a fn dir outside the
/// copy.
pub fn guard(home: &Path) -> Result<PathBuf, PublicError> {
    let home = home.canonicalize().map_err(storage)?;
    if let Some(account) = account_home() {
        sluice_process::host::refuse_live_home(&home, &account)?;
    }
    if home.join("coordinator.sock").exists() {
        return Err(refuse(format!(
            "{} has a coordinator socket: rehearse on a private copy, never a served home",
            home.display()
        )));
    }
    let mut pending = vec![home.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(storage)? {
            let entry = entry.map_err(storage)?;
            let kind = entry.file_type().map_err(storage)?;
            if entry.file_name() == ".env" {
                return Err(refuse(format!(
                    "{} is in the copy: copy a home without its .env",
                    entry.path().display()
                )));
            }
            if kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    if let Ok(bytes) = std::fs::read(home.join("config.json")) {
        let config: Value = serde_json::from_slice(&bytes).map_err(storage)?;
        for dir in config["fn_dirs"].as_array().into_iter().flatten() {
            let dir = Path::new(dir.as_str().unwrap_or("/"));
            if dir.is_absolute()
                || dir
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(refuse(format!(
                    "config.json names fn dir {} outside the copy",
                    dir.display()
                )));
            }
        }
    }
    Ok(home)
}

#[derive(Debug, Clone)]
struct LiveRun {
    run: RunId,
    attempt: AttemptId,
    project: Option<ProjectId>,
    project_name: Option<String>,
    step: Option<StepId>,
    call: Option<String>,
}

async fn command(
    broker: &Coordinator<RehearsalHost>,
    request: Value,
) -> Result<sluice_model::commands::CommandReply, PublicError> {
    let request: CommandRequest =
        decode_json(&serde_json::to_vec(&request).map_err(storage)?).map_err(storage)?;
    broker.command(request).await
}

fn parse<T: std::str::FromStr>(text: String) -> rusqlite::Result<T> {
    text.parse()
        .map_err(|_| rusqlite::Error::InvalidColumnType(0, text, rusqlite::types::Type::Text))
}

async fn live_runs(broker: &Coordinator<RehearsalHost>) -> Result<Vec<LiveRun>, PublicError> {
    broker
        .reads()
        .snapshot(|sql| {
            let mut q = sql.prepare(
                "SELECT r.run_id, r.attempt_id, r.project_id, p.name, r.step_id, c.call_id
                 FROM runs r JOIN attempts a USING(attempt_id)
                 LEFT JOIN projects p ON p.project_id = r.project_id
                 LEFT JOIN calls c ON c.run_id = r.run_id
                 WHERE a.phase <> 'terminal' OR r.finished_at IS NULL
                 ORDER BY p.name, r.step_id, r.created_at, r.run_id",
            )?;
            let rows = q
                .query_map([], |r| {
                    Ok(LiveRun {
                        run: parse(r.get(0)?)?,
                        attempt: parse(r.get(1)?)?,
                        project: r.get::<_, Option<String>>(2)?.map(parse).transpose()?,
                        project_name: r.get(3)?,
                        step: r.get::<_, Option<String>>(4)?.map(parse).transpose()?,
                        call: r.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
        .map_err(|e| e.into_public(true))
}

/// Each (project, step) with live attempts, and those attempts' runs: what gets one cancel.
fn by_step(live: &[LiveRun]) -> BTreeMap<(ProjectId, StepId), Vec<&LiveRun>> {
    let mut steps: BTreeMap<(ProjectId, StepId), Vec<&LiveRun>> = BTreeMap::new();
    for run in live {
        if let (Some(project), Some(step)) = (run.project, &run.step) {
            steps.entry((project, step.clone())).or_default().push(run);
        }
    }
    steps
}

/// The step's attempts still nonterminal, and of those the ones without cancel intent.
async fn intent(
    broker: &Coordinator<RehearsalHost>,
    project: ProjectId,
    step: &StepId,
) -> Result<(usize, Vec<String>), PublicError> {
    let (project, step) = (project.to_string(), step.to_string());
    broker
        .reads()
        .snapshot(move |sql| {
            let mut q = sql.prepare(
                "SELECT attempt_id, cancel_requested FROM attempts
                 WHERE project_id=?1 AND step_id=?2 AND phase<>'terminal'",
            )?;
            let rows = q
                .query_map([&project, &step], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((
                rows.len(),
                rows.into_iter()
                    .filter(|(_, intent)| *intent == 0)
                    .map(|(attempt, _)| attempt)
                    .collect(),
            ))
        })
        .await
        .map_err(|e| e.into_public(true))
}

/// Live work left: the drain's blockers (attempts not terminal, runs not finished, leases
/// waiting or held) and calls still running.
pub async fn blockers(broker: &Coordinator<RehearsalHost>) -> Result<Vec<Blocker>, PublicError> {
    let status = sluice_runtime::drain::status(broker.reads()).await?;
    let names = project_names(broker).await?;
    let mut out: Vec<Blocker> = status
        .blockers
        .into_iter()
        .map(|b| Blocker {
            kind: b.kind,
            identity: b.identity,
            project: b.project_id.and_then(|p| names.get(&p).cloned()),
            resource: b.resource,
        })
        .collect();
    let calls = broker
        .reads()
        .snapshot(|sql| {
            let mut q = sql.prepare(
                "SELECT c.call_id, p.name FROM calls c LEFT JOIN projects p USING(project_id)
                 WHERE c.status='running' ORDER BY c.call_id",
            )?;
            let rows = q
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<(String, Option<String>)>>>()?;
            Ok(rows)
        })
        .await
        .map_err(|e| e.into_public(true))?;
    out.extend(calls.into_iter().map(|(call, project)| Blocker {
        kind: "call.running".into(),
        identity: call,
        project,
        resource: None,
    }));
    Ok(out)
}

async fn project_names(
    broker: &Coordinator<RehearsalHost>,
) -> Result<BTreeMap<ProjectId, String>, PublicError> {
    broker
        .reads()
        .snapshot(|sql| {
            let mut q = sql.prepare("SELECT project_id, name FROM projects")?;
            let rows = q
                .query_map([], |r| Ok((parse(r.get(0)?)?, r.get(1)?)))?
                .collect::<rusqlite::Result<BTreeMap<ProjectId, String>>>()?;
            Ok(rows)
        })
        .await
        .map_err(|e| e.into_public(true))
}

fn kind_of(error: &Value) -> Option<String> {
    error["error"].as_str().map(str::to_owned)
}

/// A run's result, its step's status and error, and its call's status and error.
type RunRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// How a run ended and where its step stands, read after the play.
async fn stopped(
    broker: &Coordinator<RehearsalHost>,
    run: &LiveRun,
) -> Result<StoppedRun, PublicError> {
    let id = run.run.to_string();
    let (result, step_status, step_error, call_status, call_error): RunRow = broker
        .reads()
        .snapshot(move |sql| {
            Ok(sql.query_row(
                "SELECT r.result, s.status, s.error, c.status, c.error FROM runs r
                 LEFT JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id
                 LEFT JOIN calls c ON c.run_id=r.run_id WHERE r.run_id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?)
        })
        .await
        .map_err(|e| e.into_public(true))?;
    let json = |text: Option<String>| -> Value {
        text.and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null)
    };
    let status = |word: &Value| -> StepStatus {
        serde_json::from_value(word.clone()).unwrap_or(StepStatus::Running)
    };
    let result = json(result);
    let outcome = if result.is_object() {
        RunOutcome {
            status: status(&result["status"]),
            error: kind_of(&result["error"]),
        }
    } else if let Some(call_status) = call_status {
        RunOutcome {
            status: status(&json!(call_status)),
            error: kind_of(&json(call_error)),
        }
    } else {
        RunOutcome {
            status: StepStatus::Running,
            error: None,
        }
    };
    let is_call = run.step.is_none();
    let step_status = (!is_call)
        .then(|| step_status.map(|s| status(&json!(s))))
        .flatten();
    let step_error = (!is_call).then(|| kind_of(&json(step_error))).flatten();
    Ok(StoppedRun {
        project: run.project_name.clone(),
        step: run.step.clone(),
        call: is_call.then(|| run.call.clone().unwrap_or_else(|| run.run.to_string())),
        run: run.run,
        requested: if is_call {
            StopRequest::Stop
        } else {
            StopRequest::Cancel
        },
        advice: advice(
            is_call,
            &outcome,
            step_status.as_ref(),
            step_error.as_deref(),
        ),
        outcome,
        step_status,
        step_error,
    })
}

/// Play the deadline on the copy at `home` with the old coordinator, `catalog` being the one
/// it builds for that home. Returns the outcome and the host (for what it was asked).
pub async fn rehearse(
    home: &Path,
    catalog: Catalog,
    options: &Options,
) -> Result<(Rehearsal, RehearsalHost), PublicError> {
    let home = guard(home)?;
    let host = RehearsalHost::new();
    let broker = Coordinator::open(home, catalog, host.clone()).await?;
    let reason = reason(&options.deadline);
    let (projects, revisions) = broker
        .reads()
        .snapshot(|sql| {
            Ok((
                sql.query_row("SELECT count(*) FROM plans", [], |r| r.get::<_, i64>(0))?,
                sql.query_row("SELECT count(*) FROM plan_edits", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            ))
        })
        .await
        .map_err(|e| e.into_public(true))?;
    let live = live_runs(&broker).await?;

    // Drain, as `sluice drain --author cutover` does.
    command(
        &broker,
        json!({"command": "drain", "args": {"projects": null, "author": AUTHOR}}),
    )
    .await?;

    // One cancel per (project, step), then the intent check.
    let mut refused = Vec::new();
    for ((project, step), runs) in by_step(&live) {
        let name = runs[0].project_name.clone().unwrap_or_default();
        let request = json!({"command": "step_cancel", "args": {
            "project": {"kind": "id", "value": project},
            "selection": {"steps": [step], "tags": null},
            "reason": reason, "author": AUTHOR}});
        let refusal = match command(&broker, request).await {
            Ok(_) => match intent(&broker, project, &step).await? {
                (_, missing) if missing.is_empty() => None,
                (_, missing) => Some(PublicError::Invalid {
                    message: "cancel intent was not recorded".into(),
                    errors: missing
                        .into_iter()
                        .map(|a| format!("attempt {a} has no cancel intent"))
                        .collect(),
                }),
            },
            // A step whose attempts all ended meanwhile is no refusal.
            Err(error) => match intent(&broker, project, &step).await? {
                (0, _) => None,
                _ => Some(error),
            },
        };
        if let Some(error) = refusal {
            refused.push(CancelRefusal {
                project: name,
                step,
                runs: runs.iter().map(|r| r.run).collect(),
                attempts: runs.iter().map(|r| r.attempt).collect(),
                error,
            });
        }
    }

    let mut report = CutoverReport {
        sha: options.sha.clone(),
        schema: 3,
        deadline: options.deadline.clone(),
        reason,
        projects: projects as u64,
        revisions: revisions as u64,
        stopped: Vec::new(),
        refused,
    };
    if !report.refused.is_empty() {
        // Stop as the cutover stops: still fenced, no unit stopped, nothing adopted.
        let blockers = blockers(&broker).await?;
        return Ok((
            Rehearsal {
                report,
                blockers,
                reconciled: 0,
            },
            host,
        ));
    }

    // One adoption pass settles every nonterminal attempt through the old completion path.
    broker.adopt().await?;
    let reconciled = host.asked().reconciled.len();
    let blockers = blockers(&broker).await?;
    for run in &live {
        report.stopped.push(stopped(&broker, run).await?);
    }
    Ok((
        Rehearsal {
            report,
            blockers,
            reconciled,
        },
        host,
    ))
}
