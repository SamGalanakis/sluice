//! The differential harness: what the reference says an edit does.
//!
//! `evaluate` runs one schema-3 edit (`plan_rows::PlanOp`s on a base held as rows) through the
//! reference: `ops::apply` builds the candidate (or refuses operations), today's whole-plan
//! compiler validates it, today's `prepare_patch` checks (running steps, changed needs, plan
//! input values) run on it, and today's whole-plan simulation (`gates::simulate_edit`) gives
//! the `"all"` preview, from which §6.3's `"impact"` preview is cut. The incremental model
//! (lane C) must reach the same `Verdict`.
//!
//! `differential_cases` reads the contract's `validation.differential` fixture and
//! `run_differential` runs one case through the same path.

use crate::{
    gates::{CachedResources, GateDecision, StateSnapshot, StepState, evaluate_step, reconcile},
    ids::{Revision, StepId},
    ops::{self, Applied},
    plan::{FnSignature, Plan, ResourceLimit, SignatureProvider, inputs_hash, prepare_candidate},
    recipe::RecipeEntry,
    rpc::{JsonMap, decode_json},
    types::PathError,
};
use indexmap::{IndexMap, IndexSet};
use serde::Deserialize;
use sluice_model::{
    commands::StepStatus,
    plan_rows::{
        EditPreview, PlanChange, PlanHeader, PlanOp, PlanRows, PreviewScope, StateDelta,
        StateEpoch, StatusTransition,
    },
};

/// The catalog an edit is worked out against.
pub struct Context<'a, P> {
    pub signatures: &'a P,
    pub recipes: &'a IndexMap<String, RecipeEntry>,
}

/// One edit to evaluate: a base plan as rows (positions as the store holds them), the
/// operations, `start`, and the runtime state preparation sees.
#[derive(Debug, Clone, PartialEq)]
pub struct EditCase {
    pub base: PlanRows,
    pub ops: Vec<PlanOp>,
    pub start: bool,
    /// Plan input values, step states and the project's pause.
    pub state: StateSnapshot,
    pub resources: CachedResources,
    pub limits: IndexMap<String, ResourceLimit>,
}
impl EditCase {
    /// A case over `document`'s rows (positions `0 … n-1`, rev 1), with `start: true`, no
    /// state, no resources.
    pub fn new(document: &JsonMap, ops: Vec<PlanOp>) -> Result<Self, PathError> {
        Ok(Self {
            base: ops::rows_of(document, header(Revision(1)))?,
            ops,
            start: true,
            state: StateSnapshot::default(),
            resources: CachedResources::default(),
            limits: IndexMap::new(),
        })
    }
}
/// A plan header at `rev` with no sections and epoch 0.
pub fn header(rev: Revision) -> PlanHeader {
    PlanHeader {
        rev,
        root_order: vec![],
        state_epoch: StateEpoch(0),
    }
}

/// What the reference says of an edit.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Operations refused before validation: `invalid plan edit` with `ops[i]…` errors.
    Refused(Vec<String>),
    /// The candidate refused: `invalid plan edit` with plan-path errors, in the whole-plan
    /// compiler's order (or, when it compiles, running steps, changed needs, input values).
    Invalid(Vec<String>),
    Accepted(Box<Accepted>),
}
impl Verdict {
    /// The refusal's errors, whichever kind.
    pub fn errors(&self) -> Option<&[String]> {
        match self {
            Self::Refused(errors) | Self::Invalid(errors) => Some(errors),
            Self::Accepted(_) => None,
        }
    }
    pub fn accepted(&self) -> Option<&Accepted> {
        match self {
            Self::Accepted(accepted) => Some(accepted),
            _ => None,
        }
    }
}

/// An accepted edit.
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    /// The candidate's rows; `rows.header.rev` is the base's.
    pub rows: PlanRows,
    /// The candidate exported (`export_plan` after the commit).
    pub document: JsonMap,
    /// The candidate compiled whole.
    pub plan: Plan,
    /// The net row changes (§4, §5.1): empty for an edit that changes nothing.
    pub changes: Vec<PlanChange>,
    /// The reply's `steps`.
    pub steps: Option<Vec<StepId>>,
    /// The `"all"` preview (`changes` included): today's whole-plan simulation.
    pub all: EditPreview,
    /// The `"impact"` preview (§6.3): `all` cut to `affected`.
    pub impact: EditPreview,
    /// §6.3's affected set, in candidate order: the steps the changes put, the steps whose
    /// reconciled state or readiness the edit changes, and the ready competitors of a step
    /// whose needs or priority changed.
    pub affected: Vec<StepId>,
    /// Removed and added steps (base and candidate position order) and status transitions.
    pub state: StateDelta,
    /// The state the candidate's reconciliation leaves.
    pub reconciled: StateSnapshot,
}

