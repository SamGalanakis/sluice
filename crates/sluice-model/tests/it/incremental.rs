//! The incremental model against the reference (lane H's `sluice-reference`): today's
//! whole-plan compiler, simulation and the schema-3 meaning of each operation.
//!
//! - `compile_rows` compiles every plan today's compiler does to the same plan, and refuses
//!   every candidate it refuses with the same errors in the same order.
//! - An edit prepared incrementally (operations on a certified base, validation of what they
//!   change, the impact reconciled from a read set grown round by round as the runtime reads
//!   it) reaches the reference's verdict: the same refusals; or the same row changes, reply
//!   steps, compiled candidate, impact and full previews, and state delta. Reading a scoped
//!   state also holds that preparation never consults a step outside its read set (it panics
//!   in a debug build if it does).
//! - The store's index rows of the steps the edit puts, and the incoming edges of every step it
//!   reaches, equal the ones rebuilt from the candidate; every other step's edges are as they
//!   were.

#[path = "incremental/operation_sequences.rs"]
mod operation_sequences;

use crate::support;

use indexmap::IndexMap;
use proptest::prelude::*;
use sluice_model::{
    StateSnapshot,
    commands::StepStatus,
    error::PublicError,
    ids::{Revision, StepId},
    plan::{
        EditBase, Plan, PrepareOptions, ResourceLimit, compile_rows, preparation_reads,
        prepare_plan_edit, step_edges, step_index,
    },
    plan_index::plan_edges,
    plan_rows::{
        CompetitorRow, EdgeRow, LeaseRow, PlanChange, PlanOp, PlanRows, PreparationReads,
        PreparedPlanEdit, PreviewScope, ScopedState,
    },
    recipe::RecipeEntry,
};
use sluice_reference::{
    generate::{self, Fixture},
    harness::{EditCase, Verdict, evaluate},
    interop::{model_limits, model_signatures, model_state, recipe_catalogs},
    ops, plan_shape,
};
// Today's whole-plan compiler, as the test-only reference keeps it.
use sluice_reference::Plan as Reference;
use std::sync::{Arc, LazyLock};

static FIXTURE: LazyLock<Fixture> = LazyLock::new(Fixture::new);
static MODEL: LazyLock<Model> = LazyLock::new(Model::new);

/// The model's view of the generated catalog.
struct Model {
    signatures: IndexMap<String, sluice_model::FnSignature>,
    recipes: IndexMap<String, RecipeEntry>,
}
impl Model {
    fn new() -> Self {
        Self {
            signatures: model_signatures(&generate::catalog()),
            recipes: recipe_catalogs(generate::RECIPES).1,
        }
    }
}

/// The case's state as the store holds it: a row for every step, reconciled (the scheduler
/// reconciles every tick).
fn settle(case: &mut EditCase, fixture: &Fixture) {
    let before = Reference::parse(&ops::document_of(&case.base), &fixture.signatures).unwrap();
    for id in before.steps().keys() {
        case.state.steps.entry(id.clone()).or_default();
    }
    for _ in 0..4 {
        let next = sluice_reference::gates::reconcile(&before, &case.state);
        if next == case.state {
            break;
        }
        case.state = next;
    }
}

