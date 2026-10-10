//! Automatic retiring of done units (SPEC §6.11): for each project with `prune_done_after`,
//! the coordinator's scheduler runs `plan_prune` itself, at most once per project every
//! `RETIRE_INTERVAL`, as one edit by `sluice`, and only when something qualifies.
use crate::{coordinator::Coordinator, execution::ExecutionHost};
use sluice_model::{
    commands::{CommandReply, CommandRequest, EditOptions, PlanPrune},
    error::PublicError,
    ids::{ProjectId, ProjectSelector, Revision, UnitName},
    plan_rows::PruneResult,
    units::prune_closed,
};
use sluice_store::{
    plans,
    projects::{self, RETIRE_AUTHOR, RetireSetting},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// How often one project is looked at. A threshold is hours in practice, so five minutes is a
/// small lag on it, and a look at a large plan (its parse and each step's result) stays rare.
pub const RETIRE_INTERVAL: Duration = Duration::from_secs(300);

/// What one round did for one project.
#[derive(Debug)]
pub enum Retirement {
    /// Nothing qualified, so there was no edit.
    Nothing,
    /// The plan moved under the round (`conflict`), or the home refused the edit (a drain, a
    /// blocked registry): no edit, and no retry before the project's next turn.
    Skipped(PublicError),
    /// One plan edit removed these units.
    Retired(Box<PruneResult>),
}

/// What a round would remove, read in one snapshot at `rev`.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub project: ProjectId,
    pub rev: Revision,
    pub after: u64,
    pub keep: Vec<String>,
    pub units: Vec<UnitName>,
}

/// The edit's reason: `retire done units older than 6h` (or `90m`, `45s` when not whole hours).
pub fn reason(after: u64) -> String {
    let age = if after.is_multiple_of(3600) {
        format!("{}h", after / 3600)
    } else if after.is_multiple_of(60) {
        format!("{}m", after / 60)
    } else {
        format!("{after}s")
    };
    format!("retire done units older than {age}")
}

/// Look without writing: the units `plan_prune` with this age and these keep patterns would
/// remove now, or `None` when there are none. This is the cheap half of a round, the same
/// eligibility and closure the edit computes, without preparing or validating an edit.
pub async fn candidate<H: ExecutionHost>(
    broker: &Coordinator<H>,
    setting: &RetireSetting,
) -> Result<Option<Candidate>, PublicError> {
    let context = broker.context(setting.project).await?;
    let setting = setting.clone();
    broker
        .reads()
        .snapshot(move |sql| {
            // Refuses (conflict) a plan that moved since `context` was read.
            let evidence = plans::prune_eligible_age(sql, &context, setting.after)?;
            if evidence.units().is_empty() {
                return Ok(None);
            }
            let state = plans::read_state(sql, setting.project)?;
            let closure = prune_closed(&context.plan, &state, evidence.units(), &setting.keep)
                .map_err(|errors| PublicError::Invalid {
                    message: "prune selection".into(),
                    errors: errors.iter().map(ToString::to_string).collect(),
                })?;
            Ok((!closure.units.is_empty()).then_some(Candidate {
                project: setting.project,
                rev: context.revision,
                after: setting.after,
                keep: setting.keep,
                units: closure.units,
            }))
        })
        .await
        .map_err(|e| e.into_public(true))
}

/// Remove the candidate's units through `plan_prune`, at the revision it was read at: one
/// edit by `sluice`. A plan edited since is a conflict, and the round is skipped.
pub async fn apply<H: ExecutionHost>(broker: &Coordinator<H>, candidate: Candidate) -> Retirement {
    let request = CommandRequest::PlanPrune(PlanPrune {
        project: ProjectSelector::Id(candidate.project),
        units: None,
        tags: None,
        older_than_seconds: candidate.after,
        keep: (!candidate.keep.is_empty()).then_some(candidate.keep),
        edit: EditOptions {
            expected: Some(candidate.rev),
            dry_run: false,
            preview_scope: Default::default(),
            reason: reason(candidate.after),
            author: Some(RETIRE_AUTHOR.into()),
        },
    });
    match broker.command(request).await {
        Ok(CommandReply::Pruned(result)) if !result.units.is_empty() => {
            Retirement::Retired(Box::new(result))
        }
        Ok(_) => Retirement::Nothing,
        Err(error) => Retirement::Skipped(error),
    }
}

