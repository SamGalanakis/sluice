//! Crossing between the reference and `sluice-model`'s live types.
//!
//! The reference's state, signature and resource types are its own copies; `sluice-model`'s
//! are the ones the incremental model takes. These conversions are the one place this crate
//! names them: if lane C reshapes one, it updates the conversion here (it never keeps a copy
//! of the reference). `plan_shape!` describes a compiled plan through the query API both
//! `Plan`s keep (§8: `inputs`, `outputs`, `steps`, `units`, `dependencies`,
//! `topological_order`), so the reference's and the incremental model's compiled plans
//! compare as values.

use crate::{
    gates::{CachedResources, StateSnapshot, StepState},
    plan::{Declaration, FnSignature, Pause, ResourceLimit, SignatureProvider},
    recipe::RecipeEntry,
};
use indexmap::IndexMap;
use sluice_model::{gates as model_gates, plan as model_plan, recipe as model_recipe};

pub fn model_pause(pause: &Pause) -> model_plan::Pause {
    match pause {
        Pause::No => model_plan::Pause::No,
        Pause::Yes => model_plan::Pause::Yes,
        Pause::Reason(reason) => model_plan::Pause::Reason(reason.clone()),
    }
}
pub fn reference_pause(pause: &model_plan::Pause) -> Pause {
    match pause {
        model_plan::Pause::No => Pause::No,
        model_plan::Pause::Yes => Pause::Yes,
        model_plan::Pause::Reason(reason) => Pause::Reason(reason.clone()),
    }
}
pub fn model_state(state: &StateSnapshot) -> model_gates::StateSnapshot {
    model_gates::StateSnapshot {
        inputs: state.inputs.clone(),
        steps: state
            .steps
            .iter()
            .map(|(id, s)| {
                (
                    id.clone(),
                    model_gates::StepState {
                        status: s.status.clone(),
                        outputs: s.outputs.clone(),
                        inputs_hash: s.inputs_hash,
                        skipped: s.skipped.clone(),
                        error: s.error.clone(),
                        queued: s.queued.clone(),
                    },
                )
            })
            .collect(),
        paused: model_pause(&state.paused),
    }
}
pub fn reference_state(state: &model_gates::StateSnapshot) -> StateSnapshot {
    StateSnapshot {
        inputs: state.inputs.clone(),
        steps: state
            .steps
            .iter()
            .map(|(id, s)| {
                (
                    id.clone(),
                    StepState {
                        status: s.status.clone(),
                        outputs: s.outputs.clone(),
                        inputs_hash: s.inputs_hash,
                        skipped: s.skipped.clone(),
                        error: s.error.clone(),
                        queued: s.queued.clone(),
                    },
                )
            })
            .collect(),
        paused: reference_pause(&state.paused),
    }
}
pub fn model_resources(resources: &CachedResources) -> model_gates::CachedResources {
    model_gates::CachedResources {
        capacities: resources.capacities.clone(),
        leased: resources.leased.clone(),
    }
}
pub fn model_limits(
    limits: &IndexMap<String, ResourceLimit>,
) -> IndexMap<String, model_plan::ResourceLimit> {
    limits
        .iter()
        .map(|(name, limit)| {
            (
                name.clone(),
                match limit {
                    ResourceLimit::Fixed(n) => model_plan::ResourceLimit::Fixed(*n),
                    ResourceLimit::Dynamic => model_plan::ResourceLimit::Dynamic,
                },
            )
        })
        .collect()
}
pub fn model_signature(signature: &FnSignature) -> model_plan::FnSignature {
    model_plan::FnSignature {
        inputs: signature.inputs.clone(),
        outputs: signature.outputs.clone(),
        submits: signature
            .submits
            .iter()
            .map(|(name, d)| {
                (
                    name.clone(),
                    model_plan::Declaration {
                        ty: d.ty.clone(),
                        doc: d.doc.clone(),
                    },
                )
            })
            .collect(),
        open: signature.open,
    }
}
pub fn reference_signature(signature: &model_plan::FnSignature) -> FnSignature {
    FnSignature {
        inputs: signature.inputs.clone(),
        outputs: signature.outputs.clone(),
        submits: signature
            .submits
            .iter()
            .map(|(name, d)| {
                (
                    name.clone(),
                    Declaration {
                        ty: d.ty.clone(),
                        doc: d.doc.clone(),
                    },
                )
            })
            .collect(),
        open: signature.open,
    }
}
pub fn model_signatures(
    signatures: &IndexMap<String, FnSignature>,
) -> IndexMap<String, model_plan::FnSignature> {
    signatures
        .iter()
        .map(|(name, s)| (name.clone(), model_signature(s)))
        .collect()
}
/// A `sluice-model` signature provider read through the reference's trait.
pub struct FromModel<'a, P>(pub &'a P);
impl<P: model_plan::SignatureProvider> SignatureProvider for FromModel<'_, P> {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        self.0.signature(name).as_ref().map(reference_signature)
    }
}
/// Recipe sources (name, scope, JSON) as both catalogs, built from the same bytes.
pub fn recipe_catalogs<'a>(
    sources: &'a [(&'a str, &'a str, &'a str)],
) -> (
    IndexMap<String, RecipeEntry>,
    IndexMap<String, model_recipe::RecipeEntry>,
) {
    let bytes = || sources.iter().map(|(n, s, j)| (*n, *s, j.as_bytes()));
    (
        crate::recipe::catalog(bytes()),
        model_recipe::catalog(bytes()),
    )
}

