//! An edit's preparation (`docs/design/plan-rows.md` §4, §6): outside the writer, from a
//! certified compiled base, the runtime state of its read set, the fn catalog and the recipe
//! catalog. The operations apply to one candidate (`ops`), the candidate compiles for what
//! they change (`compile`), the running-step, resource-need and input-value rules hold, and the
//! affected runtime state is reconciled for the impact preview, the commit's state delta and
//! its index rows.
//!
//! Preparation never consults a step outside its read set: `preparation_reads` names it,
//! round by round, and `prepare_plan_edit` refuses (and in debug builds panics) if a step it
//! consults was not read. The stored state of the steps it reads is taken as the base's
//! reconciled state (the scheduler reconciles every tick); a step the edit does not reach is
//! left to the next tick.

use super::{
    Applied, Binding, Change, Delta, Plan, ResourceLimit, SignatureProvider, apply_ops,
    compile::{Compiled, Touched},
    compile_delta, inputs_hash, needs_errors, output_references, step_edges, step_index,
};
use crate::{
    commands::StepStatus,
    error::PublicError,
    gates::{
        CachedResources, Gate, GateDecision, Reference, StateSnapshot, StepState, evaluate_step,
        reconcile_step, simulate_edit,
    },
    ids::{Revision, StepId},
    plan_rows::{
        CertifiedPlan, EditPreview, PlanChange, PlanEditCommit, PlanOp, PlanRowsError,
        PreparationReads, PreparedPlanEdit, PreviewScope, RowDelta, ScopedState, StateDelta,
        StatusTransition, ValidationTokens,
    },
    recipe::RecipeEntry,
    types::PathError,
};
use indexmap::{IndexMap, IndexSet};
use std::{collections::VecDeque, sync::Arc};

/// What an edit is prepared from (§8).
pub struct EditBase<'a, P> {
    /// The certified compiled base, at `tokens.plan_rev`, compiled against the catalog of
    /// `tokens.catalog_generation`.
    pub plan: &'a Arc<Plan>,
    pub tokens: &'a ValidationTokens,
    /// The runtime state of the read set, read in one snapshot (every round's reads).
    pub state: &'a ScopedState,
    /// The current catalog: it answers for every fn the project sees, so an added step may run
    /// a fn the base never used.
    pub signatures: &'a P,
    pub recipes: &'a IndexMap<String, RecipeEntry>,
    /// Cached capacities (`None`: a capacity fn with no value yet).
    pub capacities: &'a IndexMap<String, Option<u64>>,
    pub limits: &'a IndexMap<String, ResourceLimit>,
}

/// How an edit is made (`EditOptions3` in the contract): the caller's `rev`, `dry_run`, the
/// preview's scope, `start` (`false` adds new steps paused), and who made it, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct PrepareOptions {
    pub rev: Option<Revision>,
    pub dry_run: bool,
    pub preview_scope: PreviewScope,
    pub start: bool,
    pub reason: String,
    /// The author, its default already applied (SPEC §12.3).
    pub author: String,
}

/// The read set of an edit's preparation, given what `base.state` already holds (§6.4). Read
/// it all, in one snapshot with what was read before, and ask again: the read set grows as the
/// state read so far shows what the edit reaches (a source's status, a competitor's sources),
/// and preparation can start once a round adds nothing. The result includes what `base.state`
/// holds. An edit whose operations are refused or whose candidate is invalid reads nothing more.
pub fn preparation_reads(
    base: &EditBase<'_, impl SignatureProvider>,
    ops: &[PlanOp],
    scope: PreviewScope,
) -> PreparationReads {
    let mut reads = Reads::new(base.state);
    let _ = work_out(base, ops, true, scope, &mut reads);
    reads.finish()
}

