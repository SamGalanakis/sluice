//! `status`'s units view (SPEC §8 "The units view"): one compact row per unit, from the
//! plan, its state and a few store facts per step. Pure: the store gathers the facts.

use crate::{
    commands::{StepStatus, UnitState},
    gates::{
        Gate, GateDecision, Reference, StateSnapshot, ValueRef, evaluate_step, resolve_reference,
    },
    ids::StepId,
    plan::{Binding, Plan, Step},
    shown::{self, Facts, Shown},
    types::BoundValue,
    units::Unit,
};
use indexmap::{IndexMap, IndexSet};
use serde::Serialize;
use serde_json::Value;

/// The most characters of a row's `line`.
pub const WIDTH: usize = 80;
/// The most characters of a row's `last`.
pub const LAST: usize = 200;
/// `brief` cuts strings longer than this.
pub const BRIEF: usize = 200;

/// What the store knows about one step beyond the plan state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StepFacts {
    /// `queued: needs lane 1 (56/56 held)` for a ready step short of resources.
    pub queued: Option<String>,
    /// Seconds its earliest live run has run, for a running step (fractional, so units
    /// started within a second of each other keep one order).
    pub running_for: Option<f64>,
    /// Seconds since its last change (status record, run start or finish).
    pub changed_ago: Option<f64>,
    /// When its earliest live run started and when it last changed (RFC 3339): the instants
    /// `running_for` and `changed_ago` count from, which a page ticks from without the clock.
    pub running_since: Option<String>,
    pub changed_at: Option<String>,
    /// A running step whose run has submitted: it is only finishing.
    pub finishing: Option<crate::attempt::Finishing>,
    /// A running step whose cancel was asked for: its run is stopping.
    pub stopping: bool,
}

/// A unit row's finishing step (SPEC §12.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FinishingStep {
    pub step: String,
    #[serde(flatten)]
    pub finishing: crate::attempt::Finishing,
}

/// The last message on a step's thread (`step-<id>`).
#[derive(Debug, Clone, PartialEq)]
pub struct LastMessage {
    pub id: i64,
    pub body: String,
    pub needs_reply: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnitRow {
    pub unit: String,
    pub state: UnitState,
    /// The state its steps read as first (`shown`): its mark in `line`. Not on the wire.
    #[serde(skip)]
    pub shown: Shown,
    pub age: Option<i64>,
    /// The instant `age` counts from (RFC 3339): a page ticks it, so the age never changes it.
    #[serde(skip)]
    pub since: Option<String>,
    pub engine: String,
    pub steps: String,
    pub blocked: String,
    pub last: String,
    pub line: String,
    /// Its running steps that have submitted and are only finishing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub finishing: Vec<FinishingStep>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UnitsView {
    pub rows: Vec<UnitRow>,
    /// The done units left out: (units, steps).
    pub done: Option<(usize, usize)>,
}

/// The done units (every step succeeded or skipped), in plan order.
pub fn done_units<'a>(plan: &'a Plan, state: &StateSnapshot) -> Vec<&'a Unit> {
    plan.units().values().filter(|u| u.done(state)).collect()
}

/// The steps a selection names, in plan order; `None` when it names none (no ids, no tags).
/// An unknown id is an error naming them.
pub fn select(
    plan: &Plan,
    steps: Option<&[StepId]>,
    tags: Option<&[String]>,
) -> Result<Option<IndexSet<StepId>>, String> {
    let steps = steps.unwrap_or_default();
    let tags = tags.unwrap_or_default();
    if steps.is_empty() && tags.is_empty() {
        return Ok(None);
    }
    let unknown: Vec<_> = steps
        .iter()
        .filter(|id| !plan.steps().contains_key(*id))
        .map(ToString::to_string)
        .collect();
    if !unknown.is_empty() {
        return Err(format!("no steps {}", unknown.join(", ")));
    }
    Ok(Some(
        plan.steps()
            .iter()
            .filter(|(id, step)| {
                steps.contains(id) || step.tags.iter().any(|tag| tags.contains(tag))
            })
            .map(|(id, _)| id.clone())
            .collect(),
    ))
}

