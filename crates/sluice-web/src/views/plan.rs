//! A project's plan (DESIGN.md, Pages): the navy band with the project's name, its summary
//! sentence and what finished most recently; then on the module grid, ordered by what needs the
//! owner, For you (a question to the owner, swollen), Stopped (failed, cancelled, stale), Running
//! (quiet first, then the longest), Waiting (each held up by whom) and Done (a dense index,
//! folded), with a long run's progress in the margin. Every unit is one row or module; select a
//! unit to trace its chain (`sluice-trace`).
//!
//! Generic by rule: a unit's cells are its recipe's own stages in recipe order (a block a recipe,
//! its columns that recipe's steps), a unit of no recipe draws its own small graph, a loose step
//! is one cell, and every word comes from the status table, the recipe or what the project's
//! steps reported.
use super::board::{ProjectView, Question, UnitView};
use super::icons::{Icon, icon};
use super::step::StepView;
use super::ui::{self, Cell, Shown, Stage, Swell, Trace, esc};
use super::{AskView, DashboardSnapshot, Frame, NavView, TrustedHtml, Viewer};
use crate::streams::{PatchRegion, RenderedBatch};
use askama::Template;
use sluice_model::{error::PublicError, shown::Band};
use std::collections::BTreeSet;

/// The grid every plan sits on.
pub const GRID: &str = "plan-grid";

/// Where a unit not done is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Stopped,
    Running,
    Waiting,
    Margin,
}

/// A question to the owner as For you draws it: its message, its words, and the unit whose step
/// asked it (its strip drawn with the first of its questions).
struct Ask<'a> {
    ask: &'a AskView,
    question: Option<&'a Question>,
    unit: Option<&'a UnitView>,
    step: Option<&'a StepView>,
    strip: bool,
}

/// The plan as drawn: each unit placed once, in its order.
pub struct Plan<'a> {
    view: &'a ProjectView,
    asks: Vec<Ask<'a>>,
    stopped: Vec<&'a UnitView>,
    running: Vec<&'a UnitView>,
    waiting: Vec<&'a UnitView>,
    margin: Vec<&'a UnitView>,
    /// The done units, the latest finished first.
    done: Vec<&'a UnitView>,
    /// The cancels the owner dismissed in the last ten minutes, each with its unit: Stopped
    /// says each with its Undo.
    dismissed: Vec<(&'a UnitView, &'a StepView)>,
    trace: Trace,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
/// Seconds from a stored time to now.
fn since_secs(at: &str) -> Option<f64> {
    super::timestamp(at).map(|then| now().saturating_sub(then) as f64)
}
/// The step that says how its unit stands: the first whose state is the unit's.
fn current(unit: &UnitView) -> Option<&StepView> {
    let shown = unit.shown();
    unit.steps
        .iter()
        .find(|s| s.standing() == shown)
        .or_else(|| unit.steps.first())
}
/// How long a running step's run has gone.
fn elapsed(step: &StepView) -> Option<f64> {
    let t = step.timing.as_ref()?;
    (step.running() && t.finished.is_none()).then_some(t.seconds)
}
/// A running step past twice its stage's usual time: how many times that it has run.
fn over(step: &StepView) -> Option<f64> {
    let (usually, seconds) = (step.usually?, elapsed(step)?);
    (step.overrun() && usually > 0.0).then_some(seconds / usually)
}
/// How long a quiet run has written nothing.
fn quiet_secs(step: &StepView) -> Option<f64> {
    (step.shown() == Shown::Quiet)
        .then(|| since_secs(&step.active_at))
        .flatten()
}
/// A step's name in its unit's cell: its stage, else its id without the unit's prefix.
fn short(unit: &UnitView, step: &StepView) -> String {
    unit.step_name(step)
}
/// A step as its cell: its state from the status table, its time, its page.
fn stage_of(step: &StepView, name: &str) -> Stage {
    let shown = step.shown();
    let mut stage = Stage::new(name, Some(shown))
        .href(step.href())
        .step(step.id.as_str());
    match (Cell::of(Some(shown)), step.timing.as_ref()) {
        (_, Some(t)) if step.running() && t.finished.is_none() => {
            stage = stage.running_since(t.started.clone(), t.seconds);
            if let Some(ratio) = over(step) {
                stage = stage.over(ratio);
            }
        }
        (Cell::Done | Cell::Look, _) => {
            if let Some(t) = step.shown_timing() {
                stage = stage.took(t.seconds);
            }
        }
        _ => {}
    }
    stage
}
/// A unit's cells: its recipe's stages in recipe order, else its steps in plan order.
/// Each step's state in words, "a-build succeeded|a-review skipped": what a unit drawn without
/// its cells (a stopped module, a line of the Done index) gives the page's announcer, so a step
/// whose state a patch moves on is said even when its unit changes band.
fn states_said(unit: &UnitView) -> String {
    unit.steps
        .iter()
        .map(|s| format!("{} {}", s.id, s.shown().word()))
        .collect::<Vec<_>>()
        .join("|")
}

fn strip(unit: &UnitView) -> Vec<Stage> {
    if unit.stages.is_empty() {
        return unit
            .steps
            .iter()
            .map(|s| stage_of(s, &short(unit, s)))
            .collect();
    }
    unit.stages
        .iter()
        .map(|name| match unit.stage_step(name) {
            Some(step) => stage_of(step, name),
            None => Stage::new(name.clone(), None),
        })
        .collect()
}
/// How long a unit's work took, first start to last end, and its most runs of any step.
fn took(unit: &UnitView) -> (f64, usize) {
    let mut from = u64::MAX;
    let mut to = 0;
    let mut runs = 1;
    for t in unit.steps.iter().filter_map(|s| s.shown_timing()) {
        if let Some(start) = super::timestamp(&t.started) {
            from = from.min(start);
        }
        if let Some(end) = t.finished.as_deref().and_then(super::timestamp) {
            to = to.max(end);
        }
        runs = runs.max(t.runs);
    }
    let took = if to >= from && from != u64::MAX {
        (to - from) as f64
    } else {
        0.0
    };
    (took, runs)
}
/// Where a unit's name leads: a loose step's own page, else the unit's.
/// A project's description in its head's Details, as markdown (its lists as lists), "" when it
/// has none.
fn about_html(description: &str) -> TrustedHtml {
    if description.is_empty() {
        return TrustedHtml::default();
    }
    TrustedHtml::owned(format!(
        "<div class=\"md dm-md\">{}</div>",
        crate::markdown::render(description).as_str()
    ))
}
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

