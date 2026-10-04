//! `status`'s units view (SPEC §8 "The units view"): one compact row per unit, from the
//! plan, its state and a few store facts per step. Pure: the store gathers the facts.

use crate::{
    commands::{StepStatus, UnitState},
    gates::{Gate, Reference, StateSnapshot, ValueRef, resolve_reference},
    ids::StepId,
    plan::{Binding, Plan, Step},
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
    /// Seconds its earliest live run has run, for a running step.
    pub running_for: Option<i64>,
    /// Seconds since its last change (status record, run start or finish).
    pub changed_ago: Option<i64>,
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
    pub age: Option<i64>,
    pub engine: String,
    pub steps: String,
    pub blocked: String,
    pub last: String,
    pub line: String,
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

fn missing_input(plan: &Plan, state: &StateSnapshot, step: &Step) -> Option<String> {
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

fn status_name(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Stale => "stale",
        StepStatus::Skipped => "skipped",
    }
}

fn step_mark(status: &StepStatus) -> char {
    match status {
        StepStatus::Succeeded => '✓',
        StepStatus::Running => '▶',
        StepStatus::Pending => '·',
        StepStatus::Failed => '✗',
        StepStatus::Stale => '~',
        StepStatus::Skipped => '–',
    }
}

fn state_mark(state: UnitState) -> char {
    match state {
        UnitState::Running => '▶',
        UnitState::Failed => '✗',
        UnitState::Blocked => '‖',
        UnitState::Queued => '≡',
        UnitState::Settled => '✓',
        UnitState::Pending => '·',
    }
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
            status_name(&status).into()
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

/// engine·model·effort from the unit's agent step (its first step whose fn is open): the
/// string values its `engine`, `model` and `effort` inputs are bound to now, each cut to 12.
fn engine(plan: &Plan, state: &StateSnapshot, unit: &Unit) -> String {
    let Some(step) = unit
        .steps
        .iter()
        .map(|id| &plan.steps()[id])
        .find(|s| s.signature.open)
    else {
        return String::new();
    };
    ["engine", "model", "effort"]
        .into_iter()
        .filter_map(|name| {
            let value = match step.bindings.get(name)? {
                Binding::Default(v) => v.as_value().clone(),
                Binding::Source(reference) => match resolve_reference(plan, state, reference) {
                    BoundValue::Ready(v) => v.as_value().clone(),
                    _ => return None,
                },
                _ => return None,
            };
            value.as_str().filter(|s| !s.is_empty()).map(|s| cut(s, 12))
        })
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
        state_mark(row.state),
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
    let tail = [row.blocked.as_str(), last.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("  ");
    let keep = if !row.blocked.is_empty() {
        len(&row.blocked) + 2
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
    let none = StepFacts::default();
    let fact = |id: &StepId| facts.get(id).unwrap_or(&none);
    let mut view = UnitsView::default();
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
        let unit_state = if statuses.contains(&StepStatus::Running) {
            UnitState::Running
        } else if statuses
            .iter()
            .any(|s| matches!(s, StepStatus::Failed | StepStatus::Stale))
        {
            UnitState::Failed
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
        let age = if unit_state == UnitState::Running {
            Some(
                unit.steps
                    .iter()
                    .zip(&statuses)
                    .filter(|(_, s)| **s == StepStatus::Running)
                    .map(|(id, _)| fact(id).running_for.unwrap_or(0))
                    .max()
                    .unwrap_or(0),
            )
        } else {
            unit.steps
                .iter()
                .filter_map(|id| fact(id).changed_ago)
                .min()
        };
        let prefix = format!("{name}-");
        let steps = unit
            .steps
            .iter()
            .zip(&statuses)
            .map(|(id, status)| {
                let short = id.as_str().strip_prefix(&prefix).unwrap_or(id.as_str());
                let mark = if *status == StepStatus::Pending && plan.steps()[id].paused.is_paused()
                {
                    '‖'
                } else if *status == StepStatus::Pending && fact(id).queued.is_some() {
                    '≡'
                } else {
                    step_mark(status)
                };
                format!("{short}{mark}")
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
        let mut row = UnitRow {
            unit: name.to_string(),
            state: unit_state,
            age,
            engine: engine(plan, state, unit),
            steps,
            blocked,
            last,
            line: String::new(),
        };
        row.line = line(&row);
        view.rows.push(row);
    }
    // Stable: equal ages keep plan order.
    view.rows
        .sort_by_key(|row| std::cmp::Reverse(row.age.unwrap_or(-1)));
    if done.0 > 0 {
        view.done = Some(done);
    }
    view
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(unit: &str, state: UnitState, age: Option<i64>, engine: &str, steps: &str) -> UnitRow {
        UnitRow {
            unit: unit.into(),
            state,
            age,
            engine: engine.into(),
            steps: steps.into(),
            blocked: String::new(),
            last: String::new(),
            line: String::new(),
        }
    }

    #[test]
    fn ages_cut_and_brief() {
        assert_eq!(age_text(None), "–");
        assert_eq!(age_text(Some(42)), "42s");
        assert_eq!(age_text(Some(42 * 60 + 5)), "42m");
        assert_eq!(age_text(Some(5 * 3600)), "5h");
        assert_eq!(age_text(Some(3 * 86400)), "3d");
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
        let long = "x".repeat(205);
        assert_eq!(
            brief(&serde_json::json!({"a":[long],"b":1})),
            serde_json::json!({"a":[format!("{}… [5 more characters]", "x".repeat(200))],"b":1})
        );
    }

    #[test]
    fn line_fits_and_cuts_step_names() {
        let mut r = row(
            "fig-4201",
            UnitState::Running,
            Some(42 * 60),
            "opus·xhigh",
            "fork✓ work▶ landed· close· rm·",
        );
        r.last = "Which crate owns the parser and should the lexer move with it?".into();
        let text = line(&r);
        assert!(text.chars().count() <= WIDTH, "{text}");
        assert!(
            text.starts_with("fig-4201    ▶ 42m  opus·xhigh  "),
            "{text}"
        );
        assert!(text.contains("\"Which"), "{text}");
        let mut r = row(
            "fig-4202",
            UnitState::Blocked,
            None,
            "devin",
            "fork· work· landed· close· rm·",
        );
        r.blocked = "after fig-4200-work (failed)".into();
        let text = line(&r);
        assert_eq!(
            text,
            "fig-4202    ‖   –  devin  fo· wo· la· cl· rm·  after fig-4200-work (failed)"
        );
    }
}