/// Every string over BRIEF characters, at any depth, cut to its first BRIEF and a note of
/// how much more there was.
pub fn brief(value: &Value) -> Value {
    match value {
        Value::String(s) => {
            let n = s.chars().count();
            if n > BRIEF {
                let head: String = s.chars().take(BRIEF).collect();
                Value::String(format!("{head}… [{} more characters]", n - BRIEF))
            } else {
                value.clone()
            }
        }
        Value::Array(items) => Value::Array(items.iter().map(brief).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), brief(v))).collect())
        }
        _ => value.clone(),
    }
}

/// A pending step is held (SPEC §9) when it is paused, its project is paused, it is
/// `core.external`, it reads a plan input with no value, or it waits (reads or `after`) on a
/// step that is failed, stale or itself held. Dependencies come first in topological order.
pub fn held(plan: &Plan, state: &StateSnapshot) -> IndexMap<StepId, bool> {
    let mut held = IndexMap::new();
    for id in plan.topological_order() {
        let step = &plan.steps()[id];
        let value = state.status(id) == StepStatus::Pending
            && (state.paused.is_paused()
                || step.paused.is_paused()
                || step.is_external()
                || missing_input(plan, state, step).is_some()
                || plan.dependencies(id).iter().any(|w| {
                    matches!(state.status(w), StepStatus::Failed | StepStatus::Stale)
                        || held.get(w).copied().unwrap_or(false)
                }));
        held.insert(id.clone(), value);
    }
    held
}

/// The pending steps behind a step that failed or went stale, directly or through other
/// pending steps: `blocked` (`shown::Shown::Blocked`).
pub fn blocked(plan: &Plan, state: &StateSnapshot) -> IndexSet<StepId> {
    let mut stopped = IndexSet::new();
    let mut blocked = IndexSet::new();
    for id in plan.topological_order() {
        match state.status(id) {
            StepStatus::Failed | StepStatus::Stale => {
                stopped.insert(id.clone());
            }
            StepStatus::Pending if plan.dependencies(id).iter().any(|d| stopped.contains(d)) => {
                stopped.insert(id.clone());
                blocked.insert(id.clone());
            }
            StepStatus::Pending
            | StepStatus::Running
            | StepStatus::Succeeded
            | StepStatus::Skipped => {}
        }
    }
    blocked
}

/// The plan input a pending step reads that has no value yet (it is `held`), if any.
pub fn missing_input(plan: &Plan, state: &StateSnapshot, step: &Step) -> Option<String> {
    let gates = step.after.iter().filter_map(|gate| match gate {
        Gate::Bool { reference, .. } => Some(reference),
        _ => None,
    });
    step.bindings
        .values()
        .flat_map(Binding::references)
        .chain(gates)
        .find_map(|reference| match reference.parts() {
            Ok(Reference {
                step: None, name, ..
            }) if matches!(
                resolve_reference(plan, state, reference),
                BoundValue::Waiting
            ) =>
            {
                Some(name)
            }
            _ => None,
        })
}

/// `text` in at most `width` characters, cut with "…".
pub fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// 42s, 42m, 5h, 3d; – when unknown.
pub fn age_text(seconds: Option<i64>) -> String {
    let Some(seconds) = seconds else {
        return "–".into();
    };
    for (unit, size) in [("d", 86400), ("h", 3600), ("m", 60)] {
        if seconds >= size {
            return format!("{}{unit}", seconds / size);
        }
    }
    format!("{seconds}s")
}

/// The word for a wait that blocks: its status, or `paused` for a held paused step; None
/// when it is neither failed, stale nor held.
fn blocking_wait(
    plan: &Plan,
    state: &StateSnapshot,
    held: &IndexMap<StepId, bool>,
    w: &StepId,
) -> Option<String> {
    let status = state.status(w);
    let is_held = held.get(w).copied().unwrap_or(false);
    if !(matches!(status, StepStatus::Failed | StepStatus::Stale) || is_held) {
        return None;
    }
    Some(
        if is_held && plan.steps().get(w).is_some_and(|s| s.paused.is_paused()) {
            "paused".into()
        } else {
            status.as_str().into()
        },
    )
}

