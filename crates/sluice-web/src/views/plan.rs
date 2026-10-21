//! A project's plan (DESIGN.md, Pages: a project's plan): a briefing, then a roster. The head
//! says in one sentence what needs the owner and how the work stands; **Needs you**, the one
//! raised well, says each question, stop and silence in a sentence with one action and a quick
//! dismissal; **Finished since you looked** lists what finished since this browser last looked
//! with what it produced; **At work** is a roster of the live units, a row each, grouped by
//! recipe and ordered by creation by default, with a pulse strip of its last
//! hour (a mark each minute its agent acted, a dotted tail since). A row opens in place to its steps (`unit_steps`).
//!
//! Generic by rule: every word comes from the status table, the recipe, the failure's own
//! reading or what the project's steps said and reported; the badge from what a step runs on.
use super::board::{ProjectView, UnitView};
use super::icons::{Icon, icon};
use super::step::{self, Action, ActionForm, OwnerCommand, StepView};
use super::ui::{self, Mark, Shown, esc};
use super::{DashboardSnapshot, DashboardState, Frame, NavView, TrustedHtml, Viewer, write};
use crate::clock::ClockRead;
use crate::streams::{PatchRegion, RenderedBatch};
use askama::Template;
use axum::{
    Extension,
    extract::{Form, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sluice_model::{
    commands::NeedsItem,
    error::PublicError,
    ids::{ProjectId, StepId},
    shown::Band,
};
use std::collections::BTreeMap;
use std::fmt::Write;

/// A run of no recipe, alone in its unit, that has run this long (seconds) is a long run: the
/// roster lists it with the long runs, last.
pub const LONG_RUN: f64 = 6.0 * 3600.0;
/// A unit heard from within this long (seconds) wears the live dot.
const LIVE: f64 = 300.0;
/// A roster of more than this many rows folds each group's unremarkable rows to one line...
const FOLD_ROSTER: usize = 12;
/// ...when there are more than this many of them.
const FOLD_GROUP: usize = 3;
/// A unit waiting this long (seconds) since its last run ended is worth a look in a big roster.
const WAIT_LONG: f64 = 3600.0;
/// How long since a unit was heard from (seconds) before its "heard" line takes the ink, then
/// its weight: silence shows before the quiet pill says it.
const HEARD_AGING: f64 = 15.0 * 60.0;
const HEARD_OLD: f64 = 30.0 * 60.0;
/// How long (minutes) an item the owner set aside keeps its Set aside line, with its Undo.
const UNDO_MINUTES: f64 = 10.0;
/// At most this many finished units are drawn in Finished since you looked.
const FINISHED_MOST: usize = 8;
/// The cookie that keeps how far this browser has looked at a project's finished units.
pub fn seen_cookie(project: ProjectId) -> String {
    format!("sluice_seen_{project}")
}
/// What a request's cookie says it has seen of a project: the finish time (seconds since the
/// epoch) of the last finished unit it marked seen.
pub fn seen_of(headers: &HeaderMap, project: ProjectId) -> Option<u64> {
    let name = seen_cookie(project);
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .unwrap_or("")
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| v.parse().ok())
        // 0 is the look undone: everything finished lately again
        .filter(|seen| *seen > 0)
}

/// The viewer's roster order, shared by page loads and stream reconnects through a cookie.
#[derive(Clone, Copy, Debug, Default)]
pub enum Order {
    #[default]
    Created,
    Plan,
    Recent,
}
impl Order {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "created" => Some(Self::Created),
            "plan" => Some(Self::Plan),
            "recent" => Some(Self::Recent),
            _ => None,
        }
    }
}
pub fn order_cookie(project: ProjectId) -> String {
    format!("sluice_order_{project}")
}
pub fn order_of(headers: &HeaderMap, project: ProjectId) -> Order {
    let name = order_cookie(project);
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .unwrap_or("")
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| Order::parse(v))
        .unwrap_or_default()
}

fn now() -> f64 {
    crate::clock::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}
fn epoch(at: &str) -> Option<f64> {
    super::timestamp(at).map(|t| t as f64)
}
/// The step that says how its unit stands: the first whose state is the unit's.
fn current(unit: &UnitView) -> Option<&StepView> {
    let shown = unit.shown();
    unit.steps
        .iter()
        .find(|s| s.standing() == shown)
        .or_else(|| unit.steps.first())
}
/// Where a unit's name leads: a loose step's own page, else the unit's.
fn unit_href(view: &ProjectView, unit: &UnitView) -> String {
    match unit.steps.as_slice() {
        [only] if !unit.tagged => only.href(),
        _ => unit.href(&view.project.id),
    }
}
/// A unit's title, or its id when it has none.
fn title(unit: &UnitView) -> &str {
    unit.heading()
}
/// How long a unit's work took, first start to last end.
fn took(unit: &UnitView) -> f64 {
    let mut from = f64::MAX;
    let mut to = 0.0f64;
    for t in unit.steps.iter().filter_map(|s| s.shown_timing()) {
        if let Some(start) = epoch(&t.started) {
            from = from.min(start);
        }
        if let Some(end) = t.finished.as_deref().and_then(epoch) {
            to = to.max(end);
        }
    }
    if to >= from { to - from } else { 0.0 }
}
/// A project's description in its head's Details, as markdown, "" when it has none.
fn about_html(description: &str) -> TrustedHtml {
    if description.is_empty() {
        return TrustedHtml::default();
    }
    TrustedHtml::owned(format!(
        "<div class=\"md dm-md\">{}</div>",
        crate::markdown::render(description).as_str()
    ))
}
/// "Two", "Three", … "Ten", then the figure: a count leading a sentence.
fn leading(n: usize) -> String {
    const WORDS: [&str; 9] = [
        "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten",
    ];
    match n {
        2..=10 => WORDS[n - 2].to_owned(),
        n => n.to_string(),
    }
}
/// Why a group of failures that share a kind failed, after "Three units failed: ": the kinds
/// that say the same of every unit they stop. Any other kind is said a unit at a time.
fn shared_cause(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "WallCap" => "they hit their wall-clock cap", // Historical failures.
        "OperationTimeout" => "an engine or host operation timed out",
        "StallCap" => "their agents wrote nothing for too long",
        "QuotaExhausted" => "their engines hit a usage cap",
        "AuthFailed" => "their engines could not sign in",
        "EngineExited" => "their engines exited",
        "ReadyTimeout" => "their engines never became ready",
        "ExitedWithoutSubmit" | "exited_without_submit" => {
            "they exited without submitting their outputs"
        }
        "process_lost" => "their processes were lost",
        "Transient" | "transient" => "a passing failure outlasted their retries",
        _ => return None,
    })
}

/// One item of Needs you, a hairline row: its mark, its title and how it stands, who and when,
/// what it says under them, its in-place form, its actions and what its × sets aside.
struct Need {
    mark: Mark,
    /// The unit it is about ("" for a group, whose units are listed under it).
    unit: String,
    /// Its element's id.
    id: String,
    /// What its × sets aside, as the form posts them: `stop:<step>` (a failure, a cancel or a
    /// stale input, kept against the step's generations), `quiet:<step>@<stamp>`,
    /// `ask:<message>`.
    keys: Vec<String>,
    /// Its title, HTML: the way to what it is about (`.nd-t`).
    title: String,
    /// How it stands after its title, HTML ("asks you", "failed at land after 12m").
    state: String,
    /// Who and when, faint after that, HTML ("@lead · 4m ago"); "" for none.
    who: String,
    /// What it says under its line, HTML: a question's words, a failure's reason and what to
    /// try next; "" for none.
    body: String,
    /// Its body is a question, said by whoever asked it (`who_id`, its avatar beside it).
    question: Option<String>,
    /// Its form in place under it, HTML: a question's answer, a failure's feedback; "".
    form: String,
    /// A group's list of its units, HTML ("" for one).
    more: String,
    /// Its actions, HTML.
    act: String,
    /// What its Set aside line calls it, in words.
    short: String,
    /// The owner has set it aside, and it has not changed since: when (its Undo is offered for
    /// a while).
    aside: Option<String>,
}
/// One row of At work.
struct Row<'a> {
    unit: &'a UnitView,
    /// When it was last heard from (seconds since the epoch), when it runs.
    heard: Option<f64>,
    waiting: bool,
}
/// A group of rows: a recipe's, or the units of no recipe.
struct Group<'a> {
    name: String,
    recipe: String,
    rows: Vec<Row<'a>>,
}

/// The plan as drawn.
pub struct Plan<'a> {
    view: &'a ProjectView,
    now: f64,
    needs: Vec<Need>,
    groups: Vec<Group<'a>>,
    /// The long runs, watched apart from At work: each a running loose step that has run for
    /// hours or publishes its results as it goes.
    watching: Vec<&'a UnitView>,
    /// Live units that have not started and wait on earlier work.
    later: usize,
    /// Units at work (running), the long runs not counted.
    at_work: usize,
    /// Of them, quiet.
    quiet: usize,
    /// The done units, the latest finished first.
    done: Vec<&'a UnitView>,
    /// Finished since this browser looked (or lately, with no look kept), newest first, and how
    /// many more there are.
    finished: Vec<&'a UnitView>,
    finished_more: usize,
}

/// A long run: a running step of no recipe, alone in its unit, that has run `LONG_RUN` or
/// publishes its results as it goes (`step_progress`). The plan watches it apart from At work.
fn is_long(unit: &UnitView) -> bool {
    match unit.steps.as_slice() {
        [step] if unit.recipe.is_empty() && step.running() => {
            step.progress
                .as_ref()
                .is_some_and(|p| p.live && !p.fields.is_empty())
                || step
                    .timing
                    .as_ref()
                    .is_some_and(|t| t.finished.is_none() && t.seconds >= LONG_RUN)
        }
        _ => false,
    }
}
/// When a long run last showed it was alive: its last published result or its last write.
fn long_heard(unit: &UnitView) -> Option<f64> {
    let step = unit.steps.first()?;
    let published = step.progress.as_ref().and_then(|p| epoch(&p.at));
    let wrote = epoch(&step.active_at);
    let began = step.timing.as_ref().and_then(|t| epoch(&t.started));
    published
        .into_iter()
        .chain(wrote)
        .chain(began)
        .reduce(f64::max)
}

impl<'a> Plan<'a> {
    pub fn new(view: &'a ProjectView) -> Self {
        let now = now();
        let live = |unit: &&UnitView| !unit.done;
        let mut done: Vec<&UnitView> = view.units.iter().filter(|u| u.done).collect();
        done.sort_by(|a, b| b.finished().cmp(a.finished()).then(a.pos.cmp(&b.pos)));
        // what finished since this browser looked: after its mark, else the last few
        let mut finished: Vec<&UnitView> = done
            .iter()
            .copied()
            .filter(|u| !u.finished().is_empty())
            .filter(|u| match view.seen {
                Some(seen) => epoch(u.finished()).is_some_and(|t| t > seen as f64),
                None => true,
            })
            .collect();
        let cap = if view.seen.is_some() {
            FINISHED_MOST
        } else {
            ui::LATEST
        };
        let finished_more = if view.seen.is_some() {
            finished.len().saturating_sub(cap)
        } else {
            0
        };
        finished.truncate(cap);

        // a find, a chain or a recipe's units asked for: each one a row, none only counted
        let narrowed = !view.q.is_empty() || !view.root.is_empty() || !view.recipe.is_empty();
        let mut groups: Vec<Group> = vec![];
        let mut watching: Vec<&UnitView> = vec![];
        let (mut later, mut at_work, mut quiet) = (0, 0, 0);
        for unit in view.units.iter().filter(live) {
            let shown = unit.shown();
            let band = shown.spec().band;
            if matches!(shown, Shown::Failed | Shown::Cancelled | Shown::Stale) {
                continue;
            }
            // a long run is watched, not at work: Needs you takes it only on a failure, a
            // question or a silence past the host's threshold
            if is_long(unit) {
                watching.push(unit);
                continue;
            }
            let row = if band == Band::Running {
                at_work += 1;
                if unit.steps.iter().any(|s| s.shown() == Shown::Quiet) {
                    quiet += 1;
                }
                Row {
                    unit,
                    heard: heard(unit),
                    waiting: false,
                }
            } else {
                // never begun and blocked on earlier work: counted; begun (a step ran or is
                // settled), ready, or held in a way of its own: a row
                let begun = unit
                    .steps
                    .iter()
                    .any(|s| s.timing.is_some() || s.standing().spec().band == Band::Done);
                let blocked = current(unit)
                    .is_some_and(|s| matches!(s.shown(), Shown::Pending | Shown::Blocked))
                    && !unit.steps.iter().any(|s| s.ready());
                if !begun && blocked && !narrowed {
                    later += 1;
                    continue;
                }
                Row {
                    unit,
                    heard: None,
                    waiting: true,
                }
            };
            match groups.iter_mut().find(|g| g.recipe == unit.recipe) {
                Some(group) => group.rows.push(row),
                None => groups.push(Group {
                    name: if unit.recipe.is_empty() {
                        "Other units".into()
                    } else {
                        unit.recipe.clone()
                    },
                    recipe: unit.recipe.clone(),
                    rows: vec![row],
                }),
            }
        }
        // a unit of no recipe in its own group after the recipes'
        groups.sort_by_key(|g| g.recipe.is_empty());
        for group in &mut groups {
            group.rows.sort_by(|a, b| {
                match view.order {
                    Order::Created => a.unit.created.cmp(&b.unit.created),
                    Order::Plan => std::cmp::Ordering::Equal,
                    Order::Recent => heard(b.unit)
                        .unwrap_or(0.0)
                        .total_cmp(&heard(a.unit).unwrap_or(0.0)),
                }
                .then(a.unit.pos.cmp(&b.unit.pos))
            });
        }
        watching.sort_by(|a, b| a.created.cmp(&b.created).then(a.pos.cmp(&b.pos)));
        let mut plan = Plan {
            view,
            now,
            needs: vec![],
            groups,
            watching,
            later,
            at_work,
            quiet,
            done,
            finished,
            finished_more,
        };
        plan.needs = plan.needs();
        plan
    }
    fn project(&self) -> ProjectId {
        self.view.project.id
    }
    /// Whether the owner set `key` aside and it is still as it was: a stop while its step is
    /// on the generations it was set aside at (the store holds that), a silence while the run
    /// has written nothing since, a question while it is open.
    fn aside(&self, key: &str) -> Option<String> {
        let (key, stamp) = match key.split_once('@') {
            Some((key, stamp)) => (key, stamp.parse::<i64>().ok()),
            None => (key, None),
        };
        self.view
            .set_aside
            .iter()
            .find(|s| s.key == key && stamp.is_none_or(|t| t == s.stamp))
            .map(|s| s.at.clone())
    }

