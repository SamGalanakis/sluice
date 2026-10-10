//! The reference is today's code: on generated plans, states and edits it gives exactly what
//! `sluice-model`'s compiler, gate evaluator, simulation and typed edit preparation give.
//!
//! This file names `sluice_model::Plan::parse`, `edit::prepare_edit` and the whole-plan
//! `gates`/`units` functions over today's `Plan`. Lane C deletes it when it deletes those: the
//! reference then stands alone, held by its ported tests and the harness's.

use indexmap::IndexMap;
use proptest::{prelude::*, strategy::ValueTree};
use serde_json::Value;
use sluice_reference::{
    generate::{self, Fixture},
    harness::Context,
    interop::{model_limits, model_resources, model_signatures, model_state, reference_state},
    ops, plan_shape,
};

struct Model {
    signatures: IndexMap<String, sluice_model::plan::FnSignature>,
    recipes: IndexMap<String, sluice_model::recipe::RecipeEntry>,
}
impl Model {
    fn new() -> Self {
        Self {
            signatures: model_signatures(&generate::catalog()),
            recipes: sluice_model::recipe::catalog(
                generate::RECIPES
                    .iter()
                    .map(|(n, s, j)| (*n, *s, j.as_bytes())),
            ),
        }
    }
}

/// Both compilers on one document: the same errors (`None`), or plans of the same shape and
/// document.
fn same_compile(
    document: &sluice_model::rpc::JsonMap,
    fixture: &Fixture,
    model: &Model,
) -> Result<Option<(sluice_reference::Plan, sluice_model::Plan)>, TestCaseError> {
    let reference = sluice_reference::Plan::parse(document, &fixture.signatures);
    let today = sluice_model::Plan::parse(document, &model.signatures);
    match (reference, today) {
        (Err(a), Err(b)) => {
            prop_assert_eq!(a, b);
            Ok(None)
        }
        (Ok(a), Ok(b)) => {
            prop_assert_eq!(plan_shape!(a), plan_shape!(b));
            prop_assert_eq!(a.document(), b.document());
            Ok(Some((a, b)))
        }
        (a, b) => {
            prop_assert!(
                false,
                "one compiles: reference {:?}, today {:?}",
                a.err(),
                b.err()
            );
            unreachable!()
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]

    /// Generated bases, and every candidate a generated batch of operations makes (valid or
    /// not), compile alike; on a compiled base and a generated state the gate evaluator,
    /// reconciliation, settlement and the whole-plan simulation agree.
    #[test]
    fn compile_reconcile_and_simulate_agree(case in generate::case(10, 3)) {
        let fixture = Fixture::new();
        let model = Model::new();
        let base = ops::document_of(&case.base);
        let (plan, today) = same_compile(&base, &fixture, &model)?
            .expect("a generated base compiles with both");
        let state = &case.state;
        let model_state = model_state(state);
        prop_assert_eq!(
            format!("{:?}", sluice_reference::gates::readiness(&plan, state)),
            format!("{:?}", sluice_model::gates::readiness(&today, &model_state))
        );
        prop_assert_eq!(
            &sluice_reference::gates::reconcile(&plan, state),
            &reference_state(&sluice_model::gates::reconcile(&today, &model_state))
        );
        prop_assert_eq!(
            sluice_reference::units::settled_steps(&plan, state),
            sluice_model::units::settled_steps(&today, &model_state)
        );
        let applied = ops::apply(&case.base, &case.ops, case.start, &fixture.recipes, &fixture.signatures);
        if let Ok(applied) = applied {
            let candidate = ops::document_of(&applied.rows);
            if let Some((after, after_today)) = same_compile(&candidate, &fixture, &model)? {
                let a = sluice_reference::gates::simulate_edit(&plan, state, &after, state, &case.resources);
                let b = sluice_model::gates::simulate_edit(
                    &today, &model_state, &after_today, &model_state, &model_resources(&case.resources));
                prop_assert_eq!(&a.would_start, &b.would_start);
                prop_assert_eq!(&a.would_queue, &b.would_queue);
                prop_assert_eq!(&a.would_skip, &b.would_skip);
                prop_assert_eq!(&a.would_stale, &b.would_stale);
                prop_assert_eq!(&a.errors, &b.errors);
                prop_assert_eq!(&a.reconciled, &reference_state(&b.reconciled));
            }
        }
    }

    /// Generated typed edit commands (and RFC 6902 patches) prepare alike: the same refusal,
    /// or the same patch, candidate, preview, reports and reconciled state.
    #[test]
    fn typed_edits_prepare_alike(
        document in generate::plan(10),
        state in prop::collection::vec(any::<u8>(), 24),
        command in prop::collection::vec(any::<u8>(), 12),
        misc in any::<u8>(),
    ) {
        let fixture = Fixture::new();
        let model = Model::new();
        let case = generate::build_case(&document, &state, &[[0; 12]], misc);
        let command = generate::build_command(&document, &command);
        let bytes = serde_json::to_vec(&command).unwrap();
        let ours: sluice_reference::commands::CommandRequest =
            sluice_model::rpc::decode_json(&bytes).unwrap();
        let theirs: sluice_model::commands::CommandRequest =
            sluice_model::rpc::decode_json(&bytes).unwrap();
        let ours = sluice_reference::PlanEdit::try_from(ours).unwrap();
        let theirs = sluice_model::PlanEdit::try_from(theirs).unwrap();
        let plan = sluice_reference::Plan::parse(&document, &fixture.signatures).unwrap();
        let today = sluice_model::Plan::parse(&document, &model.signatures).unwrap();
        let model_state = model_state(&case.state);
        let model_resources = model_resources(&case.resources);
        let model_limits = model_limits(&case.limits);
        let context = Context { signatures: &fixture.signatures, recipes: &fixture.recipes };
        let a = sluice_reference::prepare_edit(
            &sluice_reference::EditSnapshot {
                revision: sluice_model::ids::Revision(1),
                plan: &plan,
                state: &case.state,
                signatures: context.signatures,
                recipes: context.recipes,
                resources: &case.resources,
                limits: &case.limits,
                prune_eligible: None,
            },
            ours,
        );
        let b = sluice_model::prepare_edit(
            &sluice_model::EditSnapshot {
                revision: sluice_model::ids::Revision(1),
                plan: &today,
                state: &model_state,
                signatures: &model.signatures,
                recipes: &model.recipes,
                resources: &model_resources,
                limits: &model_limits,
                prune_eligible: None,
            },
            theirs,
        );
        match (a, b) {
            (Err(a), Err(b)) => prop_assert_eq!(a, b),
            (Ok(a), Ok(b)) => {
                prop_assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap());
                prop_assert_eq!(plan_shape!(a.plan), plan_shape!(b.plan));
                prop_assert_eq!(&a.reconciled, &reference_state(&b.reconciled));
            }
            (a, b) => prop_assert!(false, "{command}: reference {:?}, today {:?}",
                a.map(|p| p.ops.len()), b.map(|p| p.ops.len())),
        }
    }
}