/// Evaluate one edit. `Err` when the base itself does not compile.
pub fn evaluate(
    case: &EditCase,
    context: &Context<'_, impl SignatureProvider>,
) -> Result<Verdict, Vec<PathError>> {
    let before = Plan::parse_owned(ops::document_of(&case.base), context.signatures)?;
    let Applied { rows, added } = match ops::apply(
        &case.base,
        &case.ops,
        case.start,
        context.recipes,
        context.signatures,
    ) {
        Ok(applied) => applied,
        Err(errors) => return Ok(Verdict::Refused(strings(&errors))),
    };
    let document = ops::document_of(&rows);
    let after = match Plan::parse(&document, context.signatures) {
        Ok(plan) => plan,
        Err(errors) => return Ok(Verdict::Invalid(strings(&errors))),
    };
    let dry = match prepare_candidate(&before, &case.state, &after, &case.resources, &case.limits) {
        Ok(dry) => dry,
        Err(errors) => return Ok(Verdict::Invalid(strings(&errors))),
    };
    let changes = ops::changes(&case.base, &rows);
    let all = EditPreview {
        scope: PreviewScope::All,
        changes: changes.clone(),
        would_start: dry.would_start.clone(),
        would_queue: dry.would_queue.keys().cloned().collect(),
        would_skip: dry.would_skip.keys().cloned().collect(),
        would_stale: dry.would_stale.clone(),
        errors: strings(&dry.errors),
    };
    let affected = affected(
        &before,
        &after,
        &case.state,
        &dry.reconciled,
        &changes,
        &all,
    );
    let within = |ids: &[StepId]| -> Vec<StepId> {
        ids.iter()
            .filter(|id| affected.contains(*id))
            .cloned()
            .collect()
    };
    let impact = EditPreview {
        scope: PreviewScope::Impact,
        changes: changes.clone(),
        would_start: within(&all.would_start),
        would_queue: within(&all.would_queue),
        would_skip: within(&all.would_skip),
        would_stale: within(&all.would_stale),
        errors: dry
            .errors
            .iter()
            .filter(|e| {
                e.path
                    .strip_prefix("steps.")
                    .is_some_and(|id| affected.iter().any(|a| a.as_str() == id))
            })
            .map(ToString::to_string)
            .collect(),
    };
    let state = state_delta(&case.base, &rows, &case.state, &dry.reconciled, &after);
    Ok(Verdict::Accepted(Box::new(Accepted {
        rows,
        document,
        plan: after,
        changes,
        steps: added,
        all,
        impact,
        affected: affected.into_iter().collect(),
        state,
        reconciled: dry.reconciled,
    })))
}
fn strings(errors: &[PathError]) -> Vec<String> {
    errors.iter().map(ToString::to_string).collect()
}

