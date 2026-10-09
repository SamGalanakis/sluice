//! Step detail and owner actions. P6-02 injects the command service adapter.
use super::board::{self, render_error};
use super::ui::Shown;
use super::{DashboardState, NavView, TrustedHtml, Viewer};
use crate::streams::{self, PatchRegion, RenderedBatch, StreamQuery, VersionSignal};
use askama::Template;
use axum::{
    Extension,
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response, Sse, sse::KeepAlive},
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    gates::{
        GateDecision, StateSnapshot, ValueRef, evaluate_step, resolve_reference,
        wait_reasons_worded,
    },
    ids::{ProjectId, RunId, StepId},
    plan::{Binding, Pause, Plan},
    shown::Facts,
    types::BoundValue,
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

#[derive(Clone, Debug, Serialize)]
pub struct FieldView {
    pub name: String,
    pub ty: String,
    pub doc: String,
    pub value: String,
    pub available: bool,
    pub kind: String,
    pub source: String,
    /// A small flat object's names and values (kind `pairs`): `{"type": "normal", "model":
    /// "sol"}` reads "type normal · model sol", not as JSON.
    pub pairs: Vec<(String, String)>,
    /// An input: its value is the one its last run received (its source still says where
    /// that came from).
    #[serde(default)]
    pub received: bool,
    /// An output: what set it, when that is not what set the rest ("Run 2", "Set by hand").
    #[serde(default)]
    pub set_by: String,
}
impl FieldView {
    /// A true or false value its fn gives no doc for.
    pub fn is_switch(&self) -> bool {
        self.available && self.doc.is_empty() && matches!(self.kind.as_str(), "true" | "false")
    }
    pub fn new(
        name: &str,
        ty: &str,
        doc: &str,
        value: Option<&serde_json::Value>,
        source: &str,
    ) -> Self {
        Self {
            name: name.into(),
            ty: ty.into(),
            doc: doc.into(),
            value: value
                .map(|v| match (v, file_path(v)) {
                    (_, Some(path)) => path.to_owned(),
                    (serde_json::Value::String(s), _) => s.clone(),
                    _ => serde_json::to_string_pretty(v).expect("JSON value"),
                })
                .unwrap_or_default(),
            available: value.is_some(),
            pairs: value.and_then(pairs).unwrap_or_default(),
            kind: match value {
                Some(v) if file_path(v).is_some() => "file",
                Some(v) if pairs(v).is_some() => "pairs",
                Some(serde_json::Value::Null) => "null",
                Some(serde_json::Value::Bool(true)) => "true",
                Some(serde_json::Value::Bool(false)) => "false",
                Some(serde_json::Value::Number(_)) => "number",
                Some(serde_json::Value::String(_)) => "text",
                _ => "json",
            }
            .into(),
            source: source.into(),
            received: false,
            set_by: String::new(),
        }
    }
    pub fn reference(
        name: &str,
        ty: &str,
        reference: &ValueRef,
        plan: &Plan,
        state: &StateSnapshot,
    ) -> Self {
        let resolved = resolve_reference(plan, state, reference);
        let value = match &resolved {
            BoundValue::Ready(v) => Some(v.as_value()),
            _ => None,
        };
        Self::new(name, ty, "", value, &reference.0)
    }
    /// A long text value as markdown in the body's font: a spec, a summary, a report.
    /// Its text as markdown, its headings from `top` down so they nest under its section.
    pub fn prose_html(&self, top: u8) -> TrustedHtml {
        crate::markdown::render_from(&self.value, top)
    }
    /// Its type in a few words: a JSON schema reads as its `type` ("object"), not as JSON.
    pub fn ty_words(&self) -> String {
        super::ui::type_words(&self.ty)
    }
    pub fn long(&self) -> bool {
        if self.kind == "pairs" {
            return false;
        }
        self.value.contains('\n') || self.value.chars().count() > 90
    }
    pub fn key(&self, prefix: &str) -> String {
        format!("{prefix}:{}", self.name)
    }
    /// Where its value comes from, under its name: "Default", "From <step> · <output>" (each
    /// source step a link), "Plan input <name>", "A file, read when the run starts", "Submitted
    /// so far"; for an output what set it when that differs from the rest ("Run 2"). Nothing
    /// when it says nothing.
    pub fn about_html(&self, project: &ProjectId) -> TrustedHtml {
        use super::ui::esc;
        let mut parts: Vec<String> = vec![];
        let refs: Option<Vec<sluice_model::gates::Reference>> = (!self.source.is_empty())
            .then(|| {
                self.source
                    .split(", ")
                    .map(|part| ValueRef::parse(part).ok()?.parts().ok())
                    .collect()
            })
            .flatten();
        match (self.source.as_str(), refs) {
            ("", _) => {}
            ("Its default", _) => parts.push("Default".into()),
            (_, Some(refs)) => {
                let named: Vec<String> = refs
                    .iter()
                    .map(|r| {
                        let field = if r.fields.is_empty() {
                            String::new()
                        } else {
                            format!(".{}", r.fields.join("."))
                        };
                        match &r.step {
                            Some(step) => format!(
                                "<a href=\"/projects/id/{project}/steps/{step}\" data-opens=\"{step}\"><code>{s}</code></a> · {o}{f}",
                                s = esc(step.as_str()),
                                o = esc(&r.name),
                                f = esc(&field),
                            ),
                            None => format!("plan input <code>{}{}</code>", esc(&r.name), esc(&field)),
                        }
                    })
                    .collect();
                let from = named.join(", ");
                parts.push(match from.strip_prefix("plan input ") {
                    Some(rest) if refs.len() == 1 => format!("Plan input {rest}"),
                    _ => format!("From {from}"),
                });
            }
            (other, None) => parts.push(esc(other)),
        }
        if !self.set_by.is_empty() {
            parts.push(esc(&self.set_by));
        }
        TrustedHtml::owned(
            parts
                .into_iter()
                .map(|p| format!("<span class=\"f-src\">{p}</span>"))
                .collect(),
        )
    }
}
/// A small flat object's names and values: at most 8, each a string, number, true or false.
fn pairs(value: &serde_json::Value) -> Option<Vec<(String, String)>> {
    let object = value.as_object()?;
    if object.is_empty() || object.len() > 8 {
        return None;
    }
    object
        .iter()
        .map(|(k, v)| {
            let shown = match v {
                serde_json::Value::String(s) if s.chars().count() <= 60 && !s.contains('\n') => {
                    s.clone()
                }
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => return None,
            };
            Some((k.clone(), shown))
        })
        .collect()
}
/// A file reference's path: `{"file": "<path>"}`, an input read from a file when the run starts.
fn file_path(value: &serde_json::Value) -> Option<&str> {
    let object = value.as_object()?;
    (object.len() == 1).then(|| object.get("file")?.as_str())?
}
#[derive(Clone, Debug, Serialize)]
pub struct RunView {
    pub id: RunId,
    pub started: String,
    pub finished: String,
    pub session: String,
    pub engine: String,
    pub inputs: Vec<FieldView>,
    /// How it ended, read from its result.
    pub outcome: Outcome,
    /// Its result's error, read for a person.
    pub failure: Option<super::failure::Failure>,
    /// The outputs its result carried, by name.
    pub outputs: Vec<String>,
    /// What its completion action did ("applied", …), when it had one.
    pub action: String,
    /// The run's files the dashboard serves (`run_file`), those it has.
    pub files: Vec<&'static str>,
    /// Seconds it ran (to its end, or to the read).
    #[serde(skip)]
    pub seconds: Option<f64>,
    /// An agent run's calls by tool, read from its transcript (`activity::attach`): "Bash 42 ·
    /// Edit 9 · Read 17 · 2 failed"; "" for any other run.
    pub profile: String,
}
/// The run files a page may open, read-only and only from that run's own directory: its
/// stderr, its tail, its summary, the pane its agent left when it failed, and the records its
/// engine left there: Codex's readable log and protocol log, Devin's hook journal, log and
/// export (each the run's own, else its newest invocation's). A transcript is served masked
/// (`TRANSCRIPTS`).
pub const RUN_FILES: [&str; 9] = [
    "pane-at-failure.txt",
    "stderr.log",
    "stderr-tail.log",
    "summary.txt",
    "codex.log",
    "codex-wire.jsonl",
    "devin-hooks.jsonl",
    "devin.log",
    "devin.json",
];
/// The run files that are an engine's record, in the order the Activity section links the
/// first a run has: what they hold is masked as an engine's notes are (`account::redact`)
/// before it is served.
pub const TRANSCRIPTS: [&str; 5] = [
    "codex.log",
    "codex-wire.jsonl",
    "devin-hooks.jsonl",
    "devin.log",
    "devin.json",
];
/// The path of one of a run's files under `home`, when the run has it as a plain file (never
/// through a link): the run's directory is its id's, and the name one of `RUN_FILES`.
pub fn run_file(home: &std::path::Path, run: &RunId, name: &str) -> Option<std::path::PathBuf> {
    let name = RUN_FILES.iter().find(|n| **n == name)?;
    let dir = home.join("runs").join(run.to_string());
    let plain =
        |p: &std::path::Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file());
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    if TRANSCRIPTS.contains(name) && plain(&dir.join(name)) {
        return Some(dir.join(name));
    }
    if *name == "pane-at-failure.txt" || TRANSCRIPTS.contains(name) {
        let invocations = dir.join("invocations");
        if !std::fs::symlink_metadata(&invocations).is_ok_and(|m| m.is_dir()) {
            return None;
        }
        return std::fs::read_dir(&invocations)
            .ok()?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path().join(name))
            .filter(|p| plain(p))
            .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    }
    let path = dir.join(name);
    plain(&path).then_some(path)
}
/// How a run ended, as its result says it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It has not ended.
    Running,
    /// It ended: its result's status, as the step would read with it (a cancel as cancelled).
    Ended(Shown),
    /// It ended with no result recorded.
    Unrecorded,
    /// Its result names a status sluice does not know: said, never guessed.
    Unknown(String),
}
impl Outcome {
    /// Its glyph's state, when it has one.
    pub fn shown(&self) -> Option<Shown> {
        match self {
            Outcome::Running => Some(Shown::Running),
            Outcome::Ended(shown) => Some(*shown),
            Outcome::Unrecorded | Outcome::Unknown(_) => None,
        }
    }
    /// Its glyph; nothing when no result says how it ended.
    pub fn glyph(&self) -> TrustedHtml {
        self.shown()
            .map(super::ui::glyph)
            .unwrap_or_else(|| TrustedHtml::owned(String::new()))
    }
    /// In words: "Running", "Failed", "No result recorded".
    pub fn words(&self) -> String {
        match self {
            Outcome::Running => "Running".into(),
            Outcome::Ended(shown) => {
                let word = shown.word();
                word[..1].to_uppercase() + &word[1..]
            }
            Outcome::Unrecorded => "No result recorded".into(),
            Outcome::Unknown(status) => {
                format!("Its result says “{status}”, a status sluice does not know")
            }
        }
    }
}
impl RunView {
    /// How it ended, after its number: "failed", "cancelled", "ended with no result recorded".
    pub fn ended_words(&self) -> String {
        match &self.outcome {
            Outcome::Running => "is still running".into(),
            Outcome::Ended(shown) => shown.word().into(),
            Outcome::Unrecorded => "ended with no result recorded".into(),
            Outcome::Unknown(status) => format!("ended “{status}”"),
        }
    }
    /// What its failure says after its end, for a line that already says how it ended: the
    /// headline ("Its engine hit a usage cap."), a cancel's reason; "" when nothing more.
    pub fn ended_sentence(&self) -> &str {
        match &self.failure {
            Some(f) if f.cancelled => f.headline.strip_prefix("Cancelled: ").unwrap_or(""),
            Some(f) => &f.headline,
            None => "",
        }
    }
    pub fn file_href(&self, project: &ProjectId, name: &str) -> String {
        format!("/projects/id/{project}/runs/{}/files/{name}", self.id)
    }
    /// "took 2h 14m" for an ended run.
    pub fn took(&self) -> String {
        match (self.finished.is_empty(), self.seconds) {
            (false, Some(s)) => format!("took {}", super::ui::duration_text(s)),
            _ => String::new(),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct StepView {
    pub project: ProjectId,
    pub id: StepId,
    /// Its title (`sluice_model::naming`): "" when it has none and its id names it.
    pub title: String,
    /// Its stage in its unit ("land"), "" when it has none.
    pub stage: String,
    pub function: String,
    pub doc: String,
    /// Its stored status; how it reads is `shown()`.
    pub status: StepStatus,
    pub tags: Vec<String>,
    /// Paused by its own pause (Unpause applies).
    pub paused: bool,
    /// Pending with its gates met: the next to start (`is-next`). A finished step is never.
    pub ready: bool,
    /// Pending behind a step that failed or went stale (`sluice_model::status::blocked`).
    pub blocked: bool,
    /// Pending, ready and done outside sluice (`core.external`).
    pub external: bool,
    /// Pending and reading a plan input with no value.
    pub held: bool,
    /// Running and quiet past its `quiet_after`.
    pub quiet: bool,
    /// Running, and a cancel was asked for: its run is stopping.
    pub stopping: bool,
    /// What it waits on besides a pause (`hold` says that): its gates and handoffs not ready.
    pub waits: Vec<String>,
    /// A pending step's pause: its own (and who paused it, once its page has read the record)
    /// or its project's.
    pub hold: Option<Hold>,
    /// What it runs after (`after`), each with where it leads and how it stands.
    pub gates: Vec<GateView>,
    pub queued: Vec<String>,
    pub skipped: Vec<String>,
    pub error: String,
    /// What its stored error says, read for a person (a cancel among them).
    pub failure: Option<super::failure::Failure>,
    pub inputs: Vec<FieldView>,
    pub outputs: Vec<FieldView>,
    pub runs: Vec<RunView>,
    pub messages: usize,
    pub awaiting: usize,
    pub total: Option<usize>,
    pub done: usize,
    pub manual: bool,
    pub revision: u64,
    /// Running, and its run has submitted: only finishing (SPEC §6.4).
    pub finishing: Option<sluice_model::attempt::Finishing>,
    /// Its latest progress (`step_progress`) while that is fresher than its outputs.
    pub progress: Option<ProgressView>,
    /// Its conversation (`threads::Conversation`): its own thread and what it sent or was sent
    /// on any other, the Thread tab's; read when its page or drawer draws.
    #[serde(skip)]
    pub thread: super::threads::Conversation,
    /// Its own latest message (to anyone) as its first words, and while it runs the latest to
    /// it since: what Overview quotes.
    #[serde(skip)]
    pub exchange: super::threads::Conversation,
    /// While it runs: when its run last wrote anything (RFC 3339), as the board observed its
    /// run files. Read with `quiet`; it moves with every write, so no part of the version.
    #[serde(skip)]
    pub active_at: String,
    /// When its current result (its outputs, or its failure) was recorded; "" without one.
    pub result_at: String,
    /// A failed step's failure as its project's log records it (the record's seq), once its
    /// page has read it.
    pub failure_record: Option<i64>,
    /// Its current run's times, for its card's timer.
    pub timing: Option<RunTiming>,
    /// How long its stage usually takes: the median of its recipe's done units' runs of the
    /// same stage (`board::usual_durations`), none with fewer than three.
    pub usually: Option<f64>,
    /// Its unit's timeline, on its page and in the drawer (`board::load_step`).
    pub timeline: Option<super::timeline::Timeline>,
    /// It comes after a step or a step comes after it: its page links its chain on the plan
    /// (`board::load_step`).
    pub chained: bool,
    /// Its latest agent run's activity outline, read from its transcript when its page or
    /// drawer draws (`activity::attach`).
    pub activity: Option<super::activity::ActivityView>,
}
/// When a step's current run started and how long it ran or has run so far: its card's timer.
/// The current run is its latest in its current generation; a scatter's is its latest round,
/// from its first item's start to its last item's end.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct RunTiming {
    /// The current run's start, RFC 3339 UTC.
    pub started: String,
    /// When it ended; none while it runs.
    pub finished: Option<String>,
    /// How many runs the step has had in its current generation, this one included (a
    /// scatter's rounds).
    pub runs: usize,
    /// How each earlier run (a scatter's round: the first state among its items) ended, oldest
    /// first; a run with no result recorded is left out.
    pub earlier: Vec<Shown>,
    /// Seconds it ran, or has run as of the read. It moves with the clock while the run does, so
    /// it is no part of the step's version.
    #[serde(skip)]
    pub seconds: f64,
    /// Every run of its current generation, oldest first (a scatter's round one): its unit's
    /// timeline. What changes in them changes `runs`, `started` or `finished` too.
    #[serde(skip)]
    pub spans: Vec<super::timeline::RunSpan>,
}
/// What holds a pending step back: its own pause or its project's.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Hold {
    /// Its project is paused (else the step itself is).
    pub project: bool,
    /// The reason the pause gives ("" when none).
    pub reason: String,
    /// Who paused it and when, as the plan edit that paused it records ("" when not read or
    /// not found).
    pub by: String,
    pub at: String,
}
impl Hold {
    /// Who paused it, in words: "the owner", "cli", "" when unknown.
    pub fn who(&self) -> String {
        match self.by.as_str() {
            "owner" => "the owner".into(),
            by => by.into(),
        }
    }
}
/// One entry of a step's `after`: a step (linked, with how it reads), a unit (linked) or a
/// condition.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct GateView {
    pub entry: String,
    pub href: String,
    /// How its step reads; none for a unit or a condition.
    pub shown: Option<Shown>,
    #[serde(skip)]
    pub step: Option<StepId>,
}
impl GateView {
    /// Its step's glyph; nothing for a unit or a condition.
    pub fn glyph(&self) -> TrustedHtml {
        self.shown
            .map(super::ui::glyph)
            .unwrap_or_else(|| TrustedHtml::owned(String::new()))
    }
    pub fn done(&self) -> bool {
        self.shown
            .is_some_and(|s| s.spec().band == sluice_model::shown::Band::Done)
    }
}
/// A step's progress as its page shows it: the fields, when they were set, and whether the
/// step still runs (live) or the run has ended (kept until the next run starts).
#[derive(Clone, Debug, Serialize)]
pub struct ProgressView {
    pub fields: Vec<FieldView>,
    pub at: String,
    pub live: bool,
}
impl StepView {
    /// What its page lists under "Waits on": its waits, less those the After row says (each
    /// gate there carries its status).
    pub fn page_waits(&self) -> Vec<&str> {
        self.waits
            .iter()
            .filter(|w| self.gates.is_empty() || !w.starts_with("after "))
            .map(String::as_str)
            .collect()
    }
    /// Its pause in words, for a line that has no room for its time: "Paused by cli: the
    /// reason.", "Its project is paused."; "" when nothing holds it.
    pub fn hold_words(&self) -> String {
        let Some(hold) = &self.hold else {
            return String::new();
        };
        if hold.project {
            return "Its project is paused.".into();
        }
        let mut words = "Paused".to_owned();
        if !hold.who().is_empty() {
            words.push_str(&format!(" by {}", hold.who()));
        }
        if !hold.reason.is_empty() {
            words.push_str(&format!(": {}", hold.reason));
        }
        words.push('.');
        words
    }
    /// Its `after` in a few words: "98 steps, all done", "3 steps: 2 done, 1 running", with
    /// any units and conditions counted after.
    pub fn gates_words(&self) -> String {
        let steps: Vec<&GateView> = self.gates.iter().filter(|g| g.shown.is_some()).collect();
        let others = self.gates.len() - steps.len();
        let noun = super::ui::count;
        let mut words = String::new();
        if !steps.is_empty() {
            let done = steps.iter().filter(|g| g.done()).count();
            words = noun(steps.len(), "step", "steps");
            if done == steps.len() {
                words.push_str(if done == 1 { ", done" } else { ", all done" });
            } else {
                let by: super::ui::Tally = steps
                    .iter()
                    .filter(|g| !g.done())
                    .filter_map(|g| g.shown)
                    .collect();
                let mut parts = vec![];
                if done > 0 {
                    parts.push(format!("{done} done"));
                }
                parts.extend(by.iter().map(|(s, n)| format!("{n} {}", s.word())));
                words.push_str(&format!(": {}", parts.join(", ")));
            }
        }
        if others > 0 {
            if !words.is_empty() {
                words.push_str(" and ");
            }
            words.push_str(&noun(others, "unit or condition", "units or conditions"));
        }
        words
    }
    /// Now leads with what the step itself wrote last: its live progress when that is newer
    /// than its own latest message.
    pub fn progress_first(&self) -> bool {
        let own = self.exchange.rows.iter().find_map(|row| match row {
            super::threads::Row::Group(g) if g.from == super::threads::Who::This => {
                g.entries.first().map(|e| e.item.message.at.as_str())
            }
            _ => None,
        });
        self.progress
            .as_ref()
            .is_some_and(|p| p.live && own.is_none_or(|at| p.at.as_str() > at))
    }
    pub fn new(project: ProjectId, plan: &Plan, state: &StateSnapshot, id: &StepId) -> Self {
        let step = &plan.steps()[id];
        let entry = state.steps.get(id).cloned().unwrap_or_default();
        let decision = evaluate_step(plan, state, step);
        let ready = decision == GateDecision::Ready;
        let paused = step.paused.is_paused();
        let held = paused || state.paused.is_paused();
        let pending = entry.status == StepStatus::Pending;
        let hold = (held && pending).then(|| Hold {
            project: !paused,
            reason: match &step.paused {
                Pause::Reason(reason) => reason.clone(),
                _ => String::new(),
            },
            ..Hold::default()
        });
        // next to start: pending with its gates met (a finished step's gates are met too)
        let next = ready && pending;
        let inputs = step
            .bindings
            .iter()
            .map(|(n, b)| {
                let ty = step
                    .signature
                    .inputs
                    .get(n)
                    .or_else(|| step.extra_inputs.get(n))
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                match b {
                    Binding::Default(v) => {
                        FieldView::new(n, &ty, "", Some(v.as_value()), "Its default")
                    }
                    Binding::Source(r) => FieldView::reference(n, &ty, r, plan, state),
                    Binding::Sources(rs) => {
                        let fields: Vec<_> = rs
                            .iter()
                            .map(|r| FieldView::reference(n, &ty, r, plan, state))
                            .collect();
                        let values: Option<Vec<_>> = rs
                            .iter()
                            .map(|r| match resolve_reference(plan, state, r) {
                                BoundValue::Ready(v) => Some(v.as_value().clone()),
                                _ => None,
                            })
                            .collect();
                        let value = values.map(serde_json::Value::Array);
                        FieldView::new(
                            n,
                            &ty,
                            "",
                            value.as_ref(),
                            &fields
                                .iter()
                                .map(|f| f.source.clone())
                                .collect::<Vec<_>>()
                                .join(", "),
                        )
                    }
                    Binding::File(path) => FieldView::new(
                        n,
                        &ty,
                        "",
                        Some(&serde_json::json!({"file":path})),
                        "A file, read when the run starts",
                    ),
                }
            })
            .collect();
        let mut outputs = vec![];
        for (name, ty, doc) in step
            .signature
            .outputs
            .iter()
            .map(|(n, t)| (n, t, ""))
            .chain(
                step.declared_outputs
                    .iter()
                    .map(|(n, d)| (n, &d.ty, d.doc.as_deref().unwrap_or(""))),
            )
        {
            outputs.push(FieldView::new(
                name,
                &ty.to_string(),
                doc,
                entry.outputs.0.get(name).map(|v| v.as_value()),
                "",
            ));
        }
        for (n, v) in &entry.outputs.0 {
            if !outputs.iter().any(|f| &f.name == n) {
                outputs.push(FieldView::new(n, "", "", Some(v.as_value()), ""));
            }
        }
        let failure = entry
            .error
            .as_deref()
            .map(|message| super::failure::Failure::parse(message, None));
        let mut view = Self {
            project,
            id: id.clone(),
            title: String::new(),
            stage: String::new(),
            function: step.run.clone(),
            doc: step.doc.clone().unwrap_or_default(),
            status: entry.status.clone(),
            tags: step.tags.clone(),
            paused,
            ready: next,
            blocked: false,
            external: pending && ready && step.is_external(),
            held: pending && sluice_model::status::missing_input(plan, state, step).is_some(),
            quiet: false,
            stopping: false,
            waits: vec![],
            hold,
            gates: {
                let mut gates: Vec<GateView> = step
                    .after
                    .iter()
                    .map(|g| match g {
                        sluice_model::gates::Gate::Step { id, .. } => GateView {
                            entry: g.entry(),
                            href: format!("/projects/id/{project}/steps/{id}"),
                            shown: None,
                            step: Some(id.clone()),
                        },
                        sluice_model::gates::Gate::Unit { name, .. } => GateView {
                            entry: g.entry(),
                            href: format!("/projects/id/{project}/units/{name}"),
                            shown: None,
                            step: None,
                        },
                        sluice_model::gates::Gate::Bool { .. } => GateView {
                            entry: g.entry(),
                            href: String::new(),
                            shown: None,
                            step: None,
                        },
                    })
                    .collect();
                gates.sort_by(|a, b| a.entry.cmp(&b.entry));
                gates
            },
            queued: entry.queued,
            skipped: entry.skipped.iter().map(|s| s.to_string()).collect(),
            error: entry.error.unwrap_or_default(),
            failure: None,
            inputs,
            outputs,
            runs: vec![],
            messages: 0,
            awaiting: 0,
            total: None,
            done: 0,
            manual: false,
            revision: 0,
            finishing: None,
            progress: None,
            thread: Default::default(),
            exchange: Default::default(),
            active_at: String::new(),
            result_at: String::new(),
            failure_record: None,
            timing: None,
            usually: None,
            timeline: None,
            chained: false,
            activity: None,
        };
        if let Some(failure) = failure {
            view.set_failure(failure);
        }
        // what it waits on, each step named as the store has it until the board knows more
        view.name_waits(plan, state, &|id: &StepId| {
            let status = state.status(id);
            Some(sluice_model::shown::classify(&Facts::of(status)))
        });
        view
    }
    /// What it waits on (`waits`) and how each step of its `after` reads, each step named by
    /// `shown` (the board's own reading: a cancel "cancelled", a quiet run "quiet"), so its
    /// card, page and the board's StepStatus say a cancel as one. A pause is said apart
    /// (`hold`).
    pub fn name_waits(
        &mut self,
        plan: &Plan,
        state: &StateSnapshot,
        shown: &dyn Fn(&StepId) -> Option<Shown>,
    ) {
        let step = &plan.steps()[&self.id];
        let word = |id: &StepId| shown(id).map_or(state.status(id).as_str(), Shown::word);
        self.waits = match evaluate_step(plan, state, step) {
            GateDecision::Wait(_) => wait_reasons_worded(plan, state, step, &word),
            GateDecision::Invalid(e) => e.into_iter().map(|e| e.to_string()).collect(),
            GateDecision::Ready | GateDecision::Skip(_) => vec![],
        };
        self.waits
            .retain(|w| w != "paused" && !w.starts_with("paused: ") && w != "project paused");
        for gate in &mut self.gates {
            gate.shown = gate.step.as_ref().and_then(shown);
        }
    }
    /// Its stored error, read: a failed step the owner cancelled reads `cancelled`, a state of
    /// its own on every page (its glyph, caption and counts), though the store keeps it failed.
    pub fn set_failure(&mut self, failure: super::failure::Failure) {
        self.failure = Some(failure);
    }
    /// How it reads (`sluice_model::shown`): its stored status named from the facts beside it.
    /// Derived each time, so a fact set late (its run's quiet, observed after the store) is
    /// never missed by a copy taken early.
    pub fn shown(&self) -> Shown {
        sluice_model::shown::classify(&Facts {
            cancelled: self.failure.as_ref().is_some_and(|f| f.cancelled),
            paused: self.hold.is_some(),
            external: self.external,
            blocked: self.blocked,
            held: self.held,
            queued: !self.queued.is_empty(),
            quiet: self.quiet && !self.active_at.is_empty(),
            finishing: self.finishing.is_some(),
            stopping: self.stopping,
            manual: self.manual,
            ..Facts::of(self.status.clone())
        })
    }
    pub fn cancelled(&self) -> bool {
        self.shown() == Shown::Cancelled
    }
    /// Its stored status is failed (a cancel too).
    pub fn failed(&self) -> bool {
        self.status == StepStatus::Failed
    }
    /// Its stored status is running (quiet, finishing and stopping too).
    pub fn running(&self) -> bool {
        self.status == StepStatus::Running
    }
    pub fn pending(&self) -> bool {
        self.status == StepStatus::Pending
    }
    pub fn succeeded(&self) -> bool {
        self.status == StepStatus::Succeeded
    }
    /// Its run has written nothing past its cadence.
    pub fn is_quiet(&self) -> bool {
        self.shown() == Shown::Quiet
    }
    /// A running step's badge, before "running for 2h": its state's word when it is more than
    /// running ("quiet · ", "stopping · ").
    pub fn badge_lead(&self) -> String {
        match self.shown() {
            Shown::Running => String::new(),
            shown => format!("{} · ", shown.word()),
        }
    }
    /// Its badge's tone: gold when its state needs a look but is no failure.
    pub fn badge_tone(&self) -> &'static str {
        match self.shown().spec().tone {
            sluice_model::shown::Tone::Attention => " attn",
            sluice_model::shown::Tone::Ink
            | sluice_model::shown::Tone::Muted
            | sluice_model::shown::Tone::Active
            | sluice_model::shown::Tone::Paused
            | sluice_model::shown::Tone::Idle
            | sluice_model::shown::Tone::Success => "",
        }
    }
    /// How its result says it ended, for a step with no run kept: "Cancelled", "Failed",
    /// "Ended".
    pub fn ended_word(&self) -> &'static str {
        match self.shown() {
            Shown::Cancelled => "Cancelled",
            Shown::Failed => "Failed",
            _ => "Ended",
        }
    }
    /// A lane matrix's mark for a stage nothing has reached: its state's glyph, or none for
    /// plain pending (the cell's dot).
    pub fn dot(&self) -> TrustedHtml {
        match self.shown() {
            Shown::Pending => TrustedHtml::owned(String::new()),
            shown => super::ui::mark(shown),
        }
    }
    /// How a page names it: its title (and stage), its id after it.
    pub fn name(&self) -> super::ui::StepRef {
        super::ui::StepRef {
            id: self.id.to_string(),
            title: self.title.clone(),
            stage: self.stage.clone(),
        }
    }
    /// Its heading's words: its title, or its id when it has none.
    pub fn heading(&self) -> &str {
        if self.title.is_empty() {
            self.id.as_str()
        } else {
            &self.title
        }
    }
    /// It has a title apart from its id.
    pub fn titled(&self) -> bool {
        !self.title.is_empty() && self.title != self.id.as_str()
    }
    /// Its doc as its page shows it: without its first line when that line is its title, so
    /// the page never says it twice.
    pub fn doc_rest(&self) -> &str {
        let doc = self.doc.trim();
        let mut lines = doc.splitn(2, '\n');
        let first = lines.next().unwrap_or("");
        if !self.title.is_empty() && sluice_model::naming::line_title(first) == self.title {
            lines.next().unwrap_or("").trim()
        } else {
            doc
        }
    }
    /// "3/5 runs": a scattered step's items done.
    pub fn runs_tag(&self) -> TrustedHtml {
        super::ui::tag(&format!("{} runs", self.caption()), "", None)
    }
    /// Its `unit:` tag, a link to the unit's page.
    pub fn unit_tag(&self, unit: &str) -> TrustedHtml {
        super::ui::tag_link(
            &format!("/projects/id/{}/units/{unit}", self.project),
            &format!("unit: {unit}"),
            "",
            None,
        )
    }
    /// "2 awaiting reply", gold: questions on its thread nobody has answered.
    pub fn awaiting_tag(&self) -> TrustedHtml {
        super::ui::tag_link(
            "#tp-thread",
            &format!("{} awaiting reply", self.awaiting),
            "attn",
            None,
        )
    }
    pub fn href(&self) -> String {
        format!("/projects/id/{}/steps/{}", self.project, self.id)
    }
    pub fn thread_href(&self) -> String {
        format!(
            "/projects/id/{}/thread?thread=step-{}",
            self.project, self.id
        )
    }
    /// The plan focused on its chain: what it comes after and what comes after it.
    pub fn chain_href(&self) -> String {
        format!(
            "/projects/id/{}?root={}&up=1&down=1",
            self.project,
            url::form_urlencoded::byte_serialize(self.id.as_str().as_bytes()).collect::<String>()
        )
    }
    /// Its failure's own record on its project's log, when kept: "" otherwise.
    pub fn failure_log_href(&self) -> String {
        self.failure_record
            .map(|seq| failure_log_href(&self.project, self.id.as_str(), seq))
            .unwrap_or_default()
    }
    /// Its records and its thread's messages on the project's log.
    pub fn log_href(&self) -> String {
        format!("/projects/id/{}/log?step={}", self.project, self.id)
    }
    pub fn key(&self) -> String {
        format!("s:{}", self.id)
    }
    pub fn card_class(&self) -> String {
        format!(
            "node {} is-{}{}",
            if self.function.starts_with("core.") && self.function != "core.external" {
                "chip"
            } else {
                "card"
            },
            self.shown().key(),
            if self.ready { " is-next" } else { "" },
        )
    }
    /// Nothing has reached it yet: a lane matrix draws it as a small mark, not a card.
    pub fn unreached(&self) -> bool {
        match self.shown() {
            Shown::Pending | Shown::Paused | Shown::Blocked | Shown::Held => true,
            Shown::Failed
            | Shown::Cancelled
            | Shown::Stale
            | Shown::Quiet
            | Shown::Stopping
            | Shown::Finishing
            | Shown::Running
            | Shown::External
            | Shown::Queued
            | Shown::Manual
            | Shown::Succeeded
            | Shown::Skipped => false,
        }
    }
    /// Its card's caption: a failure's kind in a word ("cap", "quota", "engine", "failed"),
    /// else its state's word when the table says a card says it ("cancelled", "quiet",
    /// "outside"), else a scatter's items done ("3/5").
    pub fn caption(&self) -> String {
        let shown = self.shown();
        if shown == Shown::Failed
            && let Some(failure) = &self.failure
        {
            // the kind of failure in a word: "cap", "quota", "engine", …
            failure.caption().into()
        } else if shown.spec().caption {
            shown.word().into()
        } else if let Some(total) = self.total {
            format!("{}/{total}", self.done)
        } else {
            String::new()
        }
    }
    /// Why it stopped, in one sentence, when it failed or was cancelled: its failure's
    /// headline ("Stopped at its wall-clock cap after 10h 0m.", "Cancelled: a reason").
    pub fn why(&self) -> &str {
        match (self.shown(), &self.failure) {
            (Shown::Failed | Shown::Cancelled, Some(f)) => &f.headline,
            _ => "",
        }
    }
    /// Its card's description for a screen reader: its doc, what it waits on, why it is
    /// queued and why it stopped, each its own sentence.
    pub fn description(&self) -> String {
        let doc = self.doc.trim();
        let mut parts: Vec<String> = vec![];
        if !doc.is_empty() {
            parts.push(doc.trim_end_matches('.').to_owned());
        }
        parts.extend(self.waits.iter().cloned());
        parts.extend(self.queued.iter().map(|q| format!("queued: {q}")));
        if !self.why().is_empty() {
            parts.push(self.why().trim_end_matches('.').to_owned());
        }
        if parts.is_empty() {
            String::new()
        } else {
            parts.join(". ") + "."
        }
    }
    /// That sentence under its card or its matrix row, muted, a link to the step.
    pub fn why_html(&self) -> TrustedHtml {
        if self.why().is_empty() {
            return TrustedHtml::owned(String::new());
        }
        TrustedHtml::owned(format!(
            "<p class=\"why\"><a href=\"{}\" data-opens=\"{}\">{}</a></p>",
            self.href(),
            super::ui::esc(self.id.as_str()),
            super::ui::esc(self.why())
        ))
    }
    /// What its caption means, for the caption's title (no legend: each says itself): a
    /// failure's or a cancel's one sentence.
    pub fn caption_help(&self) -> String {
        let shown = self.shown();
        if let Some(failure) = self
            .failure
            .as_ref()
            .filter(|_| matches!(shown, Shown::Failed | Shown::Cancelled))
        {
            failure.headline.clone()
        } else if shown.spec().caption {
            shown.spec().help.into()
        } else {
            self.total
                .map(|total| format!("{} of its {total} items done", self.done))
                .unwrap_or_default()
        }
    }
    /// The run times its card shows: a running step's current run, ticking; how long a
    /// succeeded (not set by hand) or failed step's last run took. Other steps show none.
    pub fn shown_timing(&self) -> Option<&RunTiming> {
        let timing = self.timing.as_ref()?;
        let finished = timing.finished.is_some();
        match self.status {
            StepStatus::Running => Some(timing),
            StepStatus::Succeeded if finished && !self.manual => Some(timing),
            StepStatus::Failed if finished => Some(timing),
            StepStatus::Succeeded
            | StepStatus::Failed
            | StepStatus::Pending
            | StepStatus::Stale
            | StepStatus::Skipped => None,
        }
    }
    /// The card's timer, after its caption: a running run's a `<time data-since>` that
    /// `sluice.js` ticks (its text the clock's, which a page's version leaves out), a finished
    /// one's static and quieter. Visible as "2h 14m"; read as "for 2 hours 14 minutes" or
    /// "took 12 minutes" after the step's id (no comma: the card's parts are flex items, which
    /// a screen reader's name already separates with a space). Its title says how many runs the step has had.
    pub fn timer_html(&self) -> Result<TrustedHtml, askama::Error> {
        #[derive(Template)]
        #[template(
            source = "{% if live %}<time data-since=\"{{ t.started }}\" datetime=\"{{ t.started }}\" class=\"took live\" title=\"{{ title }}\"{% if let Some(u) = usually %} data-usually=\"{{ u }}\"{% endif %}><span class=\"tk\" aria-hidden=\"true\">{{ shown }}</span><span class=\"vh\"> for {{ said }}</span>{% if !usual_said.is_empty() %}<span class=\"vh tu\">, {{ usual_said }}</span>{% endif %}</time>{% else %}<span class=\"took\" title=\"{{ title }}\"><span aria-hidden=\"true\">{{ shown }}</span><span class=\"vh\"> took {{ said }}</span></span>{% endif %}",
            ext = "html"
        )]
        struct Timer<'a> {
            t: &'a RunTiming,
            live: bool,
            shown: String,
            said: String,
            title: String,
            /// While it runs, how long its stage usually takes, whole seconds: the page's script
            /// draws how far along it is against that.
            usually: Option<u64>,
            usual_said: String,
        }
        if self.shown() == Shown::Quiet {
            // how long it has written nothing, ticking, in place of its run's time
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let seconds =
                super::timestamp(&self.active_at).map_or(0, |at| now.saturating_sub(at)) as f64;
            let at = super::ui::at_text(&self.active_at);
            return Ok(TrustedHtml::owned(format!(
                "<time data-since=\"{a}\" datetime=\"{a}\" class=\"took live\" title=\"Nothing written since {at}\"><span class=\"tk\" aria-hidden=\"true\">{shown}</span><span class=\"vh\"> for {said}</span></time>",
                a = self.active_at,
                shown = super::ui::duration_text(seconds),
                said = super::ui::duration_words(seconds),
            )));
        }
        let Some(t) = self.shown_timing() else {
            return Ok(TrustedHtml::owned(String::new()));
        };
        let live = t.finished.is_none();
        let shown = super::ui::duration_text(t.seconds);
        // a running run's title must not move with the clock: it names the start
        let this = if live {
            format!("started {}", super::ui::at_text(&t.started))
        } else {
            format!("took {shown}")
        };
        let mut title = match t.runs {
            0 | 1 => {
                let mut c = this.chars();
                c.next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default()
            }
            n => format!("{n} runs; this one {this}"),
        };
        let usually = self.usually.filter(|_| live);
        if let Some(u) = usually {
            title = format!(
                "{title}; its stage usually takes {}",
                super::ui::duration_text(u)
            );
        }
        TrustedHtml::from_template(&Timer {
            t,
            live,
            shown,
            said: super::ui::duration_words(t.seconds),
            title,
            usually: usually.map(|u| u.round() as u64),
            usual_said: usually
                .map(|u| format!("usually {}", super::ui::duration_words(u)))
                .unwrap_or_default(),
        })
    }
    /// "usually 40m": how long its stage usually takes (`usually`), "" when that is not known.
    pub fn usually_text(&self) -> String {
        self.usually
            .map(|s| format!("usually {}", super::ui::duration_text(s)))
            .unwrap_or_default()
    }
    /// Its run's number and how its earlier runs ended (oldest first), when it has had more
    /// than one and the latest is its own (running, or failed); a quiet run keeps its count.
    pub fn retries(&self) -> Option<(usize, &[Shown])> {
        let t = self.timing.as_ref()?;
        let current = matches!(self.status, StepStatus::Running | StepStatus::Failed);
        (current && t.runs > 1).then_some((t.runs, t.earlier.as_slice()))
    }
    /// Its earlier runs in words: "after 2 failed", "after 1 failed and 1 cancelled"; "" when
    /// none says how it ended.
    pub fn retries_words(&self) -> String {
        let Some((_, earlier)) = self.retries() else {
            return String::new();
        };
        let tally: super::ui::Tally = earlier.iter().copied().collect();
        let parts: Vec<String> = tally
            .iter()
            .map(|(s, n)| format!("{n} {}", s.word()))
            .collect();
        match parts.as_slice() {
            [] => String::new(),
            [one] => format!("after {one}"),
            [rest @ .., last] => format!("after {} and {last}", rest.join(", ")),
        }
    }
    /// "run 3" on its card, matrix pill or drawer, after a mark for each of its last three
    /// earlier runs' ends ("+2" before them for more); nothing on its first run.
    pub fn retries_html(&self) -> TrustedHtml {
        const MARKS: usize = 3;
        let Some((run, earlier)) = self.retries() else {
            return TrustedHtml::owned(String::new());
        };
        let words = self.retries_words();
        let mut marks = String::new();
        if earlier.len() > MARKS {
            marks.push_str(&format!(
                "<span class=\"tries-more\">+{}</span>",
                earlier.len() - MARKS
            ));
        }
        for shown in &earlier[earlier.len().saturating_sub(MARKS)..] {
            marks.push_str(super::ui::mark(*shown).as_str());
        }
        let title = if words.is_empty() {
            format!("Run {run}")
        } else {
            format!("Run {run}, {words}")
        };
        TrustedHtml::owned(format!(
            "<span class=\"tries\" title=\"{title}\"><span class=\"tries-m\" aria-hidden=\"true\">{marks}</span>run {run}{vh}</span>",
            title = super::ui::esc(&title),
            vh = if words.is_empty() {
                String::new()
            } else {
                format!("<span class=\"vh\">, {}</span>", super::ui::esc(&words))
            },
        ))
    }
    /// A running step's run before this one, for Now: its number and the run.
    pub fn previous_run(&self) -> Option<(usize, &RunView)> {
        if !self.running() || self.runs.len() < 2 {
            return None;
        }
        let n = self.runs.len() - 1;
        Some((n, &self.runs[n - 1]))
    }
    /// The run its outputs came from: its number and when it ended (the latest finished run
    /// whose result carried outputs; while it runs, an earlier one); none for a value set by
    /// hand or only submitted so far.
    pub fn outputs_from(&self) -> Option<(usize, &str)> {
        if self.manual
            || self
                .outputs_set()
                .iter()
                .all(|f| f.source == "Submitted so far")
        {
            return None;
        }
        self.runs
            .iter()
            .enumerate()
            .rev()
            .find(|(_, r)| !r.outputs.is_empty() && !r.finished.is_empty())
            .map(|(i, r)| (i + 1, r.finished.as_str()))
    }
    /// Each output says what set it when that is not the run the rest came from: an earlier
    /// run ("Run 1"), or the owner ("Set by hand").
    fn mark_setters(&mut self) {
        let from = self.outputs_from().map(|(n, _)| n);
        let setters: Vec<(String, String)> = self
            .outputs
            .iter()
            .filter(|f| f.available && f.source.is_empty())
            .filter_map(|f| {
                if self.manual {
                    return Some((f.name.clone(), "Set by hand".to_owned()));
                }
                let n = self
                    .runs
                    .iter()
                    .rposition(|r| !r.finished.is_empty() && r.outputs.contains(&f.name))?
                    + 1;
                (Some(n) != from).then(|| (f.name.clone(), format!("Run {n}")))
            })
            .collect();
        for (name, by) in setters {
            if let Some(f) = self.outputs.iter_mut().find(|f| f.name == name) {
                f.set_by = by;
            }
        }
    }
    /// The inputs drawn as fields: all but the undocumented switches (`switches`).
    pub fn input_rows(&self) -> Vec<&FieldView> {
        self.inputs.iter().filter(|f| !f.is_switch()).collect()
    }
    /// The inputs that are on/off switches its fn does not document (`listen`, `queued`): one
    /// line under the fields, so plumbing does not read as the step's subject.
    pub fn switches(&self) -> Vec<&FieldView> {
        self.inputs.iter().filter(|f| f.is_switch()).collect()
    }
    /// The outputs with a value, drawn as fields; the rest are named on one line.
    pub fn outputs_set(&self) -> Vec<&FieldView> {
        self.outputs.iter().filter(|f| f.available).collect()
    }
    pub fn outputs_unset(&self) -> Vec<&FieldView> {
        self.outputs.iter().filter(|f| !f.available).collect()
    }
    /// "7 outputs not set yet", "1 output not set yet".
    pub fn unset_words(&self) -> String {
        format!(
            "{} not set yet",
            super::ui::count(self.outputs_unset().len(), "output", "outputs")
        )
    }
    /// A failed step's next move is Retry: the primary button. One the owner cancelled was
    /// stopped on purpose, so Retry stays a plain button.
    pub fn retry_first(&self) -> bool {
        self.shown() == Shown::Failed
    }
    pub fn retryable(&self) -> bool {
        matches!(
            self.status,
            StepStatus::Succeeded | StepStatus::Failed | StepStatus::Stale
        )
    }
    /// Pause holds a step that would start: a pending or stale one (a failed one starts only
    /// when retried, so pausing it would mean nothing).
    pub fn pausable(&self) -> bool {
        self.paused || matches!(self.status, StepStatus::Pending | StepStatus::Stale)
    }
    pub fn last_run(&self) -> Option<&RunView> {
        self.runs.last()
    }
    /// While it runs, when its current run started: its status tag carries the time.
    pub fn running_since(&self) -> Option<&str> {
        self.last_run()
            .filter(|r| r.finished.is_empty() && self.running())
            .map(|r| r.started.as_str())
    }
    /// The tab's words: its stage and title ("work · Ship the cron fix"), cut to 48 characters.
    pub fn tab_title(&self) -> String {
        self.name().text(48)
    }
    pub fn cancel_prompt(&self) -> String {
        let duration = self
            .last_run()
            .filter(|r| r.finished.is_empty())
            .and_then(|r| r.seconds)
            .map(super::ui::duration_text);
        if let Some(duration) = duration {
            format!("Its {duration} run stops; Retry starts it over.")
        } else {
            "It stops; Retry starts it over.".to_owned()
        }
    }
    /// Cancel's confirmation: titled by its title, its id after it ("Cancel Fix the parser
    /// `l1-work`?"), naming the run, with an optional reason.
    pub fn cancel_confirm(&self) -> super::ui::Confirm {
        super::ui::Confirm {
            opener: "Cancel".into(),
            title: if self.titled() {
                format!("Cancel {}", self.name().text(60))
            } else {
                format!("Cancel {}?", self.id)
            },
            id: if self.titled() {
                self.id.to_string()
            } else {
                String::new()
            },
            action: format!("{}/actions", self.href()),
            hidden: vec![
                ("revision", self.revision.to_string()),
                ("action", "cancel".into()),
            ],
            copy: self.cancel_prompt(),
            reason: Some("Why stop this run?"),
            confirm: "Cancel the run".into(),
            keep: "Keep running",
            ..Default::default()
        }
    }
    /// Cancel applies: running and not already stopping (a cancel asked for is not offered
    /// again), or pending work outside sluice (`step_cancel` takes no other).
    pub fn cancellable(&self) -> bool {
        match self.status {
            StepStatus::Running => self.shown() != Shown::Stopping,
            StepStatus::Pending => self.function == "core.external",
            StepStatus::Succeeded
            | StepStatus::Failed
            | StepStatus::Stale
            | StepStatus::Skipped => false,
        }
    }
    /// Its tabs, each only when it has something to show, with a count where one helps:
    /// Overview always, then Activity, Thread (its messages), Inputs, Outputs (set of all) and
    /// Runs.
    pub fn tabs(&self) -> Vec<super::ui::Tab> {
        use super::ui::{Tab, count};
        let mut tabs = vec![Tab::new("overview", "Overview")];
        if let Some(a) = &self.activity {
            let turns = a.turns.len() + a.earlier;
            tabs.push(
                Tab::new("activity", "Activity")
                    .counted(turns.to_string(), count(turns, "turn", "turns")),
            );
        }
        if !self.thread.is_empty() {
            tabs.push(Tab::new("thread", "Thread").counted(
                self.thread.count.to_string(),
                count(self.thread.count, "message", "messages"),
            ));
        }
        if !self.inputs.is_empty() {
            tabs.push(Tab::new("inputs", "Inputs").counted(self.inputs.len().to_string(), ""));
        }
        if !self.outputs.is_empty() {
            let set = self.outputs_set().len();
            let all = self.outputs.len();
            tabs.push(if set == all {
                Tab::new("outputs", "Outputs").counted(all.to_string(), "all set")
            } else {
                Tab::new("outputs", "Outputs")
                    .counted(format!("{set}/{all}"), format!("{set} of {all} set"))
            });
        }
        if !self.runs.is_empty() {
            tabs.push(Tab::new("runs", "Runs").counted(self.runs.len().to_string(), ""));
        } else if self.no_run_kept() {
            tabs.push(Tab::new("runs", "Runs"));
        }
        tabs
    }
    /// The tab a page opens on: the one asked for (`?tab=`) when it has one, else Overview.
    pub fn tab_on(&self, asked: &str) -> &'static str {
        self.tabs()
            .into_iter()
            .find(|t| t.key == asked)
            .map_or("overview", |t| t.key)
    }
    /// It finished, and no run of it is kept: an earlier release ran it, or its runs were
    /// cleared since.
    pub fn no_run_kept(&self) -> bool {
        self.runs.is_empty() && (self.failed() || self.succeeded()) && !self.manual
    }
    /// The output its Overview leads with: the first set of `summary`, `result`, `report`,
    /// `final`, `answer` or `verdict`.
    pub fn key_output(&self) -> Option<&FieldView> {
        ["summary", "result", "report", "final", "answer", "verdict"]
            .iter()
            .find_map(|name| self.outputs.iter().find(|f| f.available && f.name == *name))
    }
    /// Overview has nothing else to say: its state's own sentence stands in.
    pub fn overview_empty(&self) -> bool {
        !self.running()
            && !self.failed()
            && self.page_waits().is_empty()
            && self.hold.is_none()
            && self.gates.is_empty()
            && !self.chained
            && self.queued.is_empty()
            && self.skipped.is_empty()
            && !self.external
            && self.progress.is_none()
            && self.key_output().is_none()
            && self.exchange.is_empty()
    }
    /// What its state means, in a sentence (the status table's): Overview's when it has nothing
    /// else to say.
    pub fn state_help(&self) -> &'static str {
        self.shown().spec().help
    }
    /// The outputs drawn as fields, each with the name of an earlier one whose value it repeats
    /// ("same as summary"): a long value is said once.
    pub fn output_rows(&self) -> Vec<(&FieldView, &str)> {
        let set = self.outputs_set();
        set.iter()
            .enumerate()
            .map(|(i, f)| {
                let same = (matches!(f.kind.as_str(), "text" | "json" | "file")
                    && f.value.chars().count() >= 24)
                    .then(|| {
                        set[..i]
                            .iter()
                            .find(|e| e.kind == f.kind && e.value == f.value)
                            .map(|e| e.name.as_str())
                    })
                    .flatten()
                    .unwrap_or("");
                (*f, same)
            })
            .collect()
    }
    /// The step as the drawer draws it: its id a second-level heading under the page's; `tab`
    /// the tab it opens on.
    pub fn body(&self, tab: &str) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&StepTemplate {
            step: self,
            page: false,
            tab: self.tab_on(tab),
        })
    }
    /// The step as its own page draws it (and that page's stream): its id the page's heading.
    pub fn own_body(&self, tab: &str) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&StepTemplate {
            step: self,
            page: true,
            tab: self.tab_on(tab),
        })
    }
    /// Its own page: a way back to the plan (and its unit), then the step.
    /// `unit`: its unit's id and title ("" when it has none), named in the way back.
    pub fn page_body(
        &self,
        project: &str,
        unit: Option<(&str, &str)>,
        tab: &str,
    ) -> Result<TrustedHtml, askama::Error> {
        #[derive(Template)]
        #[template(
            source = "<nav class=\"crumbs\" aria-label=\"Breadcrumb\"><a href=\"/projects/id/{{ step.project }}\">{{ crate::views::icons::icon(crate::views::icons::Icon::ArrowLeft, 16, \"\")|safe }}{{ project }} plan</a>{% if let Some(unit) = unit %}<span aria-hidden=\"true\">/</span><a href=\"/projects/id/{{ step.project }}/units/{{ unit.0 }}\"{% if !unit.1.is_empty() %} title=\"{{ unit.1 }}\"{% endif %}>{% if unit.1.is_empty() %}unit {{ unit.0 }}{% else %}{{ crate::views::ui::cut(unit.1, 64) }}{% endif %}</a>{% endif %}</nav>{{ body|safe }}",
            ext = "html"
        )]
        struct Page<'a> {
            step: &'a StepView,
            project: &'a str,
            unit: Option<(&'a str, &'a str)>,
            body: TrustedHtml,
        }
        TrustedHtml::from_template(&Page {
            step: self,
            project,
            unit,
            body: self.own_body(tab)?,
        })
    }
}
#[derive(Template)]
#[template(path = "step.html")]
struct StepTemplate<'a> {
    step: &'a StepView,
    /// On its own page: its id is the page's `h1`.
    page: bool,
    /// The tab it opens on.
    tab: &'static str,
}
impl StepTemplate<'_> {
    /// What the inputs' values are, said once under their head: their run's, or defaults.
    fn inputs_note(&self) -> &'static str {
        let sources: Vec<&str> = self.step.inputs.iter().map(|f| f.source.as_str()).collect();
        if self.step.inputs.iter().any(|f| f.received) {
            "As its last run received them."
        } else if sources.contains(&"Its default") {
            "A value not otherwise set is its default."
        } else {
            ""
        }
    }
    /// A section's heading: under the page's `h1` an `h2`, in the drawer (its id an `h2`) an
    /// `h3`, so no level is skipped.
    fn level(&self) -> u8 {
        if self.page { 2 } else { 3 }
    }
    /// The Runs section's fold of the timeline: the step's own runs, or its unit's.
    fn timeline_words(&self) -> &'static str {
        match &self.step.timeline {
            Some(t) if t.rows.len() > 1 => "Its unit's timeline",
            _ => "Timeline",
        }
    }
    /// The first heading level inside a value: one under its section's head.
    fn value_top(&self) -> u8 {
        if self.page { 3 } else { 4 }
    }
    /// A section's heading inside a tab's panel: one under the panel's.
    fn sub(&self) -> u8 {
        self.level() + 1
    }
}
/// Its progress (`step_progress`) while the outputs have not superseded it, each field typed
/// as its output.
pub fn load_progress(
    c: &rusqlite::Connection,
    project: ProjectId,
    step: &mut StepView,
) -> sluice_store::Result<()> {
    step.progress = sluice_store::attempts::read_progress(c, project, step.id.as_str())?
        .filter(|p| !p.superseded)
        .map(|p| ProgressView {
            fields: p
                .outputs
                .iter()
                .map(|(n, v)| {
                    let output = step.outputs.iter().find(|f| &f.name == n);
                    FieldView::new(
                        n,
                        output.map(|f| f.ty.as_str()).unwrap_or(""),
                        output.map(|f| f.doc.as_str()).unwrap_or(""),
                        Some(v),
                        "",
                    )
                })
                .collect(),
            at: p.at,
            live: p.live,
        });
    Ok(())
}
/// Run history is current-generation only; a reused step id never inherits an
/// old declaration's runs. Frozen attempted inputs come from durable results.
/// Each step's latest failure as its project's log records it: the seq of its latest
/// `step.status` record to failed, by step id.
pub fn failure_records(
    c: &rusqlite::Connection,
    project: ProjectId,
) -> sluice_store::Result<std::collections::BTreeMap<String, i64>> {
    let mut q = c.prepare_cached("SELECT step_id,max(seq) FROM records WHERE project_id=?1 AND kind='step.status' AND step_id IS NOT NULL AND json_extract(payload,'$.to')='failed' GROUP BY step_id")?;
    let rows = q.query_map([project.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}
/// A failure's own record on its project's log: the step's records up to it, it first and
/// marked (`#r<seq>`, the log's `:target`).
pub fn failure_log_href(project: &ProjectId, step: &str, seq: i64) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("step", step)
        .append_pair("before", &(seq + 1).to_string())
        .finish();
    format!("/projects/id/{project}/log?{query}#r{seq}")
}
/// Its conversation, the Thread tab's (its latest 60 messages and the replies to them), and
/// what Overview quotes of it: while it runs its own latest message and the latest to it since,
/// once it failed its own latest message.
fn load_conversation(
    c: &rusqlite::Connection,
    project: ProjectId,
    step: &mut StepView,
) -> sluice_store::Result<()> {
    use super::threads::{Build, Composer, Conversation};
    let messages = sluice_store::messages::conversation(c, project, step.id.as_str())?;
    if messages.is_empty() {
        step.thread = Conversation::default();
        step.exchange = Conversation::default();
        return Ok(());
    }
    let steps = super::threads::step_names(c, project)?;
    let name: String = c
        .query_row(
            "SELECT name FROM projects WHERE project_id=?1",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let items = messages
        .into_iter()
        .map(|m| super::threads::item(c, project, &name, &steps, m))
        .collect::<Result<Vec<_>, _>>()?;
    let unread = super::threads::unread_ids(c, project, &items)?;
    let id = step.id.to_string();
    let own = items.iter().rposition(|m| m.message.from == id);
    let mut quoted = vec![];
    {
        if let Some(own) = own {
            quoted.push(items[own].clone());
        }
        if step.running()
            && let Some(inbound) = items[own.map_or(0, |i| i + 1)..]
                .iter()
                .rev()
                .find(|m| m.message.from != id)
        {
            quoted.push(inbound.clone());
        }
    }
    let here = |m: &super::threads::MessageItem| format!("#message-{}", m.id());
    step.exchange = Conversation::build(
        Build {
            project,
            subject: Some(&id),
            steps: &steps,
            unread: &Default::default(),
            most: None,
            excerpt: true,
            href: &here,
        },
        quoted,
    );
    let whole = super::threads::thread_url(project, &format!("step-{id}"));
    step.thread = Conversation::build(
        Build {
            project,
            subject: Some(&id),
            steps: &steps,
            unread: &unread,
            most: Some(60),
            excerpt: false,
            href: &here,
        },
        items,
    )
    .with_head(whole)
    .with_composer(Some(Composer {
        project,
        to: id.clone(),
        label: format!("Message to {}", step.name().text(48)),
    }));
    Ok(())
}
pub fn load_detail(
    c: &rusqlite::Connection,
    project: ProjectId,
    step: &mut StepView,
) -> sluice_store::Result<()> {
    let (manual,total,done,revision): (bool,Option<i64>,i64,i64) = c.prepare_cached("SELECT manual,total,done,(SELECT rev FROM plans WHERE project_id=?1) FROM steps WHERE project_id=?1 AND step_id=?2")?.query_row((project.to_string(),step.id.as_str()), |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    step.manual = manual;
    step.total = total.map(|n| n as usize);
    step.done = done as usize;
    step.revision = revision as u64;
    // when its current result was recorded: what a finished step with no run kept says
    step.result_at = c
        .prepare_cached("SELECT r.recorded_at FROM steps s JOIN step_results r USING(result_id) WHERE s.project_id=?1 AND s.step_id=?2")?
        .query_row((project.to_string(), step.id.as_str()), |r| r.get::<_, String>(0))
        .optional()?
        .unwrap_or_default();
    let home = c
        .path()
        .and_then(|p| std::path::Path::new(p).parent())
        .map(std::path::Path::to_path_buf);
    let mut q = c.prepare_cached("SELECT r.run_id,coalesce(r.started_at,r.created_at),coalesce(r.finished_at,''),coalesce(r.result,''),coalesce(s.session_id,''),coalesce(s.engine,''),a.request,(julianday(coalesce(r.finished_at,'now'))-julianday(coalesce(r.started_at,r.created_at)))*86400.0 FROM runs r JOIN attempts a USING(attempt_id) LEFT JOIN sessions s USING(run_id) JOIN steps st ON st.project_id=r.project_id AND st.step_id=r.step_id AND st.generation=r.generation WHERE r.project_id=?1 AND r.step_id=?2 ORDER BY r.created_at,r.run_id")?;
    let mut rows = q.query((project.to_string(), step.id.as_str()))?;
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let request: String = r.get(6)?;
        let request: serde_json::Value = serde_json::from_str(&request)?;
        let inputs = request
            .get("inputs")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .map(|(n, v)| {
                        FieldView::new(
                            n,
                            step.inputs
                                .iter()
                                .find(|f| &f.name == n)
                                .map(|f| f.ty.as_str())
                                .unwrap_or(""),
                            "",
                            Some(v),
                            "As its run received it",
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let id: RunId = id
            .parse()
            .map_err(|e| sluice_store::StoreError::InvalidDatabase(format!("{e}")))?;
        let finished: String = r.get(2)?;
        let seconds: Option<f64> = r.get(7)?;
        let result: String = r.get(3)?;
        let result: serde_json::Value = serde_json::from_str(&result).unwrap_or_default();
        let failure = result
            .get("error")
            .filter(|e| !e.is_null())
            .map(|e| super::failure::Failure::parse(&e.to_string(), seconds));
        let outcome = match (
            finished.is_empty(),
            result.get("status").and_then(|s| s.as_str()),
        ) {
            (true, _) => Outcome::Running,
            (false, None) => Outcome::Unrecorded,
            (false, Some(word)) => match word.parse::<StepStatus>() {
                Ok(status) => Outcome::Ended(sluice_model::shown::classify(&Facts {
                    cancelled: failure.as_ref().is_some_and(|f| f.cancelled),
                    ..Facts::of(status)
                })),
                Err(_) => Outcome::Unknown(word.to_owned()),
            },
        };
        let files = match &home {
            Some(home) => RUN_FILES
                .iter()
                .copied()
                .filter(|name| run_file(home, &id, name).is_some())
                .collect(),
            None => vec![],
        };
        step.runs.push(RunView {
            id,
            started: r.get(1)?,
            finished,
            session: r.get(4)?,
            engine: r.get(5)?,
            inputs,
            outcome,
            failure,
            outputs: result
                .get("outputs")
                .and_then(|o| o.as_object())
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default(),
            action: result
                .pointer("/action/outcome")
                .and_then(|o| o.as_str())
                .unwrap_or_default()
                .into(),
            files,
            seconds,
            profile: String::new(),
        });
    }
    // the values its last run received, each still saying where it came from
    if let Some(last) = step.runs.last() {
        for frozen in &last.inputs {
            if let Some(field) = step.inputs.iter_mut().find(|f| f.name == frozen.name) {
                let source = std::mem::take(&mut field.source);
                *field = FieldView {
                    source,
                    received: true,
                    ..frozen.clone()
                };
            }
        }
    }
    load_progress(c, project, step)?;
    // who paused it: the latest plan edit that set its pause
    if let Some(hold) = step.hold.as_mut().filter(|h| !h.project)
        && let Some((by, at)) = c
            .prepare_cached("SELECT coalesce(json_extract(payload,'$.author'),''),at FROM records WHERE project_id=?1 AND kind='plan.edit' AND instr(payload,?2)>0 ORDER BY seq DESC LIMIT 1")?
            .query_row(
                (project.to_string(), format!("\"/steps/{}/paused\"", step.id)),
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?
    {
        hold.by = by;
        hold.at = at;
    }
    // a failed step's failure as the log records it, which "Why it failed" links
    step.failure_record = if step.failed() {
        c.prepare_cached("SELECT max(seq) FROM records WHERE project_id=?1 AND kind='step.status' AND step_id=?2 AND json_extract(payload,'$.to')='failed'")?
            .query_row((project.to_string(), step.id.as_str()), |r| r.get(0))?
    } else {
        None
    };
    load_conversation(c, project, step)?;
    (step.messages,step.awaiting) = c.prepare_cached("SELECT count(*),coalesce(sum(needs_reply=1 AND resolved_by IS NULL AND closed_at IS NULL),0) FROM messages WHERE project_id=?1 AND thread=?2")?.query_row((project.to_string(),format!("step-{}",step.id)), |r| Ok((r.get::<_, i64>(0)? as usize,r.get::<_, i64>(1)? as usize)))?;
    step.mark_setters();
    let mut q = c.prepare_cached("SELECT sub.outputs FROM submissions sub JOIN runs r USING(run_id) JOIN steps st ON st.project_id=r.project_id AND st.step_id=r.step_id AND st.generation=r.generation WHERE r.project_id=?1 AND r.step_id=?2 AND r.finished_at IS NULL ORDER BY r.created_at DESC LIMIT 1")?;
    let submitted: Option<String> = q
        .query_row((project.to_string(), step.id.as_str()), |r| r.get(0))
        .optional()?;
    if let Some(submitted) = submitted {
        let outputs: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&submitted)?;
        for (n, v) in outputs {
            if let Some(field) = step.outputs.iter_mut().find(|f| f.name == n) {
                let source = field.clone();
                *field = FieldView::new(&n, &source.ty, &source.doc, Some(&v), "Submitted so far");
            } else {
                step.outputs
                    .push(FieldView::new(&n, "", "", Some(&v), "Submitted so far"));
            }
        }
    }
    Ok(())
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Pause,
    Unpause,
    Retry,
    Cancel,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OwnerCommand {
    pub project: ProjectId,
    pub step: Option<StepId>,
    pub action: Action,
    pub revision: u64,
    pub message: String,
    pub author: &'static str,
}
pub trait CommandService: Send + Sync {
    fn execute(
        &self,
        command: OwnerCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>>;
}
#[derive(Clone)]
pub struct Commands(pub Arc<dyn CommandService>);
#[derive(Deserialize)]
pub struct ActionForm {
    pub action: Action,
    pub revision: u64,
    #[serde(default)]
    pub message: String,
}
pub async fn action(
    State(state): State<DashboardState>,
    registry: Option<Extension<board::Registry>>,
    commands: Option<Extension<Commands>>,
    Path((project, id)): Path<(ProjectId, StepId)>,
    Form(form): Form<ActionForm>,
) -> Response {
    execute_action(state, commands, registry, project, id, form).await
}
async fn execute_action(
    state: DashboardState,
    commands: Option<Extension<Commands>>,
    registry: Option<Extension<board::Registry>>,
    project: ProjectId,
    id: StepId,
    form: ActionForm,
) -> Response {
    let Some(Extension(commands)) = commands else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Command service unavailable",
        )
            .into_response();
    };
    let view = match board::snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await {
        Ok((_, view)) => view,
        Err(e) => return board::error_response(e),
    };
    if view.revision != form.revision {
        return (
            StatusCode::CONFLICT,
            "The plan changed. Reload before acting.",
        )
            .into_response();
    }
    if form.message.len() > 16_384 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let Some(step) = view
        .units
        .iter()
        .flat_map(|u| &u.steps)
        .find(|s| s.id == id)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let valid = match form.action {
        Action::Retry => step.retryable(),
        Action::Cancel => step.cancellable(),
        Action::Pause => step.pausable(),
        Action::Unpause => step.paused,
    };
    if !valid {
        return (
            StatusCode::BAD_REQUEST,
            "Action does not apply to this state",
        )
            .into_response();
    }
    let next = format!("{}/steps/{id}", view.href());
    match commands
        .0
        .execute(OwnerCommand {
            project,
            step: Some(id),
            action: form.action,
            revision: form.revision,
            message: form.message,
            author: "owner",
        })
        .await
    {
        Ok(()) => Redirect::to(&next).into_response(),
        Err(e) => board::error_response(e),
    }
}
/// `?activity=all` draws every turn and call of its activity outline, not only the latest;
/// `?tab=` the tab its page opens on (Overview when it has no such tab).
#[derive(Deserialize, Default)]
pub struct ActivityQuery {
    #[serde(default)]
    activity: String,
    #[serde(default)]
    tab: String,
}
impl ActivityQuery {
    fn all(&self) -> bool {
        self.activity == "all"
    }
}
pub async fn step_page(
    State(state): State<DashboardState>,
    registry: Option<Extension<board::Registry>>,
    Path((project, id)): Path<(ProjectId, StepId)>,
    Query(shown): Query<ActivityQuery>,
    headers: HeaderMap,
) -> Response {
    let page = async {
        let (shared, detail) =
            board::step_detail(&state, project, registry.as_ref().map(|r| &r.0), &id, true).await?;
        let (Some(shared), Some(mut detail)) = (shared, detail) else {
            return Err(PublicError::NotFound {
                message: "step not found".into(),
            });
        };
        super::activity::attach(&state, &mut detail.step, shown.all()).await;
        let nav = NavView::new(&shared, Some(project), "plan")?;
        let step = &detail.step;
        let unit = detail
            .unit
            .as_ref()
            .map(|(id, title)| (id.as_str(), title.as_str()));
        // the version its stream's first batch has when it draws the same: nothing to patch
        let drawn = step
            .own_body(&shown.tab)
            .map(|body| RenderedBatch::new(vec![PatchRegion::new("step-detail", body)]).version)
            .map_err(render_error)?;
        step.page_body(&detail.project, unit, &shown.tab)
            .and_then(|body| {
                super::render_layout(
                    &format!("{} · {}", step.tab_title(), detail.project),
                    &body,
                    &nav,
                    &Viewer::from_headers(&headers),
                    &format!(
                        "{}/stream?page=true{}",
                        step.href(),
                        if shown.all() { "&activity=all" } else { "" }
                    ),
                    &drawn,
                    &step.href(),
                )
            })
            .map_err(render_error)
    };
    match page.await {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => board::error_response(e),
    }
}
#[derive(Deserialize)]
pub struct StepStreamQuery {
    /// The step's own page streams it, its id the page's heading; the drawer does not.
    #[serde(default)]
    page: bool,
}
/// A step no longer in the plan, where its detail was: it may have retired with its unit
/// (SPEC §6.11) or been edited out. Its log still has it.
fn gone(project: ProjectId, id: &StepId, page: bool) -> Result<TrustedHtml, askama::Error> {
    #[derive(Template)]
    #[template(
        source = "<article id=\"step-detail\" data-step=\"{{ id }}\" data-gone><header class=\"d-head\"><div class=\"hd\">{% if page %}<h1 id=\"d-title\">{{ id }}</h1>{% else %}<h2 id=\"d-title\">{{ id }}</h2>{% endif %}</div></header><p class=\"d-gone\">This step left the plan; it may have retired. <a href=\"/projects/id/{{ project }}/log?step={{ id }}\">Search the log for it</a></p></article>",
        ext = "html"
    )]
    struct Gone<'a> {
        project: ProjectId,
        id: &'a StepId,
        page: bool,
    }
    TrustedHtml::from_template(&Gone { project, id, page })
}
pub async fn step_stream(
    State(state): State<DashboardState>,
    registry: Option<Extension<board::Registry>>,
    Path((project, id)): Path<(ProjectId, StepId)>,
    Query(query): Query<StreamQuery>,
    Query(own): Query<StepStreamQuery>,
    Query(shown): Query<ActivityQuery>,
) -> Response {
    let all = shown.all();
    // the tab the page holds as it connects: its first batch draws that one chosen
    let tab = query.signal("tab");
    let stop = state.stop.clone();
    // its own page's stream is the page's (`ver`, and its stale says so in the page's banner);
    // the drawer's has its own (`sver`), so a drawer never says the board's updates paused
    let signal = if own.page {
        VersionSignal::Page
    } else {
        VersionSignal::Step
    };
    let version = query.version(signal);
    // a run going grows its transcript with no commit: its activity redraws every second
    let watch = state.watch_step(Some(project), Some(id.clone()));
    // a step that left the plan is drawn as gone, not an error: the stream stays open and
    // quiet (its drawer never reconnects for it) and draws the step again should it come back
    let loader = move || {
        let state = state.clone();
        let id = id.clone();
        let registry = registry.clone();
        let tab = tab.clone();
        async move {
            let (_, detail) =
                board::step_detail(&state, project, registry.as_ref().map(|r| &r.0), &id, false)
                    .await?;
            let Some(detail) = detail else {
                return Ok(RenderedBatch {
                    version: format!("gone:{id}"),
                    regions: vec![PatchRegion::new(
                        "step-detail",
                        gone(project, &id, own.page).map_err(render_error)?,
                    )],
                });
            };
            let mut step = detail.step;
            super::activity::attach(&state, &mut step, all).await;
            Ok(RenderedBatch::new(vec![PatchRegion::new(
                "step-detail",
                if own.page {
                    step.own_body(&tab)
                } else {
                    step.body(&tab)
                }
                .map_err(render_error)?,
            )]))
        }
    };
    Sse::new(streams::page_events(watch, loader, version, signal, stop))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}
/// One of a run's files (`RUN_FILES`), read-only, as plain text: only a run of this project,
/// only from its own directory, never through a link; a large file's last 2 MiB.
pub async fn run_file_page(
    State(state): State<DashboardState>,
    Path((project, run, name)): Path<(ProjectId, String, String)>,
) -> Response {
    const MOST: u64 = 2 * 1024 * 1024;
    // an address with no run id in it names no run: the calm 404, never the parser's words
    let Ok(run) = run.parse::<RunId>() else {
        return board::error_response(PublicError::NotFound {
            message: format!("No run is named {run}."),
        });
    };
    let home = state.reads.home().to_owned();
    let owned = state
        .reads
        .snapshot(move |c| {
            Ok(c.query_row(
                "SELECT 1 FROM runs WHERE project_id=?1 AND run_id=?2",
                (project.to_string(), run.to_string()),
                |_| Ok(()),
            )
            .optional()?
            .is_some())
        })
        .await;
    match owned {
        Ok(true) => {}
        Ok(false) => {
            return board::error_response(PublicError::NotFound {
                message: format!("Run {run} is not a run of this project."),
            });
        }
        Err(e) => return board::error_response(e.into_public(true)),
    }
    let (asked, wanted) = (run, name.clone());
    let read = tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let path = run_file(&home, &asked, &wanted)?;
        let mut file = std::fs::File::open(path).ok()?;
        let size = file.metadata().ok()?.len();
        let mut bytes = Vec::new();
        if size > MOST {
            file.seek(SeekFrom::Start(size - MOST)).ok()?;
            bytes
                .extend_from_slice(b"[the file's first part is left out: its last 2 MiB follow]\n");
        }
        file.take(MOST).read_to_end(&mut bytes).ok()?;
        Some(bytes)
    })
    .await
    .ok()
    .flatten();
    match read {
        Some(bytes) => (
            [
                (
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                ),
                (axum::http::header::CACHE_CONTROL, "no-store"),
                (
                    axum::http::header::CONTENT_SECURITY_POLICY,
                    "default-src 'none'; sandbox",
                ),
            ],
            if TRANSCRIPTS.contains(&name.as_str()) {
                sluice_agents::engines::account::redact(&String::from_utf8_lossy(&bytes))
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            },
        )
            .into_response(),
        None => board::error_response(PublicError::NotFound {
            message: format!(
                "Run {run} has no {name}{}.",
                if RUN_FILES.contains(&name.as_str()) {
                    ": it wrote none, or it was cleaned up with the run"
                } else {
                    "; a run's files are pane-at-failure.txt, stderr.log, stderr-tail.log, summary.txt, codex.log, codex-wire.jsonl, devin-hooks.jsonl, devin.log and devin.json"
                }
            ),
        }),
    }
}