    // ---- Needs you -----------------------------------------------------------------------------

    /// Every item of Needs you, in order: the questions to the owner, then the stops (failures
    /// sharing a kind together), then the silences, the longest first: a unit at work gone
    /// quiet, a long run that has said nothing past the host's threshold.
    fn needs(&self) -> Vec<Need> {
        let project = self.project();
        let mut out = vec![];
        // the questions to the owner
        for ask in &self.view.project.asks {
            let unit = self
                .view
                .units
                .iter()
                .find(|u| u.steps.iter().any(|s| s.id.as_str() == ask.step));
            // a question from a step this view leaves out is the inbox's alone
            if unit.is_none() && !ask.step.is_empty() {
                continue;
            }
            let asked = self
                .view
                .questions
                .iter()
                .find(|q| q.message == ask.message);
            let opens = if ask.step.is_empty() {
                String::new()
            } else {
                format!(" data-opens=\"{}\"", esc(&ask.step))
            };
            let title = match unit {
                Some(unit) => format!(
                    "<a class=\"nd-t\" href=\"{}\"{opens}>{}</a>",
                    esc(&ask.href(&project)),
                    esc(title(unit))
                ),
                // asked by a session, not a step: its question's own title
                None => format!(
                    "<a class=\"nd-t\" href=\"{}\">{}</a>",
                    esc(&ask.href(&project)),
                    esc(&ask.title)
                ),
            };
            // who asked (a step by its id, its card on hover) and when
            let mut who = if ask.step.is_empty() {
                ask.sender_html()
            } else {
                ui::step_ref(&ask.step)
            };
            if let Some(q) = asked {
                let _ = write!(who, " · {}", ui::ago(&q.at));
            }
            let key = format!("ask:{}", ask.message);
            // a question with a form of its own is answered on its page; else here, in place
            let (act, form) = match asked {
                Some(q) if !q.form => (String::new(), self.answer_form(ask, unit)),
                _ => (
                    ui::link_with(ui::Btn::Primary, &ask.href(&project), "Answer", &opens),
                    String::new(),
                ),
            };
            out.push(Need {
                mark: Mark::Ask,
                unit: unit.map(|u| u.id.to_string()).unwrap_or_default(),
                id: format!("n-ask-{}", ask.message),
                title,
                state: "asks you".into(),
                who,
                // the question itself in a line, then its words when they say more
                body: {
                    let words = asked.map(|q| q.body.trim()).unwrap_or("");
                    let lead = crate::markdown::plain(words);
                    let mut body = String::new();
                    // a session's question is titled by its own title already
                    if unit.is_some() && !lead.trim_start().starts_with(ask.title.trim()) {
                        let _ = write!(
                            body,
                            "<p class=\"nd-qt\">{}</p>",
                            ui::refs(&esc(&ask.title))
                        );
                    }
                    if !words.is_empty() && words != ask.title.trim() {
                        let _ = write!(
                            body,
                            "<div class=\"md\">{}</div>",
                            ui::refs(crate::markdown::render(words).as_str())
                        );
                    }
                    body
                },
                question: Some(ask.sender_id().to_owned()),
                form,
                more: String::new(),
                act,
                short: format!("the question “{}”", ui::cut(&ask.title, 60)),
                aside: self.aside(&key),
                keys: vec![key],
            });
        }
        // the stops: a failure that shares its kind with another's goes with it
        let mut stops: Vec<(&UnitView, &StepView)> = self
            .view
            .units
            .iter()
            .filter(|u| {
                !u.done && matches!(u.shown(), Shown::Failed | Shown::Cancelled | Shown::Stale)
            })
            .filter_map(|u| current(u).map(|s| (u, s)))
            .collect();
        stops.sort_by_key(|(u, _)| (u.shown(), u.pos));
        let kind = |step: &StepView| {
            step.failure
                .as_ref()
                .filter(|f| !f.cancelled && step.shown() == Shown::Failed)
                .map(|f| f.kind.clone())
                .filter(|k| shared_cause(k).is_some())
        };
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for (_, step) in &stops {
            if let Some(k) = kind(step) {
                *kinds.entry(k).or_default() += 1;
            }
        }
        let mut grouped: Vec<String> = vec![];
        for (unit, step) in &stops {
            match kind(step).filter(|k| kinds.get(k).copied().unwrap_or(0) > 1) {
                Some(k) if grouped.contains(&k) => {}
                Some(k) => {
                    let members: Vec<(&UnitView, &StepView)> = stops
                        .iter()
                        .filter(|(_, s)| kind(s).as_deref() == Some(k.as_str()))
                        .copied()
                        .collect();
                    out.push(self.group_need(&k, &members));
                    grouped.push(k);
                }
                None => out.push(self.stop_need(unit, step)),
            }
        }
        // the silences, the longest first: a quiet run at work, and a long run silent past
        // the host's threshold (a long run's own quiet is its nature, not a stop)
        let mut quiet: Vec<(&UnitView, &StepView, f64, bool)> = self
            .view
            .units
            .iter()
            .filter(|u| !u.done && !self.watching.iter().any(|w| w.id == u.id))
            .flat_map(|u| {
                u.steps
                    .iter()
                    .filter(|s| s.shown() == Shown::Quiet)
                    .filter_map(move |s| epoch(&s.active_at).map(|at| (u, s, at, false)))
            })
            .collect();
        for unit in &self.watching {
            if let (Some(at), Some(step)) = (long_heard(unit), unit.steps.first())
                && self.now - at >= self.view.long_quiet
            {
                quiet.push((unit, step, at, true));
            }
        }
        quiet.sort_by(|a, b| a.2.total_cmp(&b.2));
        for (unit, step, at, long) in quiet {
            let stamp = super::rfc3339(at as u64);
            let key = format!("quiet:{}@{}", step.id, at as i64);
            out.push(Need {
                mark: Mark::State(Shown::Quiet),
                unit: unit.id.to_string(),
                id: format!("n-quiet-{}", step.id),
                title: self.step_link(unit, step),
                state: format!(
                    "{} since {}",
                    if long {
                        "has published nothing"
                    } else {
                        "has not been heard from"
                    },
                    ui::clock(&stamp)
                ),
                who: ui::ago(&stamp).0,
                body: String::new(),
                question: None,
                form: String::new(),
                more: String::new(),
                act: ui::link_with(ui::Btn::Outlined, &step.thread_href(), "Message it", ""),
                short: format!("the silence of {}", ui::cut(title(unit), 60)),
                aside: self.aside(&key),
                keys: vec![key],
            });
        }
        out
    }
    /// A question's answer, in place: a one-line field that grows when it takes the focus,
    /// the owner's avatar before it and Answer, the one primary action, after it. ⌘↵ answers
    /// and Escape lets it go (`board.js`); what it says reaches whoever asked, as a reply.
    fn answer_form(&self, ask: &super::AskView, unit: Option<&UnitView>) -> String {
        let id = format!("answer-{}", ask.message);
        let to = match unit {
            Some(unit) => ui::cut(title(unit), 40),
            None => super::ui::Party::of(ask.sender_id()).map_or_else(String::new, |p| p.words()),
        };
        format!(
            "{open}{av}<div class=\"af\"><label class=\"vh\" for=\"{id}-t\">Your answer to {label}</label><textarea id=\"{id}-t\" name=\"body\" rows=\"1\" required data-answer data-ignore-morph placeholder=\"Answer {to}…\"></textarea><div class=\"hint\" aria-hidden=\"true\"><span><kbd>⌘↵</kbd> answer</span><span><kbd>Esc</kbd> cancel</span><span class=\"where\">sent to {whom}</span></div></div><button class=\"primary\">Answer</button>{said}</form>",
            open = ui::post_form(
                &id,
                "answer-in",
                &format!(
                    "/projects/id/{}/messages/{}/reply",
                    self.project(),
                    ask.message
                )
            ),
            av = ui::avatar("owner", ui::AvSize::Md, true),
            label = esc(&ask.title),
            to = esc(&to),
            whom = if ask.step.is_empty() {
                "whoever asked".to_owned()
            } else {
                format!("its step's thread, <code>{}</code>", esc(&ask.step))
            },
            said = ui::form_said(&id),
        )
    }
    /// A unit's title linking its stopped or quiet step, which opens in the drawer.
    fn step_link(&self, unit: &UnitView, step: &StepView) -> String {
        format!(
            "<a class=\"nd-t\" href=\"{}\" data-step=\"{}\">{}</a>",
            esc(&step.href()),
            esc(step.id.as_str()),
            esc(title(unit))
        )
    }
    /// A stop: "<title> failed at review after 12m", why under it and what to try, Retry with
    /// feedback (its field in place) and Retry; a cancel's Retry and Settle as done.
    fn stop_need(&self, unit: &UnitView, step: &StepView) -> Need {
        let shown = if step.cancelled() {
            Shown::Cancelled
        } else {
            unit.shown()
        };
        // its stage, unless its name only says its state again ("failed at failed")
        let stage = match unit.step_name(step) {
            name if unit.steps.len() > 1 && !name.eq_ignore_ascii_case(shown.word()) => {
                format!(" at {}", esc(&name))
            }
            _ => String::new(),
        };
        // how long its last run took, when it ran
        let after = step
            .timing
            .as_ref()
            .filter(|t| t.finished.is_some() && t.seconds > 0.0)
            .map(|t| format!(" after {}", esc(&ui::duration_text(t.seconds))))
            .unwrap_or_default();
        let mut who = vec![];
        let owner = unit.owner_words();
        if !owner.is_empty() {
            who.push(format!("<span class=\"own\">{}</span>", esc(&owner)));
        }
        // its step's id, unless the title already is it
        if unit.whole_heading() != step.id.as_str() {
            who.push(ui::step_ref(step.id.as_str()));
        }
        if let Some(end) = step.timing.as_ref().and_then(|t| t.finished.as_deref()) {
            who.push(ui::ago(end).0);
        }
        let mut body = String::new();
        match (shown, step.failure.as_ref()) {
            (Shown::Cancelled, Some(f)) => {
                let reason = f
                    .headline
                    .strip_prefix("Cancelled: ")
                    .unwrap_or(&f.headline);
                body.push_str(&ui::refs(&esc(reason)));
                // the failure a cancel set aside stays readable
                if !f.set_aside.is_empty() {
                    let _ = write!(
                        body,
                        " <span class=\"nd-aside\">It had failed: {}</span>",
                        esc(&ui::cut(&f.set_aside, 160))
                    );
                }
            }
            (_, Some(f)) => {
                // a failure of no known kind says only that it failed: its line said that
                if f.kind != "failed" {
                    body.push_str(&ui::refs(&esc(&f.headline)));
                }
                let said = f.said.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                if !said.is_empty() {
                    let _ = write!(
                        body,
                        " It last said “{}”",
                        ui::refs(&esc(&ui::cut(said.trim(), 140)))
                    );
                }
                // what to try next, as its page says it (`failure::Advice`)
                let next = step.next_step();
                if !next.is_empty() {
                    let _ = write!(body, " <span class=\"nd-next\">{next}</span>");
                }
            }
            (Shown::Stale, None) => body.push_str("An input it read changed after it ran."),
            _ => {}
        }
        let (act, form) = if step.retryable() && !step.retry_asks() {
            let retry = self.retry_form(&[step], "Retry");
            if step.cancelled() {
                // the cancel's other decision: settle it, so its unit counts as done
                (
                    format!("{retry}{}", self.settle_form(unit, step)),
                    String::new(),
                )
            } else if shown == Shown::Failed {
                // its feedback in place; when the failure repeated it stands open at once and a
                // bare Retry is not offered: the same run again would fail the same way
                let repeated = step.feedback_in_why();
                (
                    format!(
                        "<button type=\"button\" data-fb aria-expanded=\"{repeated}\" aria-controls=\"fb-{id}\" data-preserve-attr=\"aria-expanded\">Retry with feedback</button>{bare}",
                        id = esc(step.id.as_str()),
                        bare = if repeated { "" } else { retry.as_str() },
                    ),
                    self.feedback_form(step),
                )
            } else {
                (retry, String::new())
            }
        } else {
            (
                ui::link_with(
                    ui::Btn::Outlined,
                    &step.href(),
                    "Open step",
                    &format!(" data-step=\"{}\"", esc(step.id.as_str())),
                ),
                String::new(),
            )
        };
        // set aside against its step's generations: a retry or a new cancel brings it back
        let key = format!("stop:{}", step.id);
        Need {
            mark: Mark::State(shown),
            unit: unit.id.to_string(),
            id: format!("n-stop-{}", step.id),
            title: self.step_link(unit, step),
            state: format!("{}{stage}{after}", esc(shown.word())),
            who: who.join(" · "),
            body,
            question: None,
            form,
            more: String::new(),
            act,
            short: format!(
                "the {} of {}",
                match shown {
                    Shown::Cancelled => "cancel",
                    Shown::Stale => "stale input",
                    _ => "failure",
                },
                ui::cut(title(unit), 60)
            ),
            aside: self.aside(&key),
            keys: vec![key],
        }
    }
    /// A failure's Retry with feedback, in place under it: what its next run should do
    /// differently, sent with the retry to its step's thread.
    fn feedback_form(&self, step: &StepView) -> String {
        let id = format!("fb-{}", step.id);
        format!(
            "{open}<input type=\"hidden\" name=\"action\" value=\"retry\"><input type=\"hidden\" name=\"revision\" value=\"{rev}\"><input type=\"hidden\" name=\"seen\" value=\"{seen}\">{av}<div class=\"af\"><label class=\"vh\" for=\"{id}-t\">Feedback for its next run</label><textarea id=\"{id}-t\" name=\"message\" rows=\"1\" required maxlength=\"16384\" data-answer data-ignore-morph placeholder=\"What its next run should do differently…\"></textarea><div class=\"hint\" aria-hidden=\"true\"><span><kbd>⌘↵</kbd> retry</span><span><kbd>Esc</kbd> cancel</span></div></div><button class=\"primary\">Retry</button>{said}</form>",
            open = ui::post_form(
                &id,
                &format!(
                    "answer-in fb{}",
                    if step.feedback_in_why() { " open" } else { "" }
                ),
                &format!("{}/actions", step.href())
            ),
            rev = step.revision,
            seen = esc(&step.seen()),
            av = ui::avatar("owner", ui::AvSize::Md, true),
            id = esc(&id),
            said = ui::form_said(&id),
        )
    }
    /// Failures that share a kind, said once: "Three units failed", why after it, what to try
    /// under it, the units folded under that, Retry all.
    fn group_need(&self, kind: &str, members: &[(&UnitView, &StepView)]) -> Need {
        let mut list = String::new();
        for (unit, step) in members {
            let said = if unit.last_message.trim().is_empty() {
                String::new()
            } else {
                format!(
                    " <span>· “{}”</span>",
                    esc(&crate::markdown::cut(
                        &crate::markdown::plain(&unit.last_message),
                        90
                    ))
                )
            };
            let _ = write!(
                list,
                "<li data-unit=\"{}\" data-said=\"{}\">{}{said}</li>",
                esc(unit.id.as_str()),
                esc(&said_states(unit)),
                self.step_link(unit, step)
                    .replacen(" class=\"nd-t\"", "", 1)
            );
        }
        let n = members.len();
        let keys: Vec<String> = members
            .iter()
            .map(|(_, s)| format!("stop:{}", s.id))
            .collect();
        let steps: Vec<&StepView> = members
            .iter()
            .map(|(_, s)| *s)
            .filter(|s| s.retryable() && !s.retry_asks())
            .collect();
        let act = if steps.len() == n {
            self.retry_form(&steps, "Retry all")
        } else {
            String::new()
        };
        Need {
            mark: Mark::State(Shown::Failed),
            unit: String::new(),
            id: format!("n-cause-{}", kind.to_lowercase().replace('_', "-")),
            title: format!("<span class=\"nd-t\">{} units</span>", leading(n)),
            state: format!(
                "failed: {}",
                shared_cause(kind).unwrap_or("they share a cause")
            ),
            who: String::new(),
            // what to try next, said of them all
            body: members
                .first()
                .and_then(|(_, s)| s.failure.as_ref())
                .map(|f| f.next_step_all())
                .filter(|next| !next.is_empty())
                .map(|next| format!("<span class=\"nd-next\">{next}</span>"))
                .unwrap_or_default(),
            question: None,
            form: String::new(),
            more: format!(
                "<details class=\"qf nd-more\" data-preserve-attr=\"open\"><summary>Show the {}</summary><ul>{list}</ul></details>",
                leading(n).to_lowercase()
            ),
            act,
            short: format!(
                "the failures of {} units ({})",
                leading(n).to_lowercase(),
                shared_cause(kind).unwrap_or("one cause")
            ),
            aside: keys
                .iter()
                .map(|k| self.aside(k))
                .collect::<Option<Vec<String>>>()
                .and_then(|at| at.into_iter().max()),
            keys,
        }
    }
    /// Retry for `steps`: one step's own action form, or the plan's for several.
    fn retry_form(&self, steps: &[&StepView], word: &str) -> String {
        match steps {
            [step] => {
                let id = format!("retry-{}", step.id);
                format!(
                    "{open}<input type=\"hidden\" name=\"action\" value=\"retry\"><input type=\"hidden\" name=\"revision\" value=\"{rev}\"><input type=\"hidden\" name=\"seen\" value=\"{seen}\">{button}{said}</form>",
                    open = ui::post_form(&id, "nd-form", &format!("{}/actions", step.href())),
                    button = ui::button(ui::Btn::Outlined, &esc(word), ""),
                    rev = step.revision,
                    seen = esc(&step.seen()),
                    said = ui::form_said(&id),
                )
            }
            steps => {
                let items = steps
                    .iter()
                    .map(|s| format!("stop:{}@{}", s.id, s.seen()))
                    .collect::<Vec<_>>()
                    .join(" ");
                let id = ui::form_id(&format!("retry {items}"));
                format!(
                    "{open}<input type=\"hidden\" name=\"action\" value=\"retry\"><input type=\"hidden\" name=\"items\" value=\"{items}\"><input type=\"hidden\" name=\"revision\" value=\"{rev}\">{button}{said}</form>",
                    open = ui::post_form(&id, "nd-form", &format!("{}/needs", self.view.href())),
                    button = ui::button(ui::Btn::Outlined, &esc(word), ""),
                    items = esc(&items),
                    rev = self.view.revision,
                    said = ui::form_said(&id),
                )
            }
        }
    }
    /// Settle as done: a cancel's step's own Dismiss, so its unit counts as done (its step's page
    /// can undo it).
    fn settle_form(&self, unit: &UnitView, step: &StepView) -> String {
        let id = format!("settle-{}", step.id);
        format!(
            "{open}<input type=\"hidden\" name=\"action\" value=\"dismiss\"><input type=\"hidden\" name=\"revision\" value=\"{rev}\"><input type=\"hidden\" name=\"seen\" value=\"{seen}\">{button}{said}</form>",
            open = ui::post_form(&id, "nd-form", &format!("{}/actions", step.href())),
            rev = step.revision,
            seen = esc(&step.seen()),
            said = ui::form_said(&id),
            button = ui::button(ui::Btn::Outlined, "Settle as done", &{
                let label = esc(&format!(
                    "Settle the cancel of {} as done: its unit counts as done",
                    ui::cut(title(unit), 60)
                ));
                format!(" title=\"{label}\" aria-label=\"{label}\"")
            }),
        )
    }
    /// A form posting `action` for `keys` to the plan's Needs you; once the stream draws what it
    /// changed, the focus goes to `#back` (`data-focus-after`, `sluice-board`).
    #[allow(clippy::too_many_arguments)]
    fn needs_form(
        &self,
        action: &str,
        keys: &[String],
        class: &str,
        label: &str,
        words: &str,
        id: &str,
        back: &str,
    ) -> String {
        let items = keys.join(" ");
        let form = ui::form_id(&format!("{action} {items} {class} {back}"));
        format!(
            "{open}<input type=\"hidden\" name=\"action\" value=\"{action}\"><input type=\"hidden\" name=\"items\" value=\"{items}\"><button{id} class=\"{class}\" aria-label=\"{label}\" title=\"{label}\">{words}</button>{said}</form>",
            open = ui::post_form_with(
                &form,
                "nd-form",
                &format!("{}/needs", self.view.href()),
                "",
                &format!(" data-focus-after=\"{}\"", esc(back))
            ),
            items = esc(&items),
            said = ui::form_said(&form),
            id = if id.is_empty() {
                String::new()
            } else {
                format!(" id=\"{}\"", esc(id))
            },
            label = esc(label),
        )
    }
    /// Needs you: a hairline row an item, its mark, its line, what it says, its form in place,
    /// its actions and its ×; then a line for each item set aside lately, with its Undo.
    /// Nothing needing the owner draws the empty sheet.
    fn needs_html(&self) -> String {
        let live: Vec<&Need> = self.needs.iter().filter(|n| n.aside.is_none()).collect();
        // what was set aside lately, with its Undo; earlier ones are gone from sight
        let aside: Vec<&Need> = self
            .needs
            .iter()
            .filter(|n| {
                n.aside
                    .as_deref()
                    .and_then(epoch)
                    .is_some_and(|at| at >= self.now - UNDO_MINUTES * 60.0)
            })
            .collect();
        let mut head = ui::SectionHead::new("needs", "Needs you").count(live.len());
        // the stops and silences, at once; a question keeps its own ×, so the head and the list
        // never disagree about it
        let rest: Vec<&&Need> = live.iter().filter(|n| n.mark != Mark::Ask).collect();
        if rest.len() > 1 {
            let keys: Vec<String> = rest.iter().flat_map(|n| n.keys.clone()).collect();
            head = head.action(&TrustedHtml::owned(self.needs_form(
                "dismiss",
                &keys,
                "quiet",
                "Set all aside: each stop and silence comes back when it changes; a question stays",
                "Set all aside",
                "",
                "needs",
            )));
        }
        let mut out = format!(
            "<section id=\"plan-needs\" class=\"pb-sec\" aria-labelledby=\"needs\">{}",
            head.html()
        );
        if live.is_empty() {
            // nothing needs you: the empty sheet, and how the work stands in a sentence (a find
            // narrows Needs you too: then it says so)
            let aside = self.needs.len() - live.len();
            let words = if !self.view.q.is_empty() {
                "Nothing the find matches needs you.".to_owned()
            } else if aside > 0 {
                format!(
                    "{} set aside: each comes back when it changes, or with its Undo below.",
                    ui::count(aside, "item", "items")
                )
            } else {
                match self.at_work {
                    0 => "No questions, no failures.".to_owned(),
                    n if self.quiet == 0 => format!(
                        "No questions, no failures. {} at work, all heard from recently.",
                        ui::count(n, "unit", "units")
                    ),
                    n => format!(
                        "No questions, no failures. {} at work.",
                        ui::count(n, "unit", "units")
                    ),
                }
            };
            out.push_str(
                ui::state(ui::Sketch::Sheet, "Nothing needs you", &esc(&words), false).as_str(),
            );
        } else {
            out.push_str("<ul class=\"nd\">");
            for need in &live {
                // a region each, so a change to one patches it alone
                let attrs = match self.view.units.iter().find(|u| u.id.as_str() == need.unit) {
                    Some(unit) => format!(
                        " data-unit=\"{}\" data-said=\"{}\"",
                        esc(&need.unit),
                        esc(&said_states(unit))
                    ),
                    None => String::new(),
                };
                let x = self.needs_form(
                    "dismiss",
                    &need.keys,
                    "x",
                    &format!("Set aside {}: it comes back when it changes", need.short),
                    icon(Icon::X, 16, "").as_str(),
                    "",
                    &format!("undo-{}", need.id),
                );
                let item = ui::need_line(
                    need.mark,
                    &ui::NeedLine {
                        id: &need.id,
                        attrs: &attrs,
                        title: &need.title,
                        state: &need.state,
                        who: &need.who,
                        body: &need.body,
                        asker: need.question.as_deref(),
                        more: &need.more,
                        form: &need.form,
                        act: &need.act,
                        x: &x,
                    },
                );
                out.push_str(&region(&need.id, &item));
            }
            out.push_str("</ul>");
        }
        if !aside.is_empty() {
            out.push_str("<ul class=\"nd-gone\" role=\"status\">");
            for need in &aside {
                let _ = write!(
                    out,
                    "<li><span>Set aside: {}</span>{}</li>",
                    esc(&need.short),
                    self.needs_form(
                        "undo",
                        &need.keys,
                        "quiet",
                        &format!("Undo setting aside {}", need.short),
                        "Undo",
                        &format!("undo-{}", need.id),
                        &need.id,
                    )
                );
            }
            out.push_str("</ul>");
        }
        out.push_str("</section>");
        out
    }