/// Prepare one edit from its operations: refuse a stale explicit `rev` (`conflict`), refused
/// operations or an invalid candidate (`invalid`), then reconcile what the edit affects and
/// build the store's payload, the reply's parts and the certified candidate. An edit whose net
/// effect changes nothing (no operations at all, as a typed tool's no-op lowers to) prepares a
/// commit with no changes and certifies the base itself.
pub fn prepare_plan_edit(
    base: &EditBase<'_, impl SignatureProvider>,
    ops: Vec<PlanOp>,
    options: PrepareOptions,
) -> Result<PreparedPlanEdit, PublicError> {
    let current = base.tokens.plan_rev;
    if options.rev.is_some_and(|rev| rev != current) {
        return Err(PlanRowsError::StaleRev { current }.into());
    }
    if options.preview_scope == PreviewScope::All && !options.dry_run {
        return Err(PlanRowsError::PreviewAllNeedsDryRun.into());
    }
    if options.rev.is_none() && ops.iter().any(|op| matches!(op, PlanOp::OrderSet { .. })) {
        return Err(PlanRowsError::OrderNeedsRev.into());
    }
    let mut reads = Reads::new(base.state);
    let worked = work_out(base, &ops, options.start, options.preview_scope, &mut reads);
    if !reads.missed.is_empty() {
        debug_assert!(
            false,
            "preparation consulted steps outside its read set: {:?}",
            reads.missed
        );
        return Err(PublicError::Storage {
            message: format!(
                "the edit's read set missed {} step(s); prepare it again",
                reads.missed.len()
            ),
        });
    }
    let worked = worked.map_err(|errors| PublicError::Invalid {
        message: "invalid plan edit".into(),
        errors,
    })?;
    let compiled = CertifiedPlan {
        plan: worked
            .candidate
            .map(Arc::new)
            .unwrap_or_else(|| base.plan.clone()),
        base_rev: current,
        catalog_generation: base.tokens.catalog_generation,
    };
    Ok(PreparedPlanEdit {
        commit: PlanEditCommit {
            tokens: base.tokens.clone(),
            rev: options.rev,
            author: options.author,
            reason: options.reason,
            rows: worked.rows,
            state: worked.state,
            prune: None,
        },
        dry_run: options.dry_run,
        preview: worked.preview,
        steps: worked.added,
        board_warnings: vec![],
        inputs: None,
        compiled,
    })
}

/// What preparation read, and what it consulted without reading.
struct Reads<'s> {
    state: &'s ScopedState,
    steps: IndexSet<StepId>,
    inputs: IndexSet<String>,
    resources: IndexSet<String>,
    missed: IndexSet<StepId>,
}
impl<'s> Reads<'s> {
    fn new(state: &'s ScopedState) -> Self {
        Self {
            state,
            steps: IndexSet::new(),
            inputs: IndexSet::new(),
            resources: IndexSet::new(),
            missed: IndexSet::new(),
        }
    }
    /// A step of the base whose stored state preparation consults.
    fn step(&mut self, id: &StepId) {
        if self.steps.insert(id.clone()) && !self.state.state.steps.contains_key(id) {
            self.missed.insert(id.clone());
        }
    }
    fn input(&mut self, name: &str) {
        self.inputs.insert(name.to_owned());
    }
    fn resource(&mut self, name: &str) {
        self.resources.insert(name.to_owned());
    }
    /// What was read before and what this round consulted.
    fn finish(self) -> PreparationReads {
        let mut steps: IndexSet<StepId> = self.state.state.steps.keys().cloned().collect();
        steps.extend(self.steps);
        let mut inputs: IndexSet<String> = self.state.state.inputs.0.keys().cloned().collect();
        inputs.extend(self.inputs);
        let mut resources: IndexSet<String> = self
            .state
            .leases
            .iter()
            .map(|lease| lease.resource.clone())
            .chain(self.state.competitors.iter().map(|c| c.resource.clone()))
            .collect();
        resources.extend(self.resources);
        PreparationReads {
            steps: steps.into_iter().collect(),
            inputs: inputs.into_iter().collect(),
            resources: resources.iter().cloned().collect(),
            competitors: resources.into_iter().collect(),
        }
    }
}

/// An edit worked out.
struct Worked {
    /// The compiled candidate; `None` when the edit changes nothing (the base stands).
    candidate: Option<Plan>,
    rows: RowDelta,
    state: StateDelta,
    preview: EditPreview,
    added: Option<Vec<StepId>>,
}

