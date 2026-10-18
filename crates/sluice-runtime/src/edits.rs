//! The one edit pipeline (plan-rows §4). Every edit tool's command (`plan_edit`, `unit_update`,
//! `unit_remove` and the typed tools) is lowered to operations and prepared outside the writer,
//! in one read snapshot: the certified base from the plan cache, the read set in rounds
//! (§6.4), the model's preparation, the board's warnings. The writer then commits the prepared
//! store payload while its tokens hold. A stale preparation is prepared again from a new
//! snapshot, at most `EDIT_TRIES` times in all; the third stale one is refused `busy`,
//! retryable (`EditContended`), and nothing is written. Preparation never runs in the writer.
use crate::{
    coordinator::EditLog,
    dispatch::Catalog,
    plan_cache::{PlanCache, recipe_generation},
};
use indexmap::IndexMap;
use rusqlite::Connection;
use sluice_model::{
    commands::{CommandRequest, EditOptions, KeptUnit, ProjectIdentity, StepSelection},
    cost::{self, Counter, PreparationPoint},
    edit::{self, InputChanges, Lowered, prepare_lowered},
    error::PublicError,
    ids::{ProjectId, ProjectSelector, Revision, StepId, UnitName},
    plan::{EditBase, Plan, PrepareOptions, SignatureProvider, preparation_reads},
    plan_rows::{
        CertifiedPlan, EDIT_TRIES, EditPreview, EditResult, InputEditResult, PlanRowsError,
        PreparationReads, PreparedPlanEdit, PreviewScope, PruneResult, ScopedState,
        ValidationTokens,
    },
    recipe::RecipeEntry,
    units::{PruneHolder, PruneSet},
};
use sluice_store::{
    ReadPool, WriteTransaction,
    identities::{self, Editor, OwnerIntent},
    plans::{self, CommitOutcome, PruneEligibility},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

pub use sluice_model::commands::CommandReply;

/// The project an edit command edits; `None` for any other command. Every edit tool goes
/// through the pipeline.
pub(crate) fn edit_selector(command: &CommandRequest) -> Option<&ProjectSelector> {
    Some(match command {
        CommandRequest::PlanEdit(r) => &r.project,
        CommandRequest::UnitUpdate(r) => &r.project,
        CommandRequest::UnitRemove(r) => &r.project,
        CommandRequest::StepAdd(r) => &r.project,
        CommandRequest::UnitAdd(r) => &r.project,
        CommandRequest::StepUpdate(r) => &r.project,
        CommandRequest::StepRemove(r) => &r.project,
        CommandRequest::StepPause(r) => &r.project,
        CommandRequest::StepSetInput(r) => &r.project,
        CommandRequest::EdgeAdd(r) | CommandRequest::EdgeRemove(r) => &r.project,
        CommandRequest::UnitTag(r) => &r.project,
        CommandRequest::PlanPrune(r) => &r.project,
        _ => return None,
    })
}

/// The revision the caller says the edit was worked out from, if it gave one.
fn requested_rev(command: &CommandRequest) -> Option<Revision> {
    match command {
        CommandRequest::PlanEdit(r) => r.rev,
        CommandRequest::UnitUpdate(r) => r.rev,
        CommandRequest::UnitRemove(r) => r.rev,
        CommandRequest::StepAdd(r) => r.edit.expected,
        CommandRequest::UnitAdd(r) => r.edit.expected,
        CommandRequest::StepUpdate(r) => r.edit.expected,
        CommandRequest::StepRemove(r) => r.edit.expected,
        CommandRequest::StepPause(r) => r.edit.expected,
        CommandRequest::StepSetInput(r) => r.edit.expected,
        CommandRequest::EdgeAdd(r) | CommandRequest::EdgeRemove(r) => r.edit.expected,
        CommandRequest::UnitTag(r) => r.edit.expected,
        CommandRequest::PlanPrune(r) => r.edit.expected,
        _ => None,
    }
}

/// The refusal of what the caller supplied (plan-rows §7.6, §7.7): only `plan_edit` and
/// `unit_update` are checked; a typed tool's lowering never builds an empty list.
pub(crate) fn check_supplied(command: &CommandRequest) -> Result<(), PublicError> {
    match command {
        CommandRequest::PlanEdit(r) => r.check_supplied().map_err(Into::into),
        CommandRequest::UnitUpdate(r) => r.check_supplied().map_err(Into::into),
        _ => Ok(()),
    }
}

/// What one preparation came to.
pub(crate) enum Prepared<T> {
    /// Answered without the writer: a dry run's preview, or a typed no-op's result at the
    /// current revision (nothing is committed).
    Answer(Box<CommandReply>),
    /// For the writer to commit while its tokens hold.
    Commit(Box<T>),
}

/// The retry rule (plan-rows §4): prepare in a read snapshot, hand the result to `commit`
/// (which answers `None` when it found the preparation stale, having written nothing), and
/// prepare again; the `EDIT_TRIES`th stale preparation is refused `EditContended`. Each
/// preparation is counted by outcome (`sluice_model::cost`), and passes the preparation probe
/// (`cost::preparation_point`) inside its snapshot, once it has worked everything out and
/// before the writer sees it, so §11's contention gate can move the state under it.
pub(crate) async fn pipeline<T, P, C, F>(
    reads: &ReadPool,
    log: &mut EditLog,
    prepare: Arc<P>,
    mut commit: C,
) -> Result<CommandReply, PublicError>
where
    T: Send + 'static,
    P: Fn(&Connection) -> sluice_store::Result<(ProjectId, Prepared<T>)> + Send + Sync + 'static,
    C: FnMut(T) -> F,
    F: std::future::Future<Output = Result<Option<CommandReply>, PublicError>>,
{
    for attempt in 1..=EDIT_TRIES {
        let prepare = prepare.clone();
        let prepared = log
            .preparing(reads.snapshot(move |sql| {
                if std::thread::current().name() == Some(sluice_store::writer::WRITER_THREAD) {
                    cost::count(Counter::WriterPreparations);
                }
                let (project, prepared) = prepare(sql)?;
                cost::preparation_point(&PreparationPoint { project, attempt });
                Ok(prepared)
            }))
            .await
            .map_err(|e| e.into_public(true))?;
        match prepared {
            Prepared::Answer(reply) => {
                cost::count(Counter::PreparationsDryRun);
                return Ok(*reply);
            }
            Prepared::Commit(staged) => match commit(*staged).await? {
                Some(reply) => {
                    cost::count(Counter::PreparationsCommitted);
                    return Ok(reply);
                }
                None if attempt < EDIT_TRIES => {
                    cost::count(Counter::PreparationsStale);
                    log.retries += 1;
                }
                None => {
                    cost::count(Counter::PreparationsContended);
                    log.retries += 1;
                }
            },
        }
    }
    Err(PlanRowsError::EditContended.into())
}

/// What a preparation is given: the command and what the coordinator read before the
/// snapshot began.
pub(crate) struct Preparation {
    pub command: CommandRequest,
    pub catalog: Arc<Catalog>,
    pub home: PathBuf,
    pub cache: Arc<PlanCache>,
    /// A run's callback may edit only its own project.
    pub run_project: Option<ProjectId>,
    /// A run's callback: the run's step, whose unit's owner a unit it adds takes.
    pub run_step: Option<StepId>,
}

/// Who an edit's new units belong to, as the command asks (`identities::edit_owners` decides
/// at commit): the editing session (`me`) or run, and the owners it names.
fn owner_intent(
    command: &CommandRequest,
    run_step: Option<&StepId>,
) -> Result<OwnerIntent, PublicError> {
    use sluice_model::{identity::SessionName, plan_rows::PlanOp};
    let (me, units, steps) = match command {
        CommandRequest::StepAdd(r) => (
            r.me.as_deref(),
            vec![],
            r.owner
                .iter()
                .map(|o| (r.step.clone(), o.clone()))
                .collect(),
        ),
        CommandRequest::UnitAdd(r) => (
            r.me.as_deref(),
            r.owner
                .iter()
                .map(|o| (r.unit.to_string(), o.clone()))
                .collect(),
            vec![],
        ),
        CommandRequest::PlanEdit(r) => (
            r.me.as_deref(),
            r.ops
                .iter()
                .filter_map(|op| match op {
                    PlanOp::UnitAdd {
                        unit,
                        owner: Some(owner),
                        ..
                    } => Some((unit.to_string(), owner.clone())),
                    _ => None,
                })
                .collect(),
            vec![],
        ),
        _ => (None, vec![], vec![]),
    };
    let editor = match (run_step, me.map(str::trim).filter(|m| !m.is_empty())) {
        (Some(step), _) => Editor::Run(step.clone()),
        (None, Some(me)) => Editor::Session(SessionName::parse(me)?),
        (None, None) => Editor::Anonymous,
    };
    Ok(OwnerIntent {
        editor,
        units,
        steps,
    })
}

/// An edit prepared from a read snapshot, for the writer.
pub(crate) struct Staged {
    pub project: ProjectId,
    identity: ProjectIdentity,
    edit: PreparedPlanEdit,
    /// The tool's reports, which the reply carries whatever revision it commits at.
    inputs: Option<InputChanges>,
    prune: Option<PruneSet>,
    /// The store's age evidence for an age-filtered `plan_prune`, rechecked by the commit.
    evidence: Option<PruneEligibility>,
    /// Who the units the edit adds belong to.
    owners: OwnerIntent,
    /// `unit.add`'s bypass presets, checked against the candidate's steps.
    bypass: Bypass,
}
/// The bypass presets an edit's `unit.add` operations set (`bypass`), each resolved to its new
/// step and checked against its declared outputs while the edit is prepared; the writer
/// stores them with the edit.
#[derive(Default)]
struct Bypass {
    presets: Vec<(StepId, sluice_model::rpc::JsonMap)>,
    author: String,
    reason: String,
}
/// What a commit hands back for the plan cache: the candidate and the revision it is.
pub(crate) struct Committed {
    pub reply: CommandReply,
    pub rev: Revision,
    pub compiled: CertifiedPlan,
}
impl Staged {
    /// Whether what the coordinator compares itself still holds, immediately before the
    /// writer takes the edit: the fn catalog's and the recipe files' generations (the writer
    /// never touches files).
    pub(crate) fn generations_hold(&self, catalog: &Catalog, home: &Path) -> bool {
        let tokens = &self.edit.commit.tokens;
        catalog.generation() == tokens.catalog_generation
            && recipe_generation(home, self.project) == tokens.recipe_generation
    }
    /// Commit in the writer: `None` when a token no longer holds (nothing is written).
    pub(crate) fn commit(
        self,
        tx: &mut WriteTransaction<'_>,
    ) -> sluice_store::Result<Option<Committed>> {
        crate::drain::ensure_admission(tx, &crate::drain::Admission::Plan)?;
        // Worked out before the rows move: a unit's owner is read from the rows it keeps.
        let owners = identities::edit_owners(tx, self.project, &self.edit.commit, &self.owners)?;
        match plans::commit_plan_edit(tx, self.project, &self.edit.commit, self.evidence.as_ref())?
        {
            CommitOutcome::Stale => Ok(None),
            CommitOutcome::Committed(rev) => {
                identities::write_owners(tx, self.project, &owners)?;
                if !self.bypass.presets.is_empty() {
                    let context = plans::PlanContext {
                        project: self.project,
                        revision: rev,
                        plan: (*self.edit.compiled.plan).clone(),
                    };
                    sluice_store::bypass::set_presets(
                        tx,
                        &context,
                        self.bypass.presets,
                        &self.bypass.author,
                        &self.bypass.reason,
                    )?;
                }
                Ok(Some(Committed {
                    reply: edit_reply(
                        EditResult {
                            project: self.identity,
                            rev,
                            preview: self.edit.preview,
                            steps: self.edit.steps,
                            board_warnings: self.edit.board_warnings,
                        },
                        self.inputs,
                        self.prune,
                    ),
                    rev,
                    compiled: self.edit.compiled,
                }))
            }
        }
    }
}

/// The edit result, with `step_set_input`'s per-step report or `plan_prune`'s units.
pub(crate) fn edit_reply(
    result: EditResult,
    inputs: Option<InputChanges>,
    prune: Option<PruneSet>,
) -> CommandReply {
    if let Some(inputs) = inputs {
        return CommandReply::Inputs(InputEditResult {
            edit: result,
            changed: inputs.changed,
            running: inputs.running,
            unsupported: inputs.unsupported,
        });
    }
    if let Some(prune) = prune {
        return CommandReply::Pruned(PruneResult {
            edit: result,
            units: prune.units,
            kept: prune
                .kept
                .into_iter()
                .map(|(unit, holder)| {
                    let (mut step, mut output, mut keep) = (None, None, None);
                    match holder {
                        PruneHolder::Step(id) => step = Some(id),
                        PruneHolder::PlanOutput(name) => output = Some(name),
                        PruneHolder::Keep(pattern) => keep = Some(pattern),
                    }
                    KeptUnit {
                        unit,
                        step,
                        output,
                        keep,
                    }
                })
                .collect(),
        });
    }
    CommandReply::Edit(result)
}

/// The tokens the writer re-reads (plan-rows §3): the revision, the execution-state witness
/// and the board's revision, in the caller's snapshot.
fn store_tokens(
    sql: &Connection,
    project: ProjectId,
) -> sluice_store::Result<(Revision, u64, Revision)> {
    let (rev, epoch, board): (i64, i64, i64) = sql.query_row(
        "SELECT p.rev, p.state_epoch, j.board_rev FROM plans p JOIN projects j USING(project_id)
         WHERE p.project_id=?1",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok((Revision(rev as u64), epoch as u64, Revision(board as u64)))
}

/// What the rounds have read so far, to read each item once (plan-rows §6.4).
#[derive(Default)]
struct ReadSoFar {
    steps: HashSet<StepId>,
    inputs: HashSet<String>,
    resources: HashSet<String>,
    competitors: HashSet<String>,
}
impl ReadSoFar {
    /// The part of `wanted` not read yet, marked read.
    fn take_new(&mut self, wanted: &PreparationReads) -> PreparationReads {
        fn fresh<T: Clone + Eq + std::hash::Hash>(seen: &mut HashSet<T>, wanted: &[T]) -> Vec<T> {
            wanted
                .iter()
                .filter(|item| seen.insert((*item).clone()))
                .cloned()
                .collect()
        }
        PreparationReads {
            steps: fresh(&mut self.steps, &wanted.steps),
            inputs: fresh(&mut self.inputs, &wanted.inputs),
            resources: fresh(&mut self.resources, &wanted.resources),
            competitors: fresh(&mut self.competitors, &wanted.competitors),
        }
    }
}
fn is_empty(reads: &PreparationReads) -> bool {
    reads.steps.is_empty()
        && reads.inputs.is_empty()
        && reads.resources.is_empty()
        && reads.competitors.is_empty()
}
/// One round's state joined to the rounds before it: each item was read once, in one
/// snapshot, so nothing overlaps.
fn merge(into: &mut ScopedState, round: ScopedState) {
    into.state.inputs.0.extend(round.state.inputs.0);
    into.state.steps.extend(round.state.steps);
    into.state.paused = round.state.paused;
    into.leases.extend(round.leases);
    into.competitors.extend(round.competitors);
}

/// The read set of an edit's operations against `base` (plan-rows §6.4): read in rounds
/// inside the caller's snapshot, starting from what lowering read, until a round adds nothing.
#[allow(clippy::too_many_arguments)]
fn read_rounds(
    sql: &Connection,
    project: ProjectId,
    base: &EditBase<'_, impl SignatureProvider>,
    ops: &[sluice_model::plan_rows::PlanOp],
    scope: PreviewScope,
    read: &mut ReadSoFar,
    state: &mut ScopedState,
) -> sluice_store::Result<()> {
    loop {
        let wanted = preparation_reads(&EditBase { state, ..*base }, ops, scope);
        let fresh = read.take_new(&wanted);
        if is_empty(&fresh) {
            return Ok(());
        }
        merge(state, plans::read_scoped_state(sql, project, &fresh)?);
    }
}

/// An edit command's options for preparation (plan-rows §8's `EditOptions3`): a typed tool's
/// `edit` options with its `start`, the author's default applied as the tools apply it.
fn options(edit: &EditOptions, start: bool) -> PrepareOptions {
    PrepareOptions {
        rev: edit.expected,
        dry_run: edit.dry_run,
        preview_scope: edit.preview_scope,
        start,
        reason: edit.reason.clone(),
        author: edit.author.clone().unwrap_or_default(),
    }
}

/// The steps a typed tool's selection names in the base (by id or by tag), for the state its
/// lowering reads. An unknown id is left to the lowering, which refuses it.
fn selected(base: &Plan, selection: &StepSelection) -> Vec<StepId> {
    let mut steps: Vec<StepId> = selection.steps.clone().unwrap_or_default();
    for tag in selection.tags.as_deref().unwrap_or_default() {
        steps.extend(base.tagged(tag).cloned());
    }
    steps
}

/// What lowering `command` reads beside the compiled base (plan-rows §7.8): the stored
/// state of `step_set_input`'s selected steps (a running one is left alone), and of every
/// step for `plan_prune`, which decides which units are done and what their removal keeps.
fn lowering_reads(base: &Plan, command: &CommandRequest) -> PreparationReads {
    let steps = match command {
        CommandRequest::StepSetInput(request) => selected(base, &request.selection),
        CommandRequest::PlanPrune(_) => base.steps().keys().cloned().collect(),
        _ => vec![],
    };
    PreparationReads {
        steps,
        ..PreparationReads::default()
    }
}

/// Lower an edit command to operations against the base (plan-rows §7.6 to §7.8): `plan_edit`'s
/// own operations, `unit_update`'s and `unit_remove`'s one operation, and each typed tool's
/// lowering, with the options it is prepared under. `eligible` is the store's age evidence for
/// an age-filtered `plan_prune`.
fn lower(
    base: &Plan,
    state: &ScopedState,
    eligible: Option<&[UnitName]>,
    recipes: &IndexMap<String, RecipeEntry>,
    signatures: &impl SignatureProvider,
    command: &CommandRequest,
) -> Result<(Lowered, PrepareOptions), PublicError> {
    use edit::ReplySteps;
    Ok(match command {
        CommandRequest::PlanEdit(request) => (
            Lowered {
                ops: request.ops.clone(),
                steps: ReplySteps::Added,
                inputs: None,
                prune: None,
            },
            PrepareOptions {
                rev: request.rev,
                dry_run: request.dry_run,
                preview_scope: request.preview_scope,
                start: request.start,
                reason: request.reason.clone(),
                author: request.author.clone().unwrap_or_default(),
            },
        ),
        CommandRequest::UnitUpdate(request) => (
            edit::unit_update(base, &request.unit, &request.changes)?,
            PrepareOptions {
                rev: request.rev,
                dry_run: request.dry_run,
                preview_scope: request.preview_scope,
                start: true,
                reason: request.reason.clone(),
                author: request.author.clone().unwrap_or_default(),
            },
        ),
        CommandRequest::UnitRemove(request) => (
            edit::unit_remove(base, &request.unit)?,
            PrepareOptions {
                rev: request.rev,
                dry_run: request.dry_run,
                preview_scope: request.preview_scope,
                start: true,
                reason: request.reason.clone(),
                author: request.author.clone().unwrap_or_default(),
            },
        ),
        CommandRequest::StepAdd(request) => (
            edit::step_add(base, &request.step, &request.spec)?,
            options(&request.edit, request.start),
        ),
        CommandRequest::StepUpdate(request) => (
            edit::step_update(base, &request.step, &request.changes)?,
            options(&request.edit, true),
        ),
        CommandRequest::StepRemove(request) => (
            edit::step_remove(base, &request.selection)?,
            options(&request.edit, true),
        ),
        CommandRequest::EdgeAdd(request) => (
            edit::edge(base, &request.step, &request.after, true)?,
            options(&request.edit, true),
        ),
        CommandRequest::EdgeRemove(request) => (
            edit::edge(base, &request.step, &request.after, false)?,
            options(&request.edit, true),
        ),
        CommandRequest::UnitAdd(request) => (
            edit::unit_add(
                base,
                recipes,
                signatures,
                &request.recipe,
                &request.unit,
                &request.params,
                &request.after,
                &request.inputs,
                &request.tags,
                &request.bypass,
            )?,
            options(&request.edit, request.start),
        ),
        CommandRequest::StepSetInput(request) => (
            edit::step_set_input(
                base,
                &|id| state.state.status(id),
                &request.selection,
                &request.inputs,
            )?,
            options(&request.edit, true),
        ),
        CommandRequest::UnitTag(request) => (
            edit::unit_tag(base, &request.unit, &request.add, &request.remove)?,
            options(&request.edit, true),
        ),
        CommandRequest::StepPause(request) => (
            edit::step_pause(
                base,
                &request.selection,
                request.subtree,
                request.paused,
                &request.edit.reason,
            )?,
            options(&request.edit, true),
        ),
        CommandRequest::PlanPrune(request) => (
            edit::plan_prune(
                base,
                &state.state,
                request.units.as_deref(),
                request.tags.as_deref(),
                request.older_than_seconds,
                request.keep.as_deref(),
                eligible,
            )?,
            options(&request.edit, true),
        ),
        _ => {
            return Err(PublicError::BadRequest {
                message: "command is not a plan edit".into(),
            });
        }
    })
}

/// Prepare one edit in a read snapshot (plan-rows §4's preparation). Never in the writer.
pub(crate) fn prepare(
    sql: &Connection,
    input: &Preparation,
) -> sluice_store::Result<(ProjectId, Prepared<Staged>)> {
    let selector = edit_selector(&input.command)
        .ok_or_else(|| PublicError::BadRequest {
            message: "command is not a plan edit".into(),
        })?
        .clone();
    let project = sluice_store::messages::resolve_project(sql, &selector)?;
    if input.run_project.is_some_and(|own| own != project) {
        return Err(PublicError::Conflict {
            message: "tool project differs from run".into(),
            current_rev: None,
        }
        .into());
    }
    let identity = crate::coordinator::projects_identity(sql, project)?;
    let owners = owner_intent(&input.command, input.run_step.as_ref())?;
    // Read before any signature (`Catalog::generation`), on every preparation: a catalog
    // republished since the last one is this one's.
    let generation = input.catalog.generation();
    let (rev, epoch, board_rev) = store_tokens(sql, project)?;
    // An explicit stale revision is a conflict, refused before anything is prepared.
    if let Some(requested) = requested_rev(&input.command)
        && requested != rev
    {
        return Err(PublicError::from(PlanRowsError::StaleRev { current: rev }).into());
    }
    let signatures = input.catalog.for_project(Some(project));
    let (rev, base) = input.cache.current(sql, project, generation, &signatures)?;
    let tokens = ValidationTokens {
        plan_rev: rev,
        state_epoch: sluice_model::plan_rows::StateEpoch(epoch),
        catalog_generation: generation,
        recipe_generation: recipe_generation(&input.home, project),
        board_rev,
    };
    // An age-filtered prune keeps the store's evidence of each unit's age: lowering selects
    // from it and the commit rechecks it.
    let evidence = match &input.command {
        CommandRequest::PlanPrune(request) if request.older_than_seconds > 0 => {
            Some(plans::prune_eligible_age(
                sql,
                &plans::PlanContext {
                    project,
                    revision: rev,
                    plan: (*base).clone(),
                },
                request.older_than_seconds,
            )?)
        }
        _ => None,
    };
    let declarations = sluice_store::resources::declarations(sql, project)?;
    let capacities: IndexMap<String, Option<u64>> = declarations
        .iter()
        .map(|(name, resource)| (name.clone(), resource.capacity))
        .collect();
    let limits = crate::coordinator::resource_limits(sql, project)?;
    let recipes = crate::dispatch_ext::load_recipes(&input.home, project)?;
    let mut read = ReadSoFar::default();
    let mut state = ScopedState::default();
    let first = read.take_new(&lowering_reads(&base, &input.command));
    merge(&mut state, plans::read_scoped_state(sql, project, &first)?);
    let (lowered, options) = lower(
        &base,
        &state,
        evidence.as_ref().map(PruneEligibility::units),
        &recipes,
        &signatures,
        &input.command,
    )?;
    if lowered.ops.is_empty() {
        // A typed no-op (plan-rows §7.8): nothing to prepare or commit; the revision stays.
        let preview = EditPreview {
            scope: options.preview_scope,
            changes: vec![],
            would_start: vec![],
            would_queue: vec![],
            would_skip: vec![],
            would_stale: vec![],
            errors: vec![],
        };
        let steps = match lowered.steps {
            edit::ReplySteps::These(steps) => Some(steps),
            _ => None,
        };
        return Ok((
            project,
            Prepared::Answer(Box::new(if options.dry_run {
                CommandReply::Preview(preview)
            } else {
                edit_reply(
                    EditResult {
                        project: identity,
                        rev,
                        preview,
                        steps,
                        board_warnings: vec![],
                    },
                    lowered.inputs,
                    lowered.prune,
                )
            })),
        ));
    }
    if options.preview_scope == PreviewScope::All {
        // A full dry run simulates the whole plan before and after (plan-rows §6.3): it reads
        // every step's state, every input's value and every resource, not a read set.
        let resources: Vec<String> = limits.keys().cloned().collect();
        let all = read.take_new(&PreparationReads {
            steps: base.steps().keys().cloned().collect(),
            inputs: base.inputs().keys().cloned().collect(),
            resources: resources.clone(),
            competitors: resources,
        });
        merge(&mut state, plans::read_scoped_state(sql, project, &all)?);
    }
    let edit_base = EditBase {
        plan: &base,
        tokens: &tokens,
        state: &ScopedState::default(),
        signatures: &signatures,
        recipes: &recipes,
        capacities: &capacities,
        limits: &limits,
    };
    let scope = options.preview_scope;
    read_rounds(
        sql,
        project,
        &edit_base,
        &lowered.ops,
        scope,
        &mut read,
        &mut state,
    )?;
    let asked = bypass_asked(&lowered.ops);
    let (author, reason) = (options.author.clone(), options.reason.clone());
    let mut prepared = prepare_lowered(
        &EditBase {
            state: &state,
            ..edit_base
        },
        lowered,
        options,
    )?;
    // The steps the edit writes, and only those, are checked for a retired model string.
    let written: Vec<&StepId> = prepared
        .commit
        .rows
        .changes
        .iter()
        .filter_map(|change| match change {
            sluice_model::plan_rows::PlanChange::StepPut { step, .. } => Some(step),
            _ => None,
        })
        .collect();
    let home = sql
        .path()
        .and_then(|p| Path::new(p).parent())
        .unwrap_or_else(|| Path::new(""));
    let executable: Vec<_> = written
        .iter()
        .map(|id| &prepared.compiled.plan.steps()[*id])
        .filter(|step| !step.is_external())
        .collect();
    if !executable.is_empty() {
        let policy = crate::budget::policy(home)?;
        for step in executable {
            crate::budget::resolve(sql, Some(project), step.limits.as_ref(), &policy)?;
        }
    }
    crate::models::check_edit(&base, &prepared.compiled.plan, written, &state.state.inputs)?;
    let bypass = Bypass {
        presets: resolve_bypass(&prepared.compiled.plan, asked)?,
        author,
        reason: if reason.trim().is_empty() {
            "set when its unit was added".into()
        } else {
            reason
        },
    };
    if prepared.dry_run {
        return Ok((
            project,
            Prepared::Answer(Box::new(CommandReply::Preview(prepared.preview))),
        ));
    }
    prepared.board_warnings =
        crate::coordinator::board_drops(sql, project, &base, &prepared.compiled.plan)?;
    let inputs = prepared.inputs.take();
    // The store rechecks a prune's removal set only beside its age evidence: a prune with no
    // age filter (`older_than_seconds` 0) commits as any removal does.
    let prune = if evidence.is_some() {
        prepared.commit.prune.clone()
    } else {
        prepared.commit.prune.take()
    };
    Ok((
        project,
        Prepared::Commit(Box::new(Staged {
            project,
            identity,
            edit: prepared,
            inputs,
            prune,
            evidence,
            owners,
            bypass,
        })),
    ))
}

/// Each `unit.add`'s bypass entries: (unit, suffix or step id, outputs).
fn bypass_asked(
    ops: &[sluice_model::plan_rows::PlanOp],
) -> Vec<(UnitName, String, sluice_model::rpc::JsonMap)> {
    ops.iter()
        .filter_map(|op| match op {
            sluice_model::plan_rows::PlanOp::UnitAdd { unit, bypass, .. } => Some(
                bypass
                    .iter()
                    .map(|(key, outputs)| (unit.clone(), key.clone(), outputs.clone())),
            ),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Each bypass entry's step in the candidate: the unit's step with that id, else with the id
/// `<unit>-<suffix>`; its outputs checked against the step's declared outputs.
fn resolve_bypass(
    candidate: &Plan,
    asked: Vec<(UnitName, String, sluice_model::rpc::JsonMap)>,
) -> Result<Vec<(StepId, sluice_model::rpc::JsonMap)>, PublicError> {
    let mut presets = vec![];
    for (unit, key, outputs) in asked {
        let members = candidate
            .units()
            .get(&unit)
            .map(|u| u.steps.as_slice())
            .unwrap_or_default();
        let step = [key.clone(), format!("{unit}-{key}")]
            .iter()
            .find_map(|id| members.iter().find(|m| m.as_str() == id))
            .ok_or_else(|| PublicError::Invalid {
                message: format!("bypass.{key}: unit {unit} has no step {key}"),
                errors: vec![format!("bypass.{key}: unit {unit} has no step {key}")],
            })?;
        sluice_store::bypass::check_outputs(&candidate.steps()[step], &outputs).map_err(
            |e| match e.into_public(false) {
                PublicError::Invalid { errors, .. } => PublicError::Invalid {
                    message: format!("bypass.{key}: outputs do not match step {step}'s outputs"),
                    errors: errors
                        .into_iter()
                        .map(|e| format!("bypass.{key}.{}", e.trim_start_matches("outputs.")))
                        .collect(),
                },
                other => other,
            },
        )?;
        if presets.iter().any(|(id, _)| id == step) {
            return Err(PublicError::Invalid {
                message: format!("bypass.{key}: step {step} is named twice"),
                errors: vec![format!("bypass.{key}: step {step} is named twice")],
            });
        }
        presets.push((step.clone(), outputs));
    }
    Ok(presets)
}