impl<'a> Plan<'a> {
    pub fn new(view: &'a ProjectView) -> Self {
        // a run past any usual time by far, or reporting progress, of no recipe and alone in
        // its unit, is a long run: the margin draws it
        let longest = view.usual.values().copied().fold(0.0_f64, f64::max);
        let long = if longest > 0.0 {
            (4.0 * longest).max(2.0 * 3600.0)
        } else {
            6.0 * 3600.0
        };
        let is_long = |unit: &UnitView| match unit.steps.as_slice() {
            // a quiet one needs a look: it leads Running instead
            [step] if unit.recipe.is_empty() && step.running() && step.shown() != Shown::Quiet => {
                step.progress
                    .as_ref()
                    .is_some_and(|p| p.live && !p.fields.is_empty())
                    || elapsed(step).is_some_and(|e| e > long)
            }
            _ => false,
        };
        let live = |unit: &&UnitView| !unit.done;
        let mut asks: Vec<Ask<'a>> = vec![];
        let mut asked: BTreeSet<&str> = BTreeSet::new();
        for ask in &view.project.asks {
            let unit = view
                .units
                .iter()
                .filter(live)
                .find(|u| u.steps.iter().any(|s| s.id.as_str() == ask.step));
            let step = unit.and_then(|u| u.steps.iter().find(|s| s.id.as_str() == ask.step));
            // with no step of this view (a question from no step, or one the view leaves out),
            // only a question from no step is drawn
            if unit.is_none() && !ask.step.is_empty() {
                continue;
            }
            let strip = unit.is_some_and(|u| asked.insert(u.id.as_str()));
            asks.push(Ask {
                ask,
                question: view.questions.iter().find(|q| q.message == ask.message),
                unit,
                step,
                strip,
            });
        }
        let (mut stopped, mut running, mut waiting, mut margin) = (vec![], vec![], vec![], vec![]);
        for unit in view.units.iter().filter(live) {
            if asked.contains(unit.id.as_str()) {
                continue;
            }
            let place = match unit.shown() {
                Shown::Failed | Shown::Cancelled | Shown::Stale => Place::Stopped,
                shown if shown.spec().band == Band::Running => {
                    if is_long(unit) {
                        Place::Margin
                    } else {
                        Place::Running
                    }
                }
                _ => Place::Waiting,
            };
            match place {
                Place::Stopped => stopped.push(unit),
                Place::Running => running.push(unit),
                Place::Waiting => waiting.push(unit),
                Place::Margin => margin.push(unit),
            }
        }
        stopped.sort_by_key(|u| (u.shown(), u.pos));
        // quiet first (the longest quiet first), then the longest running
        running.sort_by(|a, b| {
            let key = |u: &UnitView| {
                let step = current(u);
                (
                    step.and_then(quiet_secs).unwrap_or(-1.0),
                    step.and_then(elapsed).unwrap_or(0.0),
                )
            };
            let (ka, kb) = (key(a), key(b));
            kb.0.total_cmp(&ka.0)
                .then(kb.1.total_cmp(&ka.1))
                .then(a.pos.cmp(&b.pos))
        });
        waiting.sort_by_key(|u| u.pos);
        let mut done: Vec<&UnitView> = view.units.iter().filter(|u| u.done).collect();
        done.sort_by(|a, b| b.finished().cmp(a.finished()).then(a.pos.cmp(&b.pos)));
        let drawn: BTreeSet<&str> = view
            .units
            .iter()
            .filter(live)
            .map(|u| u.id.as_str())
            .collect();
        let trace = Trace::new(
            view.unit_waits()
                .into_iter()
                .filter(|(u, on)| drawn.contains(u) && drawn.contains(on)),
        );
        let mut dismissed: Vec<(&UnitView, &StepView)> = view
            .units
            .iter()
            .flat_map(|u| {
                u.steps
                    .iter()
                    .filter(|s| s.dismissed && !s.dismissed_at.is_empty())
                    .map(move |s| (u, s))
            })
            .collect();
        dismissed.sort_by(|a, b| b.1.dismissed_at.cmp(&a.1.dismissed_at));
        Plan {
            view,
            asks,
            stopped,
            running,
            waiting,
            margin,
            done,
            dismissed,
            trace,
        }
    }
    /// The columns of the twelve the bands under the first row take: all of them, or ten
    /// beside the margin.
    fn cols(&self) -> u8 {
        if self.margin.is_empty() { 12 } else { 10 }
    }
    fn project(&self) -> sluice_model::ids::ProjectId {
        self.view.project.id
    }

    // ---- the band ------------------------------------------------------------------------------

