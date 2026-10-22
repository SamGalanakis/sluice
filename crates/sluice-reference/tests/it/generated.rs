//! The generators and the harness over them: every generated base compiles, every generated
//! edit gets a verdict, the generated edits reach every verdict and every operation kind, and
//! an accepted edit's parts agree with each other (its changes rebuild its candidate, its
//! export compiles, its impact preview is within its full one).

use sluice_model::plan_rows::PlanOp;
use sluice_reference::{
    generate::{self, Fixture},
    harness::{Verdict, evaluate},
};
use std::collections::BTreeMap;

/// A small deterministic byte source, so the tally below is the same on every run.
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
fn op_kind(op: &PlanOp) -> &'static str {
    match op {
        PlanOp::InputPut { .. } => "input.put",
        PlanOp::InputRemove { .. } => "input.remove",
        PlanOp::OutputPut { .. } => "output.put",
        PlanOp::OutputRemove { .. } => "output.remove",
        PlanOp::StepAdd { .. } => "step.add",
        PlanOp::StepUpdate { .. } => "step.update",
        PlanOp::StepRemove { .. } => "step.remove",
        PlanOp::EdgeAdd { .. } => "edge.add",
        PlanOp::EdgeRemove { .. } => "edge.remove",
        PlanOp::UnitAdd { .. } => "unit.add",
        PlanOp::UnitUpdate { .. } => "unit.update",
        PlanOp::UnitRemove { .. } => "unit.remove",
        PlanOp::OrderSet { .. } => "order.set",
    }
}

#[test]
fn slow_generated_edits_reach_every_verdict_and_every_operation() {
    let fixture = Fixture::new();
    let mut seed = Seed(0x5eed_1234_abcd_0001);
    let mut verdicts = BTreeMap::<&str, usize>::new();
    let mut accepted_by_op = BTreeMap::<&str, usize>::new();
    let mut seen_by_op = BTreeMap::<&str, usize>::new();
    let mut messages = BTreeMap::<String, usize>::new();
    for _ in 0..1500 {
        let steps: Vec<[u8; 16]> = (0..seed.byte() % 10).map(|_| seed.bytes()).collect();
        let document = generate::build_plan(&seed.vec(4), &steps);
        let ops: Vec<[u8; 12]> = (0..1 + seed.byte() % 3).map(|_| seed.bytes()).collect();
        let case = generate::build_case(&document, &seed.vec(24), &ops, seed.byte());
        let verdict = evaluate(&case, &fixture.context()).expect("a generated base compiles");
        let kind = match &verdict {
            Verdict::Refused(_) => "refused",
            Verdict::Invalid(_) => "invalid",
            Verdict::Accepted(a) if a.changes.is_empty() => "accepted, no change",
            Verdict::Accepted(_) => "accepted",
        };
        *verdicts.entry(kind).or_default() += 1;
        for op in &case.ops {
            *seen_by_op.entry(op_kind(op)).or_default() += 1;
            if matches!(verdict, Verdict::Accepted(_)) && case.ops.len() == 1 {
                *accepted_by_op.entry(op_kind(op)).or_default() += 1;
            }
        }
        for error in verdict.errors().into_iter().flatten() {
            let message = error.split_once(": ").map_or(error.as_str(), |(_, m)| m);
            let words: String = message
                .split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            *messages.entry(words).or_default() += 1;
        }
    }
    eprintln!("verdicts: {verdicts:?}");
    eprintln!("operations: {seen_by_op:?}");
    eprintln!("accepted single operations: {accepted_by_op:?}");
    eprintln!("refusals by opening words: {messages:?}");
    for kind in ["refused", "invalid", "accepted", "accepted, no change"] {
        assert!(
            verdicts.get(kind).copied().unwrap_or(0) > 20,
            "{kind}: {verdicts:?}"
        );
    }
    for kind in [
        "input.put",
        "input.remove",
        "output.put",
        "output.remove",
        "step.add",
        "step.update",
        "step.remove",
        "edge.add",
        "edge.remove",
        "unit.add",
        "unit.update",
        "unit.remove",
        "order.set",
    ] {
        assert!(
            accepted_by_op.get(kind).copied().unwrap_or(0) > 3,
            "{kind} is accepted alone: {accepted_by_op:?}"
        );
    }
    for message in [
        "dependency cycle",
        "unknown step",
        "cannot remove or",
        "singleton unit name",
        "a unit cannot",
        "no step ghost",
        "already exists in",
        "plan inputs and",
    ] {
        assert!(
            messages.keys().any(|m| m.starts_with(message)),
            "no generated edit is refused with {message:?}: {messages:?}"
        );
    }
}