/// Everything the store could answer about the case: each step's state, the inputs' values,
/// the leases (what the reference's `leased` says, and each running step's needs, held as
/// leases as a running step holds them) and each resource's pending competitors.
struct Store {
    state: StateSnapshot,
    leases: Vec<LeaseRow>,
    competitors: Vec<CompetitorRow>,
    capacities: IndexMap<String, Option<u64>>,
    limits: IndexMap<String, ResourceLimit>,
}
impl Store {
    fn of(case: &EditCase, base: &Plan) -> Self {
        let state = model_state(&case.state);
        let mut leases: Vec<LeaseRow> = case
            .resources
            .leased
            .iter()
            .map(|(resource, amount)| LeaseRow {
                resource: resource.clone(),
                project: None,
                step: None,
                held: true,
                amount: *amount,
                priority: 0,
            })
            .collect();
        let mut competitors = vec![];
        for (id, step) in base.steps() {
            for (resource, amount) in &step.needs {
                match state.status(id) {
                    StepStatus::Running => leases.push(LeaseRow {
                        resource: resource.clone(),
                        project: None,
                        step: Some(id.clone()),
                        held: true,
                        amount: *amount,
                        priority: step.priority,
                    }),
                    StepStatus::Pending => competitors.push(CompetitorRow {
                        resource: resource.clone(),
                        step: id.clone(),
                        priority: step.priority,
                        needs: Default::default(),
                    }),
                    _ => {}
                }
            }
        }
        Self {
            state,
            leases,
            competitors,
            capacities: case.resources.capacities.clone(),
            limits: model_limits(&case.limits),
        }
    }
    /// What `read_scoped_state` answers for `reads`: exactly the read set.
    fn read(&self, reads: &PreparationReads) -> ScopedState {
        let mut state = StateSnapshot {
            paused: self.state.paused.clone(),
            ..Default::default()
        };
        for id in &reads.steps {
            if let Some(entry) = self.state.steps.get(id) {
                state.steps.insert(id.clone(), entry.clone());
            }
        }
        for name in &reads.inputs {
            if let Some(value) = self.state.inputs.0.get(name) {
                state.inputs.0.insert(name.clone(), value.clone());
            }
        }
        ScopedState {
            state,
            leases: self
                .leases
                .iter()
                .filter(|lease| reads.resources.contains(&lease.resource))
                .cloned()
                .collect(),
            competitors: self
                .competitors
                .iter()
                .filter(|row| reads.competitors.contains(&row.resource))
                .cloned()
                .collect(),
        }
    }
}

fn union(into: &mut PreparationReads, more: &PreparationReads) -> bool {
    fn add<T: Clone + PartialEq>(into: &mut Vec<T>, more: &[T]) -> bool {
        let mut grew = false;
        for item in more {
            if !into.contains(item) {
                into.push(item.clone());
                grew = true;
            }
        }
        grew
    }
    let a = add(&mut into.steps, &more.steps);
    let b = add(&mut into.inputs, &more.inputs);
    let c = add(&mut into.resources, &more.resources);
    let d = add(&mut into.competitors, &more.competitors);
    a || b || c || d
}

/// Prepare as the runtime does: read, ask for the read set, read what it adds, until a round
/// adds nothing; then prepare from exactly what was read.
fn prepare_scoped(
    base: &Arc<Plan>,
    store: &Store,
    model: &Model,
    ops: &[PlanOp],
    options: PrepareOptions,
) -> (Result<PreparedPlanEdit, PublicError>, usize) {
    let tokens = support::tokens();
    let mut reads = PreparationReads::default();
    let mut rounds = 0;
    let state = loop {
        rounds += 1;
        assert!(rounds < 50, "the read set does not settle");
        let state = store.read(&reads);
        let edit = EditBase {
            plan: base,
            tokens: &tokens,
            state: &state,
            signatures: &model.signatures,
            recipes: &model.recipes,
            capacities: &store.capacities,
            limits: &store.limits,
        };
        let next = preparation_reads(&edit, ops, options.preview_scope);
        if !union(&mut reads, &next) {
            break state;
        }
    };
    let edit = EditBase {
        plan: base,
        tokens: &tokens,
        state: &state,
        signatures: &model.signatures,
        recipes: &model.recipes,
        capacities: &store.capacities,
        limits: &store.limits,
    };
    (prepare_plan_edit(&edit, ops.to_vec(), options), rounds)
}

fn options(start: bool, scope: PreviewScope) -> PrepareOptions {
    PrepareOptions {
        rev: Some(Revision(1)),
        dry_run: scope == PreviewScope::All,
        preview_scope: scope,
        start,
        reason: "generated".into(),
        author: "test".into(),
    }
}