    /// What the summary sentence counts: every unit of the view.
    fn facts(&self) -> Vec<ui::UnitFact> {
        self.view
            .units
            .iter()
            .map(|u| {
                let step = current(u);
                let shown = if u.done {
                    Shown::Succeeded
                } else {
                    // held up by a failure: it waits, the failure is counted where it is
                    match u.shown() {
                        Shown::Blocked => Shown::Pending,
                        shown => shown,
                    }
                };
                let quiet = step.and_then(quiet_secs);
                ui::UnitFact {
                    name: u.id.to_string(),
                    recipe: u.recipe.clone(),
                    shown: Some(shown),
                    over: step.and_then(over),
                    quiet,
                    quiet_since: if quiet.is_some() {
                        step.map(|s| s.active_at.clone()).unwrap_or_default()
                    } else {
                        String::new()
                    },
                }
            })
            .collect()
    }
    /// The page's head: the project's name, the summary sentence under it, and its Details
    /// (the project's id and its words), in one region its stream patches.
    pub fn band(&self) -> TrustedHtml {
        let facts = self.facts();
        let ask_href = match self.view.project.asks.as_slice() {
            [one] => one.href(&self.project()),
            _ => "#for-you".to_owned(),
        };
        let last = self.done.first().map(|u| u.finished()).unwrap_or("");
        let summary = ui::summary_sentence(&ui::Summary {
            asks: self.view.project.asks.len(),
            ask_href: &ask_href,
            units: &facts,
            last_done: last,
            noun: ("unit", "units"),
        });
        let project = &self.view.project;
        let details = ui::Details::new()
            .id("Project", &project.id.to_string())
            .html("About", &about_html(project.description.trim()))
            .link(
                "Settings",
                &format!("{}/settings", project.href()),
                "Its settings",
            );
        let head = ui::page_head_with(
            &TrustedHtml::default(),
            &TrustedHtml::owned(esc(&project.name)),
            &details.menu(&project.name),
            &TrustedHtml::owned(format!("<p class=\"page-line\">{summary}</p>")),
        );
        TrustedHtml::owned(format!(
            "<div id=\"plan-band\" class=\"plan-band\">{head}</div>"
        ))
    }
    /// What finished last, newest first, for the Done head.
    fn latest(&self) -> Vec<ui::Finished> {
        self.done
            .iter()
            .filter(|u| !u.finished().is_empty())
            .take(ui::LATEST)
            .map(|u| {
                let (took, runs) = took(u);
                ui::Finished {
                    name: u.id.to_string(),
                    title: if u.titled() {
                        u.title.clone()
                    } else {
                        String::new()
                    },
                    href: unit_href(self.view, u),
                    at: u.finished().to_owned(),
                    took,
                    runs,
                    shown: Some(Shown::Succeeded),
                    place: String::new(),
                }
            })
            .collect()
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
    /// The paper row's tools: Find, the Plan · Both · Board switch with a board, and the plan as
    /// Mermaid text.
    pub fn tools(&self) -> TrustedHtml {
        let view = self.view;
        let href = view.href();
        let mut out = String::new();
        if !view.plan_empty() {
            out.push_str(ui::search_open("stream", &href).as_str());
            out.push_str(&format!(
                "<form class=\"board-tools pl-find\" method=\"get\" action=\"{h}\" role=\"search\" aria-label=\"Find units\"><div class=\"q-field\">{i}<input type=\"search\" name=\"q\" value=\"{q}\" data-find placeholder=\"Find\" aria-label=\"Find units by id or title, or by a step's id, title or description\" autocomplete=\"off\" spellcheck=\"false\" enterkeyhint=\"search\" data-preserve-attr=\"value\"><a class=\"q-clear\" data-clear-q href=\"{c}\" aria-label=\"Clear the find\" title=\"Clear the find\">{x}</a></div>{focus}{show}<button class=\"apply\">Find</button></form>",
                h = esc(&href),
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
            out.push_str(ui::search_close().as_str());
        }
        if view.panel.is_some() {
            out.push_str(&view_switch(&href, view.view));
        }
        if !view.plan_empty() {
            out.push_str(&format!(
                "{}<details class=\"tool-more\" data-preserve-attr=\"open\"><summary aria-label=\"More ways to see the plan\" title=\"More ways to see the plan\">{}</summary><div class=\"menu\"><a href=\"{}?format=mermaid&amp;all=true\">The plan as Mermaid text</a></div></details>{}",
                ui::menu_open(),
                icon(Icon::Ellipsis, 16, ""),
                esc(&href),
                ui::menu_close()
            ));
        }
        TrustedHtml::owned(out)
    }

    // ---- the sheet -----------------------------------------------------------------------------

    /// The plan's sheet: the trace line, then the grid of its bands, each a region of its own.
    pub fn sheet(&self) -> TrustedHtml {
        let mut out = String::new();
        out.push_str(ui::trace_open("plan-trace").as_str());
        out.push_str(ui::grid_open(GRID, 12).as_str());
        let cols = self.cols();
        let mut rows = 0;
        // the first row: For you beside Stopped
        // Stopped stands while a unit is stopped, or a cancel was dismissed lately (its Undo)
        let stops = !self.stopped.is_empty() || !self.dismissed.is_empty();
        let stopped_span = match (self.asks.is_empty(), !stops) {
            (_, true) => 0,
            (true, false) => cols,
            (false, false) => 4,
        };
        // every band is a region, an empty one a hidden placeholder: a unit moving between
        // bands patches the bands it left and joined, never the page around them
        let band = |id: &str, html: Option<String>| match html {
            Some(html) => region(id, &html),
            None => region(id, &format!("<div id=\"{id}\" hidden></div>")),
        };
        out.push_str(&band(
            "plan-asks",
            (!self.asks.is_empty()).then(|| self.asks_html(cols - stopped_span)),
        ));
        out.push_str(&band(
            "plan-stopped",
            stops.then(|| self.stopped_html(stopped_span)),
        ));
        if !self.asks.is_empty() || stops {
            rows += 1;
        }
        let bands = [
            ("plan-running", "running", "Running", &self.running),
            ("plan-waiting", "waiting", "Waiting", &self.waiting),
        ];
        // from 2000px of sheet, Running and Waiting stand side by side when both are there
        let both = !self.running.is_empty() && !self.waiting.is_empty();
        for (id, key, name, units) in bands {
            if !units.is_empty() {
                rows += 1;
            }
            out.push_str(&band(
                id,
                (!units.is_empty()).then(|| {
                    let html = self.band_html(id, key, name, units, cols);
                    if both {
                        html.replacen(" data-span=", " data-wide=\"halve\" data-span=", 1)
                    } else {
                        html
                    }
                }),
            ));
        }
        if !self.done.is_empty() {
            rows += 1;
        }
        out.push_str(&band(
            "plan-done",
            (!self.done.is_empty()).then(|| self.done_html(cols)),
        ));
        out.push_str(&band(
            "plan-margin",
            (!self.margin.is_empty())
                .then(|| self.margin_html(rows.max(1), (rows - usize::from(both)).max(1))),
        ));
        out.push_str(ui::grid_close().as_str());
        out.push_str(ui::trace_close().as_str());
        TrustedHtml::owned(out)
    }
    /// A traced unit's attributes, for the element that holds it.
    fn attrs(&self, unit: &UnitView) -> String {
        self.trace.attrs(unit.id.as_str()).0
    }

    /// For you: each open question to the owner whole, swollen, Answer first.
    fn asks_html(&self, span: u8) -> String {
        let oldest = self
            .asks
            .iter()
            .filter_map(|a| a.question.map(|q| q.at.as_str()))
            .min()
            .unwrap_or("");
        let line = format!(
            "{}{}",
            ui::count(self.asks.len(), "question", "questions"),
            if oldest.is_empty() {
                String::new()
            } else {
                format!(", the first asked {}", ui::ago_text(oldest))
            }
        );
        let mut out = format!(
            "<section id=\"plan-asks\" class=\"pl-sec pl-asks\" style=\"--span:{span}\" data-span=\"{span}\" aria-labelledby=\"for-you\">{}",
            ui::section_head("for-you", "For you", &line)
        );
        let module = span.min(6);
        for ask in &self.asks {
            out.push_str(&region(
                &format!("q-{}", ask.ask.message),
                &self.ask_html(ask, module),
            ));
        }
        out.push_str("</section>");
        out
    }
    fn ask_html(&self, a: &Ask<'_>, span: u8) -> String {
        let project = self.project();
        let ask = a.ask;
        // who asked, in words: its stage (its unit's title is under the question), else the
        // orchestrator
        let from = match (a.unit, a.step) {
            (Some(unit), Some(step)) => short(unit, step),
            _ if ask.step.is_empty() => "the orchestrator".to_owned(),
            _ => "a step".to_owned(),
        };
        let when = a
            .question
            .map(|q| format!(" at {}, {}", ui::clock(&q.at), ui::ago(&q.at)))
            .unwrap_or_default();
        let (attrs, meta, strip, details) = match (a.unit, a.step) {
            (Some(unit), Some(step)) => {
                // the unit that asks: its title and how its run goes
                let mut meta = String::new();
                if unit.titled() {
                    meta.push_str(&format!("<b>{}</b>", esc(&ui::cut(&unit.title, 90))));
                }
                let run = self.run_words(step);
                if !run.is_empty() {
                    if !meta.is_empty() {
                        meta.push_str(" · ");
                    }
                    meta.push_str(&format!("running {run}"));
                    if let Some(ratio) = over(step) {
                        meta.push_str(&format!(" {}", ui::overrun(ratio)));
                    }
                }
                let strip = if a.strip {
                    ui::stage_strip(&format!("Stages of {}", unit.heading()), &strip(unit)).0
                } else {
                    String::new()
                };
                (
                    self.attrs(unit),
                    if meta.is_empty() {
                        String::new()
                    } else {
                        format!("<p class=\"mod-meta\">{meta}</p>")
                    },
                    strip,
                    self.unit_details(unit, Some(step)),
                )
            }
            _ => (
                String::new(),
                String::new(),
                String::new(),
                ui::Details::new(),
            ),
        };
        let details = details.text("Message", &ask.message.to_string());
        let body = a
            .question
            .map(|q| crate::markdown::render(&q.body).0)
            .unwrap_or_default();
        let answer = ask.href(&project);
        let mut actions = format!(
            "<a class=\"primary\" href=\"{}\"{}>Answer{}</a>",
            esc(&answer),
            if ask.step.is_empty() {
                String::new()
            } else {
                format!(" data-opens=\"{}\"", esc(&ask.step))
            },
            icon(Icon::ArrowRight, 16, "")
        );
        if let Some(step) = a.step {
            actions.push_str(&format!(
                "<a href=\"{}\" data-step=\"{}\">Open step</a><a class=\"quiet-act\" href=\"{}\">{}Message</a>",
                esc(&step.href()),
                esc(step.id.as_str()),
                esc(&step.thread_href()),
                icon(Icon::MessageSquare, 16, ""),
            ));
        }
        actions.push_str(&format!(
            "<form class=\"pl-close\" method=\"post\" action=\"/messages/close\"><input type=\"hidden\" name=\"m\" value=\"{project}/{m}\"><input type=\"hidden\" name=\"next\" value=\"{next}\"><button class=\"quiet-act\">Close question</button></form>",
            m = ask.message,
            next = esc(&self.view.href()),
        ));
        let head = format!(
            "<span class=\"mod-k\">{role}{q}<b>Question for you</b><span class=\"quiet\">from {from}{when}</span></span><span class=\"mod-t\">{title}</span>",
            role = if attrs.is_empty() {
                String::new()
            } else {
                ui::trace_role().0
            },
            q = icon(Icon::MessageSquare, 16, "ask-i"),
            from = esc(&from),
            title = esc(&ask.title),
        );
        let head = if attrs.is_empty() {
            head
        } else {
            format!(
                "{}{head}{}",
                ui::trace_button_open(&ask.title, ""),
                ui::trace_button_close()
            )
        };
        format!(
            "<div id=\"q-{m}\" class=\"pl-item pl-ask\" style=\"--span:{span}\"{attrs}>{open}{head}{menu}{meta}{strip}<div class=\"mod-body md\">{body}</div><div class=\"mod-actions\">{actions}</div>{close}</div>",
            m = ask.message,
            open = ui::module_open(
                span,
                Swell::Ask,
                &format!("Question for you: {}", ask.title)
            ),
            menu = details.menu(&ask.title),
            close = ui::module_close(),
        )
    }
    /// How long a running step has run against its usual time: "1h 15m, usually 21m" (ticking),
    /// ", silent 42m" after it when it is quiet; "" when it is not running.
    fn run_words(&self, step: &StepView) -> String {
        let Some(t) = step.timing.as_ref().filter(|_| step.running()) else {
            return String::new();
        };
        let mut words = format!("<span class=\"pl-time\">{}</span>", ui::since(&t.started));
        let usually = step.usually_text();
        if !usually.is_empty() {
            words.push_str(&format!(
                "<span class=\"pl-usual\">, {}</span>",
                esc(&usually)
            ));
        }
        if step.shown() == Shown::Quiet && !step.active_at.is_empty() {
            words.push_str(&format!(
                "<span class=\"pl-usual\">, silent {}</span>",
                ui::since(&step.active_at)
            ));
        }
        words
    }
    /// What identifies a unit, behind its row's or module's "⋯": its id and recipe, the step
    /// that says how it stands (its id, fn, run and tags), its params and its page.
    fn unit_details(&self, unit: &UnitView, step: Option<&StepView>) -> ui::Details {
        let mut details = ui::Details::new()
            .id("Unit", unit.id.as_str())
            .text("Recipe", &unit.recipe);
        if let Some(step) = step {
            details = details
                .id("Step", step.id.as_str())
                .code("Function", &step.function);
            if let Some((run, _)) = step.retries() {
                let words = format!("{run}, {}", step.retries_words());
                details = details.text("Run", words.trim_end_matches(", "));
            }
            let tags: Vec<&str> = step
                .tags
                .iter()
                .map(String::as_str)
                .filter(|t| !t.starts_with("unit:"))
                .collect();
            details = details.text("Tags", &tags.join(", "));
        }
        for (name, value) in &unit.params {
            details = match ui::ValueSet::of_text(value) {
                ui::ValueSet::Detail => details.id(name, value),
                _ => details.text(name, value),
            };
        }
        details.link("Page", &unit_href(self.view, unit), "The unit's page")
    }

    /// Stopped: each failed, cancelled or stale unit on the sand, its failure's sentence and
    /// Retry with feedback first.
    fn stopped_html(&self, span: u8) -> String {
        let tally: ui::Tally = self.stopped.iter().map(|u| u.shown()).collect();
        let module = if span <= 6 {
            span
        } else if span == 10 {
            5
        } else {
            4
        };
        let mut out = format!(
            "<section id=\"plan-stopped\" class=\"pl-sec pl-stopped\" style=\"--span:{span}\" data-span=\"{span}\" aria-labelledby=\"stopped\">{}",
            ui::section_head("stopped", "Stopped", &ui::states_words(&tally))
        );
        for unit in &self.stopped {
            out.push_str(&region(
                &format!("s-{}", unit.id),
                &self.stop_html(unit, module),
            ));
        }
        // a cancel dismissed in the last ten minutes: one polite line, with its Undo
        if !self.dismissed.is_empty() {
            out.push_str("<div class=\"pl-dismissed\" role=\"status\">");
            for (unit, step) in &self.dismissed {
                // its unit's title, and its stage on a unit of several (never its id)
                let name = if unit.steps.len() > 1 && !step.stage.is_empty() {
                    format!("{} ({})", title(unit), step.stage)
                } else {
                    title(unit).to_owned()
                };
                // Undo comes back to the card it restores, the focus on it
                out.push_str(&format!(
                    "<form method=\"post\" action=\"{h}/actions\"><input type=\"hidden\" name=\"action\" value=\"undismiss\"><input type=\"hidden\" name=\"next\" value=\"{next}#s-{u}\"><p>{ic}<span class=\"pl-dn\">Dismissed: <a href=\"{h}\">{name}</a></span><button id=\"undo-{s}\" class=\"text-button\" aria-label=\"Undo dismissing {name}\">Undo</button></p></form>",
                    h = esc(&step.href()),
                    next = esc(&self.view.href()),
                    u = esc(unit.id.as_str()),
                    s = esc(step.id.as_str()),
                    ic = icon(Icon::Check, 16, ""),
                    name = esc(&name),
                ));
            }
            out.push_str("</div>");
        }
        out.push_str("</section>");
        out
    }
    fn stop_html(&self, unit: &UnitView, span: u8) -> String {
        let shown = unit.shown();
        let step = current(unit);
        let mut said = String::new();
        if let Some(step) = step {
            // the stage that stopped, on a unit of several; a unit of one step is that step,
            // whose title heads the card (its id is in the card's Details)
            // its stage on a unit of several; never a step's id (a unit of no recipe's steps
            // have no stage: the card's title says the unit)
            let stage = (unit.steps.len() > 1 && !step.stage.is_empty()).then(|| esc(&step.stage));
            let when = step
                .timing
                .as_ref()
                .and_then(|t| t.finished.as_ref().map(|end| (end, t.seconds)))
                .map(|(end, took)| {
                    format!(
                        "at {} after {}",
                        ui::clock(end),
                        esc(&ui::duration_text(took))
                    )
                });
            said = [stage, when]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ");
        }
        let marks = ui::stage_marks(&format!("Stages of {}", unit.heading()), &strip(unit));
        let mut body = String::new();
        if let Some(step) = step {
            if !step.why().is_empty() {
                body.push_str(&format!("<p class=\"pl-why\">{}</p>", esc(step.why())));
            }
            let next = step.next_step();
            if !next.is_empty() && shown == Shown::Failed {
                body.push_str(&format!("<p class=\"meta\">{}</p>", esc(&next)));
            }
        }
        let mut actions = String::new();
        if let Some(step) = step.filter(|s| s.retryable() && !s.retry_asks()) {
            let first = step.retry_first();
            actions.push_str(&format!(
                "<form class=\"pl-retry\" method=\"post\" action=\"{h}/actions\"><input type=\"hidden\" name=\"revision\" value=\"{rev}\"><input type=\"hidden\" name=\"seen\" value=\"{seen}\"><input type=\"hidden\" name=\"next\" value=\"{next}\"><details class=\"pl-fb\" data-preserve-attr=\"open\"><summary class=\"{fb}\">{ic}Retry with feedback</summary><label class=\"vh\" for=\"fb-{s}\">Feedback for the next run of {name}</label><textarea id=\"fb-{s}\" name=\"message\" rows=\"3\" maxlength=\"16384\" placeholder=\"What its next run should do differently\" data-ignore-morph></textarea><button name=\"action\" value=\"retry\" class=\"primary\">Retry with this feedback</button></details><button name=\"action\" value=\"retry\">Retry</button></form>",
                h = esc(&step.href()),
                rev = step.revision,
                seen = esc(&step.seen()),
                next = esc(&self.view.href()),
                fb = if first { "primary" } else { "" },
                ic = icon(Icon::RotateCw, 16, ""),
                s = esc(step.id.as_str()),
                name = esc(&short(unit, step)),
            ));
        }
        if let Some(step) = step {
            actions.push_str(&format!(
                "<a href=\"{}\" data-step=\"{}\">Open step</a>",
                esc(&step.href()),
                esc(step.id.as_str())
            ));
        }
        actions.push_str(&format!(
            "<a class=\"quiet-act\" href=\"{}\">Unit page</a>",
            esc(&unit_href(self.view, unit))
        ));
        // a cancel is set aside here, no confirm (Stopped then offers its Undo); a failure is
        // never dismissed
        if let Some(step) = step.filter(|s| s.cancelled() && !s.dismissed) {
            actions.push_str(&format!(
                "<form class=\"pl-dismiss\" method=\"post\" action=\"{h}/actions\"><input type=\"hidden\" name=\"action\" value=\"dismiss\"><input type=\"hidden\" name=\"next\" value=\"{next}#undo-{s}\"><button class=\"quiet-act\" aria-label=\"Dismiss {name}\" title=\"It stays on its unit, but no longer marks the unit or its project\">Dismiss</button></form>",
                h = esc(&step.href()),
                next = esc(&self.view.href()),
                s = esc(step.id.as_str()),
                name = esc(title(unit)),
            ));
        }
        format!(
            "<div id=\"s-{id}\" class=\"pl-item pl-stop\" tabindex=\"-1\" style=\"--span:{span}\"{attrs} data-said=\"{states}\">{open}{pick}<span class=\"mod-k\" id=\"k-s-{id}\">{role}{g}<b>{word}</b>{kind}<span class=\"quiet\">{said}</span>{marks}</span><span class=\"mod-t\">{title}</span>{pick_end}{menu}<div class=\"mod-body\">{body}</div><div class=\"mod-actions\">{actions}</div>{close}</div>",
            id = esc(unit.id.as_str()),
            attrs = self.attrs(unit),
            states = esc(&states_said(unit)),
            open = ui::module_open(
                span,
                Swell::Look,
                &format!("{}, {}", unit.heading(), shown.word())
            ),
            pick = ui::trace_button_open(title(unit), &format!("k-s-{}", unit.id)),
            role = ui::trace_role(),
            g = ui::mark(shown),
            word = esc(shown.word()),
            // a failure's kind in a word ("quota", "lost", "cap") when it says more
            kind = step
                .map(|s| s.caption())
                .filter(|c| !c.is_empty() && c != shown.word())
                .map(|c| format!(" <span class=\"pl-kind\">{}</span>", esc(&c)))
                .unwrap_or_default(),
            title = esc(title(unit)),
            pick_end = ui::trace_button_close(),
            menu = self.unit_details(unit, step).menu(title(unit)),
            close = ui::module_close(),
        )
    }

    /// Running or Waiting: a block a recipe (its columns its stages), then the units of no
    /// recipe, each unit one row.
    fn band_html(&self, id: &str, key: &str, name: &str, units: &[&UnitView], cols: u8) -> String {
        // the blocks: a recipe's units together, in the order of their first unit
        let mut groups: Vec<(&str, Vec<&UnitView>)> = vec![];
        for unit in units {
            match groups.iter_mut().find(|(r, _)| *r == unit.recipe.as_str()) {
                Some((_, rows)) => rows.push(unit),
                None => groups.push((unit.recipe.as_str(), vec![unit])),
            }
        }
        let line = match key {
            "running" => self.running_line(),
            _ => self.waiting_line(),
        };
        let mut out = format!(
            "<section id=\"{id}\" class=\"pl-sec pl-rows\" style=\"--span:{cols}\" data-span=\"{cols}\" aria-labelledby=\"{key}\">"
        );
        let single = groups.len() == 1;
        if !single {
            out.push_str(&rail_slot(&ui::section_head(key, name, &line).0));
        }
        for (recipe, rows) in &groups {
            let stages: Vec<&str> = rows[0].stages.iter().map(String::as_str).collect();
            let head = if recipe.is_empty() {
                // a loose step is one cell; a unit of no recipe draws its own steps
                let loose = rows.iter().filter(|u| u.steps.len() == 1).count();
                let boxed = rows.len() - loose;
                let words = match (boxed, loose) {
                    (0, n) => ui::count(n, "loose step", "loose steps"),
                    (n, 0) => format!(
                        "{}: each draws its own steps",
                        ui::no_recipe(n, ("unit", "units"))
                    ),
                    (n, m) => format!(
                        "{} and {}",
                        ui::no_recipe(n, ("unit", "units")),
                        ui::count(m, "loose step", "loose steps")
                    ),
                };
                let title = if boxed == 0 {
                    "Loose steps"
                } else {
                    "No recipe"
                };
                if single {
                    ui::section_head(key, name, &format!("{line} {words}.")).0
                } else {
                    group_head(title, &words, &[])
                }
            } else {
                let tally: ui::Tally = rows.iter().map(|u| u.shown()).collect();
                let words = format!(
                    "{} · {}",
                    ui::count(rows.len(), "unit", "units"),
                    ui::states_words(&tally)
                );
                // the recipe's name leads to every unit it made, done ones too
                let link = format!(
                    "<a class=\"sec-a\" href=\"{}?recipe={}&#38;show=all\">{}</a>",
                    esc(&self.view.href()),
                    url::form_urlencoded::byte_serialize(recipe.as_bytes()).collect::<String>(),
                    esc(recipe)
                );
                if single {
                    ui::strip_head(key, name, &format!("{line} {recipe}: {words}."), &stages)
                        .0
                        .replacen(&format!(" {}: ", esc(recipe)), &format!(" {link}: "), 1)
                } else {
                    group_head(recipe, &words, &stages).replacen(
                        &format!("<h3>{}</h3>", esc(recipe)),
                        &format!("<h3>{link}</h3>"),
                        1,
                    )
                }
            };
            out.push_str(&format!(
                "<div class=\"pl-group{}\" style=\"--n:{}\">{}",
                if recipe.is_empty() { " pl-free" } else { "" },
                stages.len().max(1),
                rail_slot(&head)
            ));
            // a recipe whose view does not check: its rows go on without it, said once
            if let Some(error) = self
                .view
                .names
                .recipe_of(rows[0].id.as_str())
                .and_then(|r| r.view_error())
            {
                out.push_str(super::unit_view::broken(error).as_str());
            }
            for unit in rows {
                out.push_str(&region(&format!("u-{}", unit.id), &self.row_html(unit)));
            }
            out.push_str("</div>");
        }
        out.push_str("</section>");
        out
    }
    /// "Quiet first, then the longest. 1 unit asks you above. 1 long run is shown apart." (the
    /// margin is at the sheet's right on a wide sheet but under the band on a narrow one, so
    /// the line names no place)
    fn running_line(&self) -> String {
        let mut line = "Quiet first, then the longest.".to_owned();
        let above = self
            .asks
            .iter()
            .filter_map(|a| a.unit.map(|u| u.id.as_str()))
            .collect::<BTreeSet<_>>()
            .len();
        if above > 0 {
            line.push_str(&format!(
                " {} you above.",
                ui::count(above, "unit asks", "units ask")
            ));
        }
        if !self.margin.is_empty() {
            line.push_str(&format!(
                " {} shown apart.",
                ui::count(self.margin.len(), "long run is", "long runs are")
            ));
        }
        line
    }
    /// "3 waiting, all held by Review the spring guide." or "3 waiting; 1 held up by a failure."
    fn waiting_line(&self) -> String {
        let n = self.waiting.len();
        let firsts: BTreeSet<String> = self
            .waiting
            .iter()
            .filter_map(|u| {
                u.all_waits()
                    .and_then(|(_, w)| w.first().map(|w| w.name.text(60)))
            })
            .collect();
        let held = self
            .waiting
            .iter()
            .filter(|u| u.all_waits().is_some())
            .count();
        let mut line = format!("{n} waiting");
        if n > 1 && held == n && firsts.len() == 1 {
            line.push_str(&format!(
                ", all held by {}",
                firsts.iter().next().map(String::as_str).unwrap_or("")
            ));
        }
        line.push('.');
        let blocked = self
            .waiting
            .iter()
            .filter(|u| u.shown() == Shown::Blocked)
            .count();
        if blocked > 0 {
            line.push_str(&format!(
                " {} held up by a failure up its chain.",
                ui::count(blocked, "unit is", "units are")
            ));
        }
        line
    }
    /// One unit's row: its head (the trace's button: its state, how long it has run against
    /// its usual time, its title), its one line under it, its cells beside it at their recipe's
    /// columns, its "⋯" at the end, and what opens in place when it is traced.
    fn row_html(&self, unit: &UnitView) -> String {
        let shown = unit.shown();
        let step = current(unit);
        let mut state = String::new();
        let mut chip = String::new();
        if let Some(step) = step {
            let run = self.run_words(step);
            if !run.is_empty() {
                state = run;
                if let Some(ratio) = over(step) {
                    chip = ui::overrun(ratio).0;
                }
            } else if unit.steps.len() > 1 && shown.spec().band != Band::Running {
                // its steps by state, when they stand apart ("1 succeeded · 5 pending"); one
                // state for all its steps is the word before it already
                let tally = unit.tally_words();
                if tally.contains('·') {
                    state = format!("<span class=\"pl-usual\">, {}</span>", esc(&tally));
                }
            }
        }
        // its one line: what it said last (its recipe's view, else its last message); a waiting
        // unit what holds it
        let sub = if shown.spec().band == Band::Running {
            self.said_html(unit)
        } else {
            let waits = waits_html(unit);
            if waits.is_empty() {
                self.said_html(unit)
            } else {
                waits
            }
        };
        let (cells, graph) = match (unit.stages.is_empty(), unit.steps.len()) {
            (false, _) => (unit.stages.len(), false),
            (true, 1) => (1, false),
            (true, _) => (layers(unit).len(), true),
        };
        let cells_html = if graph {
            self.graph_html(unit)
        } else {
            ui::stage_strip(&format!("Stages of {}", unit.heading()), &strip(unit)).0
        };
        format!(
            "<div id=\"u-{id}\" class=\"pl-row rail-slot{g}\" style=\"--cells:{cells}\"{attrs}>{rail}<div class=\"pl-lead\">{pick}<span class=\"pl-head\"><span class=\"pl-k\" id=\"k-u-{id}\"><span class=\"pl-w\">{role}{mark}<b>{word}</b></span> {state}{chip}</span> <span class=\"pl-t\">{title}</span></span>{pick_end}{sub}<a class=\"pl-page nojs\" href=\"{href}\">Open the unit</a></div><div class=\"pl-cells\">{cells_html}</div><div class=\"pl-end\">{menu}</div>{more}{more_body}{more_end}</div>",
            id = esc(unit.id.as_str()),
            g = if graph { " pl-graphed" } else { "" },
            attrs = self.attrs(unit),
            rail = ui::rail(),
            pick = ui::trace_button_open(title(unit), &format!("k-u-{}", unit.id)),
            role = ui::trace_role(),
            mark = ui::mark(shown),
            word = esc(shown.word()),
            title = esc(title(unit)),
            pick_end = ui::trace_button_close(),
            href = esc(&unit_href(self.view, unit)),
            menu = self.unit_details(unit, step).menu(title(unit)),
            more = ui::trace_more_open(),
            more_body = self.more_html(unit),
            more_end = ui::trace_more_close(),
        )
    }
    /// A unit's one line: its recipe's view (the project's own summary of it, its params kept
    /// in the row's Details), else its last message, its sender named in words and its text on
    /// one line.
    fn said_html(&self, unit: &UnitView) -> String {
        if let Some(view) = self
            .view
            .names
            .recipe_of(unit.id.as_str())
            .and_then(|r| r.view())
        {
            let drawn = super::unit_view::draw(view, unit, true);
            if super::unit_view::empty(&drawn) {
                return String::new();
            }
            return format!("<div class=\"pl-sub pl-view\">{drawn}</div>");
        }
        if unit.last_message.trim().is_empty() {
            return String::new();
        }
        format!(
            "<p class=\"pl-sub\"><b>{}</b> {}: {}</p>",
            esc(&unit.sender()),
            ui::ago(&unit.changed),
            esc(&crate::markdown::cut(
                &crate::markdown::plain(&unit.last_message),
                240
            ))
        )
    }
    /// What opens in place while a unit is traced: its steps, its last message, its buttons.
    fn more_html(&self, unit: &UnitView) -> String {
        let mut steps = String::new();
        for step in &unit.steps {
            let shown = step.shown();
            let time = match step.timing.as_ref() {
                Some(t) if step.running() => format!(" since {}", ui::clock(&t.started)),
                Some(_) => step
                    .shown_timing()
                    .map(|t| format!(" {}", esc(&ui::duration_text(t.seconds))))
                    .unwrap_or_default(),
                None => String::new(),
            };
            let waits = unit
                .waits_of(step)
                .first()
                .map(|w| format!(", waits for {}", esc(&w.name.text(48))))
                .unwrap_or_default();
            steps.push_str(&format!(
                "<li><a href=\"{h}\" data-step=\"{s}\">{name}</a><span class=\"pl-st\">{g}{w}{time}{waits}</span></li>",
                h = esc(&step.href()),
                s = esc(step.id.as_str()),
                name = esc(&short(unit, step)),
                g = ui::mark(shown),
                w = esc(shown.word()),
            ));
        }
        let said = if unit.last_message.trim().is_empty() {
            "<p class=\"meta\">No message yet.</p>".to_owned()
        } else {
            format!(
                "<p class=\"meta\"><b>{}{}</b> · {}</p><div class=\"md pl-said-t\">{}</div>",
                if unit.last_received { "Note from " } else { "" },
                esc(&unit.sender()),
                ui::ago(&unit.changed),
                crate::markdown::excerpt(&unit.last_message, 420).0
            )
        };
        let mut actions = String::new();
        if let Some(step) = current(unit) {
            actions.push_str(&format!(
                "<a class=\"primary\" href=\"{}\" data-step=\"{}\">Open step{}</a><a class=\"quiet-act\" href=\"{}\">{}Message</a>",
                esc(&step.href()),
                esc(step.id.as_str()),
                icon(Icon::ArrowRight, 16, ""),
                esc(&step.thread_href()),
                icon(Icon::MessageSquare, 16, ""),
            ));
        }
        actions.push_str(&format!(
            "<a class=\"quiet-act\" href=\"{}\">Unit page</a>",
            esc(&unit_href(self.view, unit))
        ));
        format!(
            "<div class=\"pl-more\"><div class=\"pl-steps\"><h3 class=\"pl-mh\">Steps</h3><ol>{steps}</ol></div><div class=\"pl-said\"><h3 class=\"pl-mh\">Last message</h3>{said}</div><div class=\"mod-actions\">{actions}</div></div>"
        )
    }
    /// A unit of no recipe as its own small graph: its steps in columns by how far down its
    /// own chain each is, each a cell, with a small connector for each wait between them (a
    /// fan-in or fan-out reads as itself).
    fn graph_html(&self, unit: &UnitView) -> String {
        let layers = layers(unit);
        let rows = layers.iter().map(Vec::len).max().unwrap_or(1).max(1);
        // each cell's place: its column and its row, centred in the tallest column
        let mut at = std::collections::BTreeMap::<&str, (usize, f64)>::new();
        for (c, layer) in layers.iter().enumerate() {
            let off = (rows - layer.len()) as f64 / 2.0;
            for (r, step) in layer.iter().enumerate() {
                at.insert(step.id.as_str(), (c, r as f64 + off));
            }
        }
        let edges = self.inner_edges(unit);
        let mut columns = vec![];
        let mut out = String::new();
        for (c, layer) in layers.iter().enumerate() {
            if c > 0 {
                // the connectors into this column, one path a wait
                let mut paths = String::new();
                for (from, to) in &edges {
                    let (Some(&(fc, fr)), Some(&(tc, tr))) =
                        (at.get(from.as_str()), at.get(to.as_str()))
                    else {
                        continue;
                    };
                    if tc != c {
                        continue;
                    }
                    let (y1, y2) = ((fr + 0.5) * 100.0, (tr + 0.5) * 100.0);
                    paths.push_str(&format!(
                        "<path d=\"M0 {y1} C14 {y1} 14 {y2} 28 {y2}\"{} vector-effect=\"non-scaling-stroke\"/>",
                        if fc + 1 == tc {
                            ""
                        } else {
                            " stroke-dasharray=\"3 3\""
                        }
                    ));
                }
                out.push_str(&format!(
                    "<svg class=\"pl-wire\" viewBox=\"0 0 28 {}\" preserveAspectRatio=\"none\" aria-hidden=\"true\">{paths}</svg>",
                    rows * 100
                ));
                columns.push("28px".to_owned());
            }
            let stages: Vec<Stage> = layer.iter().map(|s| stage_of(s, &short(unit, s))).collect();
            let off = (rows - layer.len()) as f64 / 2.0;
            out.push_str(&format!(
                "<div class=\"pl-layer\" style=\"--off:{off}\">{}</div>",
                ui::stage_strip(
                    &format!("Stages of {}, step {} of its chain", unit.id, c + 1),
                    &stages
                )
            ));
            columns.push("minmax(0, 1fr)".to_owned());
        }
        format!(
            "<div class=\"pl-graph\" style=\"--rows:{rows};grid-template-columns:{}\">{out}</div>",
            columns.join(" ")
        )
    }
    /// The waits between a unit's own steps, by step id.
    fn inner_edges(&self, unit: &UnitView) -> Vec<(String, String)> {
        let ids: BTreeSet<&str> = unit.steps.iter().map(|s| s.id.as_str()).collect();
        let mut out = vec![];
        for r in &self.view.relations {
            if let (super::board::Endpoint::Step(from), super::board::Endpoint::Step(to)) =
                (&r.from, &r.to)
                && ids.contains(from.as_str())
                && ids.contains(to.as_str())
            {
                let edge = (from.to_string(), to.to_string());
                if !out.contains(&edge) {
                    out.push(edge);
                }
            }
        }
        out
    }

    /// The margin: each long run's progress as it reported it.
    /// The margin: the long runs, down the sheet's right beside `rows` of it (`wide` from
    /// 2000px, where Running and Waiting share one).
    fn margin_html(&self, rows: usize, wide: usize) -> String {
        let mut out = format!(
            "<aside id=\"plan-margin\" class=\"pl-margin\" style=\"--span:2;--rows:{rows};--rows-w:{wide}\" data-span=\"2\" aria-label=\"Long runs\">"
        );
        for unit in &self.margin {
            let Some(step) = unit.steps.first() else {
                continue;
            };
            let progress = step.progress.as_ref();
            let run = ui::LongRun {
                name: step.id.to_string(),
                title: step.heading().to_owned(),
                doc: step.doc_rest().to_owned(),
                href: step.href(),
                run: step.timing.as_ref().map_or(1, |t| t.runs),
                since: step
                    .timing
                    .as_ref()
                    .map(|t| t.started.clone())
                    .unwrap_or_default(),
                fields: progress
                    .map(|p| {
                        p.fields
                            .iter()
                            .map(|f| (f.name.clone(), serde_json::Value::String(f.value.clone())))
                            .collect()
                    })
                    .unwrap_or_default(),
                at: progress.map(|p| p.at.clone()).unwrap_or_default(),
            };
            out.push_str(&region(
                &format!("m-{}", unit.id),
                &format!(
                    "<div id=\"m-{}\" class=\"pl-item\"{} data-said=\"{}\">{}</div>",
                    esc(unit.id.as_str()),
                    self.attrs(unit),
                    esc(&states_said(unit)),
                    // its title opens the step in the drawer, as a stage's cell does
                    ui::margin_module(&run).0.replacen(
                        &format!("<a href=\"{}\">", esc(&run.href)),
                        &format!(
                            "<a href=\"{}\" id=\"n-{s}\" data-step=\"{s}\">",
                            esc(&run.href),
                            s = esc(step.id.as_str())
                        ),
                        1
                    )
                ),
            ));
        }
        out.push_str("</aside>");
        out
    }

    /// Done: a dense index of every done unit, newest first, folded.
    fn done_html(&self, cols: u8) -> String {
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
        let times: Vec<f64> = self
            .done
            .iter()
            .map(|u| took(u).0)
            .filter(|t| *t > 0.0)
            .collect();
        let range = match (
            times.iter().copied().reduce(f64::min),
            times.iter().copied().reduce(f64::max),
        ) {
            (Some(a), Some(b)) if a < b => format!(
                "They took {} to {}, newest first.",
                ui::duration_text(a),
                ui::duration_text(b)
            ),
            _ => "Newest first.".to_owned(),
        };
        let latest = ui::latest_list("Finished last", &self.latest());
        let mut items = String::new();
        for unit in &self.done {
            let (took, _) = took(unit);
            // a cancel the owner dismissed leaves its unit done, still said: "b cancelled and
            // dismissed" (its step's page has the Undo)
            let cancels: Vec<String> = unit
                .steps
                .iter()
                .filter(|s| s.cancelled())
                .map(|s| short(unit, s))
                .collect();
            let dismissed = unit.steps.iter().any(|s| s.cancelled() && s.dismissed);
            items.push_str(&format!(
                "<li data-said=\"{states}\"><a href=\"{h}\"><span class=\"pl-at\">{at}</span>{g}<span class=\"pl-dt\">{t}</span><span class=\"pl-dk\">{k}</span></a></li>",
                h = esc(&unit_href(self.view, unit)),
                states = esc(&states_said(unit)),
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
                        if dismissed { " and dismissed" } else { "" }
                    )
                } else if took > 0.0 {
                    format!("took {}", esc(&ui::duration_text(took)))
                } else {
                    esc(unit.shown().word())
                },
            ));
        }
        // open when asked for: Show done, a find, or a recipe's every unit
        let open =
            self.view.show == "done" || !self.view.q.is_empty() || !self.view.recipe.is_empty();
        format!(
            "<section id=\"plan-done\" class=\"pl-sec pl-done\" style=\"--span:{cols}\" data-span=\"{cols}\" aria-labelledby=\"done\">{head}{latest}<details class=\"pl-index\" data-preserve-attr=\"open\"{open}><summary>{chev}<span class=\"pl-show\">Show the {n}</span><span class=\"pl-hide\">Hide the {n}</span></summary><p class=\"meta pl-range\">{range}</p><ol class=\"pl-idx\">{items}</ol></details></section>",
            head = ui::section_head("done", "Done", &line),
            open = if open { " open" } else { "" },
            chev = icon(Icon::ChevronDown, 16, "chev"),
            n = esc(&ui::count(self.done.len(), "done unit", "done units")),
        )
    }
}

/// The Plan · Both · Board switch: a link each, so a choice works without script (the page
/// drawn in it and the choice kept in a cookie); with script `board.js` takes the click, shows
/// the view at once and keeps it per project, wide and narrow apart. The chosen one is filled.
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
/// A head between rows, a place the trace's rail runs through.
fn rail_slot(html: &str) -> String {
    format!("<div class=\"rail-slot\">{}{html}</div>", ui::rail())
}
/// A block's head inside a band: its recipe's name and count, each stage over its column.
fn group_head(name: &str, line: &str, stages: &[&str]) -> String {
    ui::strip_head("", name, line, stages)
        .0
        .replacen(
            "class=\"sec-h sec-strip\"",
            "class=\"sec-h sec-strip sec-group\"",
            1,
        )
        .replacen("<h2>", "<h3>", 1)
        .replacen("</h2>", "</h3>", 1)
}
/// A unit's steps in columns by their own chain: a step after the column of the latest step of
/// its unit it waits for, in plan order within a column.
fn layers(unit: &UnitView) -> Vec<Vec<&StepView>> {
    // the unit's rows are the plan's depths: number them from its own first
    let mut out: Vec<Vec<&StepView>> = unit
        .rows
        .iter()
        .map(|row| {
            row.iter()
                .filter_map(|s| unit.steps.iter().find(|o| o.id == s.id))
                .collect()
        })
        .filter(|row: &Vec<&StepView>| !row.is_empty())
        .collect();
    if out.is_empty() {
        out.push(unit.steps.iter().collect());
    }
    out
}
/// A unit's waits, said: "review waits for publish · Terns (running)"; or what else holds its
/// first step.
fn waits_html(unit: &UnitView) -> String {
    if let Some((step, waits)) = unit.all_waits() {
        const NAMED: usize = 2;
        let named: Vec<String> = waits
            .iter()
            .take(NAMED)
            .map(|w| {
                // by its title in words; an untitled step by its id, its only name
                let words = if w.name.titled() {
                    esc(&w.name.text(60))
                } else {
                    format!("<code>{}</code>", esc(&w.name.id))
                };
                format!(
                    "<a href=\"{}\"{}>{words}</a>{}",
                    esc(&w.href),
                    if w.opens.is_empty() {
                        String::new()
                    } else {
                        format!(" data-opens=\"{}\"", esc(&w.opens))
                    },
                    if w.note.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", esc(&w.note))
                    }
                )
            })
            .collect();
        let more = waits.len().saturating_sub(NAMED);
        let mut list = match named.as_slice() {
            [one] => one.clone(),
            [a, b] if more == 0 => format!("{a} and {b}"),
            all => all.join(", "),
        };
        if more > 0 {
            list.push_str(&format!(" and {more} more"));
        }
        return format!(
            "<p class=\"pl-sub pl-waits\">{} waits for {list}.</p>",
            esc(&short(unit, step))
        );
    }
    let Some(step) = unit
        .steps
        .iter()
        .find(|s| s.standing().spec().band != Band::Done)
    else {
        return String::new();
    };
    let words = match step.shown() {
        Shown::Paused => step.hold_words(),
        Shown::Queued => step
            .queued
            .first()
            .map(|q| format!("Queued: {q}"))
            .unwrap_or_else(|| "Queued.".into()),
        Shown::Held => step
            .waits
            .first()
            .cloned()
            .unwrap_or_else(|| "Held.".into()),
        Shown::Blocked => "Held up by a failure up its chain.".into(),
        _ if step.ready() => "Ready: it starts when the runner next looks.".into(),
        _ => step.waits.first().cloned().unwrap_or_default(),
    };
    if words.is_empty() {
        return String::new();
    }
    format!("<p class=\"pl-sub pl-waits\">{}</p>", esc(&words))
}

#[derive(Template)]
#[template(path = "project.html")]
struct ProjectTemplate<'a> {
    view: &'a ProjectView,
    plan: &'a Plan<'a>,
    js_url: String,
    board_js_url: String,
}
/// The page body: the plan's sheet in its pane, the board beside it, the step drawer.
pub fn body(view: &ProjectView, plan: &Plan<'_>) -> Result<TrustedHtml, askama::Error> {
    TrustedHtml::from_template(&ProjectTemplate {
        view,
        plan,
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
    let body = body(view, &plan).map_err(render_error)?;
    let frame = Frame {
        head: plan.band(),
        meta: plan.meta(),
        tools: plan.tools(),
        inner: false,
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
/// The whole page.
pub fn render(
    view: &ProjectView,
    shared: &DashboardSnapshot,
    viewer: &Viewer,
) -> Result<TrustedHtml, PublicError> {
    let nav = NavView::new(shared, Some(view.project.id), "plan")?;
    let (body, frame, batch) = draw(view, shared, viewer)?;
    super::render_framed(
        &view.project.tab_words(),
        &body,
        &nav,
        viewer,
        &format!("{}/stream?{}", view.href(), view.query),
        &batch.version,
        &view.href(),
        &frame,
    )
    .map_err(render_error)
}
