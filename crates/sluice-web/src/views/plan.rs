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

/// The grid every plan sits on, and its show-grid switch's target.
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
    if !step.stage.is_empty() {
        return step.stage.clone();
    }
    let id = step.id.as_str();
    id.strip_prefix(&format!("{}-", unit.id))
        .unwrap_or(id)
        .to_owned()
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
        Plan {
            view,
            asks,
            stopped,
            running,
            waiting,
            margin,
            done,
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
    /// The band's head and its Recently finished, in one region its stream patches.
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
        // the description's first block, the rest folded under More (kept open per project)
        let lead = if self.view.project.description.trim().is_empty() {
            TrustedHtml::default()
        } else {
            let (first, rest) = self.view.about();
            let mut lead = first.0;
            if let Some(more) = rest {
                lead.push_str(&format!(
                    "{}<div class=\"md\">{}</div>{}",
                    ui::more_open(
                        "about-more",
                        "More",
                        "Less",
                        ui::keyed("sluice.about", self.view.project.id).as_str(),
                        false
                    ),
                    more.0,
                    ui::more_close("")
                ));
            }
            TrustedHtml::owned(lead)
        };
        let head = ui::band_head(&self.view.project.name, &lead, &summary);
        let finished: Vec<ui::Finished> = self
            .done
            .iter()
            .filter(|u| !u.finished().is_empty())
            .take(5)
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
                }
            })
            .collect();
        let today = self
            .done
            .iter()
            .filter(|u| since_secs(u.finished()).is_some_and(|s| s < 86_400.0))
            .count();
        let line = match today {
            0 => "Newest first.".to_owned(),
            n => format!("Newest first. {} in the last day.", n),
        };
        let strip = ui::recent_strip("plan-rf", &line, &finished);
        TrustedHtml::owned(format!(
            "<div id=\"plan-band\" class=\"plan-band\">{head}{strip}</div>"
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
    /// The paper row's tools: Find, the Plan · Both · Board switch with a board, Show grid and
    /// the plan as Mermaid text.
    pub fn tools(&self) -> TrustedHtml {
        let view = self.view;
        let href = view.href();
        let mut out = String::new();
        if !view.plan_empty() {
            out.push_str(ui::search_open("stream", &href).as_str());
            out.push_str(&format!(
                "<form class=\"board-tools pl-find\" method=\"get\" action=\"{h}\" role=\"search\" aria-label=\"Find units\"><div class=\"q-field\">{i}<input type=\"search\" name=\"q\" value=\"{q}\" data-find placeholder=\"Find unit, step or title\" aria-label=\"Find units by id or title, or by a step's id, title or description\" autocomplete=\"off\" spellcheck=\"false\" enterkeyhint=\"search\" data-preserve-attr=\"value\"><a class=\"q-clear\" data-clear-q href=\"{c}\" aria-label=\"Clear the find\" title=\"Clear the find\">{x}</a></div>{focus}{show}<button class=\"apply\">Find</button></form>",
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
            out.push_str(&format!(
                "<div class=\"view-switch\" role=\"group\" aria-label=\"Show\"><button type=\"button\" data-view-tab=\"plan\" aria-pressed=\"false\" data-preserve-attr=\"aria-pressed\">{}Plan</button><button type=\"button\" data-view-tab=\"both\" aria-pressed=\"false\" data-preserve-attr=\"aria-pressed\">{}Both</button><button type=\"button\" data-view-tab=\"board\" aria-pressed=\"false\" data-preserve-attr=\"aria-pressed\">{}Board</button></div>",
                icon(Icon::Workflow, 16, ""),
                icon(Icon::Columns2, 16, ""),
                icon(Icon::LayoutDashboard, 16, ""),
            ));
        }
        if !view.plan_empty() {
            out.push_str(ui::grid_toggle(GRID).as_str());
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
        let stopped_span = match (self.asks.is_empty(), self.stopped.is_empty()) {
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
            (!self.stopped.is_empty()).then(|| self.stopped_html(stopped_span)),
        ));
        if !self.asks.is_empty() || !self.stopped.is_empty() {
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
        let from = if ask.step.is_empty() {
            "the orchestrator".to_owned()
        } else {
            ask.step.clone()
        };
        let when = a
            .question
            .map(|q| format!(" at {}, {}", ui::clock(&q.at), ui::ago(&q.at)))
            .unwrap_or_default();
        let (attrs, meta, strip) = match (a.unit, a.step) {
            (Some(unit), Some(step)) => {
                // the unit that asks: its id, its title, its recipe and how its run goes
                let mut meta = format!("<b>{}</b>", esc(unit.id.as_str()));
                if unit.titled() {
                    meta.push_str(&format!(" {}", esc(&ui::cut(&unit.title, 90))));
                }
                if !unit.recipe.is_empty() {
                    meta.push_str(&format!(" · {}", esc(&unit.recipe)));
                }
                if let Some(t) = step.timing.as_ref().filter(|_| step.running()) {
                    meta.push_str(&format!(" · running {}", ui::since(&t.started)));
                }
                let usually = step.usually_text();
                if !usually.is_empty() {
                    meta.push_str(&format!(" · {}", esc(&usually)));
                }
                if let Some(ratio) = over(step) {
                    meta.push_str(&format!(" {}", ui::overrun(ratio)));
                }
                let strip = if a.strip {
                    ui::stage_strip(&format!("Stages of {}", unit.id), &strip(unit)).0
                } else {
                    String::new()
                };
                (
                    self.attrs(unit),
                    format!("<p class=\"mod-meta\">{meta}</p>"),
                    strip,
                )
            }
            _ => (String::new(), String::new(), String::new()),
        };
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
                "<a href=\"{}\" data-step=\"{s}\">Open {s}</a><a class=\"quiet-act\" href=\"{}\">{}Message</a>",
                esc(&step.href()),
                esc(&step.thread_href()),
                icon(Icon::MessageSquare, 16, ""),
                s = esc(step.id.as_str()),
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
                ui::trace_button_open(),
                ui::trace_button_close()
            )
        };
        format!(
            "<div id=\"q-{m}\" class=\"pl-item pl-ask\" style=\"--span:{span}\"{attrs}>{open}{head}{meta}{strip}<div class=\"mod-body md\">{body}</div><div class=\"mod-actions\">{actions}</div>{close}</div>",
            m = ask.message,
            open = ui::module_open(
                span,
                Swell::Ask,
                &format!("Question for you: {}", ask.title)
            ),
            close = ui::module_close(),
        )
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
        out.push_str("</section>");
        out
    }
    fn stop_html(&self, unit: &UnitView, span: u8) -> String {
        let shown = unit.shown();
        let step = current(unit);
        let mut said = String::new();
        if let Some(step) = step {
            said.push_str(&esc(&short(unit, step)));
            if let Some((n, _)) = step.retries() {
                said.push_str(&format!(", run {n}"));
            }
            if let Some(t) = step.timing.as_ref()
                && let Some(end) = &t.finished
            {
                said.push_str(&format!(
                    ", at {} after {}",
                    ui::clock(end),
                    esc(&ui::duration_text(t.seconds))
                ));
            }
        }
        let mut meta = format!("<b>{}</b>", esc(unit.id.as_str()));
        if !unit.recipe.is_empty() {
            meta.push_str(&format!(" · {}", esc(&unit.recipe)));
        }
        meta.push_str(&format!(
            " {}",
            ui::stage_marks(&format!("Stages of {}", unit.id), &strip(unit))
        ));
        let mut body = String::new();
        if let Some(step) = step {
            if !step.why().is_empty() {
                body.push_str(&format!("<p>{}</p>", esc(step.why())));
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
                "<form class=\"pl-retry\" method=\"post\" action=\"{h}/actions\"><input type=\"hidden\" name=\"revision\" value=\"{rev}\"><input type=\"hidden\" name=\"seen\" value=\"{seen}\"><input type=\"hidden\" name=\"next\" value=\"{next}\"><details class=\"pl-fb\" data-preserve-attr=\"open\"><summary class=\"{fb}\">{ic}Retry with feedback</summary><label class=\"vh\" for=\"fb-{s}\">Feedback for {s}'s next run</label><textarea id=\"fb-{s}\" name=\"message\" rows=\"3\" maxlength=\"16384\" placeholder=\"What its next run should do differently\" data-ignore-morph></textarea><button name=\"action\" value=\"retry\" class=\"primary\">Retry with this feedback</button></details><button name=\"action\" value=\"retry\">Retry</button></form>",
                h = esc(&step.href()),
                rev = step.revision,
                seen = esc(&step.seen()),
                next = esc(&self.view.href()),
                fb = if first { "primary" } else { "" },
                ic = icon(Icon::RotateCw, 16, ""),
                s = esc(step.id.as_str()),
            ));
        }
        if let Some(step) = step {
            actions.push_str(&format!(
                "<a href=\"{}\" data-step=\"{s}\">Open {s}</a>",
                esc(&step.href()),
                s = esc(step.id.as_str())
            ));
        }
        actions.push_str(&format!(
            "<a class=\"quiet-act\" href=\"{}\">Unit page</a>",
            esc(&unit_href(self.view, unit))
        ));
        format!(
            "<div id=\"s-{id}\" class=\"pl-item pl-stop\" style=\"--span:{span}\"{attrs} data-said=\"{states}\">{open}{pick}<span class=\"mod-k\">{role}{g}<b>{word}</b>{kind}<span class=\"quiet\">{said}</span></span><span class=\"mod-t\">{title}</span>{pick_end}<p class=\"mod-meta\">{meta}</p><div class=\"mod-body\">{body}</div><div class=\"mod-actions\">{actions}</div>{close}</div>",
            id = esc(unit.id.as_str()),
            attrs = self.attrs(unit),
            states = esc(&states_said(unit)),
            open = ui::module_open(span, Swell::Look, &format!("{}, {}", unit.id, shown.word())),
            pick = ui::trace_button_open(),
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
            close = ui::module_close(),
        )
    }

    /// Running or Waiting: a block a recipe (its columns its stages), then the units of no
    /// recipe, each unit one row on the grid.
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
            let (lead, cells) = widths(ROW, stages.len());
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
                    group_head(title, &words, lead, &[])
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
                    ui::strip_head(
                        key,
                        name,
                        &format!("{line} {recipe}: {words}."),
                        lead,
                        &stages,
                    )
                    .0
                    .replacen(
                        &format!(" {}: ", esc(recipe)),
                        &format!(" {link}: "),
                        1,
                    )
                } else {
                    group_head(recipe, &words, lead, &stages).replacen(
                        &format!("<h3>{}</h3>", esc(recipe)),
                        &format!("<h3>{link}</h3>"),
                        1,
                    )
                }
            };
            out.push_str(&format!(
                "<div class=\"pl-group\" style=\"--lead:{lead};--cells:{cells};{}\">{}",
                medium(cells),
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
    /// "Quiet first, then the longest. a-7 asks you above; survey runs in the margin."
    fn running_line(&self) -> String {
        let mut line = "Quiet first, then the longest.".to_owned();
        let above: Vec<&str> = self
            .asks
            .iter()
            .filter_map(|a| a.unit.map(|u| u.id.as_str()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if !above.is_empty() {
            line.push_str(&format!(
                " {} {} you above.",
                join(&above),
                if above.len() == 1 { "asks" } else { "ask" }
            ));
        }
        if !self.margin.is_empty() {
            let names: Vec<&str> = self.margin.iter().map(|u| u.id.as_str()).collect();
            line.push_str(&format!(
                " {} {} in the margin.",
                join(&names),
                if names.len() == 1 { "runs" } else { "run" }
            ));
        }
        line
    }
    /// "3 waiting, all held by a-6-publish." or "3 waiting; 1 held up by a failure."
    fn waiting_line(&self) -> String {
        let n = self.waiting.len();
        let firsts: BTreeSet<String> = self
            .waiting
            .iter()
            .filter_map(|u| {
                u.all_waits()
                    .and_then(|(_, w)| w.first().map(|w| w.name.id.clone()))
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
    /// One unit's row: its head (the trace's button), its line, its cells, and what opens in
    /// place when it is traced.
    fn row_html(&self, unit: &UnitView) -> String {
        let shown = unit.shown();
        let step = current(unit);
        let mut facts = format!("<b class=\"pl-id\">{}</b>", esc(unit.id.as_str()));
        if !unit.recipe.is_empty() {
            facts.push_str(&format!(" · {}", esc(&unit.recipe)));
        }
        let mut chip = String::new();
        if let Some(step) = step {
            if let Some(t) = step.timing.as_ref().filter(|_| step.running()) {
                if shown == Shown::Quiet && !step.active_at.is_empty() {
                    facts.push_str(&format!(" · silent {}", ui::since(&step.active_at)));
                }
                facts.push_str(&format!(" · running {}", ui::since(&t.started)));
                let usually = step.usually_text();
                if !usually.is_empty() {
                    facts.push_str(&format!(" · {}", esc(&usually)));
                }
                if let Some(ratio) = over(step) {
                    chip = ui::overrun(ratio).0;
                }
            } else if unit.steps.len() > 1 {
                facts.push_str(&format!(" · {}", esc(&unit.tally_words())));
            }
            // a retried step's run and how the runs before it ended
            let tries = step.retries_html();
            if !tries.as_str().is_empty() {
                facts.push_str(&format!(" · {}", tries.as_str()));
            }
        }
        // what it said (its recipe's view, else its last message); a waiting unit what holds
        // it first
        let sub = if shown.spec().band == Band::Running {
            self.said_html(unit)
        } else {
            format!("{}{}", waits_html(unit), self.said_html(unit))
        };
        let (lead, cells) = match (unit.stages.is_empty(), unit.steps.len()) {
            (false, _) => widths(ROW, unit.stages.len()),
            (true, 1) => (ROW - 2, 2),
            (true, _) => {
                let layers = layers(unit).len();
                let cells = (2 * layers as u8).clamp(2, ROW - 4);
                (ROW - cells, cells)
            }
        };
        let cells_html = if unit.stages.is_empty() && unit.steps.len() > 1 {
            self.graph_html(unit)
        } else {
            ui::stage_strip(&format!("Stages of {}", unit.id), &strip(unit)).0
        };
        format!(
            "<div id=\"u-{id}\" class=\"pl-row rail-slot\" style=\"--lead:{lead};--cells:{cells};{m}\"{attrs}>{rail}<div class=\"pl-lead\">{pick}<span class=\"pl-k\">{role}{g}<b>{word}</b>{chip}</span><span class=\"pl-t\">{title}</span><span class=\"pl-m\">{facts}</span>{pick_end}{sub}<a class=\"pl-page nojs\" href=\"{href}\">Open {id}</a></div><div class=\"pl-cells{dense}\">{cells_html}</div>{more}{more_body}{more_end}</div>",
            id = esc(unit.id.as_str()),
            m = medium(cells),
            // five stages or more: a cell is too narrow for its overrun tag (the row's chip says it)
            dense = if unit.stages.len() >= 5 {
                " pl-dense"
            } else {
                ""
            },
            attrs = self.attrs(unit),
            rail = ui::rail(),
            pick = ui::trace_button_open(),
            role = ui::trace_role(),
            g = ui::mark(shown),
            word = esc(shown.word()),
            title = esc(title(unit)),
            pick_end = ui::trace_button_close(),
            href = esc(&unit_href(self.view, unit)),
            more = ui::trace_more_open(),
            more_body = self.more_html(unit),
            more_end = ui::trace_more_close(),
        )
    }
    /// A running unit's line: its recipe's view (the project's own summary of it), else its
    /// last message.
    fn said_html(&self, unit: &UnitView) -> String {
        if let Some(view) = self
            .view
            .names
            .recipe_of(unit.id.as_str())
            .and_then(|r| r.view())
        {
            return format!(
                "<div class=\"pl-sub pl-view\">{}</div>",
                super::unit_view::draw(view, unit, true)
            );
        }
        if unit.last_message.trim().is_empty() {
            return String::new();
        }
        format!(
            "<p class=\"pl-sub\"><b>{}</b> {}: {}</p>",
            esc(&unit.last_from),
            ui::ago(&unit.changed),
            esc(&crate::markdown::cut(
                &crate::markdown::plain(&unit.last_message),
                160
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
                .map(|w| format!(", waits for {}", esc(&w.name.id)))
                .unwrap_or_default();
            steps.push_str(&format!(
                "<li><a href=\"{h}\" data-step=\"{s}\">{name}</a><code class=\"pl-fn\">{f}</code><span class=\"pl-st\">{g}{w}{time}{waits}</span></li>",
                h = esc(&step.href()),
                s = esc(step.id.as_str()),
                name = esc(&short(unit, step)),
                f = esc(&step.function),
                g = ui::mark(shown),
                w = esc(shown.word()),
            ));
        }
        let said = if unit.last_message.trim().is_empty() {
            "<p class=\"meta\">No message yet.</p>".to_owned()
        } else {
            format!(
                "<p class=\"meta\"><b>{}</b> · {}</p><div class=\"md pl-said-t\">{}</div>",
                esc(&unit.last_from),
                ui::ago(&unit.changed),
                crate::markdown::excerpt(&unit.last_message, 420).0
            )
        };
        let mut actions = String::new();
        if let Some(step) = current(unit) {
            actions.push_str(&format!(
                "<a class=\"primary\" href=\"{}\" data-step=\"{s}\">Open {s}{}</a><a class=\"quiet-act\" href=\"{}\">{}Message</a>",
                esc(&step.href()),
                icon(Icon::ArrowRight, 16, ""),
                esc(&step.thread_href()),
                icon(Icon::MessageSquare, 16, ""),
                s = esc(step.id.as_str()),
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
        let mut items = String::new();
        for unit in &self.done {
            let (took, _) = took(unit);
            // a cancel the owner dismissed leaves its unit done, still said
            let cancels: Vec<&str> = unit
                .steps
                .iter()
                .filter(|s| s.cancelled())
                .map(|s| s.id.as_str())
                .collect();
            items.push_str(&format!(
                "<li data-said=\"{states}\"><a href=\"{h}\"><span class=\"pl-at\">{at}</span>{g}<code class=\"pl-did\">{id}</code><span class=\"pl-dt\">{t}</span><span class=\"pl-dk\">{k}</span></a></li>",
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
                id = esc(unit.id.as_str()),
                t = if unit.titled() {
                    esc(&unit.title)
                } else {
                    String::new()
                },
                k = if !cancels.is_empty() {
                    format!("{} cancelled", esc(&join(&cancels)))
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
            "<section id=\"plan-done\" class=\"pl-sec pl-done\" style=\"--span:{cols}\" data-span=\"{cols}\" aria-labelledby=\"done\">{head}<details class=\"pl-index\" data-preserve-attr=\"open\"{open}><summary>{chev}<span class=\"pl-show\">Show the {n}</span><span class=\"pl-hide\">Hide the {n}</span></summary><p class=\"meta pl-range\">{range}</p><ol class=\"pl-idx\">{items}</ol></details></section>",
            head = ui::section_head("done", "Done", &line),
            open = if open { " open" } else { "" },
            chev = icon(Icon::ChevronDown, 16, "chev"),
            n = esc(&ui::count(self.done.len(), "done unit", "done units")),
        )
    }
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
fn group_head(name: &str, line: &str, lead: u8, stages: &[&str]) -> String {
    ui::strip_head("", name, line, lead, stages)
        .0
        .replacen(
            "class=\"sec-h sec-strip\"",
            "class=\"sec-h sec-strip sec-group\"",
            1,
        )
        .replacen("<h2>", "<h3>", 1)
        .replacen("</h2>", "</h3>", 1)
}
/// A band of rows lays out on twelve columns of its own, the strip rule's (a head and its rows
/// share them at every width), whatever share of the sheet the band takes.
const ROW: u8 = 12;
/// The same split on a medium sheet (under 1200px), where a row is narrow: a strip of three
/// stages or more takes eight of the twelve columns, its head the other four.
fn medium(cells: u8) -> String {
    let cells = if cells >= 6 { 8 } else { cells };
    format!("--lead-m:{};--cells-m:{cells}", ROW - cells)
}
/// How a row of `cols` columns splits for `n` stages: its head's columns and its cells'. A
/// stage takes two columns while that leaves the head four, else one, else the cells share
/// what is left.
fn widths(cols: u8, n: usize) -> (u8, u8) {
    let n = n.max(1) as u8;
    let room = cols.saturating_sub(4).max(1);
    let cells = if 2 * n <= room {
        2 * n
    } else if n <= room {
        n
    } else {
        room
    };
    (cols - cells, cells)
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
/// A unit's waits, said: "review waits for Terns `a-6-publish` (running)"; or what else holds
/// its first step.
fn waits_html(unit: &UnitView) -> String {
    if let Some((step, waits)) = unit.all_waits() {
        const NAMED: usize = 2;
        let named: Vec<String> = waits
            .iter()
            .take(NAMED)
            .map(|w| {
                let words = if w.name.titled() {
                    format!(
                        "{} <code>{}</code>",
                        esc(&ui::cut(&w.name.title, 60)),
                        esc(&w.name.id)
                    )
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