/// The reference's verdict and the incremental model's agree on `case`.
fn agree(mut case: EditCase, fixture: &Fixture, model: &Model) -> Result<(), TestCaseError> {
    settle(&mut case, fixture);
    let verdict = evaluate(&case, &fixture.context()).expect("a generated base compiles");
    let base = Arc::new(compile_rows(&case.base, &model.signatures).expect("the base compiles"));
    let store = Store::of(&case, &base);
    let (prepared, _) = prepare_scoped(
        &base,
        &store,
        model,
        &case.ops,
        options(case.start, PreviewScope::Impact),
    );
    let ops_text = serde_json::to_string(&case.ops).unwrap();
    match (&verdict, prepared) {
        (Verdict::Refused(errors) | Verdict::Invalid(errors), Err(error)) => {
            prop_assert_eq!(
                &error,
                &PublicError::Invalid {
                    message: "invalid plan edit".into(),
                    errors: errors.clone(),
                },
                "{}",
                ops_text
            );
        }
        (Verdict::Accepted(accepted), Ok(prepared)) => {
            prop_assert_eq!(
                &prepared.commit.rows.changes,
                &accepted.changes,
                "{}",
                ops_text
            );
            prop_assert_eq!(
                serde_json::to_string(&ops::document_of(&ops::apply_changes(
                    &case.base,
                    &prepared.commit.rows.changes
                )))
                .unwrap(),
                serde_json::to_string(&accepted.document).unwrap()
            );
            prop_assert_eq!(&prepared.steps, &accepted.steps, "{}", ops_text);
            prop_assert_eq!(
                plan_shape!(prepared.compiled.plan),
                plan_shape!(accepted.plan),
                "{}",
                ops_text
            );
            prop_assert_eq!(&prepared.preview, &accepted.impact, "{}", ops_text);
            prop_assert_eq!(&prepared.commit.state, &accepted.state, "{}", ops_text);
            indexes_hold(&base, &prepared, &accepted.rows)?;
            let (all, _) = prepare_scoped(
                &base,
                &store,
                model,
                &case.ops,
                options(case.start, PreviewScope::All),
            );
            prop_assert_eq!(&all.unwrap().preview, &accepted.all, "{}", ops_text);
        }
        (ours, theirs) => prop_assert!(
            false,
            "{}: the reference says {:?}, the model {:?}",
            ops_text,
            ours.errors(),
            theirs.map(|p| p.commit.rows.changes)
        ),
    }
    Ok(())
}

/// The index rows the commit writes are the candidate's own, and every step whose incoming
/// edges it does not rewrite keeps the base's. Each step's edges in the compiled candidate are
/// the ones `plan_index::plan_edges` derives from the candidate's rows alone, as the converter
/// and `verify` derive them.
fn indexes_hold(
    base: &Plan,
    prepared: &PreparedPlanEdit,
    rows: &PlanRows,
) -> Result<(), TestCaseError> {
    let after = &prepared.compiled.plan;
    let is_step = |name: &str| StepId::new(name).is_ok_and(|id| after.steps().contains_key(&id));
    let mut put = vec![];
    for change in &prepared.commit.rows.changes {
        if let PlanChange::StepPut {
            step, declaration, ..
        } = change
        {
            put.push(step_index(step, declaration, &is_step));
        }
    }
    prop_assert_eq!(&prepared.commit.rows.step_index, &put);
    let rewritten: IndexMap<&StepId, &Vec<_>> = prepared
        .commit
        .rows
        .edges
        .iter()
        .map(|(id, edges)| (id, edges))
        .collect();
    let same = |a: &[EdgeRow], b: &[EdgeRow]| a.len() == b.len() && a.iter().all(|e| b.contains(e));
    let derived = plan_edges(rows);
    for id in after.steps().keys() {
        let edges = step_edges(after, id);
        let from_rows: Vec<EdgeRow> = derived
            .iter()
            .filter(|edge| &edge.target == id)
            .cloned()
            .collect();
        prop_assert!(
            same(&from_rows, &edges),
            "{}: plan_edges {:?} vs step_edges {:?}",
            id,
            from_rows,
            edges
        );
        match rewritten.get(id) {
            Some(written) => prop_assert!(
                same(written, &edges),
                "{}: {:?} vs {:?}",
                id,
                written,
                edges
            ),
            None => prop_assert!(
                same(&step_edges(base, id), &edges),
                "{}'s edges changed unwritten",
                id
            ),
        }
    }
    Ok(())
}