    // ---- the head ------------------------------------------------------------------------------

    /// The head's sentence, from status counts, silences and open questions only, each state in
    /// its own word: "1 question for you, 2 failed, 1 quiet. 10 at work; 73 of 92 done." or,
    /// healthy, "10 at work, all heard from recently. 73 of 92 done."
    fn sentence(&self) -> String {
        // what is true, whatever the owner set aside: setting aside is about attention alone
        // (a cancel settled as done, its step's Dismiss, leaves its unit done)
        let standing: Vec<&Need> = self
            .needs
            .iter()
            .filter(|n| {
                !self
                    .view
                    .units
                    .iter()
                    .any(|u| u.done && u.id.as_str() == n.unit)
            })
            .collect();
        let asks = self.view.project.asks.len();
        // each stop and silence by its state's word, in the status table's order
        let mut states: BTreeMap<Shown, usize> = BTreeMap::new();
        for need in &standing {
            for key in &need.keys {
                let step = key.split_once(':').map_or("", |(_, rest)| {
                    rest.split_once('@').map_or(rest, |(step, _)| step)
                });
                let shown = match need.mark {
                    Mark::Ask => continue,
                    Mark::State(Shown::Quiet) => Shown::Quiet,
                    Mark::State(_) => self
                        .view
                        .units
                        .iter()
                        .flat_map(|u| u.steps.iter())
                        .find(|s| s.id.as_str() == step)
                        .map_or(Shown::Failed, |s| s.shown()),
                };
                *states.entry(shown).or_default() += 1;
            }
        }
        let mut first: Vec<String> = vec![];
        if asks > 0 {
            let href = match self.view.project.asks.as_slice() {
                [one] => one.href(&self.project()),
                _ => "#needs".to_owned(),
            };
            first.push(format!(
                "<a class=\"ask\" href=\"{}\">{}</a>",
                esc(&href),
                ui::count(asks, "question for you", "questions for you")
            ));
        }
        for (shown, n) in states {
            first.push(format!("{n} {}", esc(shown.word())));
        }
        let total = self.view.units.len();
        let done = format!("{} of {total} done.", self.done.len());
        let mut work = match self.at_work {
            0 => "Nothing at work".to_owned(),
            n => format!("{n} at work"),
        };
        // the long runs, watched apart
        if !self.watching.is_empty() {
            let _ = write!(
                work,
                " and {} watched",
                ui::count(self.watching.len(), "long run", "long runs")
            );
        }
        if total == 0 {
            return "No units yet.".into();
        }
        if first.is_empty() {
            if self.at_work > 0 && self.quiet == 0 {
                format!("{work}, all heard from recently. {done}")
            } else {
                format!("{work}; {done}")
            }
        } else {
            format!("{}. {work}; {done}", first.join(", "))
        }
    }
    // ---- the plan in brief, as home's card draws it ----------------------------------------------