fn work_out(
    base: &EditBase<'_, impl SignatureProvider>,
    ops: &[PlanOp],
    start: bool,
    scope: PreviewScope,
    reads: &mut Reads<'_>,
) -> Result<Worked, Vec<String>> {
    let before: &Plan = base.plan;
    let strings = |errors: Vec<PathError>| errors.iter().map(ToString::to_string).collect();
    let Applied {
        delta,
        changes,
        added,
    } = apply_ops(before, ops, start, base.recipes, base.signatures).map_err(strings)?;
    if changes.is_empty() {
        let preview = match scope {
            PreviewScope::Impact => empty_preview(scope, vec![]),
            PreviewScope::All => whole_preview(base, before, before, vec![], reads),
        };
        return Ok(Worked {
            candidate: None,
            rows: RowDelta::default(),
            state: StateDelta::default(),
            preview,
            added,
        });
    }
    let Compiled {
        plan: after,
        touched,
    } = compile_delta(before, &delta, base.signatures).map_err(strings)?;

    // What compiling cannot see: a running step changes only its pause and tags, changed needs
    // fit the project's resources, and a re-declared input's value fits its new type.
    let mut errors = vec![];
    let mut changed: Vec<(u64, &StepId)> = touched
        .written
        .iter()
        .chain(&touched.removed)
        .chain(&touched.recompiled)
        .filter_map(|id| Some((before.position(id)?, id)))
        .collect::<IndexSet<_>>()
        .into_iter()
        .collect();
    changed.sort();
    for (_, id) in changed {
        reads.step(id);
        if base.state.state.status(id) == StepStatus::Running
            && !after
                .steps()
                .get(id)
                .is_some_and(|new| before.steps()[id].same_work(new))
        {
            errors.push(super::diagnostic(
                &format!("steps.{id}"),
                "cannot remove or change a running step except paused and tags",
            ));
        }
    }
    let mut written: Vec<(u64, &StepId)> = touched
        .written
        .iter()
        .filter_map(|id| Some((after.position(id)?, id)))
        .collect();
    written.sort();
    for (_, id) in written {
        let step = &after.steps()[id];
        if before
            .steps()
            .get(id)
            .is_none_or(|old| old.needs != step.needs)
        {
            errors.extend(needs_errors(step, base.limits));
        }
    }
    for name in &touched.inputs {
        reads.input(name);
    }
    for (name, value) in &base.state.state.inputs.0 {
        if touched.inputs.contains(name)
            && before.inputs().contains_key(name)
            && let Some(declaration) = after.inputs().get(name)
            && let Err(found) = crate::types::check_value_at(
                &declaration.ty,
                value.as_value(),
                &format!("inputs.{name}"),
            )
        {
            errors.extend(found);
        }
    }
    if !errors.is_empty() {
        return Err(strings(errors));
    }

    let mut impact = Impact::work_out(base, before, &after, &touched, &changes, reads);
    let preview = match scope {
        PreviewScope::Impact => impact.preview(changes.clone(), reads),
        PreviewScope::All => whole_preview(base, before, &after, changes.clone(), reads),
    };
    let state = impact.state_delta(&delta);
    drop(impact);
    let rows = row_delta(before, &after, &touched, changes);
    Ok(Worked {
        candidate: Some(after),
        rows,
        state,
        preview,
        added,
    })
}

fn empty_preview(scope: PreviewScope, changes: Vec<PlanChange>) -> EditPreview {
    EditPreview {
        scope,
        changes,
        would_start: vec![],
        would_queue: vec![],
        would_skip: vec![],
        would_stale: vec![],
        errors: vec![],
    }
}

/// What each live lease holds, by resource.
fn held(state: &ScopedState) -> IndexMap<String, u64> {
    let mut held = IndexMap::<String, u64>::new();
    for lease in state.leases.iter().filter(|lease| lease.held) {
        let amount = held.entry(lease.resource.clone()).or_default();
        *amount = amount.saturating_add(lease.amount);
    }
    held
}