/// Why a blocked unit's first held step is held.
fn blocked_reason(
    plan: &Plan,
    state: &StateSnapshot,
    held: &IndexMap<StepId, bool>,
    unit: &Unit,
) -> String {
    let Some(step) = unit
        .steps
        .iter()
        .find(|id| held.get(*id).copied().unwrap_or(false))
        .map(|id| &plan.steps()[id])
    else {
        return String::new();
    };
    if let Some(reason) = step.paused.waiting_reason() {
        return reason;
    }
    if state.paused.is_paused() {
        return "project paused".into();
    }
    if step.is_external() {
        return "external".into();
    }
    if let Some(name) = missing_input(plan, state, step) {
        return format!("input {name} (no value)");
    }
    for w in step.data_dependencies() {
        if let Some(what) = blocking_wait(plan, state, held, &w) {
            return format!("reads {w} ({what})");
        }
    }
    for gate in &step.after {
        match gate {
            Gate::Step { id, .. } => {
                if let Some(what) = blocking_wait(plan, state, held, id) {
                    return format!("after {id} ({what})");
                }
            }
            Gate::Bool { reference, .. } => {
                if let Some(w) = reference_step(reference)
                    && let Some(what) = blocking_wait(plan, state, held, &w)
                {
                    return format!("after {w} ({what})");
                }
            }
            Gate::Unit { name, .. } => {
                for exit in plan
                    .units()
                    .get(name)
                    .map(|u| &u.exits[..])
                    .unwrap_or_default()
                {
                    if let Some(what) = blocking_wait(plan, state, held, exit) {
                        return format!("after unit:{name} (exit {exit} {what})");
                    }
                }
            }
        }
    }
    String::new()
}

fn reference_step(reference: &ValueRef) -> Option<StepId> {
    reference.parts().ok()?.step
}

/// engine·model·effort from the unit's agent step (its first step whose fn is open): the values
/// its `engine` and `model` inputs are bound to now, each part cut to 12. A model object gives
/// its model and effort (`fusion` and the main model's effort for a fusion); a stored step's
/// retired string `model` and `effort` show as they are.
fn engine(plan: &Plan, state: &StateSnapshot, unit: &Unit) -> String {
    let Some(step) = unit
        .steps
        .iter()
        .map(|id| &plan.steps()[id])
        .find(|s| s.signature.open)
    else {
        return String::new();
    };
    let bound = |name: &str| -> Option<Value> {
        match step.bindings.get(name)? {
            Binding::Default(v) => Some(v.as_value().clone()),
            Binding::Source(reference) => match resolve_reference(plan, state, reference) {
                BoundValue::Ready(v) => Some(v.as_value().clone()),
                _ => None,
            },
            _ => None,
        }
    };
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
    let mut parts = vec![text(bound("engine").as_ref())];
    match bound("model") {
        Some(Value::Object(model))
            if model.get("type").and_then(Value::as_str) == Some("fusion") =>
        {
            parts.push(Some("fusion".into()));
            parts.push(text(model.get("main").and_then(|m| m.get("effort"))));
        }
        Some(Value::Object(model)) => {
            parts.push(text(model.get("model")));
            parts.push(text(model.get("effort")));
        }
        model => {
            parts.push(text(model.as_ref()));
            parts.push(text(bound("effort").as_ref()));
        }
    }
    parts
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .map(|s| cut(&s, 12))
        .collect::<Vec<_>>()
        .join("·")
}