#[test]
fn input_value_errors_have_a_canonical_order() {
    let fixture = Fixture::new();
    let model = Model::new();
    let document = serde_json::json!({
        "inputs": {
            "repo": "int",
            "note": {"type": "record", "fields": {"z": "int", "a": "boolean"}}
        },
        "steps": {}
    });
    let reference = Reference::parse(&support::map(document.clone()), &fixture.signatures).unwrap();
    let plan = support::compile(document, &model.signatures).unwrap();
    let expected = [
        "inputs.note.a: missing required field",
        "inputs.note.z: expected int, got \"bad\"",
        "inputs.repo: expected int, got \"bad\"",
        "inputs.unknown: unknown plan input",
    ];
    for values in [
        serde_json::json!({"repo": "bad", "unknown": 1, "note": {"z": "bad"}}),
        serde_json::json!({"note": {"z": "bad"}, "unknown": 1, "repo": "bad"}),
    ] {
        let values = support::map(values);
        for errors in [
            plan.validate_input_values(&values).unwrap_err(),
            reference.validate_input_values(&values).unwrap_err(),
        ] {
            assert_eq!(
                errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
                expected
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/incremental.proptest-regressions")))), ..ProptestConfig::with_cases(64) })]

    #[test]
    fn an_incremental_edit_reaches_the_references_verdict(case in generate::case(12, 4)) {
        agree(case, &FIXTURE, &MODEL)?;
    }



    #[test]
    fn compile_rows_refuses_what_today_refuses(case in generate::case(10, 4)) {
        // The candidate an edit's operations make, compiled whole by both compilers.
        let fixture = &*FIXTURE;
        let model = &*MODEL;
        if let Ok(applied) = ops::apply(&case.base, &case.ops, case.start, &fixture.recipes, &fixture.signatures) {
            let document = ops::document_of(&applied.rows);
            let theirs = Reference::parse(&document, &fixture.signatures);
            let ours = compile_rows(&applied.rows, &model.signatures);
            match (theirs, ours) {
                (Ok(theirs), Ok(ours)) => prop_assert_eq!(plan_shape!(ours), plan_shape!(theirs)),
                (Err(theirs), Err(ours)) => prop_assert_eq!(ours, theirs),
                (theirs, ours) => prop_assert!(false, "{:?} vs {:?}", theirs.err(), ours.err()),
            }
        }
    }
}

/// The contract's `validation.differential` cases, through the incremental path: each base
/// compiles, and each edit is refused with exactly the case's errors.
#[test]
fn the_validation_counterexamples_are_refused_incrementally() {
    operation_sequences::check();
    for case in sluice_reference::harness::differential_cases() {
        let edit = sluice_reference::harness::differential_edit(&case);
        let open = OpenModel;
        let base = Arc::new(compile_rows(&edit.base, &open).unwrap());
        let mut state = model_state(&edit.state);
        for id in base.steps().keys() {
            state.steps.entry(id.clone()).or_default();
        }
        let prepared = support::prepare(&base, &state, &open, case.ops.clone(), support::options());
        assert_eq!(
            prepared.unwrap_err(),
            PublicError::Invalid {
                message: "invalid plan edit".into(),
                errors: case.errors.clone(),
            },
            "{}",
            case.case
        );
    }
}

/// Every fn open with no declared inputs or outputs (`harness::Open`), as the model's provider.
struct OpenModel;
impl sluice_model::SignatureProvider for OpenModel {
    fn signature(&self, _: &str) -> Option<sluice_model::FnSignature> {
        Some(sluice_model::FnSignature {
            open: true,
            ..Default::default()
        })
    }
}