/// `preview_scope: "all"`: the whole plan reconciled and simulated before and after, from the
/// whole state (the read set is every step, every input and every resource).
fn whole_preview(
    base: &EditBase<'_, impl SignatureProvider>,
    before: &Plan,
    after: &Plan,
    changes: Vec<PlanChange>,
    reads: &mut Reads<'_>,
) -> EditPreview {
    for id in before.steps().keys() {
        reads.step(id);
    }
    for name in before.inputs().keys().chain(after.inputs().keys()) {
        reads.input(name);
    }
    let resources: IndexSet<&String> = base
        .capacities
        .keys()
        .chain(before.needing.keys())
        .chain(after.needing.keys())
        .collect();
    for name in resources {
        reads.resource(name);
    }
    let stored = &base.state.state;
    let mut projected = stored.clone();
    projected
        .inputs
        .0
        .retain(|name, _| before.inputs().contains_key(name) && after.inputs().contains_key(name));
    let dry = simulate_edit(
        before,
        stored,
        after,
        &projected,
        &CachedResources {
            capacities: base.capacities.clone(),
            leased: held(base.state),
        },
    );
    EditPreview {
        scope: PreviewScope::All,
        changes,
        would_start: dry.would_start,
        would_queue: dry.would_queue.into_keys().collect(),
        would_skip: dry.would_skip.into_keys().collect(),
        would_stale: dry.would_stale,
        errors: dry.errors.iter().map(ToString::to_string).collect(),
    }
}

/// The edit's reach over runtime state (§6.3, §6.4): the steps it reconciles again, their state
/// before (as stored) and after, and the affected set the impact preview describes.
struct Impact<'p> {
    before: &'p Plan,
    after: &'p Plan,
    /// The stored state of the read set.
    stored: &'p StateSnapshot,
    /// The candidate's state: the stored state of the steps it keeps, the values of the inputs
    /// it keeps, and each reached step reconciled again.
    now: StateSnapshot,
    /// The steps reconciled again (the steps the edit adds among them).
    reached: IndexSet<StepId>,
    /// §6.3's affected set, in candidate order.
    affected: Vec<StepId>,
    /// Of the affected steps, the ready ones admission would queue, in admission order.
    queued: Vec<StepId>,
    /// Whether a step would start in the candidate, once worked out.
    ready: IndexMap<StepId, bool>,
}

impl<'p> Impact<'p> {
    fn work_out(
        base: &'p EditBase<'_, impl SignatureProvider>,
        before: &'p Plan,
        after: &'p Plan,
        touched: &Touched,
        changes: &[PlanChange],
        reads: &mut Reads<'_>,
    ) -> Self {
        let stored = &base.state.state;
        let mut now = stored.clone();
        now.inputs.0.retain(|name, _| {
            before.inputs().contains_key(name) && after.inputs().contains_key(name)
        });
        let mut impact = Self {
            before,
            after,
            stored,
            now,
            reached: IndexSet::new(),
            affected: vec![],
            queued: vec![],
            ready: IndexMap::new(),
        };
        impact.reach(touched, reads);

        // The affected set: what the edit puts, and every reached step whose state, inputs hash
        // or readiness the edit changes; then the ready steps competing for the resources of a
        // step whose needs or priority it changes.
        let put: IndexSet<&StepId> = changes
            .iter()
            .filter_map(|change| match change {
                PlanChange::StepPut { step, .. } => Some(step),
                _ => None,
            })
            .collect();
        let mut affected: IndexSet<StepId> = IndexSet::new();
        let fresh = StepState::default();
        for id in impact.reached.clone() {
            if put.contains(&id) || !before.steps().contains_key(&id) {
                affected.insert(id);
                continue;
            }
            let old = stored.steps.get(&id).unwrap_or(&fresh);
            let new = impact.now.steps.get(&id).unwrap_or(&fresh);
            let differs = old.status != new.status
                || old.skipped != new.skipped
                || old.error != new.error
                || inputs_hash(before, stored, &before.steps()[&id])
                    != inputs_hash(after, &impact.now, &after.steps()[&id])
                || ready(before, stored, &id) != impact.is_ready(&id, reads);
            if differs {
                affected.insert(id);
            }
        }
        let mut contested: IndexSet<String> = IndexSet::new();
        for id in &touched.written {
            if let Some(step) = after.steps().get(id) {
                let old = before.steps().get(id);
                if old.is_none_or(|old| old.needs != step.needs || old.priority != step.priority) {
                    contested.extend(step.needs.keys().cloned());
                    if let Some(old) = old {
                        contested.extend(old.needs.keys().cloned());
                    }
                }
            }
        }
        for resource in &contested {
            affected.extend(impact.competitors(base, resource, reads));
        }
        let mut affected: Vec<(u64, StepId)> = affected
            .into_iter()
            .filter_map(|id| Some((after.position(&id)?, id)))
            .collect();
        affected.sort();
        impact.affected = affected.into_iter().map(|(_, id)| id).collect();

        // Admission among the ready steps that share a resource, directly or through each
        // other, with an affected ready step.
        let mut resources: IndexSet<String> = IndexSet::new();
        for id in impact.affected.clone() {
            if impact.is_ready(&id, reads) {
                resources.extend(after.steps()[&id].needs.keys().cloned());
            }
        }
        let mut contenders: IndexSet<StepId> = IndexSet::new();
        let mut index = 0;
        while index < resources.len() {
            let resource = resources[index].clone();
            index += 1;
            for id in impact.competitors(base, &resource, reads) {
                if contenders.insert(id.clone()) {
                    resources.extend(after.steps()[&id].needs.keys().cloned());
                }
            }
        }
        if !contenders.is_empty() {
            let queued = impact.admit(base, contenders);
            impact.queued = queued
                .into_iter()
                .filter(|id| impact.affected.contains(id))
                .collect();
        }
        impact
    }

