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

mod support;

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
use std::sync::Arc;

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
fn settle(case: &mut EditCase) {
    let fixture = Fixture::new();
    let before =
        sluice_reference::Plan::parse(&ops::document_of(&case.base), &fixture.signatures).unwrap();
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

/// What an agreeing case exercised, for the sweep's tally.
#[derive(Debug, Default)]
struct Reached {
    rounds: usize,
    parts: Vec<&'static str>,
}

/// The reference's verdict and the incremental model's agree on `case`.
fn agree(mut case: EditCase) -> Result<Reached, TestCaseError> {
    settle(&mut case);
    let fixture = Fixture::new();
    let model = Model::new();
    let verdict = evaluate(&case, &fixture.context()).expect("a generated base compiles");
    let base = Arc::new(compile_rows(&case.base, &model.signatures).expect("the base compiles"));
    let store = Store::of(&case, &base);
    let (prepared, rounds) = prepare_scoped(
        &base,
        &store,
        &model,
        &case.ops,
        options(case.start, PreviewScope::Impact),
    );
    let mut reached = Reached {
        rounds,
        parts: vec![],
    };
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
                &model,
                &case.ops,
                options(case.start, PreviewScope::All),
            );
            prop_assert_eq!(&all.unwrap().preview, &accepted.all, "{}", ops_text);
            let preview = &prepared.preview;
            for (part, hit) in [
                ("would_start", !preview.would_start.is_empty()),
                ("would_queue", !preview.would_queue.is_empty()),
                ("would_skip", !preview.would_skip.is_empty()),
                ("would_stale", !preview.would_stale.is_empty()),
                ("errors", !preview.errors.is_empty()),
                ("transitions", !prepared.commit.state.transitions.is_empty()),
                ("removed", !prepared.commit.state.removed.is_empty()),
                ("edges", !prepared.commit.rows.edges.is_empty()),
            ] {
                if hit {
                    reached.parts.push(part);
                }
            }
        }
        (ours, theirs) => prop_assert!(
            false,
            "{}: the reference says {:?}, the model {:?}",
            ops_text,
            ours.errors(),
            theirs.map(|p| p.commit.rows.changes)
        ),
    }
    Ok(reached)
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(768))]

    #[test]
    fn an_incremental_edit_reaches_the_references_verdict(case in generate::case(12, 4)) {
        agree(case)?;
    }

    #[test]
    fn compile_rows_compiles_what_today_compiles(document in generate::plan(14)) {
        let fixture = Fixture::new();
        let model = Model::new();
        let reference = sluice_reference::Plan::parse(&document, &fixture.signatures).unwrap();
        let rows = support::rows(&serde_json::to_value(&document).unwrap());
        let plan = compile_rows(&rows, &model.signatures).unwrap();
        prop_assert_eq!(plan_shape!(plan), plan_shape!(reference));
    }

    #[test]
    fn compile_rows_refuses_what_today_refuses(case in generate::case(10, 4)) {
        // The candidate an edit's operations make, compiled whole by both compilers.
        let fixture = Fixture::new();
        let model = Model::new();
        if let Ok(applied) = ops::apply(&case.base, &case.ops, case.start, &fixture.recipes, &fixture.signatures) {
            let document = ops::document_of(&applied.rows);
            let theirs = sluice_reference::Plan::parse(&document, &fixture.signatures);
            let ours = compile_rows(&applied.rows, &model.signatures);
            match (theirs, ours) {
                (Ok(theirs), Ok(ours)) => prop_assert_eq!(plan_shape!(ours), plan_shape!(theirs)),
                (Err(theirs), Err(ours)) => prop_assert_eq!(ours, theirs),
                (theirs, ours) => prop_assert!(false, "{:?} vs {:?}", theirs.err(), ours.err()),
            }
        }
    }
}

/// Many more edits than the property tests above, from a fixed seed, so every operation and
/// every refusal the generators reach is held on every run.
#[test]
fn a_fixed_sweep_of_generated_edits_agrees() {
    struct Seed(u64);
    impl Seed {
        fn byte(&mut self) -> u8 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 24) as u8
        }
        fn bytes<const N: usize>(&mut self) -> [u8; N] {
            std::array::from_fn(|_| self.byte())
        }
        fn vec(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.byte()).collect()
        }
    }
    let mut seed = Seed(0x5eed_c0de_1a7e_0003);
    let mut verdicts = IndexMap::<&str, usize>::new();
    let mut parts = IndexMap::<&str, usize>::new();
    let mut rounds = IndexMap::<usize, usize>::new();
    for round in 0..2500 {
        let steps: Vec<[u8; 16]> = (0..seed.byte() % 12).map(|_| seed.bytes()).collect();
        let document = generate::build_plan(&seed.vec(4), &steps);
        let ops: Vec<[u8; 12]> = (0..1 + seed.byte() % 4).map(|_| seed.bytes()).collect();
        let case = generate::build_case(&document, &seed.vec(24), &ops, seed.byte());
        let fixture = Fixture::new();
        let kind = match evaluate(&case, &fixture.context()).unwrap() {
            Verdict::Refused(_) => "refused",
            Verdict::Invalid(_) => "invalid",
            Verdict::Accepted(a) if a.changes.is_empty() => "no change",
            Verdict::Accepted(_) => "accepted",
        };
        *verdicts.entry(kind).or_default() += 1;
        match agree(case.clone()) {
            Ok(reached) => {
                *rounds.entry(reached.rounds).or_default() += 1;
                for part in reached.parts {
                    *parts.entry(part).or_default() += 1;
                }
            }
            Err(error) => panic!(
                "round {round}: {error}\nbase: {}\nops: {}",
                serde_json::to_string(&ops::document_of(&case.base)).unwrap(),
                serde_json::to_string(&case.ops).unwrap()
            ),
        }
    }
    eprintln!("verdicts: {verdicts:?}");
    eprintln!("accepted edits whose preview or delta has: {parts:?}");
    eprintln!("read rounds: {rounds:?}");
    for kind in ["refused", "invalid", "accepted", "no change"] {
        assert!(
            verdicts.get(kind).copied().unwrap_or(0) > 50,
            "{kind}: {verdicts:?}"
        );
    }
}

/// The contract's `validation.differential` cases, through the incremental path: each base
/// compiles, and each edit is refused with exactly the case's errors.
#[test]
fn the_validation_counterexamples_are_refused_incrementally() {
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