/// §6.3's affected set, in the candidate's step order.
fn affected(
    before: &Plan,
    after: &Plan,
    state: &StateSnapshot,
    reconciled: &StateSnapshot,
    changes: &[PlanChange],
    all: &EditPreview,
) -> IndexSet<StepId> {
    let put: IndexSet<&StepId> = changes
        .iter()
        .filter_map(|c| match c {
            PlanChange::StepPut { step, .. } => Some(step),
            _ => None,
        })
        .collect();
    let old = reconcile(before, state);
    let default = StepState::default();
    let ready = |plan: &Plan, state: &StateSnapshot, id: &StepId| {
        state.status(id) == StepStatus::Pending
            && evaluate_step(plan, state, &plan.steps()[id]) == GateDecision::Ready
    };
    let mut resources = IndexSet::new();
    for (id, step) in after.steps() {
        let changed = before
            .steps()
            .get(id)
            .is_none_or(|old| old.needs != step.needs || old.priority != step.priority);
        if changed {
            resources.extend(step.needs.keys().cloned());
            if let Some(old) = before.steps().get(id) {
                resources.extend(old.needs.keys().cloned());
            }
        }
    }
    after
        .steps()
        .iter()
        .filter(|(id, step)| {
            if put.contains(id) {
                return true;
            }
            let (Some(old_step), a, b) = (
                before.steps().get(*id),
                old.steps.get(*id).unwrap_or(&default),
                reconciled.steps.get(*id).unwrap_or(&default),
            ) else {
                return true;
            };
            a.status != b.status
                || a.skipped != b.skipped
                || a.error != b.error
                || inputs_hash(before, &old, old_step) != inputs_hash(after, reconciled, step)
                || ready(before, &old, id) != ready(after, reconciled, id)
                || (all.would_start.contains(id)
                    && step.needs.keys().any(|r| resources.contains(r)))
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// The edit's `StateDelta`: removed steps (base position order), added steps (candidate
/// position order), and each candidate step whose status, skip reasons or error the
/// reconciliation changes from what it holds (an added step holds a fresh `pending`).
fn state_delta(
    base: &PlanRows,
    candidate: &PlanRows,
    state: &StateSnapshot,
    reconciled: &StateSnapshot,
    after: &Plan,
) -> StateDelta {
    let kept: IndexSet<&StepId> = candidate.steps.iter().map(|r| &r.step).collect();
    let was: IndexSet<&StepId> = base.steps.iter().map(|r| &r.step).collect();
    let by_position = |rows: &PlanRows, keep: &dyn Fn(&StepId) -> bool| {
        let mut rows: Vec<_> = rows.steps.iter().filter(|r| keep(&r.step)).collect();
        rows.sort_by_key(|r| r.position);
        rows.into_iter().map(|r| r.step.clone()).collect::<Vec<_>>()
    };
    let removed = by_position(base, &|id| !kept.contains(id));
    let added = by_position(candidate, &|id| !was.contains(id));
    let fresh = StepState::default();
    let transitions = after
        .steps()
        .keys()
        .filter_map(|id| {
            let from = if was.contains(id) {
                state.steps.get(id).unwrap_or(&fresh)
            } else {
                &fresh
            };
            let to = reconciled.steps.get(id).unwrap_or(&fresh);
            (from.status != to.status || from.skipped != to.skipped || from.error != to.error).then(
                || StatusTransition {
                    step: id.clone(),
                    from: from.status.clone(),
                    to: to.status.clone(),
                    skipped: to.skipped.clone(),
                    error: to.error.clone(),
                },
            )
        })
        .collect();
    StateDelta {
        removed,
        added,
        transitions,
    }
}

/// A signature provider that knows every fn as open with no declared inputs or outputs, as
/// `validation.differential`'s cases assume.
pub struct Open;
impl SignatureProvider for Open {
    fn signature(&self, _: &str) -> Option<FnSignature> {
        Some(FnSignature {
            open: true,
            ..Default::default()
        })
    }
}

/// One case of the contract's `validation.differential` fixture (§6.1).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DifferentialCase {
    pub case: String,
    pub base: JsonMap,
    /// The plan inputs' current values.
    #[serde(default)]
    pub values: Option<JsonMap>,
    pub ops: Vec<PlanOp>,
    /// The base with `ops` applied.
    pub candidate: JsonMap,
    /// The candidate's validation errors, in order.
    pub errors: Vec<String>,
}
/// The fixture's text, read from `sluice-model`'s fixtures when this crate is built.
pub const DIFFERENTIAL_FIXTURE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../sluice-model/tests/fixtures/plan_rows/validation.differential.json"
));
pub fn differential_cases() -> Vec<DifferentialCase> {
    decode_json(DIFFERENTIAL_FIXTURE.as_bytes()).expect("validation.differential decodes")
}
/// A fixture case as an `EditCase` (base rows at `0 … n-1`, its values as the state).
pub fn differential_edit(case: &DifferentialCase) -> EditCase {
    let mut edit = EditCase::new(&case.base, case.ops.clone()).expect("a fixture base");
    edit.state.inputs = case.values.clone().unwrap_or_default();
    edit
}
/// Run one fixture case through the reference with `Open` signatures and no recipes.
pub fn run_differential(case: &DifferentialCase) -> Verdict {
    evaluate(
        &differential_edit(case),
        &Context {
            signatures: &Open,
            recipes: &IndexMap::new(),
        },
    )
    .unwrap_or_else(|e| panic!("{}: the base does not compile: {e:?}", case.case))
}