    /// Reconcile again, each after the steps it depends on, every step the edit can reach:
    /// what it writes or moves, what it compiled again, what gates on a unit whose exits
    /// changed; then every dependent of a step whose status changes.
    fn reach(&mut self, touched: &Touched, reads: &mut Reads<'_>) {
        let after = self.after;
        let mut pending: IndexSet<StepId> = touched
            .written
            .iter()
            .chain(&touched.moved)
            .chain(&touched.recompiled)
            .chain(&touched.dependencies)
            .filter(|id| after.steps().contains_key(*id))
            .cloned()
            .collect();
        let mut queue: VecDeque<StepId> = pending.iter().cloned().collect();
        while let Some(start) = queue.pop_front() {
            let mut stack = vec![start];
            while let Some(top) = stack.last().cloned() {
                if !pending.contains(&top) {
                    stack.pop();
                    continue;
                }
                if let Some(dependency) = after
                    .dependencies(&top)
                    .iter()
                    .find(|d| pending.contains(*d) && !stack.contains(d))
                {
                    stack.push(dependency.clone());
                    continue;
                }
                stack.pop();
                pending.shift_remove(&top);
                if self.reconcile(&top, reads) {
                    for dependent in after.dependents(&top) {
                        if pending.insert(dependent.clone()) {
                            queue.push_back(dependent);
                        }
                    }
                }
            }
        }
    }

    /// Reconcile `id` again in the candidate, from its stored state and its dependencies' state
    /// now. Whether its status changed from what its dependents last saw.
    fn reconcile(&mut self, id: &StepId, reads: &mut Reads<'_>) -> bool {
        let (before, after) = (self.before, self.after);
        if before.steps().contains_key(id) {
            reads.step(id);
        }
        for dependency in after.dependencies(id).iter().chain(before.dependencies(id)) {
            if before.steps().contains_key(dependency) {
                reads.step(dependency);
            }
        }
        for name in input_names(after, id)
            .into_iter()
            .chain(input_names(before, id))
        {
            reads.input(&name);
        }
        let seen = self.now.status(id);
        match self
            .stored
            .steps
            .get(id)
            .filter(|_| before.steps().contains_key(id))
        {
            Some(state) => {
                self.now.steps.insert(id.clone(), state.clone());
            }
            None => {
                self.now.steps.shift_remove(id);
            }
        }
        reconcile_step(after, &mut self.now, id);
        self.reached.insert(id.clone());
        self.now.status(id) != seen
    }

    /// Whether `id` would start in the candidate: pending, not `core.external`, gates open.
    fn is_ready(&mut self, id: &StepId, reads: &mut Reads<'_>) -> bool {
        if let Some(ready) = self.ready.get(id) {
            return *ready;
        }
        let (before, after) = (self.before, self.after);
        if before.steps().contains_key(id) && !self.reached.contains(id) {
            reads.step(id);
        }
        for dependency in after.dependencies(id) {
            if before.steps().contains_key(dependency) && !self.reached.contains(dependency) {
                reads.step(dependency);
            }
        }
        for name in input_names(after, id) {
            reads.input(&name);
        }
        let ready = ready(after, &self.now, id);
        self.ready.insert(id.clone(), ready);
        ready
    }

