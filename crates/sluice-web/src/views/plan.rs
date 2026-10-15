//! A project's plan (DESIGN.md, Pages: a project's plan): a briefing, then a roster. The head
//! says in one sentence what needs the owner and how the work stands; **Needs you**, the one
//! raised well, says each question, stop and silence in a sentence with one action and a quick
//! dismissal; **Finished since you looked** lists what finished since this browser last looked
//! with what it produced; **At work** is a roster of the live units, a row each, grouped by
//! recipe and ordered by how long since each was heard from, with its activity over the last
//! twelve hours on a log scale. A row opens in place to its steps (`unit_steps`).
//!
//! Generic by rule: every word comes from the status table, the recipe, the failure's own
//! reading or what the project's steps said and reported; the badge from what a step runs on.
use super::board::{ProjectView, SAID_WINDOW, UnitView};
use super::icons::{Icon, icon};
use super::step::{self, Action, ActionForm, OwnerCommand, StepView};
use super::ui::{self, Mark, Shown, esc};
use super::{DashboardSnapshot, DashboardState, Frame, NavView, TrustedHtml, Viewer, write};
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
        "WallCap" => "they hit their wall-clock cap",
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

/// One item of Needs you: a sentence, its one action and what its dismissal sets aside.
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
    /// Its sentence, HTML.
    sentence: String,
    /// A group's list of its units, HTML ("" for one).
    more: String,
    /// Its one action, HTML.
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
/// A group of rows: a recipe's, the units of no recipe, the long runs.
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
    /// Units at work (running).
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

        // the roster: every unit at work, and every waiting unit that has begun
        let is_long = |unit: &UnitView| match unit.steps.as_slice() {
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
        };
        // a find, a chain or a recipe's units asked for: each one a row, none only counted
        let narrowed = !view.q.is_empty() || !view.root.is_empty() || !view.recipe.is_empty();
        let mut groups: Vec<Group> = vec![];
        let mut long: Vec<Row> = vec![];
        let (mut later, mut at_work, mut quiet) = (0, 0, 0);
        for unit in view.units.iter().filter(live) {
            let shown = unit.shown();
            let band = shown.spec().band;
            if matches!(shown, Shown::Failed | Shown::Cancelled | Shown::Stale) {
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
            if is_long(unit) {
                long.push(row);
                continue;
            }
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
        // a unit of no recipe in its own group after the recipes'; the long runs last
        groups.sort_by_key(|g| g.recipe.is_empty());
        if !long.is_empty() {
            groups.push(Group {
                name: "Long runs".into(),
                recipe: String::new(),
                rows: long,
            });
        }
        // running first, the longest silent first; then waiting, in plan order
        for group in &mut groups {
            group.rows.sort_by(|a, b| {
                a.waiting
                    .cmp(&b.waiting)
                    .then(
                        a.heard
                            .unwrap_or(f64::MAX)
                            .total_cmp(&b.heard.unwrap_or(f64::MAX)),
                    )
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
    /// sharing a kind together), then the silences, the longest first.
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
            let who = match unit {
                Some(unit) => format!(
                    "<a href=\"{}\"{}>{}</a>",
                    esc(&ask.href(&project)),
                    if ask.step.is_empty() {
                        String::new()
                    } else {
                        format!(" data-opens=\"{}\"", esc(&ask.step))
                    },
                    esc(title(unit))
                ),
                None => ask.sender_html(),
            };
            let key = format!("ask:{}", ask.message);
            out.push(Need {
                mark: Mark::Ask,
                unit: unit.map(|u| u.id.to_string()).unwrap_or_default(),
                id: format!("n-ask-{}", ask.message),
                sentence: format!("<b>{who}</b> asks: {}", esc(&ask.title)),
                more: String::new(),
                act: ui::link_with(
                    ui::Btn::Primary,
                    &ask.href(&project),
                    "Answer",
                    &if ask.step.is_empty() {
                        String::new()
                    } else {
                        format!(" data-opens=\"{}\"", esc(&ask.step))
                    },
                ),
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
        // the silences, the longest first
        let mut quiet: Vec<(&UnitView, &StepView, f64)> = self
            .view
            .units
            .iter()
            .filter(|u| !u.done)
            .flat_map(|u| {
                u.steps
                    .iter()
                    .filter(|s| s.shown() == Shown::Quiet)
                    .filter_map(move |s| epoch(&s.active_at).map(|at| (u, s, at)))
            })
            .collect();
        quiet.sort_by(|a, b| a.2.total_cmp(&b.2));
        for (unit, step, at) in quiet {
            let key = format!("quiet:{}@{}", step.id, at as i64);
            out.push(Need {
                mark: Mark::State(Shown::Quiet),
                unit: unit.id.to_string(),
                id: format!("n-quiet-{}", step.id),
                sentence: format!(
                    "<b>{}</b> has not been heard from since {}, {}.",
                    self.step_link(unit, step),
                    ui::clock(&step.active_at),
                    ui::ago(&step.active_at)
                ),
                more: String::new(),
                act: ui::link_with(ui::Btn::Outlined, &step.thread_href(), "Message it", ""),
                short: format!("the silence of {}", ui::cut(title(unit), 60)),
                aside: self.aside(&key),
                keys: vec![key],
            });
        }
        out
    }
    /// A unit's title linking its stopped or quiet step, which opens in the drawer.
    fn step_link(&self, unit: &UnitView, step: &StepView) -> String {
        format!(
            "<a href=\"{}\" data-step=\"{}\">{}</a>",
            esc(&step.href()),
            esc(step.id.as_str()),
            esc(title(unit))
        )
    }
    /// A stop said alone: "<title> failed at review. Its fn failed: …", Retry.
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
        let owner = unit.owner_words();
        // how long its last run took, when it ran
        let after = step
            .timing
            .as_ref()
            .filter(|t| t.finished.is_some() && t.seconds > 0.0)
            .map(|t| format!(" after {}", esc(&ui::duration_text(t.seconds))))
            .unwrap_or_default();
        // a title that is a sentence of its own ends there: what happened is the next one
        let whole = title(unit).trim_end().ends_with(['.', '!', '?']);
        let mut sentence = format!(
            "<b>{}</b>{} {}{}{stage}{after}.",
            self.step_link(unit, step),
            if owner.is_empty() {
                String::new()
            } else {
                format!(" <span class=\"own\">{}</span>", esc(&owner))
            },
            if whole { "It " } else { "" },
            esc(shown.word())
        );
        match (shown, step.failure.as_ref()) {
            (Shown::Cancelled, Some(f)) => {
                let reason = f
                    .headline
                    .strip_prefix("Cancelled: ")
                    .unwrap_or(&f.headline);
                let _ = write!(sentence, " {}", esc(reason));
                // the failure a cancel set aside stays readable
                if !f.set_aside.is_empty() {
                    let _ = write!(
                        sentence,
                        " <span class=\"nd-aside\">It had failed: {}</span>",
                        esc(&ui::cut(&f.set_aside, 160))
                    );
                }
            }
            (_, Some(f)) => {
                // a failure of no known kind says only "It failed.": the sentence said that
                if f.kind != "failed" {
                    let _ = write!(sentence, " {}", esc(&f.headline));
                }
                let said = f.said.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                if !said.is_empty() {
                    let _ = write!(
                        sentence,
                        " It last said “{}”",
                        esc(&ui::cut(said.trim(), 140))
                    );
                }
                // what to try next, as its page says it (`failure::Advice`)
                let next = step.next_step();
                if !next.is_empty() {
                    let _ = write!(sentence, " <span class=\"nd-next\">{next}</span>");
                }
            }
            (Shown::Stale, None) => sentence.push_str(" An input it read changed after it ran."),
            _ => {}
        }
        let act = if step.feedback_in_why() && shown == Shown::Failed {
            // a failure that repeated: its feedback first, on its page
            ui::link_with(
                ui::Btn::Outlined,
                &format!("{}#why-feedback", step.href()),
                "Retry with feedback",
                &format!(" data-step=\"{}\"", esc(step.id.as_str())),
            )
        } else if step.retryable() && !step.retry_asks() {
            let retry = self.retry_form(&[step], "Retry");
            if step.cancelled() {
                // the cancel's other decision: settle it, so its unit counts as done
                format!("{retry}{}", self.settle_form(unit, step))
            } else {
                retry
            }
        } else {
            ui::link_with(
                ui::Btn::Outlined,
                &step.href(),
                "Open step",
                &format!(" data-step=\"{}\"", esc(step.id.as_str())),
            )
        };
        // set aside against its step's generations: a retry or a new cancel brings it back
        let key = format!("stop:{}", step.id);
        Need {
            mark: Mark::State(shown),
            unit: unit.id.to_string(),
            id: format!("n-stop-{}", step.id),
            sentence,
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
    /// Failures that share a kind, said once: "Three units failed: they hit their wall-clock
    /// cap.", the units under it, Retry all.
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
            sentence: format!(
                "{} units failed: {}.{}",
                leading(n),
                shared_cause(kind).unwrap_or("they share a cause"),
                // what to try next, said of them all
                members
                    .first()
                    .and_then(|(_, s)| s.failure.as_ref())
                    .map(|f| f.next_step_all())
                    .filter(|next| !next.is_empty())
                    .map(|next| format!(" <span class=\"nd-next\">{next}</span>"))
                    .unwrap_or_default()
            ),
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
    /// Needs you: the well of what needs the owner, each item a sentence with its one action
    /// and a ×, then a line for each item set aside, with its Undo.
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
        // the stops and silences, at once; a question keeps its own ×, so the head and the well
        // never disagree about it
        let rest: Vec<&&Need> = live.iter().filter(|n| n.mark != Mark::Ask).collect();
        if rest.len() > 1 {
            let keys: Vec<String> = rest.iter().flat_map(|n| n.keys.clone()).collect();
            // said one way in every project; a question is never in it
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
            // one muted line, not an empty well
            let aside = self.needs.iter().filter(|n| n.aside.is_some()).count();
            let _ = write!(
                out,
                "<p class=\"nd-none\">Nothing needs you.{}</p>",
                if aside > 0 {
                    format!(" {aside} set aside.")
                } else {
                    String::new()
                }
            );
        } else {
            out.push_str("<ul class=\"nd\">");
            for need in &live {
                // a question's Answer is the well's one filled action; every other is outlined,
                // in every project
                let act = need.act.clone();
                // a region each, so a change to one patches it alone
                let attrs = match self.view.units.iter().find(|u| u.id.as_str() == need.unit) {
                    Some(unit) => format!(
                        " data-unit=\"{}\" data-said=\"{}\"",
                        esc(&need.unit),
                        esc(&said_states(unit))
                    ),
                    None => String::new(),
                };
                let item = ui::need_line(
                    need.mark,
                    &need.id,
                    &attrs,
                    &need.sentence,
                    &need.more,
                    &act,
                    &self.needs_form(
                        "dismiss",
                        &need.keys,
                        "icon",
                        &format!("Set aside {}: it comes back when it changes", need.short),
                        "×",
                        "",
                        &format!("undo-{}", need.id),
                    ),
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
        let work = match self.at_work {
            0 => "Nothing at work".to_owned(),
            n => format!("{n} at work"),
        };
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
                            esc(&ui::duration_text((self.now - t).max(60.0)))
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
        let head = ui::page_head_with(
            &TrustedHtml::default(),
            &TrustedHtml::owned(esc(&project.name)),
            &details.menu(&project.name),
            &TrustedHtml::owned(format!("<p class=\"page-line\">{}</p>", self.sentence())),
        );
        TrustedHtml::owned(format!(
            "<div id=\"plan-band\" class=\"plan-band\">{head}</div>"
        ))
    }
    /// The paper row's count line: "21 units · 52 steps".
    pub fn meta(&self) -> TrustedHtml {
        let steps: usize = self.view.units.iter().map(|u| u.steps.len()).sum();
        TrustedHtml::owned(format!(
            "<span id=\"plan-meta\">{} · {}</span>",
            ui::count(self.view.units.len(), "unit", "units"),
            ui::count(steps, "step", "steps")
        ))
    }
    /// The paper row's tools: Find, and the Plan · Both · Board switch with a board (the plan as
    /// Mermaid text is in the head's Details, its one ⋯).
    pub fn tools(&self) -> TrustedHtml {
        let view = self.view;
        let href = view.href();
        let mut out = String::new();
        if !view.plan_empty() {
            // the find is the page's `q` signal: typing reopens the stream on it, which draws the
            // plan as found (Datastar's active search); Enter goes to the page of that find, an
            // address like any other; Escape clears it
            let reopen = ui::stream_reopen(&view.stream_href());
            out.push_str(&format!(
                "<form class=\"board-tools pl-find\" method=\"get\" action=\"{h}\" role=\"search\" aria-label=\"Find units\"><div class=\"q-field\">{i}<input type=\"search\" name=\"q\" value=\"{q}\" data-find placeholder=\"Find\" aria-label=\"Find units by id or title, or by a step's id, title or description\" autocomplete=\"off\" spellcheck=\"false\" enterkeyhint=\"search\" data-preserve-attr=\"value\" data-reopens-stream data-bind:q__root data-on:input__debounce.200ms=\"{get}\" data-on:keydown=\"evt.key === 'Escape' &amp;&amp; $q &amp;&amp; (evt.preventDefault(), evt.stopPropagation(), $q = '', {get})\"><a class=\"q-clear\" href=\"{c}\" aria-label=\"Clear the find\" title=\"Clear the find\">{x}</a></div>{focus}{show}</form>",
                h = esc(&href),
                get = esc(&reopen),
                i = icon(Icon::Search, 16, "q-icon"),
                q = esc(&view.q),
                c = esc(&view.clear_href()),
                x = icon(Icon::X, 16, ""),
                focus = view.focus_inputs(),
                show = if view.show == "all" {
                    String::new()
                } else {
                    format!(
                        "<input type=\"hidden\" name=\"show\" value=\"{}\">",
                        esc(&view.show)
                    )
                },
            ));
        }
        if view.panel.is_some() {
            out.push_str(&view_switch(&href, view.view));
        }
        TrustedHtml::owned(out)
    }

    // ---- the sheet -----------------------------------------------------------------------------

    /// The plan's sheet: Needs you, At work, Finished since you looked, each a region of its
    /// own, in that order at every width (what is moving matters more than what is done); the
    /// done index when it is asked for.
    pub fn sheet(&self) -> TrustedHtml {
        let mut out = String::from("<div class=\"plan-sheet\">");
        out.push_str(&region("plan-needs", &self.needs_html()));
        out.push_str(&region("plan-roster", &self.roster_html()));
        out.push_str(&region(
            "plan-finished",
            &match (self.finished.is_empty(), self.view.seen) {
                // marked seen and nothing finished since: said once, with its Undo
                (true, Some(seen)) => self.seen_html(seen),
                (true, None) => "<div id=\"plan-finished\" hidden></div>".to_owned(),
                (false, _) => self.finished_html(),
            },
        ));
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
        out.push_str("</div>");
        TrustedHtml::owned(out)
    }

    // ---- Finished since you looked ------------------------------------------------------------

    /// What Mark as seen leaves under the section's head: "Seen through 04:21 · Undo", one
    /// line, Undo taking the look back so what finished lately is drawn again.
    fn seen_html(&self, seen: u64) -> String {
        format!(
            "<section id=\"plan-finished\" class=\"pb-sec pb-seen\" aria-labelledby=\"finished\"{watch}>{head}{form}<p class=\"seen-line\">Seen through {at} · <button class=\"quiet\">Undo</button></p><input type=\"hidden\" name=\"through\" value=\"0\">{said}</form></section>",
            watch = self.seen_watch(),
            head = ui::SectionHead::new("finished", "Finished lately").html(),
            form = ui::post_form(
                "mark-seen",
                "seen-form",
                &format!("{}/seen", self.view.href())
            ),
            said = ui::form_said("mark-seen"),
            at = ui::clock(&super::rfc3339(seen)),
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
                "Finished since you looked",
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
            head = ui::SectionHead::new("finished", head)
                .note(&TrustedHtml::owned(since))
                .action(&TrustedHtml::owned(mark))
                .html(),
            list = finished_list(self.view, &self.finished),
        );
        if self.finished_more > 0 {
            let _ = write!(
                out,
                "<p class=\"pb-note\"><a href=\"{}?show=done#done\">and {} more</a></p>",
                esc(&self.view.href()),
                self.finished_more
            );
        }
        out.push_str("</section>");
        out
    }

    // ---- At work -------------------------------------------------------------------------------

    /// At work: the axis's labels, then each group's rows, then the line that counts the rest.
    fn roster_html(&self) -> String {
        let rows: usize = self.groups.iter().map(|g| g.rows.len()).sum();
        // counted as the summary counts them: a blocked or queued unit waits, it is not at work
        let waiting: usize = self
            .groups
            .iter()
            .flat_map(|g| &g.rows)
            .filter(|r| r.waiting)
            .count();
        let note = match (rows - waiting, waiting) {
            (0, 0) => "Nothing now".to_owned(),
            (0, w) => format!("Nothing at work · {w} waiting"),
            (n, 0) => ui::count(n, "unit", "units"),
            (n, w) => format!("{} · {w} waiting", ui::count(n, "unit", "units")),
        };
        // the ⓘ that says how to read a track sits by the note, not at the far end
        let note = if rows == 0 {
            esc(&note)
        } else {
            format!("{}{}", esc(&note), track_help())
        };
        let mut out = format!(
            "<section id=\"plan-roster\" class=\"pb-sec\" aria-labelledby=\"at-work\">{}",
            ui::SectionHead::new("at-work", "At work")
                .note(&TrustedHtml::owned(note))
                .html()
        );
        if rows > 0 {
            // a badge says what a row's steps run on only where that tells rows apart
            let badges: std::collections::BTreeSet<String> = self
                .groups
                .iter()
                .flat_map(|g| &g.rows)
                .map(|r| current(r.unit).map(|s| s.badge()).unwrap_or_default())
                .collect();
            let badged = badges.len() > 1;
            let _ = write!(
                out,
                "<div class=\"ro{}\">",
                if badged { "" } else { " no-badge" }
            );
            out.push_str(&axis(""));
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
                                esc(&ui::duration_text((self.now - t).max(60.0)))
                            )
                        })
                        .unwrap_or_default()
                };
                let counts = row_counts(&group.rows.iter().collect::<Vec<_>>());
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
                    "<details class=\"ro-g\"{}><summary class=\"ro-group\"><h3><b>{name}</b> · {counts}{}</h3></summary>{}",
                    if whole { "" } else { " open" },
                    if whole {
                        heard(&healthy)
                    } else {
                        String::new()
                    },
                    axis(" ro-axis-g"),
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
                            &self.row_html(row, badged),
                        ));
                    }
                };
                if fold && !whole {
                    draw(&mut out, &marked);
                    let _ = write!(
                        out,
                        "<details class=\"qf ro-fold\"><summary>+{}{}</summary>",
                        row_counts(&healthy),
                        heard(&healthy)
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
            more.push(format!("<b>{} waiting</b> on earlier work", self.later));
        }
        if !self.done.is_empty() {
            let last = self.done.first().map(|u| u.finished()).unwrap_or("");
            more.push(format!(
                "<a href=\"{}?show=done#done\"><b>{} done</b></a>{}",
                esc(&self.view.href()),
                self.done.len(),
                if last.is_empty() {
                    String::new()
                } else {
                    format!(", the last {}", ui::ago(last))
                }
            ));
        }
        if !more.is_empty() {
            let _ = write!(out, "<p class=\"ro-more\">{}</p>", more.join(" · "));
        }
        out.push_str("</section>");
        out
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
    /// One unit's row: its badge, its title and what it said last, its stage and when it was
    /// heard from, its activity; open, its steps.
    fn row_html(&self, row: &Row<'_>, badged: bool) -> String {
        let unit = row.unit;
        let shown = unit.shown();
        let step = current(unit);
        // an engine's name where badges tell rows apart; never "fn"
        let (badge, runs_on) = step
            .filter(|_| badged)
            .map(|s| (s.badge(), s.runs_on()))
            .filter(|(b, _)| b != "fn")
            .unwrap_or_default();
        // where it is: its stage and how long in it, or what holds it
        let mut meta = String::new();
        if let Some(step) = step {
            let stage = if unit.steps.len() > 1 || !unit.recipe.is_empty() {
                unit.step_name(step)
            } else {
                shown.word().to_owned()
            };
            if row.waiting {
                // a loose step's stage is its state: said once
                let words = if stage == shown.word() {
                    stage.clone()
                } else {
                    format!("{} {stage}", waiting_words(shown))
                };
                meta.push_str(ui::status_pill(shown, &esc(&words)).as_str());
                let why = why_waiting(unit);
                if !why.is_empty() {
                    let _ = write!(meta, "<span class=\"ro-why\">{why}</span>");
                }
            } else {
                // how long it has run, or, quiet, how long since it was heard from
                let quiet = unit
                    .steps
                    .iter()
                    .find(|s| s.shown() == Shown::Quiet && !s.active_at.is_empty());
                let ran = match quiet {
                    Some(q) if stage == Shown::Quiet.word() => {
                        format!(" {}", ui::since(&q.active_at).0)
                    }
                    Some(q) => format!(" · quiet {}", ui::since(&q.active_at).0),
                    None => step
                        .timing
                        .as_ref()
                        .filter(|t| t.finished.is_none())
                        .map(|t| format!(" · {}", ui::since(&t.started).0))
                        .unwrap_or_default(),
                };
                let words = format!(
                    "{}{}{ran}",
                    // its state in words where its stage does not say it
                    if stage == shown.word() || quiet.is_some() || stage == Shown::Quiet.word() {
                        String::new()
                    } else {
                        format!("<span class=\"vh\">{}, </span>", esc(shown.word()))
                    },
                    esc(&stage),
                );
                meta.push_str(
                    ui::status_pill(if quiet.is_some() { Shown::Quiet } else { shown }, &words)
                        .as_str(),
                );
            }
        }
        if !row.waiting {
            let quiet_step = unit.steps.iter().find(|s| s.shown() == Shown::Quiet);
            match (quiet_step, row.heard) {
                // quiet: its pill says for how long
                (Some(q), _) if !q.active_at.is_empty() => {}
                // heard from since its run began: when, the line taking the ink as it ages
                (_, Some(heard)) if heard - started(unit).unwrap_or(heard) > 120.0 => {
                    let at = super::rfc3339(heard as u64);
                    let age = self.now - heard;
                    let _ = write!(
                        meta,
                        "<span class=\"ro-heard{}\">{}heard {}</span>",
                        if age >= HEARD_OLD {
                            " is-old"
                        } else if age >= HEARD_AGING {
                            " is-aging"
                        } else {
                            ""
                        },
                        if age < LIVE {
                            "<span class=\"dot run pulse\" aria-hidden=\"true\"></span> "
                        } else {
                            ""
                        },
                        ui::ago(&at)
                    );
                }
                // nothing since its run began: said once, the pill already says how long
                (_, Some(_)) => meta.push_str("<span class=\"ro-heard\">no word yet</span>"),
                _ => {}
            }
        }
        roster_row(&RowParts {
            id: unit.id.as_str(),
            waiting: row.waiting,
            said_states: &said_states(unit),
            badge: &badge,
            runs_on: &runs_on,
            title: title(unit),
            owner: &unit.owner_words(),
            said: &self.said(unit),
            meta: &meta,
            track: &self.track_html(row),
            drill: &format!("{}{}", self.steps_html(unit), self.foot_html(unit)),
        })
    }
    /// A unit's one line: its recipe's view (the project's own summary of it), else what it said
    /// last.
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
        // nothing said leaves the line empty: its meta says when it was last heard from
        said_html(unit)
    }
    /// A unit's activity over the last twelve hours on a log scale: its earlier runs faint, the
    /// one going now blue, sand once it went quiet, a tick a message, dotted while it waits.
    fn track_html(&self, row: &Row<'_>) -> String {
        let unit = row.unit;
        let now = (self.now / 60.0).floor() * 60.0;
        let from = now - SAID_WINDOW;
        let mut segs: Vec<(&'static str, f64, f64)> = vec![];
        let mut last_end: Option<f64> = None;
        for step in &unit.steps {
            let Some(timing) = step.timing.as_ref() else {
                continue;
            };
            let quiet = (step.shown() == Shown::Quiet)
                .then(|| epoch(&step.active_at))
                .flatten();
            for span in &timing.spans {
                let end = span.to.unwrap_or(now);
                last_end = Some(last_end.map_or(end, |e: f64| e.max(end)));
                if end < from {
                    continue;
                }
                let a = span.from.max(from);
                let open = span.to.is_none() && step.running();
                match (open, quiet) {
                    (true, Some(q)) if q > a => {
                        segs.push(("tk-run", a, q));
                        segs.push(("tk-quiet", q, now));
                    }
                    (true, Some(_)) => segs.push(("tk-quiet", a, now)),
                    (true, None) => segs.push(("tk-run", a, now)),
                    (false, _) => segs.push(("tk-past", a, end)),
                }
            }
        }
        if row.waiting
            && let Some(end) = last_end.filter(|e| *e < now)
        {
            segs.push(("tk-wait", end.max(from), now));
        }
        let ticks: Vec<f64> = unit.said.iter().map(|(_, t)| *t).collect();
        track(&segs, &ticks, now)
    }
    /// A unit's steps, open under its row: its step view (`unit_steps`), its rows opening in
    /// the drawer, its foot the plan's own.
    fn steps_html(&self, unit: &UnitView) -> String {
        let mut steps = super::unit_steps::of_unit(unit, &self.view.relations, true, "");
        steps.owner.clear();
        steps.latest = None;
        steps.recent = true;
        steps.html().0
    }
    /// The drilldown's foot: when it was last heard from (its row already says what it said
    /// last), then Message and Unit page, and Open step.
    fn foot_html(&self, unit: &UnitView) -> String {
        let mut out = String::from("<div class=\"ro-foot\"><p class=\"ro-ev\">");
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
        out.push_str("</p>");
        out.push_str("<span class=\"ro-links\">");
        if let Some(step) = current(unit) {
            let _ = write!(out, "<a href=\"{}\">Message</a>", esc(&step.thread_href()),);
        }
        let _ = write!(
            out,
            "<a href=\"{}\">Unit page</a></span>",
            esc(&unit_href(self.view, unit))
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
    let running = rows.len() - quiet - waiting;
    [(quiet, "quiet"), (running, "running"), (waiting, "waiting")]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---- the plan's parts, drawn here and on `/_ui` -------------------------------------------------

/// A card of Finished since you looked: when it finished and how long it took (nothing for no
/// time), its title linking its page, what it produced (`output`, HTML), its steps' states
/// (`said`, as `data-said` carries them; "" for none).
pub fn finished_card(
    at: &str,
    took: f64,
    href: &str,
    title: &str,
    output: &str,
    said: &str,
) -> String {
    format!(
        "<li class=\"fin-i\"{said}><p class=\"fin-w\">{at}{took}</p><p class=\"fin-t\"><a href=\"{href}\">{title}</a></p>{output}</li>",
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
/// What the axis says of its scale, to a screen reader and on hover.
const SCALE: &str = "The last 12 hours on a log scale, now at the right; a tick is a message.";
/// At work's ⓘ: how to read a row's track, behind a small round button by its head (the
/// "⋯" Details menu's popover, so it closes on a click elsewhere or Escape).
fn track_help() -> String {
    format!(
        "{}<details class=\"dm pb-info\" data-preserve-attr=\"open\"><summary class=\"dm-b\" aria-label=\"How to read At work\" title=\"How to read At work\" data-preserve-attr=\"aria-expanded\">{}</summary><div class=\"menu dm-p\" role=\"group\" aria-label=\"How to read At work\" data-preserve-attr=\"style\"><p class=\"dm-h\">How to read At work</p><p>Each unit's track is its last 12 hours on a log scale: the last hour takes most of the width, and now is at the right edge.</p><p>A blue bar is the run going now, a pale one an earlier run, a sand one a run gone quiet, and a dotted line time it spent waiting. Each tick is a message to or from it.</p></div></details>{}",
        ui::menu_open(),
        icon(Icon::Info, 16, ""),
        ui::menu_close(),
    )
}
/// The labels over At work's tracks: 12h, 4h, 1h, 15m and now, where the log scale puts them;
/// `class` " ro-axis-g" for a group's own, which a narrow plan shows over its tracks.
pub fn axis(class: &str) -> String {
    let mut out = format!(
        "<div class=\"ro-axis{class}\" title=\"{SCALE}\"><span class=\"vh\">{SCALE}</span><span class=\"ro-ticks\" aria-hidden=\"true\">"
    );
    for (ago, words) in super::unit_steps::RECENT {
        let _ = write!(
            out,
            "<span style=\"left:{:.2}%\">{words}</span>",
            super::unit_steps::recent_at(ago)
        );
    }
    out.push_str("</span></div>");
    out
}
/// A unit's track over the last twelve hours to `now` on the log scale: each `(class, from,
/// to)` a bar (`tk-past` an earlier run, `tk-run` the one going, `tk-quiet` its silence,
/// `tk-wait` dotted while it waits), a tick for each message in `ticks`.
pub fn track(segs: &[(&str, f64, f64)], ticks: &[f64], now: f64) -> String {
    let from = now - SAID_WINDOW;
    let mut bars = String::new();
    for (class, a, b) in segs {
        if *b < from {
            continue;
        }
        let (l, r) = (
            super::unit_steps::recent_at(now - a.max(from)),
            super::unit_steps::recent_at(now - b),
        );
        let _ = write!(
            bars,
            "<i class=\"{class}\" style=\"left:{l:.2}%;width:{:.2}%\"></i>",
            (r - l).max(0.0)
        );
    }
    let mut said = 0;
    for t in ticks.iter().filter(|t| **t >= from && **t <= now + 60.0) {
        said += 1;
        let _ = write!(
            bars,
            "<i class=\"tk-m\" style=\"left:{:.2}%\"></i>",
            super::unit_steps::recent_at(now - t)
        );
    }
    format!(
        "<span class=\"ro-track\" role=\"img\" aria-label=\"{}\">{bars}</span>",
        esc(&format!(
            "Its last twelve hours, on a log scale: {}",
            ui::count(said, "message", "messages")
        ))
    )
}
/// What a row of At work is drawn from (HTML where it says so).
pub struct RowParts<'a> {
    pub id: &'a str,
    /// It waits (its track dotted since its last run); else it is at work.
    pub waiting: bool,
    /// Its steps' states, as `data-said` carries them ("" for none).
    pub said_states: &'a str,
    /// What its steps run on, short ("" for no badge: rows that all run on one thing, or a fn).
    pub badge: &'a str,
    pub runs_on: &'a str,
    pub title: &'a str,
    /// Who owns it (`@lead`, "you"), small and muted after its title; "" for no one.
    pub owner: &'a str,
    /// HTML: what it said last.
    pub said: &'a str,
    /// HTML: its stage pill and when it was heard from.
    pub meta: &'a str,
    /// HTML: its `track`.
    pub track: &'a str,
    /// HTML: what it opens to.
    pub drill: &'a str,
}
/// A row of At work: a disclosure whose summary is the row and whose body is its steps, opening
/// in place.
pub fn roster_row(row: &RowParts<'_>) -> String {
    format!(
        "<details id=\"u-{id}\" class=\"ro-u{wait}\" data-unit=\"{id}\"{states} data-preserve-attr=\"open\"><summary class=\"ro-r\">{badge}<span class=\"ro-who\"><span class=\"ro-t\">{title}{own}</span><span class=\"ro-s\">{said}</span></span><span class=\"ro-meta\">{meta}</span>{track}<span class=\"ro-chev\" aria-hidden=\"true\">{chev}</span></summary><div class=\"ro-drill\">{drill}</div></details>",
        id = esc(row.id),
        wait = if row.waiting { " is-wait" } else { "" },
        states = if row.said_states.is_empty() {
            String::new()
        } else {
            format!(" data-said=\"{}\"", esc(row.said_states))
        },
        // what its steps run on, where that tells rows apart ("" for no badge)
        badge = if row.badge.is_empty() {
            String::new()
        } else {
            format!(
                "<span class=\"ro-badge\" title=\"Runs on {}\">{}</span>",
                esc(row.runs_on),
                esc(&ui::cut(row.badge, 8))
            )
        },
        title = esc(row.title),
        own = if row.owner.is_empty() {
            String::new()
        } else {
            format!(" <span class=\"own\">{}</span>", esc(row.owner))
        },
        said = row.said,
        meta = row.meta,
        track = row.track,
        chev = icon(Icon::ChevronDown, 16, ""),
        drill = row.drill,
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
    let text = esc(&crate::markdown::cut(
        &crate::markdown::plain(&unit.last_message),
        240,
    ));
    let own = unit.steps.iter().any(|s| s.id.as_str() == unit.last_from);
    if own {
        text
    } else {
        format!("<b>{}</b>: {text}", unit.sender_html())
    }
}
/// A waiting row's pill, before its stage: "waiting for", or the state that holds it.
fn waiting_words(shown: Shown) -> String {
    match shown {
        Shown::Queued | Shown::Paused | Shown::Held | Shown::Blocked | Shown::External => {
            format!("{} at", shown.word())
        }
        _ => "waiting for".into(),
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
/// What a done unit produced, led by the values that identify it or say it in a word (as
/// `ui::ValueSet` sorts them: a commit, a count of files), each named, then the output that
/// explains it (its summary) or else its last message; `None` when it left neither.
fn produced(unit: &UnitView) -> Option<String> {
    let name = |n: &str| esc(&n.replace(['_', '-'], " "));
    let mut facts: Vec<String> = vec![];
    for step in unit.steps.iter().rev() {
        for field in step.outputs.iter().filter(|f| f.available) {
            if facts.len() == 3 || facts.iter().any(|f| f.starts_with(&name(&field.name))) {
                continue;
            }
            match ui::ValueSet::of_text(&field.value) {
                ui::ValueSet::Detail if field.identifies() => facts.push(format!(
                    "{} <code>{}</code>",
                    name(&field.name),
                    esc(&ui::cut(&field.value, 12))
                )),
                ui::ValueSet::Show if !field.value.trim().is_empty() => facts.push(format!(
                    "{} <b>{}</b>",
                    name(&field.name),
                    esc(field.value.trim())
                )),
                _ => {}
            }
        }
    }
    let said = unit
        .steps
        .iter()
        .rev()
        .find_map(|step| step.overview_outputs().first().map(|f| f.value.clone()))
        .filter(|v| ui::ValueSet::of_text(v) == ui::ValueSet::Prose)
        .or_else(|| {
            let m = unit.last_message.trim();
            (!m.is_empty()).then(|| m.to_owned())
        });
    if facts.is_empty() && said.is_none() {
        return None;
    }
    let mut out = String::new();
    if !facts.is_empty() {
        out.push_str(&format!("<p class=\"fin-facts\">{}</p>", facts.join(" · ")));
    }
    if let Some(said) = said {
        out.push_str(&format!(
            "<p class=\"fin-v\">{}</p>",
            esc(&crate::markdown::cut(&crate::markdown::plain(&said), 160))
        ));
    }
    Some(out)
}
/// Finished, drawn one way on the plan and on home: a card a unit that produced something,
/// then the rest, which finished with nothing to say, in one line naming them.
pub fn finished_list(view: &ProjectView, units: &[&UnitView]) -> String {
    let mut cards = String::new();
    let mut quiet: Vec<String> = vec![];
    for unit in units {
        match produced(unit) {
            Some(output) => cards.push_str(&finished_card(
                unit.finished(),
                took(unit),
                &unit_href(view, unit),
                title(unit),
                &output,
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
    if !cards.is_empty() {
        let _ = write!(out, "<ul class=\"fin\">{cards}</ul>");
    }
    if !quiet.is_empty() {
        let _ = write!(
            out,
            "<p class=\"fin-quiet\">{} finished with nothing to say: {}</p>",
            if cards.is_empty() {
                ui::count(quiet.len(), "unit", "units")
            } else {
                format!("{} more", quiet.len())
            },
            quiet.join(", ")
        );
    }
    out
}

/// The Plan · Both · Board switch: a link each, which the server answers with the page in that
/// view and keeps in the project's cookie. The chosen one is filled; with none chosen, the
/// stylesheet fills the one its width shows.
fn view_switch(href: &str, chosen: Option<&str>) -> String {
    let links: String = [
        ("plan", "Plan", Icon::Workflow),
        ("both", "Both", Icon::Columns2),
        ("board", "Board", Icon::LayoutDashboard),
    ]
    .into_iter()
    .map(|(key, word, glyph)| {
        format!(
            "<a href=\"{h}?view={key}\" data-view-tab=\"{key}\" aria-current=\"{on}\" data-preserve-attr=\"aria-current\">{i}{word}</a>",
            h = esc(href),
            on = chosen == Some(key),
            i = icon(glyph, 16, ""),
        )
    })
    .collect();
    format!("<nav class=\"tabs view-switch\" aria-label=\"View\">{links}</nav>")
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
    viewer: &Viewer,
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
        meta: plan.meta(),
        tools: plan.tools(),
        signals,
    };
    let batch = RenderedBatch::new(vec![
        PatchRegion::new("project-board", board_region(&body)),
        PatchRegion::new("plan-band", frame.head.clone()),
        PatchRegion::new("plan-meta", frame.meta.clone()),
        PatchRegion::new(
            "top-nav",
            super::render_nav(&nav, viewer, &view.href()).map_err(render_error)?,
        ),
    ]);
    Ok((body, frame, batch))
}
/// The whole page: its title the project's glance, its favicon the glance's.
pub fn render(
    view: &ProjectView,
    shared: &DashboardSnapshot,
    viewer: &Viewer,
) -> Result<TrustedHtml, PublicError> {
    let nav = NavView::new(shared, Some(view.project.id), "plan")?;
    let (body, frame, batch) = draw(view, shared, viewer)?;
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