    /// The head's sentence, as its card says it.
    pub fn sentence_html(&self) -> String {
        self.sentence()
    }
    /// The plan's lead line: its sentence, "Nothing needs you" first when nothing does (Needs
    /// you is then not drawn; never under a find, which narrows Needs you too), and its steps
    /// counted after its units: "Nothing needs you. 9 at work, all heard from recently. 75 of
    /// 106 done · 487 steps."
    fn lead(&self) -> String {
        if self.view.units.is_empty() {
            return self.sentence();
        }
        let steps: usize = self.view.units.iter().map(|u| u.steps.len()).sum();
        let sentence = self.sentence();
        let sentence = sentence.strip_suffix('.').unwrap_or(&sentence);
        let counted = format!("{sentence} · {}.", ui::count(steps, "step", "steps"));
        if self.needs_count() > 0 || !self.view.q.is_empty() {
            return counted;
        }
        let aside = self.needs.iter().filter(|n| n.aside.is_some()).count();
        let nothing = match aside {
            0 => "Nothing needs you.".to_owned(),
            n => format!("Nothing needs you; {n} set aside."),
        };
        format!("{nothing} {counted}")
    }
    /// How many items of Needs you stand (what the owner set aside not counted).
    pub fn needs_count(&self) -> usize {
        self.needs.iter().filter(|n| n.aside.is_none()).count()
    }
    /// At work in brief: a line a group, its name, its rows counted and, when none of them is
    /// remarkable, how lately each was heard from; its remarkable rows named under it.
    pub fn group_lines(&self) -> String {
        let mut out = String::new();
        for group in &self.groups {
            let rows: Vec<&Row<'_>> = group.rows.iter().collect();
            let marked: Vec<&Row<'_>> = rows
                .iter()
                .copied()
                .filter(|r| self.remarkable(r))
                .collect();
            let heard = if marked.is_empty() {
                rows.iter()
                    .filter_map(|r| r.heard)
                    .reduce(f64::min)
                    .map(|t| {
                        format!(
                            " · each heard from in the last {}",
                            ui::since(&super::rfc3339(t as u64))
                        )
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            };
            let named: Vec<String> = marked
                .iter()
                .take(3)
                .map(|r| {
                    format!(
                        "<a href=\"{}\">{}</a>",
                        esc(&unit_href(self.view, r.unit)),
                        esc(title(r.unit))
                    )
                })
                .collect();
            let _ = write!(
                out,
                "<li><b>{}</b> · {}{heard}{}</li>",
                esc(&group.name),
                row_counts(&rows),
                if named.is_empty() {
                    String::new()
                } else {
                    format!(
                        "<span class=\"pm-g-said\">{}{}</span>",
                        named.join(", "),
                        match marked.len().saturating_sub(3) {
                            0 => String::new(),
                            n => format!(" and {n} more"),
                        }
                    )
                }
            );
        }
        // the long runs watched: counted, a quiet one named
        if !self.watching.is_empty() {
            let quiet: Vec<String> = self
                .watching
                .iter()
                .filter(|u| u.shown() == Shown::Quiet)
                .take(3)
                .map(|u| {
                    format!(
                        "<a href=\"{}\">{}</a>",
                        esc(&unit_href(self.view, u)),
                        esc(title(u))
                    )
                })
                .collect();
            let _ = write!(
                out,
                "<li><b>Watching</b> · {}{}</li>",
                if quiet.is_empty() {
                    ui::count(self.watching.len(), "long run", "long runs")
                } else {
                    format!("{} quiet", quiet.len())
                },
                if quiet.is_empty() {
                    String::new()
                } else {
                    format!("<span class=\"pm-g-said\">{}</span>", quiet.join(", "))
                }
            );
        }
        if out.is_empty() {
            return String::new();
        }
        format!("<ul class=\"pm-groups\">{out}</ul>")
    }
    /// The units that finished last, at most `n`, by name in one line, each its unit page's
    /// link (a home card's Finished lately: the plan draws what they produced).
    pub fn finished_names(&self, n: usize) -> String {
        self.done[..self.done.len().min(n)]
            .iter()
            .map(|unit| {
                format!(
                    "<a href=\"{}\">{}</a>",
                    esc(&unit_href(self.view, unit)),
                    esc(title(unit))
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
    /// The page's head: the project's name, the sentence under it, and its Details (the
    /// project's id and its words), in one region its stream patches.
    pub fn band(&self) -> TrustedHtml {
        let project = &self.view.project;
        let details = ui::Details::new()
            .id("Project", &project.id.to_string())
            .html("About", &about_html(project.description.trim()))
            .link(
                "Settings",
                &format!("{}/settings", project.href()),
                "Its settings",
            )
            .link(
                "Export",
                &if self.view.plan_empty() {
                    String::new()
                } else {
                    format!("{}?format=mermaid&all=true", self.view.href())
                },
                "The plan as Mermaid text",
            );
        // the switcher names the project: the lead line is the summary, its "⋯" beside it
        // beside the sentence: the plan · both · board switch when it has a board, its "⋯"
        let switch = if self.view.panel.is_some() {
            view_switch(&self.view.href(), self.view.view)
        } else {
            String::new()
        };
        let head = ui::lead_head(
            &project.name,
            &TrustedHtml::owned(self.lead()),
            &TrustedHtml::owned(format!("{switch}{}", details.menu(&project.name))),
            &TrustedHtml::default(),
        );
        TrustedHtml::owned(format!(
            "<div id=\"plan-band\" class=\"plan-band\">{head}</div>"
        ))
    }
    /// The sidebar's find, live on this page: the page's `q` signal, so typing reopens the
    /// stream on it, which draws the plan as found (Datastar's active search); Enter goes to the
    /// page of that find, an address like any other; Escape clears it.
    pub fn find(&self) -> TrustedHtml {
        let view = self.view;
        let reopen = ui::stream_reopen(&view.stream_href());
        let attrs = format!(
            " data-preserve-attr=\"value\" data-reopens-stream data-bind:q__root data-on:input__debounce.200ms=\"{get}\" data-on:keydown=\"evt.key === 'Escape' &amp;&amp; $q &amp;&amp; (evt.preventDefault(), evt.stopPropagation(), $q = '', {get})\"",
            get = esc(&reopen),
        );
        let more = format!(
            "{}{}",
            view.focus_inputs(),
            if view.show == "all" {
                String::new()
            } else {
                format!(
                    "<input type=\"hidden\" name=\"show\" value=\"{}\">",
                    esc(&view.show)
                )
            }
        );
        ui::find_form(
            &view.href(),
            &view.q,
            "Find steps",
            "Find units by id or title, or by a step's id, title or description",
            &attrs,
            &more,
        )
    }

    // ---- the sheet -----------------------------------------------------------------------------

    /// The plan's sheet: its column (Needs you, At work) and its rail (Watching, Finished since
    /// you looked), each a region of its own. From 1600px the rail stands beside the column,
    /// sticking to the top; narrower, it follows it. The done index, when asked for, after both.
    pub fn sheet(&self) -> TrustedHtml {
        let mut out = String::from("<div class=\"plan-sheet\"><div class=\"plan-col\">");
        out.push_str(&region("plan-needs", &self.needs_html()));
        out.push_str(&region("plan-roster", &self.roster_html()));
        out.push_str("</div><aside class=\"plan-rail\" aria-label=\"Watching and finished\">");
        out.push_str(&region("plan-watching", &self.watching_html()));
        out.push_str(&region(
            "plan-finished",
            &match (self.finished.is_empty(), self.view.seen) {
                // marked seen and nothing finished since: said once, with its Undo
                (true, Some(seen)) => self.seen_html(seen),
                (true, None) => "<div id=\"plan-finished\" hidden></div>".to_owned(),
                (false, _) => self.finished_html(),
            },
        ));
        out.push_str("</aside></div>");
        let index = self.view.show == "done"
            || !self.view.q.is_empty()
            || !self.view.recipe.is_empty()
            || !self.view.root.is_empty();
        out.push_str(&region(
            "plan-done",
            &if index && !self.done.is_empty() {
                self.done_html()
            } else {
                "<div id=\"plan-done\" hidden></div>".to_owned()
            },
        ));
        TrustedHtml::owned(out)
    }

    // ---- Watching ------------------------------------------------------------------------------

    /// Watching: the long runs, a line each: its title, its latest result in its colour (red a
    /// failure, green a pass, muted neither), its last results a square each, and when it last
    /// published. Nothing watched leaves the region's place.
    fn watching_html(&self) -> String {
        if self.watching.is_empty() {
            return "<section id=\"plan-watching\" hidden></section>".into();
        }
        let mut out = format!(
            "<section id=\"plan-watching\" class=\"pb-sec\" aria-labelledby=\"watching\">{}<div class=\"wl\">",
            ui::SectionHead::new("watching", "Watching")
                .count(self.watching.len())
                .html()
        );
        for unit in &self.watching {
            let Some(step) = unit.steps.first() else {
                continue;
            };
            let progress = step.progress.as_ref();
            // its latest result: its short fields as they stand, in its verdict's colour
            let latest = progress
                .map(|p| {
                    p.shown()
                        .into_iter()
                        .filter(|f| !f.value.trim().is_empty() && !f.long())
                        .take(2)
                        .map(|f| format!("{} {}", f.name.replace(['_', '-'], " "), f.value.trim()))
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .filter(|words| !words.is_empty());
            let verdict = progress
                .and_then(|p| p.history.last())
                .map_or(step::Verdict::Neither, |(_, v)| *v);
            let result = match (&latest, verdict) {
                (Some(words), step::Verdict::Red) => {
                    format!(
                        "<span class=\"result red\"><span>{}</span></span>",
                        esc(words)
                    )
                }
                (Some(words), step::Verdict::Green) => {
                    format!(
                        "<span class=\"result green\"><span>{}</span></span>",
                        esc(words)
                    )
                }
                (Some(words), _) => {
                    format!("<span class=\"result\"><span>{}</span></span>", esc(words))
                }
                (None, _) => "<span class=\"result\"><span>no result yet</span></span>".to_owned(),
            };
            // its last results, oldest first, the latest taller
            let history = progress.map(|p| p.history.as_slice()).unwrap_or_default();
            let hist = if history.is_empty() {
                String::new()
            } else {
                let red = history
                    .iter()
                    .filter(|(_, v)| *v == step::Verdict::Red)
                    .count();
                let green = history
                    .iter()
                    .filter(|(_, v)| *v == step::Verdict::Green)
                    .count();
                format!(
                    "<span class=\"w-hist\" role=\"img\" aria-label=\"{}\">{}</span>",
                    esc(&format!(
                        "The last {}: {red} red, {green} green",
                        ui::count(history.len(), "result", "results")
                    )),
                    history
                        .iter()
                        .map(|(at, v)| format!(
                            "<i class=\"{}\" title=\"{}\"></i>",
                            v.key(),
                            esc(&ui::at_text(at))
                        ))
                        .collect::<String>()
                )
            };
            let when = match progress {
                Some(p) => format!("last result {}", ui::ago(&p.at)),
                None => step
                    .timing
                    .as_ref()
                    .map(|t| format!("running {}", ui::since(&t.started)))
                    .unwrap_or_default(),
            };
            let line = format!(
                "<div id=\"w-{id}\" class=\"wline\" data-unit=\"{id}\" data-said=\"{said}\"><a class=\"wt\" href=\"{href}\" data-step=\"{step}\">{title}</a>{result}{hist}<span class=\"when\">{when}</span></div>",
                id = esc(unit.id.as_str()),
                said = esc(&said_states(unit)),
                href = esc(&step.href()),
                step = esc(step.id.as_str()),
                title = esc(unit.whole_heading()),
            );
            out.push_str(&region(&format!("w-{}", unit.id), &line));
        }
        out.push_str("</div></section>");
        out
    }

    // ---- Finished since you looked ------------------------------------------------------------

    /// What Mark as seen leaves: the ruled lines, "Nothing new since 04:21", Undo taking the
    /// look back so what finished lately is drawn again.
    fn seen_html(&self, seen: u64) -> String {
        let at = ui::clock(&super::rfc3339(seen));
        format!(
            "<section id=\"plan-finished\" class=\"pb-sec pb-seen\" aria-labelledby=\"finished\"{watch}>{head}{form}{state}<input type=\"hidden\" name=\"through\" value=\"0\">{said}</form></section>",
            watch = self.seen_watch(),
            head = ui::SectionHead::new("finished", "Finished lately").html(),
            form = ui::post_form(
                "mark-seen",
                "seen-form",
                &format!("{}/seen", self.view.href())
            ),
            state = ui::state(
                ui::Sketch::Lines,
                "Nothing new since you looked",
                &format!("Seen through {at} · <button class=\"quiet\">Undo</button>"),
                true
            ),
            said = ui::form_said("mark-seen"),
        )
    }
    /// The look is this browser's: Mark as seen's answer sets the page's `seen` signal (and
    /// its cookie, for its next page), and the change reopens the stream on it, which draws
    /// the plan from that look (Datastar's `data-on-signal-patch`, filtered to `seen`).
    fn seen_watch(&self) -> String {
        format!(
            " data-reopens-stream data-on-signal-patch=\"{}\" data-on-signal-patch-filter=\"{{include: /^seen$/}}\"",
            esc(&ui::stream_reopen(&self.view.stream_href()))
        )
    }
    fn finished_html(&self) -> String {
        let (head, since) = match self.view.seen {
            Some(seen) => (
                "Finished<span class=\"lk\"> since you looked</span>",
                format!("since {}", ui::clock(&super::rfc3339(seen))),
            ),
            None => ("Finished lately", String::new()),
        };
        let through = self
            .finished
            .iter()
            .filter_map(|u| super::timestamp(u.finished()))
            .max()
            .unwrap_or(0)
            .max(self.view.seen.unwrap_or(0));
        let mark = format!(
            "{form}<input type=\"hidden\" name=\"through\" value=\"{through}\"><button class=\"quiet\">Mark as seen</button>{said}</form>",
            form = ui::post_form(
                "mark-seen",
                "seen-form",
                &format!("{}/seen", self.view.href())
            ),
            said = ui::form_said("mark-seen"),
        );
        let mut out = format!(
            "<section id=\"plan-finished\" class=\"pb-sec\" aria-labelledby=\"finished\"{watch}>{head}{list}",
            watch = self.seen_watch(),
            head = ui::SectionHead::new_html("finished", head)
                .note(&TrustedHtml::owned(since))
                .action(&TrustedHtml::owned(mark))
                .html(),
            list = finished_list(self.view, &self.finished),
        );
        if self.finished_more > 0 {
            let _ = write!(
                out,
                "<p class=\"fin-more\"><a href=\"{}?show=done#done\">and {} more</a></p>",
                esc(&self.view.href()),
                self.finished_more
            );
        }
        out.push_str("</section>");
        out
    }

    // ---- At work -------------------------------------------------------------------------------

    /// At work: its head (its name, its counts, the order menu, the ⓘ; "1h ago … now" over the
    /// strips' column alone), each group's rows, then the line that counts the rest. Nothing at
    /// work draws the spiral at rest.
    fn roster_html(&self) -> String {
        let rows: usize = self.groups.iter().map(|g| g.rows.len()).sum();
        // counted as the summary counts them: a blocked or queued unit waits, it is not at work
        let waiting: usize = self
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .filter(|r| r.waiting)
            .count();
        let counts = match (rows - waiting, waiting) {
            (0, 0) => String::new(),
            (0, w) => format!("{w} waiting"),
            (n, 0) => ui::count(n, "unit", "units"),
            (n, w) => format!("{} · {w} waiting", ui::count(n, "unit", "units")),
        };
        let counts = region(
            "ro-counts",
            &format!("<span id=\"ro-counts\" class=\"n\">{}</span>", esc(&counts)),
        );
        // an engine's chip where any row runs on one: the column is the agents'
        let engined = self
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .any(|r| current(r.unit).is_some_and(|s| s.badge() != "fn"));
        let left = format!(
            "<div class=\"aw-left\"><h2 id=\"at-work\">At work</h2>{counts}{}{}</div>",
            if rows > 1 {
                self.order_menu()
            } else {
                String::new()
            },
            if rows > 0 {
                track_help()
            } else {
                String::new()
            },
        );
        // a strip's pick asks for its details (`pulse`), drawn into the page's popover
        let mut out = format!(
            "<section id=\"plan-roster\" class=\"pb-sec\" aria-labelledby=\"at-work\"{}>",
            super::pulse::listen(&format!("{}/pulse", self.view.href()), "pulse-pop")
        );
        if rows == 0 {
            let _ = write!(out, "<div class=\"aw-head\">{left}</div>");
            out.push_str(
                ui::state(
                    ui::Sketch::Spiral,
                    "Nothing at work",
                    &esc(&if self.view.q.is_empty() {
                        "No unit runs now.".to_owned()
                    } else {
                        "Nothing the find matches runs now.".to_owned()
                    }),
                    false,
                )
                .as_str(),
            );
        } else {
            let _ = write!(
                out,
                "<div class=\"ro{}\"><div class=\"aw-head\">{left}<div class=\"scale\" aria-hidden=\"true\" title=\"{SCALE}\"><span>1h ago</span><span>now</span></div><span class=\"vh\">{SCALE}</span><span class=\"pad\"></span></div>",
                if engined { "" } else { " no-engine" }
            );
            for group in &self.groups {
                // a big roster shows a group's remarkable rows first and folds the rest to one
                // line ("+21 running · each heard from in the last 28m"), opening in place; a
                // group with nothing remarkable folds whole to its own line
                let (marked, healthy): (Vec<&Row<'_>>, Vec<&Row<'_>>) =
                    group.rows.iter().partition(|r| self.remarkable(r));
                let fold = rows > FOLD_ROSTER && healthy.len() > FOLD_GROUP;
                let whole = fold && marked.is_empty();
                let heard = |rows: &[&Row<'_>]| {
                    rows.iter()
                        .filter_map(|r| r.heard)
                        .reduce(f64::min)
                        .map(|t| {
                            format!(
                                " · each heard from in the last {}",
                                ui::since(&super::rfc3339(t as u64))
                            )
                        })
                        .unwrap_or_default()
                };
                let group_id = format!(
                    "ro-g-{:x}",
                    crate::streams::skeleton_hash(&format!("{}:{}", group.recipe, group.name))
                );
                let counts = region(
                    &format!("{group_id}-counts"),
                    &format!(
                        "<span id=\"{group_id}-counts\">{}{}</span>",
                        row_counts(&group.rows.iter().collect::<Vec<_>>()),
                        if whole {
                            heard(&healthy)
                        } else {
                            String::new()
                        }
                    ),
                );
                let name = if group.recipe.is_empty() {
                    esc(&group.name)
                } else {
                    format!(
                        "<a href=\"{}?recipe={}&amp;show=all\">{}</a>",
                        esc(&self.view.href()),
                        url::form_urlencoded::byte_serialize(group.recipe.as_bytes())
                            .collect::<String>(),
                        esc(&group.name)
                    )
                };
                let _ = write!(
                    out,
                    "<details class=\"ro-g\" id=\"{group_id}\" data-plan-fold=\"{group_id}\" data-preserve-attr=\"open\"{}><summary class=\"ro-group\">{}<b>{name}</b>{counts}</summary>",
                    if whole { "" } else { " open" },
                    icon(Icon::ChevronRight, 16, "chev"),
                );
                // a recipe whose view does not check: said once
                if let Some(error) = group
                    .rows
                    .first()
                    .and_then(|r| self.view.names.recipe_of(r.unit.id.as_str()))
                    .and_then(|r| r.view_error())
                {
                    out.push_str(super::unit_view::broken(error).as_str());
                }
                let draw = |out: &mut String, rows: &[&Row<'_>]| {
                    for row in rows {
                        out.push_str(&region(
                            &format!("u-{}", row.unit.id),
                            &self.row_html(row, engined),
                        ));
                    }
                };
                if fold && !whole {
                    draw(&mut out, &marked);
                    let _ = write!(
                        out,
                        "<details class=\"qf ro-fold\" id=\"{group_id}-quiet\" data-plan-fold=\"{group_id}-quiet\" data-preserve-attr=\"open\"><summary>{}</summary>",
                        region(
                            &format!("{group_id}-quiet-counts"),
                            &format!(
                                "<span id=\"{group_id}-quiet-counts\">+{}{}</span>",
                                row_counts(&healthy),
                                heard(&healthy)
                            )
                        )
                    );
                    draw(&mut out, &healthy);
                    out.push_str("</details>");
                } else {
                    draw(&mut out, &group.rows.iter().collect::<Vec<_>>());
                }
                out.push_str("</details>");
            }
            out.push_str("</div>");
        }
        // the rest: what waits on earlier work, and what is done
        let mut more = vec![];
        if self.later > 0 {
            more.push(format!("{} waiting on earlier work", self.later));
        }
        if !self.done.is_empty() {
            let last = self.done.first().map(|u| u.finished()).unwrap_or("");
            more.push(format!(
                "<a href=\"{}?show=done#done\">{} done</a>{}",
                esc(&self.view.href()),
                self.done.len(),
                if last.is_empty() {
                    String::new()
                } else {
                    format!(", the last {}", ui::ago(last))
                }
            ));
        }
        out.push_str(&region(
            "ro-more",
            &format!(
                "<p id=\"ro-more\" class=\"ro-more\"{}>{}</p>",
                if more.is_empty() { " hidden" } else { "" },
                more.join(" · ")
            ),
        ));
        out.push_str("</section>");
        out
    }
    /// The order menu beside At work's name: "Order: Created", its choices radio items that
    /// keep this browser's choice in the project's cookie and reopen the stream on it.
    fn order_menu(&self) -> String {
        let current = match self.view.order {
            Order::Created => "Created",
            Order::Plan => "Plan order",
            Order::Recent => "Recent activity",
        };
        let items: String = [
            ("created", "Created", matches!(self.view.order, Order::Created)),
            ("plan", "Plan order", matches!(self.view.order, Order::Plan)),
            ("recent", "Recent activity", matches!(self.view.order, Order::Recent)),
        ]
        .into_iter()
        .map(|(value, label, on)| {
            let pick = format!(
                "document.cookie = '{}={value}; Path=/; Max-Age=34560000; SameSite=Lax'; el.closest('details').open = false; {}",
                order_cookie(self.project()),
                ui::stream_reopen(&self.view.stream_href())
            );
            format!(
                "<button type=\"button\" role=\"menuitemradio\" aria-checked=\"{on}\" data-on:click=\"{}\">{}{label}</button>",
                esc(&pick),
                icon(Icon::Check, 16, "")
            )
        })
        .collect();
        format!(
            "<details class=\"order-menu\" data-preserve-attr=\"open\"><summary aria-label=\"Row order: {current}\">Order: <b>{current}</b>{}</summary><div class=\"menu\" role=\"menu\" aria-label=\"Order rows by\"><p class=\"dm-h\">Order rows by</p>{items}</div></details>",
            icon(Icon::ChevronDown, 14, "")
        )
    }
    /// A row a big roster keeps in sight: a silence, a wait of an hour or more since its last
    /// run, or a unit with an open item in Needs you.
    fn remarkable(&self, row: &Row<'_>) -> bool {
        let unit = row.unit;
        unit.steps.iter().any(|s| s.shown() == Shown::Quiet)
            || (row.waiting && last_end(unit).is_some_and(|end| self.now - end >= WAIT_LONG))
            || self.needs.iter().filter(|n| n.aside.is_none()).any(|n| {
                n.unit == unit.id.as_str()
                    || n.keys.iter().any(|k| {
                        k.strip_prefix("stop:")
                            .is_some_and(|step| unit.steps.iter().any(|s| s.id.as_str() == step))
                    })
            })
    }
    /// One unit's row: its engine, its title on two lines, its stage track with when it was
    /// heard from, what it said last on one line, its pulse strip; open, its conversation.
    fn row_html(&self, row: &Row<'_>, engined: bool) -> String {
        let unit = row.unit;
        let step = current(unit);
        let engine = step
            .filter(|_| engined)
            .map(|s| (s.badge(), s.runs_on()))
            .filter(|(b, _)| b != "fn");
        // its stages, the one it stands at in its state's colour (a question to the owner its
        // step's, in teal)
        let stages: Vec<ui::Stage> = super::unit_steps::ordered(unit)
            .iter()
            .map(|s| {
                let shown = s.standing();
                let mut stage = ui::Stage::new(unit.step_name(s), Some(shown))
                    .href(s.href())
                    .step(s.id.as_str())
                    .asks(s.asking.is_some() && self.view.project.ask_of(s.id.as_str()).is_some());
                if let Some(t) = s.timing.as_ref() {
                    stage = if t.finished.is_none() && s.running() {
                        stage.running_since(&t.started, t.seconds)
                    } else {
                        stage.took(t.seconds)
                    };
                }
                if shown == Shown::Quiet && !s.active_at.is_empty() {
                    stage = stage.running_since(&s.active_at, 0.0);
                }
                s.bypass_marks(stage)
            })
            .collect();
        let mut track = ui::stage_track(&format!("Stages of {}", unit.id), &stages, None).0;
        if let Some(park) = step.and_then(|s| s.parked.as_ref()) {
            // parked: since when and what it waits for, never a quiet warning
            let _ = write!(
                track,
                "<span class=\"heard ro-why\">parked {} · {}</span>",
                ui::since(&park.since).0,
                esc(&park.until)
            );
        } else if row.waiting {
            let why = why_waiting(unit);
            if !why.is_empty() {
                let _ = write!(track, "<span class=\"heard ro-why\">{why}</span>");
            }
        } else {
            let quiet = unit.steps.iter().any(|s| s.shown() == Shown::Quiet);
            // the live dot a phone shows in the strip's place: green while heard lately, its
            // class the clock's as the heard line's is
            let _ = match row.heard {
                _ if quiet => write!(
                    track,
                    "<span class=\"live quiet\" aria-hidden=\"true\"></span>"
                ),
                Some(h) => write!(
                    track,
                    "<span data-heard=\"{h:.0}\" class=\"live{}\" aria-hidden=\"true\"></span>",
                    if self.now - h < LIVE { " is-live" } else { "" }
                ),
                None => write!(track, "<span class=\"live\" aria-hidden=\"true\"></span>"),
            };
            match row.heard {
                // quiet: its stage says for how long
                _ if quiet => {}
                // heard from since its run began: when, the words taking the ink as they age
                Some(heard) if heard - started(unit).unwrap_or(heard) > 120.0 => {
                    let at = super::rfc3339(heard as u64);
                    let age = self.now - heard;
                    // its class is the clock's: `nav.js` keeps it from `data-heard`, and a
                    // stream's version leaves it out
                    let _ = write!(
                        track,
                        "<span data-heard=\"{heard:.0}\" class=\"heard{}\">heard {}</span>",
                        if age >= HEARD_OLD {
                            " is-old"
                        } else if age >= HEARD_AGING {
                            " is-aging"
                        } else {
                            ""
                        },
                        ui::ago(&at)
                    );
                }
                Some(_) => track.push_str("<span class=\"heard\">no word yet</span>"),
                None => {}
            }
        }
        roster_row(&RowParts {
            id: unit.id.as_str(),
            waiting: row.waiting,
            said_states: &said_states(unit),
            engine: engine.as_ref().map(|(b, _)| b.as_str()).unwrap_or(""),
            runs_on: engine.as_ref().map(|(_, r)| r.as_str()).unwrap_or(""),
            // whole: the row clamps it to two lines, and open shows it all
            title: unit.whole_heading(),
            owner: &unit.owner_words(),
            said: &self.said(unit),
            track: &track,
            pulse: &self.pulse_html(row),
            drill: Some(Drill {
                src: format!("{}/units/{}/drill", self.view.href(), unit.id),
                stamp: drill_stamp(unit),
                steps: super::unit_steps::ordered(unit)
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            }),
        })
    }
    /// A unit's one line: its recipe's view (the project's own summary of it), else what it said
    /// last, after its sender's avatar.
    fn said(&self, unit: &UnitView) -> String {
        if let Some(view) = self
            .view
            .names
            .recipe_of(unit.id.as_str())
            .and_then(|r| r.view())
        {
            let drawn = super::unit_view::draw(view, unit, true);
            if !super::unit_view::empty(&drawn) {
                return drawn.0;
            }
        }
        // nothing said leaves the line empty: its track says when it was last heard from
        said_html(unit)
    }
    /// A unit's pulse strip: what its running steps did in the last hour (their agents' turns
    /// and calls, what they said, when they last wrote), and since when they have been silent;
    /// a waiting unit's only what it said.
    fn pulse_html(&self, row: &Row<'_>) -> String {
        let unit = row.unit;
        let running: Vec<&super::step::StepView> =
            unit.steps.iter().filter(|s| s.running()).collect();
        let mut acts: Vec<f64> = running
            .iter()
            .flat_map(|s| s.pulse.iter().copied())
            .collect();
        acts.sort_by(f64::total_cmp);
        let said: Vec<f64> = unit.said.iter().map(|(_, t)| *t).collect();
        let live = !row.waiting && !running.is_empty();
        track(
            unit.id.as_str(),
            &super::unit_steps::Pulse {
                live,
                start: started(unit).filter(|_| live),
                acts: &acts,
                said: &said,
                active: running
                    .iter()
                    .filter_map(|s| epoch(&s.active_at))
                    .reduce(f64::max),
                quiet: running.iter().any(|s| s.shown() == Shown::Quiet),
            },
            self.now,
        )
    }

    // ---- the done index ------------------------------------------------------------------------

    /// Done, when asked for (Show: done, a find, a recipe's units): a dense index of every done
    /// unit, newest first.
    fn done_html(&self) -> String {
        let steps: usize = self.done.iter().map(|u| u.steps.len()).sum();
        let last = self.done.first().map(|u| u.finished()).unwrap_or("");
        let mut line = format!(
            "{} · {}",
            ui::count(self.done.len(), "unit", "units"),
            ui::count(steps, "step", "steps")
        );
        if !last.is_empty() {
            line.push_str(&format!(" · the last finished {}", ui::ago_text(last)));
        }
        let mut items = String::new();
        for unit in &self.done {
            let took = took(unit);
            // a cancel the owner dismissed leaves its unit done, still said
            let cancels: Vec<String> = unit
                .steps
                .iter()
                .filter(|s| s.cancelled())
                .map(|s| unit.step_name(s))
                .collect();
            let dismissed = unit.steps.iter().any(|s| s.cancelled() && s.dismissed);
            let _ = write!(
                items,
                "<li data-said=\"{said}\"><a href=\"{h}\"><span class=\"pl-at\">{at}</span>{g}<span class=\"pl-dt\">{t}</span><span class=\"pl-dk\">{k}</span></a></li>",
                said = esc(&said_states(unit)),
                h = esc(&unit_href(self.view, unit)),
                at = if unit.finished().is_empty() {
                    String::new()
                } else {
                    ui::clock(unit.finished()).0
                },
                g = ui::glyph(if cancels.is_empty() {
                    unit.shown()
                } else {
                    Shown::Cancelled
                }),
                t = esc(title(unit)),
                k = if !cancels.is_empty() {
                    let cancels: Vec<&str> = cancels.iter().map(String::as_str).collect();
                    format!(
                        "{} cancelled{}",
                        esc(&join(&cancels)),
                        if dismissed { ", settled as done" } else { "" }
                    )
                } else if took > 0.0 {
                    format!("took {}", esc(&ui::duration_text(took)))
                } else {
                    esc(unit.shown().word())
                },
            );
        }
        format!(
            "<section id=\"plan-done\" class=\"pb-sec\" aria-labelledby=\"done\">{}<ol class=\"pl-idx\">{items}</ol></section>",
            ui::section_head("done", "Done", &line)
        )
    }
}

/// When a unit's last run ended (seconds since the epoch), none while one goes.
fn last_end(unit: &UnitView) -> Option<f64> {
    unit.steps
        .iter()
        .filter_map(|s| s.timing.as_ref())
        .flat_map(|t| t.spans.iter())
        .filter_map(|s| s.to)
        .reduce(f64::max)
}
/// When its running step's current run began.
fn started(unit: &UnitView) -> Option<f64> {
    unit.steps
        .iter()
        .filter(|s| s.running())
        .filter_map(|s| s.timing.as_ref().and_then(|t| epoch(&t.started)))
        .reduce(f64::max)
}
/// Rows counted by how they stand: "1 quiet, 7 running, 2 waiting".
fn row_counts(rows: &[&Row<'_>]) -> String {
    let quiet = rows
        .iter()
        .filter(|r| !r.waiting && r.unit.steps.iter().any(|s| s.shown() == Shown::Quiet))
        .count();
    let waiting = rows.iter().filter(|r| r.waiting).count();
    let parked = rows
        .iter()
        .filter(|r| r.unit.shown() == Shown::Parked)
        .count();
    let running = rows.len() - quiet - waiting - parked;
    [
        (quiet, "quiet"),
        (parked, "parked"),
        (running, "running"),
        (waiting, "waiting"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, word)| format!("{n} {word}"))
    .collect::<Vec<_>>()
    .join(", ")
}

// ---- the plan's parts, drawn here and on `/_ui` -------------------------------------------------

/// A row of Finished: its title linking its page, its result in a line under it, when it
/// finished and how long it took at the right (nothing for no time); its steps' states
/// (`said`, as `data-said` carries them; "" for none).
pub fn finished_row(
    at: &str,
    took: f64,
    href: &str,
    title: &str,
    result: &str,
    said: &str,
) -> String {
    format!(
        "<li class=\"fin-r\"{said}><a class=\"fin-t\" href=\"{href}\">{title}</a><span class=\"fin-v\">{result}</span><span class=\"fin-w\">{at}{took}</span></li>",
        at = ui::clock(at),
        // a value set by hand took no time of its own
        took = if took > 0.0 {
            format!(" · took {}", esc(&ui::duration_text(took)))
        } else {
            String::new()
        },
        href = esc(href),
        title = esc(title),
        said = if said.is_empty() {
            String::new()
        } else {
            format!(" data-said=\"{}\"", esc(said))
        },
    )
}
/// What the strips' labels say of them, to a screen reader and on hover.
const SCALE: &str = "The last hour, now at the right: a mark each minute the agent did something, a taller one each message, a dotted line since it last did anything.";
/// At work's ⓘ: how to read a row, each mark of a strip drawn beside its meaning, behind a
/// small round button by its name (a Details menu's popover: it closes on a click elsewhere or
/// Escape).
fn track_help() -> String {
    let demo = |marks: &str| format!("<span class=\"strip\" aria-hidden=\"true\">{marks}</span>");
    let acts: String = [8, 22, 30, 41, 47, 60, 72, 80]
        .iter()
        .map(|l| format!("<i class=\"pk\" style=\"--l:{l}%\"></i>"))
        .collect();
    format!(
        "{open}<details class=\"dm info\" data-preserve-attr=\"open\"><summary class=\"dm-b\" aria-label=\"How to read At work\" title=\"How to read At work\" data-preserve-attr=\"aria-expanded\">{i}</summary><div class=\"menu dm-p\" role=\"group\" aria-label=\"How to read At work\" data-preserve-attr=\"style\"><p class=\"dm-h\">How to read a row</p><p>The line under a title is the unit's stages: ✓ the one done before, the green one at work and for how long, the next after it; the rest are folded into a count that names them.</p><p>The strip at the right is its last hour, now at the right edge.</p><div class=\"legend\">{a}<span>a minute its agent did something: a turn, a tool call</span>{m}<span>a message it sent</span>{s}<span>where its run began</span>{t}<span>time since it last acted, faint while recent</span>{q}<span>ochre once it has been quiet long enough to need a look</span></div></div></details>{close}",
        open = ui::menu_open(),
        i = icon(Icon::Info, 16, ""),
        a = demo(&acts),
        m = demo(
            "<i class=\"pk\" style=\"--l:20%\"></i><i class=\"pk pk-m\" style=\"--l:46%\"></i><i class=\"pk\" style=\"--l:70%\"></i>"
        ),
        s = demo(
            "<span class=\"pk-start\" style=\"--l:30%\"></span><i class=\"pk\" style=\"--l:44%\"></i><i class=\"pk\" style=\"--l:60%\"></i>"
        ),
        t = demo(
            "<i class=\"pk\" style=\"--l:12%\"></i><i class=\"pk\" style=\"--l:26%\"></i><span class=\"pk-tail\" style=\"--l:26%;--w:66%\"></span>"
        ),
        q = demo(
            "<i class=\"pk\" style=\"--l:8%\"></i><span class=\"pk-tail is-quiet\" style=\"--l:8%;--w:86%\"></span>"
        ),
        close = ui::menu_close(),
    )
}
/// `unit`'s strip over the last hour to `now` (`unit_steps::Pulse`): its places are the
/// clock's (`unit_steps::bar`, `mark`), drawn here to `now`, kept at the reader's now by
/// `nav.js` and left out of a stream's version, so it changes only when the unit does
/// something. A `sluice-pulse`: pointed at, a mark says what it did that minute, and its left
/// edge, faded while its run began before the window, when it began (`pulse`).
pub fn track(unit: &str, pulse: &super::unit_steps::Pulse<'_>, now: f64) -> String {
    let (marks, said) = pulse.html(now);
    super::pulse::host(
        "ro-track",
        unit,
        "",
        pulse.start,
        &format!("Its pulse: {said}"),
    )
    .wrap(&TrustedHtml::owned(marks))
    .to_string()
}
/// What a row of At work is drawn from (HTML where it says so).
pub struct RowParts<'a> {
    pub id: &'a str,
    /// It waits (nothing runs, its strip only what it said); else it is at work.
    pub waiting: bool,
    /// Its steps' states, as `data-said` carries them ("" for none).
    pub said_states: &'a str,
    /// What its work runs on, short: an agent's model or engine ("" for a fn's row, whose chip
    /// stays empty to keep the column).
    pub engine: &'a str,
    pub runs_on: &'a str,
    pub title: &'a str,
    /// Who owns it (`@lead`, "you"), small and faint after its title; "" for no one.
    pub owner: &'a str,
    /// HTML: what it said last.
    pub said: &'a str,
    /// HTML: its stage track and when it was heard from.
    pub track: &'a str,
    /// HTML: its pulse strip (`track`).
    pub pulse: &'a str,
    /// Where what it opens to is asked for when it opens, None for nothing.
    pub drill: Option<Drill>,
}
/// A row's drilldown, drawn only once the row opens: asked for from `src` (`drill`), asked
/// again while open whenever its `stamp` (what it draws, the clock left out) changes.
pub struct Drill {
    pub src: String,
    pub stamp: u64,
    /// Its steps in its step view's order, space-separated: the row carries them (`data-steps`)
    /// so `[` and `]` step through a closed row's steps as through an open one's.
    pub steps: String,
}
/// What a row's drilldown draws, as it changes: its steps' states, what it said last and when
/// it was heard from.
fn drill_stamp(unit: &UnitView) -> u64 {
    crate::streams::skeleton_hash(&format!(
        "{}|{}|{}|{}",
        said_states(unit),
        unit.last_message,
        unit.changed,
        heard(unit).map_or(0, |h| h as u64)
    ))
}
/// One message of a unit's conversation, as its drilldown draws it.
pub struct Said {
    pub from: String,
    pub to: String,
    pub at: String,
    pub body: String,
}
/// The last messages on a unit's steps' threads, oldest first: its conversation, as a row
/// opens to it.
fn conversation(
    c: &rusqlite::Connection,
    project: ProjectId,
    unit: &UnitView,
) -> sluice_store::Result<Vec<Said>> {
    let threads: Vec<String> = unit
        .steps
        .iter()
        .map(|s| format!("step-{}", s.id))
        .collect();
    let threads = serde_json::to_string(&threads).expect("thread names serialize");
    let mut q = c.prepare_cached(
        "SELECT \"from\",coalesce(\"to\",''),at,body FROM messages WHERE project_id=?1 AND thread IN (SELECT value FROM json_each(?2)) ORDER BY at DESC, id DESC LIMIT ?3",
    )?;
    let mut said: Vec<Said> = q
        .query_map(
            rusqlite::params![project.to_string(), threads, CONVERSATION as i64],
            |r| {
                Ok(Said {
                    from: r.get(0)?,
                    to: r.get(1)?,
                    at: r.get(2)?,
                    body: r.get(3)?,
                })
            },
        )?
        .collect::<Result<_, _>>()?;
    said.reverse();
    Ok(said)
}
/// How many of its last messages a row opens to.
const CONVERSATION: usize = 4;
/// Who a message is from or to, in words, as its unit names it: you, a session, sluice, a
/// step of the unit by its stage, another step by its id.
fn party_words(unit: &UnitView, id: &str) -> String {
    if let Some(party) = ui::Party::of(id) {
        return party.words();
    }
    unit.steps
        .iter()
        .find(|s| s.id.as_str() == id)
        .map(|s| unit.step_name(s))
        .unwrap_or_else(|| id.to_owned())
}
/// What a row of At work opens to, with no panel: its conversation, a message each with its
/// sender's avatar (the owner a circle, the orchestrator and a worker agent squares told apart),
/// then when it was last heard from and its ways on: Message · Unit page · Open step.
fn drill_html(view: &ProjectView, unit: &UnitView, said: &[Said]) -> String {
    let mut out = String::new();
    for m in said {
        // a worker is its unit: every step of it wears the unit's mark
        let ours = unit.steps.iter().any(|s| s.id.as_str() == m.from);
        let seed = if ours {
            unit.id.as_str()
        } else {
            m.from.as_str()
        };
        let agent = !matches!(ui::Actor::of(&m.from), ui::Actor::Owner | ui::Actor::Sluice);
        let to = if m.to.is_empty() {
            String::new()
        } else {
            format!("to {} · ", esc(&party_words(unit, &m.to)))
        };
        let long = m.body.chars().count() > 900;
        let body = if long {
            crate::markdown::cut(&m.body, 900)
        } else {
            m.body.clone()
        };
        let _ = write!(
            out,
            "<div class=\"msg\"><div class=\"by\">{av}<b{cls}>{who}</b><span>{to}{at}, {ago}</span></div><div class=\"body md\">{body}</div>{more}</div>",
            av = ui::avatar(seed, ui::AvSize::Md, true),
            cls = if agent { " class=\"agent\"" } else { "" },
            who = esc(&party_words(unit, &m.from)),
            at = ui::clock(&m.at),
            ago = ui::ago(&m.at),
            body = ui::refs(crate::markdown::render(&body).as_str()),
            more = if long {
                format!(
                    "<a class=\"more\" href=\"{}\">Read it whole</a>",
                    esc(&unit.last_thread_href(&view.project.id))
                )
            } else {
                String::new()
            },
        );
    }
    out.push_str("<div class=\"ro-foot\"><span>");
    let quiet = unit.steps.iter().any(|s| s.shown() == Shown::Quiet);
    match heard(unit).or_else(|| last_end(unit)) {
        Some(at) => {
            let at = super::rfc3339(at as u64);
            let _ = write!(
                out,
                "{} {} ({})",
                if quiet {
                    "Not heard from since"
                } else {
                    "Last heard"
                },
                ui::clock(&at),
                ui::ago(&at)
            );
        }
        None => out.push_str("Not begun"),
    }
    out.push_str("</span><span class=\"grow\"></span>");
    if let Some(step) = current(unit) {
        let _ = write!(
            out,
            "<a href=\"{}\">Message</a><span aria-hidden=\"true\">·</span>",
            esc(&step.thread_href())
        );
    }
    let _ = write!(
        out,
        "<a href=\"{}\">Unit page</a>",
        esc(&unit_href(view, unit))
    );
    if let Some(step) = current(unit) {
        out.push_str(&ui::link_with(
            ui::Btn::Outlined,
            &step.href(),
            "Open step",
            &format!(" data-step=\"{}\"", esc(step.id.as_str())),
        ));
    }
    out.push_str("</div>");
    out
}
/// A row of At work: a disclosure whose summary is the row and whose body is its conversation,
/// opening in place. Its body is asked for when it opens (`drill`): the stream sends the row,
/// never what a closed row would open to; while open, a new stamp on the row asks for it again.
/// A change in how its steps stand flashes it once (`board.js`, `data-flash`); it never moves.
pub fn roster_row(row: &RowParts<'_>) -> String {
    let (on_open, drill) = match &row.drill {
        Some(d) => (
            format!(
                " data-steps=\"{steps}\" data-on:toggle=\"el.open &amp;&amp; @get('{src}')\"",
                steps = esc(&d.steps),
                src = esc(&d.src)
            ),
            format!(
                "<div id=\"drill-{id}\" class=\"ro-drill\" data-ignore-morph></div><i hidden data-init=\"el.parentElement.open &amp;&amp; @get('{src}?at={stamp:x}')\"></i>",
                id = esc(row.id),
                src = esc(&d.src),
                stamp = d.stamp
            ),
        ),
        None => (String::new(), "<div class=\"ro-drill\"></div>".to_owned()),
    };
    format!(
        "<details id=\"u-{id}\" class=\"ro-u{wait}\" data-unit=\"{id}\"{states} data-preserve-attr=\"open data-flash\"{on_open}><summary class=\"ro-r\"><span class=\"engine\"{runs}>{engine}</span><span class=\"ro-main\"><span class=\"ro-t\" title=\"{plain}\">{title}{own}</span><span class=\"track\">{track}</span><span class=\"ro-said\">{said}</span></span>{pulse}<span class=\"ro-fold-i\" aria-hidden=\"true\">{chev}</span></summary>{drill}</details>",
        id = esc(row.id),
        wait = if row.waiting { " is-wait" } else { "" },
        states = if row.said_states.is_empty() {
            String::new()
        } else {
            format!(" data-said=\"{}\"", esc(row.said_states))
        },
        runs = if row.runs_on.is_empty() {
            " aria-hidden=\"true\"".to_owned()
        } else {
            format!(" title=\"Runs on {}\"", esc(row.runs_on))
        },
        engine = esc(&ui::cut(row.engine, 10)),
        plain = esc(row.title),
        title = esc(row.title),
        own = if row.owner.is_empty() {
            String::new()
        } else {
            format!(" <span class=\"own\">{}</span>", esc(row.owner))
        },
        track = row.track,
        said = row.said,
        pulse = row.pulse,
        chev = icon(Icon::ChevronDown, 16, ""),
    )
}
/// Each of a unit's steps and how it reads, as `data-said` carries them for `board.js` to say a
/// step's new state to a screen reader ("a-build succeeded|a-review skipped").
fn said_states(unit: &UnitView) -> String {
    unit.steps
        .iter()
        .map(|s| format!("{} {}", s.id, s.shown().word()))
        .collect::<Vec<_>>()
        .join("|")
}
/// When a unit at work was last heard from: its running step's last write, else its last
/// message, else its run's start.
fn heard(unit: &UnitView) -> Option<f64> {
    let running = unit.steps.iter().filter(|s| s.running());
    let wrote = running
        .clone()
        .filter_map(|s| epoch(&s.active_at))
        .reduce(f64::max);
    let said = epoch(&unit.changed).filter(|_| !unit.last_message.trim().is_empty());
    let started = running
        .filter_map(|s| s.timing.as_ref().and_then(|t| epoch(&t.started)))
        .reduce(f64::max);
    match (wrote, said) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b).or(started),
    }
}
/// What a unit said last, on one line: its latest message, or its live progress when that is
/// newer; another's message to it after who sent it.
fn said_html(unit: &UnitView) -> String {
    let progress = unit
        .steps
        .iter()
        .filter_map(|s| s.progress.as_ref().filter(|p| p.live))
        .max_by(|a, b| a.at.cmp(&b.at));
    let message_at = if unit.last_message.trim().is_empty() {
        ""
    } else {
        unit.changed.as_str()
    };
    if let Some(p) = progress.filter(|p| p.at.as_str() > message_at) {
        let words: Vec<String> = p
            .shown()
            .into_iter()
            .filter(|f| !f.value.trim().is_empty() && !f.long())
            .take(3)
            .map(|f| format!("{} {}", f.name.replace(['_', '-'], " "), f.value.trim()))
            .collect();
        if !words.is_empty() {
            return esc(&words.join(" · "));
        }
    }
    if message_at.is_empty() {
        return String::new();
    }
    let text = ui::refs(&esc(&crate::markdown::cut(
        &crate::markdown::plain(&unit.last_message),
        240,
    )));
    // its sender's avatar first: a step of its own wears the unit's mark and needs no name
    let own = unit.steps.iter().any(|s| s.id.as_str() == unit.last_from);
    if own {
        format!(
            "{}{text}",
            ui::avatar(unit.id.as_str(), ui::AvSize::Sm, true)
        )
    } else {
        format!(
            "{}<b>{}</b> {text}",
            ui::avatar(&unit.last_from, ui::AvSize::Sm, true),
            unit.sender_html()
        )
    }
}
/// Why a waiting unit waits, in words: what it waits for in other units, or what else holds
/// its first step not done.
fn why_waiting(unit: &UnitView) -> String {
    if let Some((_, waits)) = unit.all_waits() {
        let named: Vec<String> = waits
            .iter()
            .take(2)
            .map(|w| {
                format!(
                    "<a href=\"{}\"{}>{}</a>{}",
                    esc(&w.href),
                    if w.opens.is_empty() {
                        String::new()
                    } else {
                        format!(" data-opens=\"{}\"", esc(&w.opens))
                    },
                    esc(&w.name.text(48)),
                    if w.note.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", esc(&w.note))
                    }
                )
            })
            .collect();
        let more = waits.len().saturating_sub(2);
        return format!(
            "behind {}{}",
            named.join(" and "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        );
    }
    let Some(step) = unit
        .steps
        .iter()
        .find(|s| s.standing().spec().band != Band::Done)
    else {
        return String::new();
    };
    esc(&match step.shown() {
        Shown::Paused => step.hold_words(),
        Shown::Queued => step
            .queued
            .first()
            .map(|q| format!("Queued: {q}"))
            .unwrap_or_default(),
        Shown::Blocked => "Held up by a failure upstream.".into(),
        _ => step.waits.first().cloned().unwrap_or_default(),
    })
}
/// A done unit's result in one line: a commit it names (its sha, its card on hover), then
/// the output that explains it (its summary) or else its last message; `None` when it left
/// neither. The fields it set otherwise are its page's.
fn result_line(unit: &UnitView) -> Option<String> {
    let sha = unit
        .steps
        .iter()
        .rev()
        .flat_map(|s| s.outputs.iter().filter(|f| f.available))
        .find(|f| ui::is_sha(f.value.trim()))
        .map(|f| f.value.trim().to_owned());
    let said = unit
        .steps
        .iter()
        .rev()
        .find_map(|step| step.overview_outputs().first().map(|f| f.value.clone()))
        .filter(|v| ui::ValueSet::of_text(v) == ui::ValueSet::Prose)
        .or_else(|| {
            let m = unit.last_message.trim();
            (!m.is_empty()).then(|| m.to_owned())
        })
        .map(|said| crate::markdown::cut(&crate::markdown::plain(&said), 200))
        // with no words, its short outputs are its result ("entries 14 · photos 9")
        .or_else(|| {
            let mut facts: Vec<String> = vec![];
            for field in unit.steps.iter().rev().flat_map(|s| s.outputs.iter()) {
                let value = field.value.trim();
                if !field.available
                    || facts.len() == 2
                    || ui::is_sha(value)
                    || ui::ValueSet::of_text(value) != ui::ValueSet::Show
                    || value.is_empty()
                {
                    continue;
                }
                facts.push(format!("{} {}", field.name.replace(['_', '-'], " "), value));
            }
            (!facts.is_empty()).then(|| facts.join(" · "))
        });
    match (sha, said) {
        (None, None) => None,
        // a summary that names its commit already says it
        (Some(sha), Some(said)) if said.contains(&sha[..sha.len().min(10)]) => {
            Some(ui::refs(&esc(&said)))
        }
        (Some(sha), said) => Some(format!(
            "{}{}",
            ui::sha_ref(&sha),
            said.map(|s| format!(" {}", ui::refs(&esc(&s))))
                .unwrap_or_default()
        )),
        (None, Some(said)) => Some(ui::refs(&esc(&said))),
    }
}
/// Finished, drawn one way on the plan and on home: a hairline row a unit that has something to
/// say (its commit, its summary or last message, else its short outputs), then the rest, which finished with nothing to say, in one line naming them.
pub fn finished_list(view: &ProjectView, units: &[&UnitView]) -> String {
    let mut rows = String::new();
    let mut quiet: Vec<String> = vec![];
    for unit in units {
        match result_line(unit) {
            Some(result) => rows.push_str(&finished_row(
                unit.finished(),
                took(unit),
                &unit_href(view, unit),
                unit.whole_heading(),
                &result,
                &said_states(unit),
            )),
            None => quiet.push(format!(
                "<a{} href=\"{}\">{}</a>",
                match said_states(unit) {
                    s if s.is_empty() => String::new(),
                    s => format!(" data-said=\"{}\"", esc(&s)),
                },
                esc(&unit_href(view, unit)),
                esc(title(unit))
            )),
        }
    }
    let mut out = String::new();
    if !rows.is_empty() {
        let _ = write!(out, "<ul class=\"fin\">{rows}</ul>");
    }
    if !quiet.is_empty() {
        let _ = write!(
            out,
            "<p class=\"fin-quiet\">{} finished with nothing to say: {}</p>",
            if rows.is_empty() {
                ui::count(quiet.len(), "unit", "units")
            } else {
                format!("{} more", quiet.len())
            },
            quiet.join(", ")
        );
    }
    out
}

/// The view switch as `/_ui` shows it, `chosen` the view filled.
pub fn view_switch_sample(chosen: Option<&str>) -> String {
    view_switch("#", chosen)
}
/// The Plan · Both · Board switch in the bar: three icons, each named by its title and label, a
/// link each, which the server answers with the page in that view and keeps in the project's
/// cookie. The chosen one is filled; with none chosen, the stylesheet fills the one its width
/// shows.
fn view_switch(href: &str, chosen: Option<&str>) -> String {
    let links: String = [
        ("plan", "Plan", Icon::Workflow),
        ("both", "Both", Icon::Columns2),
        ("board", "Board", Icon::LayoutDashboard),
    ]
    .into_iter()
    .map(|(key, word, glyph)| {
        format!(
            "<a href=\"{h}?view={key}\" data-view-tab=\"{key}\" aria-current=\"{on}\" data-preserve-attr=\"aria-current\" title=\"{word}\" aria-label=\"{word}\">{i}</a>",
            h = esc(href),
            on = chosen == Some(key),
            i = icon(glyph, 18, ""),
        )
    })
    .collect();
    format!("<nav class=\"view-switch\" aria-label=\"View\">{links}</nav>")
}
/// "a", "a and b", "a, b and c".
fn join(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
/// `html` marked as a region a stream patches alone (`streams::PatchRegion`): its element's id.
fn region(id: &str, html: &str) -> String {
    format!("<!--r:{id}-->{html}<!--/r:{id}-->")
}

#[derive(Template)]
#[template(path = "project.html")]
struct ProjectTemplate<'a> {
    view: &'a ProjectView,
    plan: &'a Plan<'a>,
    glance: (String, String),
    js_url: String,
    board_js_url: String,
}
/// The page body: the plan's sheet in its pane, the board beside it, the step drawer.
fn body(
    view: &ProjectView,
    plan: &Plan<'_>,
    glance: (String, String),
) -> Result<TrustedHtml, askama::Error> {
    TrustedHtml::from_template(&ProjectTemplate {
        view,
        plan,
        glance,
        js_url: super::asset_url("sluice.js"),
        board_js_url: super::asset_url("board.js"),
    })
}
/// The part of the body a stream patches: all of it before the drawer.
fn board_region(body: &TrustedHtml) -> TrustedHtml {
    let end = body
        .as_str()
        .find(&format!("<{}", ui::DRAWER))
        .expect("owned template drawer boundary");
    TrustedHtml::owned(body.as_str()[..end].trim().to_owned())
}
fn render_error(error: askama::Error) -> PublicError {
    super::board::render_error(error)
}
/// The project's glance as the snapshot reads it (`ProjectView::tab_words`, `favicon`): its
/// tab's words and its favicon's address.
fn glance(view: &ProjectView, shared: &DashboardSnapshot) -> (String, String) {
    let summary = shared
        .projects
        .iter()
        .find(|p| p.id == view.project.id)
        .unwrap_or(&view.project);
    (summary.tab_words(), super::asset_url(summary.favicon()))
}
/// The page body, its frame's parts and the batch its stream patches: the board region, the
/// band's head, the count line and the nav, versioned by that HTML, so the page and its stream
/// agree while nothing shown changes.
pub fn draw(
    view: &ProjectView,
    shared: &DashboardSnapshot,
) -> Result<(TrustedHtml, Frame, RenderedBatch), PublicError> {
    let plan = Plan::new(view);
    let nav = NavView::new(shared, Some(view.project.id), "plan")?;
    let body = body(view, &plan, glance(view, shared)).map_err(render_error)?;
    // the find and the read cursor are the page's signals: its stream reads them when it opens,
    // and the find and Mark as seen reopen it on new ones
    let mut signals = serde_json::Map::new();
    signals.insert("q".into(), view.q.clone().into());
    signals.insert("seen".into(), view.seen.unwrap_or(0).into());
    let frame = Frame {
        head: plan.band(),
        find: plan.find(),
        signals,
    };
    let mut regions = vec![
        PatchRegion::new("project-board", board_region(&body)),
        PatchRegion::new("plan-band", frame.head.clone()),
    ];
    regions.extend(super::nav_regions(&nav, &view.href()));
    let batch = RenderedBatch::new(regions);
    Ok((body, frame, batch))
}
/// The whole page: its title the project's glance, its favicon the glance's.
pub fn render(
    view: &ProjectView,
    shared: &DashboardSnapshot,
    viewer: &Viewer,
) -> Result<TrustedHtml, PublicError> {
    let nav = NavView::new(shared, Some(view.project.id), "plan")?;
    let (body, frame, batch) = draw(view, shared)?;
    let (words, favicon) = glance(view, shared);
    let html = super::render_framed(
        &words,
        &body,
        &nav,
        viewer,
        &view.stream_href(),
        &batch.version,
        &view.href(),
        &frame,
    )
    .map_err(render_error)?;
    // the favicon says what the title says
    Ok(TrustedHtml::owned(html.0.replacen(
        &format!("href=\"{}\"", super::asset_url("favicon.svg")),
        &format!("href=\"{}\"", esc(&favicon)),
        1,
    )))
}

// ---- the plan's own posts --------------------------------------------------------------------

#[derive(Deserialize)]
pub struct NeedsForm {
    /// `dismiss`, `undo` or `retry`.
    pub action: String,
    /// The items, space apart: `stop:<step>`, `quiet:<step>@<stamp>`, `ask:<message>`; for
    /// `retry`, `stop:<step>@<seen>`.
    pub items: String,
    #[serde(default)]
    pub revision: u64,
}
/// `POST /projects/id/<p>/needs`: set items of Needs you aside or back (`NeedsDismiss`: about
/// attention alone, nothing settled), or retry a group's steps. Retry all retries each step as
/// its own Retry would, the first refusal answered as that step's.
pub async fn needs(
    State(state): State<DashboardState>,
    registry: Option<Extension<super::board::Registry>>,
    commands: Option<Extension<step::Commands>>,
    Path(project): Path<ProjectId>,
    headers: HeaderMap,
    Form(form): Form<NeedsForm>,
) -> Response {
    let items: Vec<&str> = form.items.split_whitespace().collect();
    if items.is_empty() || items.len() > 500 {
        return write::failed(
            &headers,
            PublicError::BadRequest {
                message: "name one to 500 items".into(),
            },
        );
    }
    if form.action == "retry" {
        for item in &items {
            let Some((step, seen)) = item
                .strip_prefix("stop:")
                .and_then(|rest| rest.split_once('@'))
            else {
                return write::failed(
                    &headers,
                    PublicError::BadRequest {
                        message: format!("{item} is not a stop to retry"),
                    },
                );
            };
            let Ok(id) = step.parse::<StepId>() else {
                return write::failed(
                    &headers,
                    PublicError::BadRequest {
                        message: format!("{step} is not a step"),
                    },
                );
            };
            let form = ActionForm {
                action: Action::Retry,
                revision: form.revision,
                message: String::new(),
                seen: seen.to_owned(),
            };
            match step::try_action(
                &state,
                commands.clone(),
                registry.as_ref(),
                project,
                &id,
                form,
            )
            .await
            {
                Ok(()) => {}
                Err(Ok(refusal)) => return refusal.answer(&headers),
                Err(Err(e)) => return write::failed(&headers, e),
            }
        }
        return write::done(&headers);
    }
    let dismissed = match form.action.as_str() {
        "dismiss" => true,
        "undo" => false,
        other => {
            return write::failed(
                &headers,
                PublicError::BadRequest {
                    message: format!("{other} is not an action on Needs you"),
                },
            );
        }
    };
    let Some(Extension(commands)) = commands else {
        return write::failed(
            &headers,
            PublicError::Busy {
                message:
                    "Nothing was done: sluice's coordinator is not answering. Try again in a moment."
                        .into(),
                retryable: true,
            },
        );
    };
    let mut marks = vec![];
    for item in items {
        let (key, stamp) = match item.split_once('@') {
            Some((key, stamp)) => match stamp.parse::<i64>() {
                Ok(stamp) => (key, stamp),
                Err(_) => {
                    return write::failed(
                        &headers,
                        PublicError::BadRequest {
                            message: format!("{item} has no stamp"),
                        },
                    );
                }
            },
            None => (item, 0),
        };
        marks.push(NeedsItem {
            key: key.to_owned(),
            stamp,
        });
    }
    if !marks.is_empty() {
        let sent = commands
            .0
            .execute(OwnerCommand {
                project,
                step: None,
                action: if dismissed {
                    Action::SetAside
                } else {
                    Action::Restore
                },
                revision: 0,
                message: String::new(),
                author: "owner",
                items: marks,
            })
            .await;
        if let Err(e) = sent {
            return write::failed(&headers, e);
        }
    }
    write::done(&headers)
}
#[derive(Deserialize)]
pub struct SeenForm {
    /// The finish time (seconds since the epoch) of the last finished unit the page drew.
    pub through: u64,
}
/// `POST /projects/id/<p>/seen`: Mark as seen (or its Undo, `through=0`). This browser's look
/// at the project's finished units moves to `through`, kept in its cookie (`sluice_seen_<p>`)
/// for its next page. To the page's form the answer empties its notice and sets the page's
/// `seen` signal, whose change reopens the stream on the new look (`seen_watch`).
/// A row's drilldown (`roster_row`), asked for as it opens: its unit's steps and foot, drawn
/// in place of what it showed (the row's stream never morphs it).
pub async fn drill(
    State(state): State<DashboardState>,
    registry: Option<axum::Extension<super::board::Registry>>,
    Path((project, unit)): Path<(ProjectId, sluice_model::ids::UnitName)>,
) -> Response {
    let drawn = async {
        let (_, view) =
            super::board::snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?;
        let Some(found) = view.units.iter().find(|u| u.id == unit) else {
            return Ok(None);
        };
        // its conversation, read after the plan's snapshot: a message newer than the plan only
        // shows sooner
        let steps = found.clone();
        let said = state
            .reads
            .clock_snapshot(move |c| conversation(c, project, &steps))
            .await
            .map_err(|e| e.into_public(true))?;
        Ok::<_, sluice_model::error::PublicError>(Some(format!(
            "<div id=\"drill-{id}\" class=\"ro-drill\" data-ignore-morph>{body}</div>",
            id = esc(found.id.as_str()),
            body = drill_html(&view, found, &said)
        )))
    };
    match drawn.await {
        Ok(Some(html)) => write::events([crate::streams::StreamEvent::Elements(
            datastar::prelude::PatchElements::new(html)
                .mode(datastar::prelude::ElementPatchMode::Replace),
        )]),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => super::board::error_response(e),
    }
}
#[derive(Deserialize)]
pub struct RefQuery {
    /// `step:<id>`, `sha:<hex>` or `t:<ticket>`.
    pub r: String,
}
/// `GET /projects/id/<p>/ref?r=…`: a reference's hover card (`ui::refs`), what the plan knows of
/// it: a step's state, its fn and its unit; the steps whose outputs name a commit; the units a
/// ticket names. HTML for `#hover-card`; 404 for a reference of no known kind.
pub async fn reference(
    State(state): State<DashboardState>,
    registry: Option<axum::Extension<super::board::Registry>>,
    Path(project): Path<ProjectId>,
    axum::extract::Query(query): axum::extract::Query<RefQuery>,
) -> Response {
    let Some((kind, key)) = query.r.split_once(':') else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !matches!(kind, "step" | "sha" | "t") || key.is_empty() || key.len() > 200 {
        return StatusCode::NOT_FOUND.into_response();
    }
    let view = match super::board::snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await
    {
        Ok((_, view)) => view,
        Err(e) => return super::board::error_response(e),
    };
    let html = ref_card(&view, kind, key);
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}
/// A reference's card, from the plan as it stands.
fn ref_card(view: &ProjectView, kind: &str, key: &str) -> String {
    let steps = view
        .units
        .iter()
        .flat_map(|u| u.steps.iter().map(move |s| (u, s)));
    // a step a line: its unit's title, its stage, its state and when it last moved
    let line = |unit: &UnitView, step: &StepView| {
        let when = step
            .timing
            .as_ref()
            .map(|t| match t.finished.as_deref() {
                Some(end) => format!(" · {}", ui::ago(end)),
                None => format!(" · for {}", ui::since(&t.started)),
            })
            .unwrap_or_default();
        format!(
            "<li><a href=\"{}\" data-step=\"{}\">{}</a> <span class=\"meta\">{} {}{when}</span></li>",
            esc(&step.href()),
            esc(step.id.as_str()),
            esc(&ui::cut(title(unit), 70)),
            esc(&unit.step_name(step)),
            esc(step.shown().word()),
        )
    };
    match kind {
        "step" => match steps.clone().find(|(_, s)| s.id.as_str() == key) {
            Some((unit, step)) => format!(
                "<p class=\"hc-h\"><code>{id}</code>{state}</p><p class=\"hc-t\">{title}</p><p class=\"meta\"><code>{f}</code> · unit {unit}{when}</p>",
                id = esc(key),
                state = ui::status_pill(step.shown(), &esc(step.shown().word())),
                title = esc(step.heading()),
                f = esc(&step.function),
                unit = esc(unit.id.as_str()),
                when = step
                    .timing
                    .as_ref()
                    .map(|t| match t.finished.as_deref() {
                        Some(end) => format!(" · ended {}", ui::ago(end)),
                        None => format!(" · running {}", ui::since(&t.started)),
                    })
                    .unwrap_or_default(),
            ),
            None => format!(
                "<p class=\"hc-h\"><code>{}</code></p><p class=\"meta\">Not in this plan now.</p>",
                esc(key)
            ),
        },
        "sha" => {
            let named: Vec<String> = steps
                .filter(|(_, s)| {
                    s.outputs
                        .iter()
                        .any(|f| f.available && f.value.trim().starts_with(key))
                })
                .take(4)
                .map(|(u, s)| line(u, s))
                .collect();
            format!(
                "<p class=\"hc-h\"><code>{}</code> <span class=\"meta\">commit</span></p>{}",
                esc(&key[..key.len().min(12)]),
                if named.is_empty() {
                    "<p class=\"meta\">No step of this plan names it in its outputs.</p>".to_owned()
                } else {
                    format!("<ul class=\"hc-l\">{}</ul>", named.join(""))
                }
            )
        }
        _ => {
            let units: Vec<String> = view
                .units
                .iter()
                .filter(|u| {
                    u.whole_heading().contains(key) || u.id.as_str().eq_ignore_ascii_case(key)
                })
                .take(4)
                .filter_map(|u| current(u).map(|s| line(u, s)))
                .collect();
            format!(
                "<p class=\"hc-h\"><b>{}</b></p>{}",
                esc(key),
                if units.is_empty() {
                    "<p class=\"meta\">No unit of this plan names it.</p>".to_owned()
                } else {
                    format!("<ul class=\"hc-l\">{}</ul>", units.join(""))
                }
            )
        }
    }
}
pub async fn mark_seen(
    Path(project): Path<ProjectId>,
    headers: axum::http::HeaderMap,
    Form(form): Form<SeenForm>,
) -> Response {
    let mut response = match super::write::form(&headers) {
        Some(id) => super::write::events([
            super::write::notice(&id, ""),
            crate::streams::StreamEvent::Signals(datastar::prelude::PatchSignals::new(
                serde_json::json!({ "seen": form.through }).to_string(),
            )),
        ]),
        None => StatusCode::NO_CONTENT.into_response(),
    };
    let cookie = format!(
        "{}={}; Path=/; Max-Age=34560000; SameSite=Lax",
        seen_cookie(project),
        form.through
    );
    response.headers_mut().append(
        axum::http::header::SET_COOKIE,
        cookie.parse().expect("an ASCII cookie"),
    );
    response
}