    /// The candidate's ready steps that need `resource`: the stored pending competitors and the
    /// reached steps, each ready now.
    fn competitors(
        &mut self,
        base: &EditBase<'_, impl SignatureProvider>,
        resource: &str,
        reads: &mut Reads<'_>,
    ) -> Vec<StepId> {
        reads.resource(resource);
        let mut ids: IndexSet<StepId> = base
            .state
            .competitors
            .iter()
            .filter(|row| row.resource == resource)
            .map(|row| row.step.clone())
            .collect();
        ids.extend(self.reached.iter().cloned());
        ids.into_iter()
            .filter(|id| {
                self.after
                    .steps()
                    .get(id)
                    .is_some_and(|step| step.needs.contains_key(resource))
            })
            .filter(|id| self.is_ready(id, reads))
            .collect()
    }

    /// Admission among `contenders` (ready steps): by priority, highest first, then the
    /// candidate's topological order; each starts when every resource it needs has room on the
    /// cached capacity beside what live leases hold. The ones queued, in admission order.
    fn admit(
        &self,
        base: &EditBase<'_, impl SignatureProvider>,
        contenders: IndexSet<StepId>,
    ) -> Vec<StepId> {
        let after = self.after;
        let mut order: Vec<StepId> = contenders.into_iter().collect();
        let priorities: IndexSet<i64> = order.iter().map(|id| after.steps()[id].priority).collect();
        if priorities.len() < order.len() {
            let rank: IndexMap<&StepId, usize> = after
                .topological_order()
                .iter()
                .enumerate()
                .map(|(rank, id)| (id, rank))
                .collect();
            order.sort_by_key(|id| rank[id]);
        }
        order.sort_by_key(|id| std::cmp::Reverse(after.steps()[id].priority));
        let mut held = held(base.state);
        let mut queued = vec![];
        for id in order {
            let step = &after.steps()[&id];
            let short = step.needs.iter().any(|(name, need)| {
                *need > 0
                    && base
                        .capacities
                        .get(name)
                        .copied()
                        .flatten()
                        .is_none_or(|capacity| {
                            capacity.saturating_sub(held.get(name).copied().unwrap_or(0)) < *need
                        })
            });
            if short {
                queued.push(id);
            } else {
                for (name, need) in &step.needs {
                    let amount = held.entry(name.clone()).or_default();
                    *amount = amount.saturating_add(*need);
                }
            }
        }
        queued
    }

    /// The impact preview: today's whole preview, cut to the affected set.
    fn preview(&mut self, changes: Vec<PlanChange>, reads: &mut Reads<'_>) -> EditPreview {
        let mut preview = empty_preview(PreviewScope::Impact, changes);
        let fresh = StepState::default();
        for id in self.affected.clone() {
            let old = self
                .stored
                .steps
                .get(&id)
                .filter(|_| self.before.steps().contains_key(&id));
            let new = self.now.steps.get(&id).unwrap_or(&fresh).clone();
            if self.is_ready(&id, reads) {
                preview.would_start.push(id.clone());
            }
            if new.status == StepStatus::Skipped
                && old.is_none_or(|old| {
                    old.status != StepStatus::Skipped || old.skipped != new.skipped
                })
            {
                preview.would_skip.push(id.clone());
            }
            if new.status == StepStatus::Stale
                && old.is_none_or(|old| old.status != StepStatus::Stale)
            {
                preview.would_stale.push(id.clone());
            }
            if new.status == StepStatus::Failed
                && old.is_none_or(|old| old.status != StepStatus::Failed || old.error != new.error)
                && let Some(error) = &new.error
            {
                preview.errors.push(format!("steps.{id}: {error}"));
            }
        }
        preview.would_queue = self.queued.clone();
        preview
    }

