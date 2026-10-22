//! A project's plan (DESIGN.md, Pages: a project's plan): a briefing, then a roster. The head
//! says in one sentence what needs the owner and how the work stands; **Needs you**, the one
//! list, says each question, stop and silence in a sentence with one action and a quick
//! dismissal; **At work** is a roster of the live units, a row each, grouped by
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

pub(super) fn now() -> f64 {
    crate::clock::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}
fn epoch(at: &str) -> Option<f64> {
    super::timestamp(at).map(|t| t as f64)
}
/// The step that says how its unit stands: the first whose state is the unit's.
pub(super) fn current(unit: &UnitView) -> Option<&StepView> {
    let shown = unit.shown();
    unit.steps
        .iter()
        .find(|s| s.standing() == shown)
        .or_else(|| unit.steps.first())
}
/// Where a unit's name leads: a loose step's own page, else the unit's.
pub(super) fn unit_href(view: &ProjectView, unit: &UnitView) -> String {
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
pub(super) fn took(unit: &UnitView) -> f64 {
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
/// A recipe's rows, long runs, or plain rows with no name or fold.
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
    /// Live units that have not started and wait on earlier work.
    later: usize,
    /// Units at work, including long runs.
    at_work: usize,
    /// The done units, the latest finished first.
    done: Vec<&'a UnitView>,
}

/// A long run: a running step of no recipe, alone in its unit, that has run `LONG_RUN` or
/// publishes its results as it goes (`step_progress`). The plan lists it in the last At work group.
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

/// Home's grouping changes at these deadlines even with no store write. The times drawn
/// inside a card tick in the browser; retaining it needs no periodic full-board reload.
pub(super) fn next_brief_change(view: &ProjectView) -> u64 {
    let now = now();
    view.units
        .iter()
        .filter(|u| !u.done)
        .flat_map(|unit| {
            let long = match unit.steps.as_slice() {
                [step] if unit.recipe.is_empty() && step.running() => step
                    .timing
                    .as_ref()
                    .and_then(|t| epoch(&t.started))
                    .map(|at| at + LONG_RUN),
                _ => None,
            };
            let quiet = is_long(unit)
                .then(|| long_heard(unit).map(|at| at + view.long_quiet))
                .flatten();
            let wait = last_end(unit).map(|at| at + WAIT_LONG);
            [long, quiet, wait].into_iter().flatten()
        })
        .filter(|at| *at > now)
        .map(|at| at.ceil() as u64)
        .min()
        .unwrap_or(u64::MAX)
}

impl<'a> Plan<'a> {
    pub fn new(view: &'a ProjectView) -> Self {
        let now = now();
        let live = |unit: &&UnitView| !unit.done;
        let mut done: Vec<&UnitView> = view.units.iter().filter(|u| u.done).collect();
        done.sort_by(|a, b| b.finished().cmp(a.finished()).then(a.pos.cmp(&b.pos)));
        // a find, a chain or a recipe's units asked for: each one a row, none only counted
        let narrowed = !view.q.is_empty() || !view.root.is_empty() || !view.recipe.is_empty();
        let mut groups: Vec<Group> = vec![];
        let mut long_rows: Vec<Row> = vec![];
        let (mut later, mut at_work) = (0, 0);
        for unit in view.units.iter().filter(live) {
            let shown = unit.shown();
            let band = shown.spec().band;
            if matches!(shown, Shown::Failed | Shown::Cancelled | Shown::Stale) {
                continue;
            }
            let row = if band == Band::Running {
                at_work += 1;
                Row {
                    unit,
                    heard: if is_long(unit) {
                        long_heard(unit)
                    } else {
                        heard(unit)
                    },
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
            if is_long(unit) {
                long_rows.push(row);
                continue;
            }
            match groups.iter_mut().find(|g| g.recipe == unit.recipe) {
                Some(group) => group.rows.push(row),
                None => groups.push(Group {
                    name: unit.recipe.clone(),
                    recipe: unit.recipe.clone(),
                    rows: vec![row],
                }),
            }
        }
        // Recipe groups first, then other units, with long runs last.
        groups.sort_by_key(|g| g.recipe.is_empty());
        if !long_rows.is_empty() {
            groups.push(Group {
                name: "Long runs".into(),
                recipe: String::new(),
                rows: long_rows,
            });
        }
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
        let mut plan = Plan {
            view,
            now,
            needs: vec![],
            groups,
            later,
            at_work,
            done,
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
            .filter(|u| !u.done && !is_long(u))
            .flat_map(|u| {
                u.steps
                    .iter()
                    .filter(|s| s.shown() == Shown::Quiet)
                    .filter_map(move |s| epoch(&s.active_at).map(|at| (u, s, at, false)))
            })
            .collect();
        for unit in self.view.units.iter().filter(|u| is_long(u)) {
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
        let (act, form) = if step.retryable() {
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
            .filter(|s| s.retryable())
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
    /// With no live items, only recent Undo lines remain.
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
        if live.is_empty() && aside.is_empty() {
            return "<div id=\"plan-needs\" hidden></div>".into();
        }
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
            "<section id=\"plan-needs\" class=\"pb-sec\" {}>{}",
            if live.is_empty() {
                ""
            } else {
                " aria-labelledby=\"needs\""
            },
            if live.is_empty() {
                String::new()
            } else {
                head.html().to_string()
            }
        );
        if !live.is_empty() {
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

    /// A short summary of owner attention, work and completion counts.
    fn sentence(&self) -> String {
        // what is true, whatever the owner set aside: setting aside is about attention alone
        // (a cancel settled as done, its step's Dismiss, leaves its unit done)
        let standing: Vec<&Need> = self
            .needs
            .iter()
            .filter(|n| {
                n.aside.is_none()
                    && !self
                        .view
                        .units
                        .iter()
                        .any(|u| u.done && u.id.as_str() == n.unit)
            })
            .collect();
        let asks = standing.iter().filter(|n| n.mark == Mark::Ask).count();
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
        if total == 0 {
            return "No units yet.".into();
        }
        first.push(format!("{} at work", self.at_work));
        if self.later > 0 {
            first.push(format!("{} waiting", self.later));
        }
        let done = format!("{} of {total} done", self.done.len());
        first.push(if self.done.is_empty() {
            done
        } else {
            format!("<a href=\"{}/history\">{done}</a>", esc(&self.view.href()))
        });
        first.join(" · ")
    }
    // ---- the plan in brief, as home's card draws it ----------------------------------------------

    /// The head's sentence, as its card says it.
    pub fn sentence_html(&self) -> String {
        self.sentence()
    }
    /// The plan's short summary, led by what needs the owner.
    fn lead(&self) -> String {
        self.sentence()
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
            if group.name.is_empty() {
                for row in &group.rows {
                    let _ = write!(
                        out,
                        "<li><a href=\"{}\">{}</a> · {}</li>",
                        esc(&unit_href(self.view, row.unit)),
                        esc(title(row.unit)),
                        row_counts(&[row])
                    );
                }
                continue;
            }
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
        let details = details.html("Order", &TrustedHtml::owned(self.order_menu()));
        let details = if self.view.panel.is_some() {
            details.html(
                "View",
                &TrustedHtml::owned(view_links(&self.view.href(), self.view.view)),
            )
        } else {
            details
        };
        let head = ui::lead_head(
            &project.name,
            &TrustedHtml::owned(self.lead()),
            &details.menu(&project.name),
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

    /// Needs you and At work, with a quiet way to History and a done index on demand.
    pub fn sheet(&self) -> TrustedHtml {
        let mut out = String::from("<div class=\"plan-sheet\">");
        out.push_str(&region("plan-needs", &self.needs_html()));
        out.push_str(&region("plan-roster", &self.roster_html()));
        let today = super::history::finished_today(self.view);
        if today > 0 {
            let unseen = super::history::has_unseen(self.view);
            out.push_str(&format!("<p class=\"plan-history meta\">{today} finished today{} · <a href=\"{}/history\">History →</a></p>", if unseen { " <span class=\"history-new\" aria-label=\"Finished since you looked\">·</span>" } else { "" }, esc(&self.view.href())));
        }
        out.push_str("</div>");
        if !self.done.is_empty()
            && (!self.view.q.is_empty()
                || !self.view.recipe.is_empty()
                || !self.view.root.is_empty()
                || self.view.show == "done")
        {
            out.push_str(&region("plan-done", &self.done_html()));
        } else {
            out.push_str(&region(
                "plan-done",
                "<section id=\"plan-done\" hidden></section>",
            ));
        }
        TrustedHtml::owned(out)
    }

    // ---- At work -------------------------------------------------------------------------------

    /// At work: its name and counts, recipe groups, plain rows and long runs.
    fn roster_count(&self) -> String {
        let rows: usize = self.groups.iter().map(|g| g.rows.len()).sum();
        // counted as the summary counts them: a blocked or queued unit waits, it is not at work
        let waiting: usize = self
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .filter(|r| r.waiting)
            .count();
        match (rows - waiting, waiting) {
            (0, 0) => String::new(),
            (0, w) => format!("{w} waiting"),
            (n, 0) => ui::count(n, "unit", "units"),
            (n, w) => format!("{} · {w} waiting", ui::count(n, "unit", "units")),
        }
    }
    fn roster_html(&self) -> String {
        let rows: usize = self.groups.iter().map(|g| g.rows.len()).sum();
        let counts = self.roster_count();
        let counts = region(
            "ro-counts",
            &format!(
                "<span data-count=\"rosterCount\" id=\"ro-counts\" class=\"n\" data-text__root=\"$rosterCount\">{}</span>",
                esc(&counts)
            ),
        );
        // an engine's chip where any row runs on one: the column is the agents'
        let engined = self
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .any(|r| current(r.unit).is_some_and(|s| s.badge() != "fn"));
        let left = format!("<div class=\"aw-left\"><h2 id=\"at-work\">At work</h2>{counts}</div>");
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
                "<div class=\"ro{}\"><div class=\"aw-head\">{left}</div>",
                if engined { "" } else { " no-engine" }
            );
            let draw = |out: &mut String, rows: &[&Row<'_>]| {
                for row in rows {
                    out.push_str(&region(
                        &format!("u-{}", row.unit.id),
                        &self.row_html(row, engined),
                    ));
                }
            };
            for group in &self.groups {
                if group.name.is_empty() {
                    draw(&mut out, &group.rows.iter().collect::<Vec<_>>());
                    continue;
                }
                // a big roster shows a group's remarkable rows first and folds the rest to one
                // line ("+21 running · each heard from in the last 28m"), opening in place; a
                // group with nothing remarkable folds whole to its own line
                let (marked, healthy): (Vec<&Row<'_>>, Vec<&Row<'_>>) =
                    group.rows.iter().partition(|r| self.remarkable(r));
                let fold = rows > FOLD_ROSTER
                    && healthy.len() > FOLD_GROUP
                    && self.view.recipe.is_empty()
                    && self.view.q.is_empty();
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
                let expanded = self.view.folds.get(&group_id).copied().unwrap_or(!whole);
                let _ = write!(out, "<!--r:{group_id}-->");
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
                    "<details class=\"ro-g\" id=\"{group_id}\" data-plan-fold=\"{group_id}\" data-roster-fold data-fold-loaded=\"{}\" data-preserve-attr=\"open\"{}><summary class=\"ro-group\">{}<b>{name}</b>{counts}</summary>",
                    expanded,
                    if !expanded { "" } else { " open" },
                    icon(Icon::ChevronRight, 16, "chev"),
                );
                if !expanded {
                    let _ = write!(out, "</details><!--/r:{group_id}-->");
                    continue;
                }
                // a recipe whose view does not check: said once
                if let Some(error) = group
                    .rows
                    .first()
                    .and_then(|r| self.view.names.recipe_of(r.unit.id.as_str()))
                    .and_then(|r| r.view_error())
                {
                    out.push_str(super::unit_view::broken(error).as_str());
                }
                if fold && !whole {
                    draw(&mut out, &marked);
                    let _ = write!(
                        out,
                        "<details class=\"qf ro-fold\" id=\"{group_id}-quiet\" data-plan-fold=\"{group_id}-quiet\" data-roster-fold data-fold-loaded=\"{}\" data-preserve-attr=\"open\"{}><summary>{}</summary>",
                        self.view
                            .folds
                            .get(&format!("{group_id}-quiet"))
                            .copied()
                            .unwrap_or(false),
                        if self
                            .view
                            .folds
                            .get(&format!("{group_id}-quiet"))
                            .copied()
                            .unwrap_or(false)
                        {
                            " open"
                        } else {
                            ""
                        },
                        region(
                            &format!("{group_id}-quiet-counts"),
                            &format!(
                                "<span id=\"{group_id}-quiet-counts\">+{}{}</span>",
                                row_counts(&healthy),
                                heard(&healthy)
                            )
                        )
                    );
                    if self
                        .view
                        .folds
                        .get(&format!("{group_id}-quiet"))
                        .copied()
                        .unwrap_or(false)
                    {
                        draw(&mut out, &healthy);
                    }
                    out.push_str("</details>");
                } else {
                    draw(&mut out, &group.rows.iter().collect::<Vec<_>>());
                }
                let _ = write!(out, "</details><!--/r:{group_id}-->");
            }
            out.push_str("</div>");
        }
        out.push_str("</section>");
        out
    }
    /// Order choices inside the page menu, radio items that
    /// keep this browser's choice in the project's cookie and reopen the stream on it.
    fn order_menu(&self) -> String {
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
            "<div class=\"order-options\" role=\"menu\" aria-label=\"Order rows by\">{items}</div>"
        )
    }

    /// A row a big roster keeps in sight: a silence, a wait of an hour or more since its last
    /// run, or a unit with an open item in Needs you.
    fn remarkable(&self, row: &Row<'_>) -> bool {
        let unit = row.unit;
        (!is_long(unit) && unit.steps.iter().any(|s| s.shown() == Shown::Quiet))
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
        let mut track = step.map(|s| {
            let name = esc(&unit.step_name(s));
            let shown = if is_long(unit) { Shown::Running } else { s.shown() };
            let words = if shown == Shown::Paused {
                format!("paused at {name}")
            } else if shown == Shown::Quiet {
                format!("{name} quiet {}", ui::since(&s.active_at))
            } else if s.running() {
                format!("{name} {}", s.timing.as_ref().map(|t| ui::since(&t.started).to_string()).unwrap_or_default())
            } else { format!("{name} next") };
            format!("<span class=\"current-stage {}\" data-state=\"{}\"><a href=\"{}\" data-step=\"{}\">{words}</a></span>", match shown { Shown::Running => "run", Shown::Quiet => "quiet", Shown::Paused => "paused", _ => "" }, shown.key(), esc(&s.href()), esc(s.id.as_str()))
        }).unwrap_or_default();
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
            let quiet = !is_long(unit) && unit.steps.iter().any(|s| s.shown() == Shown::Quiet);
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
                        "<span data-heard=\"{heard:.0}\" class=\"heard{}\">· heard {}</span>",
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
            owner: &if unit.owner == self.view.fallback
                || self
                    .groups
                    .iter()
                    .flat_map(|g| &g.rows)
                    .all(|r| r.unit.owner == unit.owner)
            {
                String::new()
            } else {
                unit.owner_words()
            },
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
        if is_long(unit)
            && let Some(progress) = unit.steps.first().and_then(|s| s.progress.as_ref())
        {
            return esc(&progress
                .shown()
                .into_iter()
                .filter(|f| !f.value.trim().is_empty() && !f.long())
                .take(3)
                .map(|f| format!("{} {}", f.name.replace(['_', '-'], " "), f.value.trim()))
                .collect::<Vec<_>>()
                .join(" · "));
        }
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
                quiet: !is_long(unit) && running.iter().any(|s| s.shown() == Shown::Quiet),
            },
            self.now,
        )
    }

    // ---- the done index ------------------------------------------------------------------------

    /// Finished work lives in History; legacy filters retain a link and count.
    fn done_html(&self) -> String {
        format!(
            "<section id=\"plan-done\" class=\"meta\"><a href=\"{}/history\">{} finished units · History →</a></section>",
            esc(&self.view.href()),
            self.done.len()
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
        .filter(|r| {
            !r.waiting && !is_long(r.unit) && r.unit.steps.iter().any(|s| s.shown() == Shown::Quiet)
        })
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
    let quiet = !is_long(unit) && unit.steps.iter().any(|s| s.shown() == Shown::Quiet);
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
                " data-steps=\"{steps}\" data-on:toggle=\"el.open &amp;&amp; @get('{src}?at=' + el.querySelector('[data-drill-stamp]').dataset.drillStamp)\"",
                steps = esc(&d.steps),
                src = esc(&d.src)
            ),
            format!(
                "<!--r:stamp-{id}--><i id=\"stamp-{id}\" hidden data-drill-stamp=\"{stamp:x}\"></i><!--/r:stamp-{id}--><div id=\"drill-{id}\" class=\"ro-drill\" data-ignore-morph></div>",
                id = esc(row.id),
                stamp = d.stamp,
            ),
        ),
        None => (String::new(), "<div class=\"ro-drill\"></div>".to_owned()),
    };
    format!(
        "<details id=\"u-{id}\" class=\"ro-u{wait}\" data-unit=\"{id}\"{states} data-preserve-attr=\"open data-flash\"{on_open}><summary class=\"ro-r\"><span class=\"engine\"{runs}>{engine}</span><span class=\"ro-main\"><span class=\"ro-t\" title=\"{plain}\">{title}{own}</span><!--r:track-{id}--><span id=\"track-{id}\" class=\"track\">{track}</span><!--/r:track-{id}--><!--r:said-{id}--><span id=\"said-{id}\" class=\"ro-said\">{said}</span><!--/r:said-{id}--></span><!--r:pulse-{id}--><span id=\"pulse-{id}\" class=\"ro-pulse-region\">{pulse}</span><!--/r:pulse-{id}--><span class=\"ro-fold-i\" aria-hidden=\"true\">{chev}</span></summary>{drill}</details>",
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
/// Text choices inside the project's page menu, preserving the per-project view cookie.
pub fn view_links(href: &str, chosen: Option<&str>) -> String {
    let links: String = [
        ("plan", "Plan"),
        ("both", "Plan and board"),
        ("board", "Board"),
    ]
    .into_iter()
    .map(|(key, word)| {
        format!(
            "<a href=\"{}?view={key}\" data-view-tab=\"{key}\" aria-current=\"{}\">{word}</a>",
            esc(href),
            chosen == Some(key)
        )
    })
    .collect();
    format!("<div class=\"view-options\">{links}</div>")
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
    // The find reopens the stream as it is typed.
    let mut signals = serde_json::Map::new();
    signals.insert("q".into(), view.q.clone().into());
    signals.insert("rosterCount".into(), plan.roster_count().into());
    signals.insert("folds".into(), serde_json::json!(view.folds));
    let frame = Frame {
        head: plan.band(),
        find: plan.find(),
        signals,
    };
    let mut regions = vec![
        PatchRegion::new("project-board", board_region(&body)),
        PatchRegion::new("plan-band", frame.head.clone()),
        PatchRegion::signals(
            "roster-count",
            serde_json::json!({"rosterCount":plan.roster_count()}),
        ),
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