/// A generated plan perturbed into an invalid one (a key renamed, a value replaced, a section
/// dropped) is refused alike: the same errors in the same order.
#[test]
fn malformed_documents_are_refused_alike() {
    let fixture = Fixture::new();
    let model = Model::new();
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let strategy = (generate::plan(8), any::<[u8; 6]>());
    let mut refused = 0;
    for _ in 0..400 {
        let (document, choice) = strategy.new_tree(&mut runner).unwrap().current();
        let mut value = serde_json::to_value(&document).unwrap();
        perturb(&mut value, &choice);
        let Ok(document) = sluice_model::rpc::decode_json::<sluice_model::rpc::JsonMap>(
            &serde_json::to_vec(&value).unwrap(),
        ) else {
            continue;
        };
        let a = sluice_reference::Plan::parse(&document, &fixture.signatures);
        let b = sluice_model::Plan::parse(&document, &model.signatures);
        assert_eq!(a.as_ref().err(), b.as_ref().err(), "{value}");
        if let (Ok(a), Ok(b)) = (a, b) {
            assert_eq!(plan_shape!(a), plan_shape!(b));
        } else {
            refused += 1;
        }
    }
    assert!(refused > 100, "only {refused} perturbed plans were refused");
}
/// Change one thing in a plan document, chosen by `choice`: rename or drop a key on the way
/// down, or replace the value reached.
fn perturb(value: &mut Value, choice: &[u8; 6]) {
    let replacements = [
        serde_json::json!(null),
        serde_json::json!(7),
        serde_json::json!("x"),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!({"source": "nowhere/out"}),
        serde_json::json!(["s0", "unit:none", "!s1"]),
    ];
    let mut path: Vec<String> = vec![];
    for b in &choice[..4] {
        let node = value.pointer_mut(&pointer(&path)).unwrap();
        match node {
            Value::Object(map) if !map.is_empty() => {
                let key = map.keys().nth(*b as usize % map.len()).unwrap().clone();
                if b % 7 == 0 {
                    let taken = map.shift_remove(&key).unwrap();
                    map.insert(format!("{key}_x"), taken);
                    return;
                }
                if b % 11 == 1 {
                    map.shift_remove(&key);
                    return;
                }
                path.push(key);
            }
            Value::Array(items) if !items.is_empty() => {
                path.push((*b as usize % items.len()).to_string());
            }
            _ => break,
        }
    }
    *value.pointer_mut(&pointer(&path)).unwrap() =
        replacements[choice[4] as usize % replacements.len()].clone();
}
fn pointer(path: &[String]) -> String {
    path.iter()
        .map(|p| format!("/{}", p.replace('~', "~0").replace('/', "~1")))
        .collect()
}