    /// Removed and added steps, and each reached step whose status, skip reasons or error the
    /// edit's reconciliation changes from what it holds (an added step holds a fresh `pending`).
    fn state_delta(&self, delta: &Delta) -> StateDelta {
        let (before, after) = (self.before, self.after);
        let mut removed: Vec<(u64, StepId)> = delta
            .steps
            .iter()
            .filter(|(_, change)| matches!(change, Change::Removed))
            .filter_map(|(id, _)| Some((before.position(id)?, id.clone())))
            .collect();
        removed.sort();
        let mut added: Vec<(u64, StepId)> = delta
            .steps
            .keys()
            .filter(|id| !before.steps().contains_key(*id))
            .filter_map(|id| Some((after.position(id)?, id.clone())))
            .collect();
        added.sort();
        let fresh = StepState::default();
        let mut reached: Vec<(u64, &StepId)> = self
            .reached
            .iter()
            .filter_map(|id| Some((after.position(id)?, id)))
            .collect();
        reached.sort();
        let transitions = reached
            .into_iter()
            .filter_map(|(_, id)| {
                let from = if before.steps().contains_key(id) {
                    self.stored.steps.get(id).unwrap_or(&fresh)
                } else {
                    &fresh
                };
                let to = self.now.steps.get(id).unwrap_or(&fresh);
                (from.status != to.status || from.skipped != to.skipped || from.error != to.error)
                    .then(|| StatusTransition {
                        step: id.clone(),
                        from: from.status.clone(),
                        to: to.status.clone(),
                        skipped: to.skipped.clone(),
                        error: to.error.clone(),
                    })
            })
            .collect();
        StateDelta {
            removed: removed.into_iter().map(|(_, id)| id).collect(),
            added: added.into_iter().map(|(_, id)| id).collect(),
            transitions,
        }
    }
}

/// Whether `id` would start in `plan` with `state`: pending, not `core.external`, gates open.
fn ready(plan: &Plan, state: &StateSnapshot, id: &StepId) -> bool {
    plan.steps().get(id).is_some_and(|step| {
        state.status(id) == StepStatus::Pending
            && !step.is_external()
            && evaluate_step(plan, state, step) == GateDecision::Ready
    })
}

/// The plan inputs a step reads (bindings and boolean gates).
fn input_names(plan: &Plan, id: &StepId) -> Vec<String> {
    let Some(step) = plan.steps().get(id) else {
        return vec![];
    };
    let mut names = vec![];
    for reference in step.bindings.values().flat_map(Binding::references) {
        if let Ok(Reference {
            step: None, name, ..
        }) = reference.parts()
        {
            names.push(name);
        }
    }
    for gate in &step.after {
        if let Gate::Bool { reference, .. } = gate
            && let Ok(Reference {
                step: None, name, ..
            }) = reference.parts()
        {
            names.push(name);
        }
    }
    names
}

/// Everything the store writes for the edit's rows: the changes, the index rows of every step
/// and output put, and the incoming edges of every step whose edges change.
fn row_delta(before: &Plan, after: &Plan, touched: &Touched, changes: Vec<PlanChange>) -> RowDelta {
    let is_step = |name: &str| StepId::new(name).is_ok_and(|id| after.steps().contains_key(&id));
    let mut step_index_rows = vec![];
    let mut output_refs = vec![];
    for change in &changes {
        match change {
            PlanChange::StepPut {
                step, declaration, ..
            } => step_index_rows.push(step_index(step, declaration, &is_step)),
            PlanChange::OutputPut { name, binding, .. } => {
                output_refs.push((name.clone(), output_references(name, binding)));
            }
            _ => {}
        }
    }
    let mut targets: Vec<(u64, &StepId)> = touched
        .written
        .iter()
        .chain(&touched.dependencies)
        .collect::<IndexSet<_>>()
        .into_iter()
        .filter_map(|id| Some((after.position(id)?, id)))
        .collect();
    targets.sort();
    let mut edges = vec![];
    for (_, id) in targets {
        let new = step_edges(after, id);
        let changed = !before.steps().contains_key(id) || {
            let old = step_edges(before, id);
            old.len() != new.len() || !old.iter().all(|edge| new.contains(edge))
        };
        if changed {
            edges.push((id.clone(), new));
        }
    }
    RowDelta {
        changes,
        step_index: step_index_rows,
        output_refs,
        edges,
    }
}