/// A compiled plan as comparable values: what each query of the shared query API returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanShape {
    /// Name, type, doc.
    pub inputs: Vec<(String, String, Option<String>)>,
    /// Name, ref.
    pub outputs: Vec<(String, String)>,
    pub steps: Vec<StepShape>,
    pub units: Vec<UnitShape>,
    pub order: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepShape {
    pub id: String,
    pub run: String,
    pub unit: String,
    /// Input name and the binding's `Debug` form.
    pub bindings: Vec<(String, String)>,
    pub extra_inputs: Vec<(String, String)>,
    pub declared_outputs: Vec<(String, String)>,
    pub scatter: Option<String>,
    pub doc: Option<String>,
    pub paused: String,
    /// Each gate's entry text.
    pub after: Vec<String>,
    pub tags: Vec<String>,
    pub needs: Vec<(String, u64)>,
    pub priority: i64,
    pub dependencies: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitShape {
    pub name: String,
    pub tagged: bool,
    pub steps: Vec<String>,
    pub entries: Vec<String>,
    pub exits: Vec<String>,
}

/// A compiled plan's `PlanShape`, through the query API: works on any `Plan` that keeps
/// today's (`sluice_reference::Plan`, `sluice_model::Plan` before and after lane C).
#[macro_export]
macro_rules! plan_shape {
    ($plan:expr) => {{
        let plan = &$plan;
        $crate::interop::PlanShape {
            inputs: plan
                .inputs()
                .iter()
                .map(|(n, d)| (n.to_string(), d.ty.to_string(), d.doc.clone()))
                .collect(),
            outputs: plan
                .outputs()
                .iter()
                .map(|(n, r)| (n.to_string(), r.to_string()))
                .collect(),
            steps: plan
                .steps()
                .iter()
                .map(|(id, s)| $crate::interop::StepShape {
                    id: id.to_string(),
                    run: s.run.to_string(),
                    unit: s.unit_name().to_string(),
                    bindings: s
                        .bindings
                        .iter()
                        .map(|(n, b)| (n.to_string(), format!("{b:?}")))
                        .collect(),
                    extra_inputs: s
                        .extra_inputs
                        .iter()
                        .map(|(n, t)| (n.to_string(), t.to_string()))
                        .collect(),
                    declared_outputs: s
                        .declared_outputs
                        .iter()
                        .map(|(n, d)| (n.to_string(), d.ty.to_string()))
                        .collect(),
                    scatter: s.scatter.clone(),
                    doc: s.doc.clone(),
                    paused: format!("{:?}", s.paused),
                    after: s.after.iter().map(|g| g.entry()).collect(),
                    tags: s.tags.iter().map(ToString::to_string).collect(),
                    needs: s.needs.iter().map(|(n, a)| (n.to_string(), *a)).collect(),
                    priority: s.priority,
                    dependencies: plan
                        .dependencies(id)
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                })
                .collect(),
            units: plan
                .units()
                .iter()
                .map(|(n, u)| $crate::interop::UnitShape {
                    name: n.to_string(),
                    tagged: u.tagged,
                    steps: u.steps.iter().map(ToString::to_string).collect(),
                    entries: u.entries.iter().map(ToString::to_string).collect(),
                    exits: u.exits.iter().map(ToString::to_string).collect(),
                })
                .collect(),
            order: plan
                .topological_order()
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }};
}