/// One round for one project: look, then edit only when something qualifies.
pub async fn retire<H: ExecutionHost>(
    broker: &Coordinator<H>,
    setting: &RetireSetting,
) -> Retirement {
    match candidate(broker, setting).await {
        Ok(None) => Retirement::Nothing,
        Ok(Some(candidate)) => apply(broker, candidate).await,
        Err(error) => Retirement::Skipped(error),
    }
}

/// Each project's next turn. A project is looked at once per `interval`, whatever the round
/// did, so a refused or raced round waits for its next turn rather than retrying.
#[derive(Debug)]
pub struct Retirer {
    interval: Duration,
    next: HashMap<ProjectId, Instant>,
}
impl Retirer {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: HashMap::new(),
        }
    }
    /// A round for each project with the setting (live, not paused or archived) whose turn
    /// has come by `now`.
    pub async fn pass<H: ExecutionHost>(
        &mut self,
        broker: &Coordinator<H>,
        now: Instant,
    ) -> Result<Vec<(ProjectId, Retirement)>, PublicError> {
        let settings = broker
            .reads()
            .snapshot(projects::retire_settings)
            .await
            .map_err(|e| e.into_public(true))?;
        self.next
            .retain(|project, _| settings.iter().any(|s| s.project == *project));
        let mut done = vec![];
        for setting in settings {
            if self.next.get(&setting.project).is_some_and(|at| now < *at) {
                continue;
            }
            self.next.insert(setting.project, now + self.interval);
            let started = Instant::now();
            let looked = candidate(broker, &setting).await;
            let look_ms = started.elapsed().as_millis();
            let outcome = match looked {
                Ok(None) => Retirement::Nothing,
                Ok(Some(candidate)) => apply(broker, candidate).await,
                Err(error) => Retirement::Skipped(error),
            };
            match &outcome {
                Retirement::Retired(result) => tracing::info!(
                    project = %setting.project,
                    rev = %result.edit.rev,
                    units = result.units.len(),
                    steps = result.edit.steps.as_ref().map_or(0, Vec::len),
                    kept = result.kept.len(),
                    look_ms,
                    edit_ms = started.elapsed().as_millis() - look_ms,
                    "retired done units"
                ),
                Retirement::Skipped(error) => tracing::info!(
                    project = %setting.project,
                    %error,
                    "retiring done units skipped this round"
                ),
                Retirement::Nothing => {}
            }
            done.push((setting.project, outcome));
        }
        Ok(done)
    }
}

/// The scheduler's handle on retiring: one pass at a time, in its own task, so a pass that
/// edits a large plan never holds admission up.
#[derive(Default)]
pub struct Background {
    idle: Option<Retirer>,
    running: Option<tokio::task::JoinHandle<Retirer>>,
}
impl Background {
    /// Start a pass unless the last one is still running.
    pub async fn tick<H: ExecutionHost>(&mut self, broker: &Coordinator<H>) {
        if let Some(running) = self.running.take() {
            if !running.is_finished() {
                self.running = Some(running);
                return;
            }
            self.idle = running.await.ok();
        }
        let mut retirer = self
            .idle
            .take()
            .unwrap_or_else(|| Retirer::new(RETIRE_INTERVAL));
        let broker = broker.clone();
        self.running = Some(tokio::spawn(async move {
            if let Err(error) = retirer.pass(&broker, Instant::now()).await {
                tracing::warn!(%error, "retiring done units deferred");
            }
            retirer
        }));
    }
}
impl Drop for Background {
    fn drop(&mut self) {
        if let Some(running) = &self.running {
            running.abort();
        }
    }
}