/// The row in at most WIDTH characters: name, state and age, engine, step marks, then what
/// it is blocked on and its last message. Step names are cut to 4, then 2 characters (the
/// marks stay) to keep room for the whole blocked reason and the start of the message;
/// what still does not fit is cut with "…".
pub fn line(row: &UnitRow) -> String {
    let len = |s: &str| s.chars().count() as i64;
    let mut head = format!(
        "{:<10}  {} {:>3}",
        cut(&row.unit, 18),
        row.shown.spec().lane,
        age_text(row.age)
    );
    if !row.engine.is_empty() {
        head.push_str("  ");
        head.push_str(&cut(&row.engine, 20));
    }
    let last = if row.last.is_empty() {
        String::new()
    } else {
        format!("\"{}\"", row.last)
    };
    let finishing = if row.finishing.is_empty() {
        String::new()
    } else {
        let prefix = format!("{}-", row.unit);
        format!(
            "finishing {}",
            row.finishing
                .iter()
                .map(|f| f.step.strip_prefix(&prefix).unwrap_or(&f.step))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let note = if row.blocked.is_empty() {
        finishing.as_str()
    } else {
        row.blocked.as_str()
    };
    let tail = [note, last.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("  ");
    let keep = if !note.is_empty() {
        len(note) + 2
    } else if !last.is_empty() {
        (len(&last) + 2).min(16)
    } else {
        0
    };
    let room = WIDTH as i64 - len(&head) - 2;
    let mut steps = row.steps.clone();
    for n in [4, 2] {
        if len(&steps) <= room - keep {
            break;
        }
        steps = row
            .steps
            .split(' ')
            .map(|token| {
                let chars: Vec<char> = token.chars().collect();
                let Some((mark, name)) = chars.split_last() else {
                    return String::new();
                };
                name.iter().take(n).chain(std::iter::once(mark)).collect()
            })
            .collect::<Vec<String>>()
            .join(" ");
    }
    let text = format!("{head}  {}", cut(&steps, room.max(0) as usize));
    let rest = WIDTH as i64 - len(&text) - 2;
    if !tail.is_empty() && rest >= 6 {
        format!("{text}  {}", cut(&tail, rest as usize))
    } else {
        text
    }
}

/// The units view's rows, oldest first (unknown age last). `selected` (when given) keeps the
/// units with a selected step and keeps done units; otherwise, unless `all`, done units are
/// left out and counted. `wanted` keeps only the units in those states.
pub fn units_view(
    plan: &Plan,
    state: &StateSnapshot,
    facts: &IndexMap<StepId, StepFacts>,
    last: &IndexMap<StepId, LastMessage>,
    selected: Option<&IndexSet<StepId>>,
    all: bool,
    wanted: Option<&[UnitState]>,
) -> UnitsView {
    let held = held(plan, state);
    let blocked = blocked(plan, state);
    let none = StepFacts::default();
    let fact = |id: &StepId| facts.get(id).unwrap_or(&none);
    let mut view = UnitsView::default();
    let mut exacts: Vec<f64> = vec![];
    let mut done = (0, 0);
    for (name, unit) in plan.units() {
        if let Some(selected) = selected {
            if !unit.steps.iter().any(|id| selected.contains(id)) {
                continue;
            }
        } else if !all && unit.done(state) {
            done.0 += 1;
            done.1 += unit.steps.len();
            continue;
        }
        let statuses: Vec<_> = unit.steps.iter().map(|id| state.status(id)).collect();
        let is_held = |id: &StepId| held.get(id).copied().unwrap_or(false);
        let queued = unit.steps.iter().find_map(|id| fact(id).queued.clone());
        // a stopped step first, as every status reads (`shown`): a unit with a failed step and a
        // running one is failed
        let unit_state = if statuses
            .iter()
            .any(|s| matches!(s, StepStatus::Failed | StepStatus::Stale))
        {
            UnitState::Failed
        } else if statuses.contains(&StepStatus::Running) {
            UnitState::Running
        } else if statuses
            .iter()
            .all(|s| matches!(s, StepStatus::Succeeded | StepStatus::Skipped))
        {
            UnitState::Settled
        } else if unit.steps.iter().any(is_held)
            && unit
                .steps
                .iter()
                .zip(&statuses)
                .all(|(id, s)| *s != StepStatus::Pending || is_held(id))
        {
            UnitState::Blocked
        } else if queued.is_some() {
            UnitState::Queued
        } else {
            UnitState::Pending
        };
        if wanted.is_some_and(|w| !w.contains(&unit_state)) {
            continue;
        }
        // the longest-running step's run, or the latest change: its seconds and its instant
        let (exact, since) = if unit_state == UnitState::Running {
            let mut longest = (0.0, None);
            for (id, _) in unit
                .steps
                .iter()
                .zip(&statuses)
                .filter(|(_, s)| **s == StepStatus::Running)
            {
                let secs = fact(id).running_for.unwrap_or(0.0);
                if longest.1.is_none() || secs > longest.0 {
                    longest = (secs, fact(id).running_since.clone());
                }
            }
            (Some(longest.0), longest.1)
        } else {
            unit.steps
                .iter()
                .filter_map(|id| {
                    fact(id)
                        .changed_ago
                        .map(|secs| (secs, &fact(id).changed_at))
                })
                .reduce(|a, b| if b.0 < a.0 { b } else { a })
                .map_or((None, None), |(secs, at)| (Some(secs), at.clone()))
        };
        let age = exact.map(|secs| secs as i64);
        let prefix = format!("{name}-");
        // each step as the tools know it: a cancel stays failed, a quiet run running
        let shown: Vec<Shown> = unit
            .steps
            .iter()
            .zip(&statuses)
            .map(|(id, status)| {
                let step = &plan.steps()[id];
                let pending = *status == StepStatus::Pending;
                shown::classify(&Facts {
                    paused: state.paused.is_paused() || step.paused.is_paused(),
                    external: pending
                        && step.is_external()
                        && evaluate_step(plan, state, step) == GateDecision::Ready,
                    blocked: blocked.contains(id),
                    held: pending && missing_input(plan, state, step).is_some(),
                    queued: fact(id).queued.is_some(),
                    finishing: fact(id).finishing.is_some(),
                    stopping: fact(id).stopping,
                    ..Facts::of(status.clone())
                })
            })
            .collect();
        let steps = unit
            .steps
            .iter()
            .zip(&shown)
            .map(|(id, shown)| {
                let short = id.as_str().strip_prefix(&prefix).unwrap_or(id.as_str());
                format!("{short}{}", shown.spec().lane)
            })
            .collect::<Vec<_>>()
            .join(" ");
        let blocked = match unit_state {
            UnitState::Blocked => blocked_reason(plan, state, &held, unit),
            UnitState::Queued => queued.unwrap_or_default(),
            _ => String::new(),
        };
        let last = unit
            .steps
            .iter()
            .filter_map(|id| last.get(id))
            .max_by_key(|m| m.id)
            .map(|m| {
                let body = m.body.split_whitespace().collect::<Vec<_>>().join(" ");
                let q = if m.needs_reply { "Q: " } else { "" };
                cut(&format!("{q}{body}"), LAST)
            })
            .unwrap_or_default();
        let finishing = unit
            .steps
            .iter()
            .zip(&statuses)
            .filter(|(_, s)| **s == StepStatus::Running)
            .filter_map(|(id, _)| {
                fact(id).finishing.clone().map(|finishing| FinishingStep {
                    step: id.to_string(),
                    finishing,
                })
            })
            .collect();
        let mut row = UnitRow {
            unit: name.to_string(),
            state: unit_state,
            shown: shown.iter().copied().min().unwrap_or(Shown::Pending),
            age,
            since,
            engine: engine(plan, state, unit),
            steps,
            blocked,
            last,
            line: String::new(),
            finishing,
        };
        row.line = line(&row);
        exacts.push(exact.unwrap_or(-1.0));
        view.rows.push(row);
    }
    // Oldest first by the exact age, not the whole seconds shown: two units started within a
    // second of each other would otherwise swap places as the clock ticks. Stable: equal ages
    // keep plan order.
    let mut order: Vec<usize> = (0..view.rows.len()).collect();
    order.sort_by(|&a, &b| exacts[b].total_cmp(&exacts[a]));
    let mut rows: Vec<Option<UnitRow>> = std::mem::take(&mut view.rows)
        .into_iter()
        .map(Some)
        .collect();
    view.rows = order.into_iter().filter_map(|i| rows[i].take()).collect();
    if done.0 > 0 {
        view.done = Some(done);
    }
    view
}
