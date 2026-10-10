//! Home (`/`, DESIGN.md, Pages: Home): every project as a cover and an index. The band holds
//! "sluice" and one sentence across the projects, the runner stopped when it is, and what
//! finished most recently; under it For you (every open question to the owner, swollen and
//! answerable in place) beside the day in a line a project; then each project as a module,
//! the ones that need the owner first: its own summary sentence, a square a unit, its questions,
//! its stopped steps, its running steps against their usual time and what it finished last;
//! then the index of every project, archived ones too.
use super::*;
use crate::streams::{PatchRegion, RenderedBatch};
use axum::{
    Extension,
    extract::{Query, State},
    response::Html,
    routing::get,
};
use sluice_model::shown::Band;
use std::collections::BTreeMap;
use ui::{Shown, esc};

/// One project as home draws it: its summary, its board (none when its plan cannot be read)
/// and what home reads from it.
#[derive(Clone, Debug)]
pub struct HomeProject {
    pub project: ProjectView,
    /// Each unit as the summary counts it, and as a square.
    pub units: Vec<ui::UnitFact>,
    /// Its running steps: their unit (or step), its page, since when, its usual time, quiet.
    pub running: Vec<Running>,
    /// Its units (or steps of no unit) that finished last, newest first.
    pub finished: Vec<ui::Finished>,
    /// How many steps it has.
    pub steps: usize,
    /// Its board was read: a project whose plan cannot be read says only its counts.
    pub read: bool,
}
/// A run is past its usual time, as home says it, from half again that time.
pub const OVER: f64 = 1.5;
/// A running step as a project's module lists it.
#[derive(Clone, Debug)]
pub struct Running {
    pub step: String,
    pub unit: String,
    pub href: String,
    pub since: String,
    pub seconds: f64,
    pub usually: Option<f64>,
    pub quiet: bool,
    pub shown: Shown,
}
impl Running {
    /// Past its stage's usual time: how far.
    pub fn over(&self) -> Option<f64> {
        let usual = self.usually.filter(|u| *u > 0.0)?;
        let ratio = self.seconds / usual;
        (ratio >= 1.0).then_some(ratio)
    }
}
impl HomeProject {
    /// What its board says, as home reads it.
    pub fn new(project: ProjectView, board: Option<&board::ProjectView>, now: u64) -> Self {
        let Some(board) = board else {
            // its plan not read: its running steps as the summary has them
            let running = project
                .running
                .iter()
                .map(|r| Running {
                    step: r.step.clone(),
                    unit: String::new(),
                    href: format!("{}/steps/{}", project.href(), r.step),
                    since: r.started.clone(),
                    seconds: timestamp(&r.started).map_or(0.0, |t| now.saturating_sub(t) as f64),
                    usually: None,
                    quiet: r.quiet,
                    shown: r.shown(),
                })
                .collect();
            return Self {
                steps: project.counts.total(),
                project,
                units: vec![],
                running,
                finished: vec![],
                read: false,
            };
        };
        let mut units = vec![];
        let mut finished = vec![];
        let mut steps = 0;
        // every running step as the summary has it, with what its board knows: its stage's
        // usual time, when its run started, its unit
        let mut known = BTreeMap::new();
        for unit in &board.units {
            for step in unit.steps.iter().filter(|s| s.running()) {
                known.insert(step.id.to_string(), (unit, step));
            }
        }
        let running: Vec<Running> = project
            .running
            .iter()
            .map(|r| {
                let found = known.get(&r.step);
                let since = found
                    .and_then(|(_, s)| s.timing.as_ref().map(|t| t.started.clone()))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| r.started.clone());
                Running {
                    step: r.step.clone(),
                    unit: found
                        .filter(|(u, _)| u.tagged)
                        .map(|(u, _)| u.id.to_string())
                        .unwrap_or_default(),
                    href: format!("{}/steps/{}", project.href(), r.step),
                    seconds: timestamp(&since).map_or(0.0, |t| now.saturating_sub(t) as f64),
                    since,
                    usually: found.and_then(|(_, s)| s.usually),
                    quiet: r.quiet,
                    shown: r.shown(),
                }
            })
            .collect();
        for unit in &board.units {
            steps += unit.steps.len();
            let mut over: Option<f64> = None;
            let mut quiet: Option<f64> = None;
            let mut quiet_since = String::new();
            for run in running
                .iter()
                .filter(|r| unit.steps.iter().any(|s| s.id.as_str() == r.step))
            {
                if let Some(r) = run.over().filter(|r| *r >= OVER) {
                    over = Some(over.map_or(r, |o: f64| o.max(r)));
                }
                if run.quiet {
                    let active = unit
                        .steps
                        .iter()
                        .find(|s| s.id.as_str() == run.step)
                        .map(|s| s.active_at.as_str())
                        .unwrap_or("");
                    quiet = Some(
                        timestamp(active).map_or(run.seconds, |t| now.saturating_sub(t) as f64),
                    );
                    quiet_since = active.to_owned();
                }
            }
            units.push(ui::UnitFact {
                name: unit.id.to_string(),
                recipe: unit.recipe.clone(),
                shown: Some(unit.shown()),
                over,
                quiet,
                quiet_since,
            });
            let shown = unit.shown();
            let at = unit.finished();
            if !at.is_empty() && (unit.done || shown.spec().band == Band::Stopped) {
                let took: f64 = unit
                    .steps
                    .iter()
                    .filter_map(|s| s.timing.as_ref().map(|t| t.seconds))
                    .sum();
                let runs = unit
                    .steps
                    .iter()
                    .filter_map(|s| s.timing.as_ref().map(|t| t.runs))
                    .max()
                    .unwrap_or(1);
                finished.push(ui::Finished {
                    name: unit.id.to_string(),
                    title: if unit.titled() {
                        unit.heading().to_owned()
                    } else {
                        String::new()
                    },
                    href: if unit.tagged {
                        unit.href(&project.id)
                    } else {
                        format!(
                            "{}/steps/{}",
                            project.href(),
                            unit.steps.first().map(|s| s.id.as_str()).unwrap_or("")
                        )
                    },
                    at: at.to_owned(),
                    took,
                    runs,
                    shown: Some(shown),
                    place: String::new(),
                });
            }
        }
        finished.sort_by(|a, b| b.at.cmp(&a.at));
        finished.truncate(5);
        let mut running = running;
        // quiet first, then the longest past its usual time, then the longest running
        running.sort_by(|a, b| {
            b.quiet
                .cmp(&a.quiet)
                .then(b.over().unwrap_or(0.0).total_cmp(&a.over().unwrap_or(0.0)))
                .then(b.seconds.total_cmp(&a.seconds))
        });
        Self {
            project,
            units,
            running,
            finished,
            steps,
            read: true,
        }
    }
    /// What it needs, as home orders the projects: a question to the owner, then something
    /// stopped, running, waiting, done; a project with no steps last.
    pub fn rank(&self) -> u8 {
        let p = &self.project;
        if !p.asks.is_empty() {
            return 0;
        }
        if p.counts.total() == 0 {
            return 6;
        }
        match p.shown().spec().band {
            Band::Stopped => 1,
            Band::Running => 2,
            Band::Waiting if p.paused => 4,
            Band::Waiting => 3,
            Band::Done => 5,
        }
    }
    /// How many units it has (a step of no unit counts as one).
    pub fn unit_count(&self) -> usize {
        self.units.len()
    }
    /// Its units counted by band: running, stopped, waiting, done.
    pub fn bands(&self) -> [usize; 4] {
        let mut n = [0; 4];
        for unit in &self.units {
            match unit.shown.map(|s| s.spec().band) {
                Some(Band::Running) => n[0] += 1,
                Some(Band::Stopped) => n[1] += 1,
                Some(Band::Done) => n[3] += 1,
                _ => n[2] += 1,
            }
        }
        n
    }
    /// Its summary sentence (`ui::summary_sentence`), or its counts when its plan was not read.
    pub fn sentence(&self) -> TrustedHtml {
        let p = &self.project;
        if !self.read {
            let words = ui::states_words(&p.standing());
            return TrustedHtml::owned(if words.is_empty() {
                "No steps yet.".into()
            } else {
                format!("{}.", esc(&words))
            });
        }
        let ask = p
            .asks
            .first()
            .map(|a| a.href(&p.id))
            .unwrap_or_default();
        let last = self
            .finished
            .iter()
            .find(|f| f.shown.is_some_and(|s| s.spec().band == Band::Done))
            .map(|f| f.at.as_str())
            .unwrap_or("");
        ui::summary_sentence(&ui::Summary {
            asks: p.asks.len(),
            ask_href: &ask,
            units: &self.units,
            last_done: last,
            noun: ("unit", "units"),
        })
    }
    /// A square a unit, by band (asking, stopped, running, waiting, done), its counts said.
    pub fn squares(&self) -> TrustedHtml {
        if self.units.is_empty() {
            return TrustedHtml::default();
        }
        let asking: Vec<&str> = self
            .project
            .asks
            .iter()
            .map(|a| a.step.as_str())
            .collect();
        let asks_in = |unit: &str| {
            asking
                .iter()
                .any(|s| *s == unit || s.starts_with(&format!("{unit}-")))
        };
        let mut squares: Vec<(u8, &'static str)> = self
            .units
            .iter()
            .map(|u| {
                if asks_in(&u.name) {
                    return (0, "ask");
                }
                match ui::Cell::of(u.shown) {
                    ui::Cell::Look => (1, "look"),
                    ui::Cell::Run => (2, "run"),
                    ui::Cell::Empty => (3, "empty"),
                    ui::Cell::Done => (4, "done"),
                }
            })
            .collect();
        squares.sort();
        let tally: ui::Tally = self.units.iter().filter_map(|u| u.shown).collect();
        let said = format!(
            "{}: {}",
            ui::count(self.units.len(), "unit", "units"),
            ui::states_words(&tally)
        );
        TrustedHtml::owned(format!(
            "<p class=\"pm-squares\" role=\"img\" aria-label=\"{}\">{}</p>",
            esc(&said),
            squares
                .iter()
                .map(|(_, k)| format!("<i class=\"mk mk-{k}\"></i>"))
                .collect::<String>()
        ))
    }
    /// Its rows: each question to the owner, each stopped step, then its running steps (five at
    /// most, the rest counted).
    pub fn rows(&self) -> TrustedHtml {
        let p = &self.project;
        let mut html = String::new();
        for ask in &p.asks {
            let who = if ask.step.is_empty() {
                "<span class=\"pm-who\">The orchestrator</span>".to_owned()
            } else {
                format!(
                    "<span class=\"pm-who\">{}</span>",
                    p.step_ref(&ask.step).html(40)
                )
            };
            html.push_str(&format!(
                "<li class=\"pm-row pm-ask\"><i class=\"mk mk-ask\" aria-hidden=\"true\"></i><span class=\"pm-line\">{who} <span class=\"pm-what\">asks you: <a href=\"{}\">{}</a></span></span></li>",
                esc(&ask.href(&p.id)),
                esc(&ask.title)
            ));
        }
        for s in p.stopped_rows() {
            let sref = p.step_ref(&s.step);
            let href = format!("{}/steps/{}", p.href(), s.step);
            let dismiss = if s.cancelled {
                format!(
                    "<form class=\"sr-dismiss\" method=\"post\" action=\"{}/steps/{}/actions\"><input type=\"hidden\" name=\"action\" value=\"dismiss\"><input type=\"hidden\" name=\"next\" value=\"/\"><button class=\"text-button\" aria-label=\"Dismiss the cancel of {}\" title=\"It stays on its unit, but no longer marks the project\">Dismiss</button></form>",
                    p.href(),
                    esc(&s.step),
                    esc(&sref.link_name(72))
                )
            } else {
                String::new()
            };
            html.push_str(&format!(
                "<li class=\"pm-row pm-stopped sr-{key}\">{glyph}<span class=\"pm-line\"><span class=\"pm-who\">{name}</span> <span class=\"pm-what\"><span class=\"pm-word\">{word}</span>{why}</span></span>{dismiss}</li>",
                key = s.shown().key(),
                glyph = ui::glyph(s.shown()),
                name = step_link(&sref, &href),
                word = esc(s.shown().word()),
                // "cancelled: pivot …", the word said once
                why = match s.headline.strip_prefix("Cancelled: ").unwrap_or(&s.headline) {
                    "" => String::new(),
                    headline => format!(
                        ": <a class=\"sr-why\" href=\"{}\" title=\"{t}\">{t}</a>",
                        esc(&s.log_href(&p.id)),
                        t = esc(headline)
                    ),
                },
            ));
        }
        if p.stopped_more() > 0 {
            html.push_str(&format!(
                "<li class=\"pm-row pm-more\"><a href=\"{}?show=attention\">and {} more stopped</a></li>",
                p.href(),
                p.stopped_more()
            ));
        }
        const RUNNING: usize = 5;
        for run in self.running.iter().take(RUNNING) {
            let sref = p.step_ref(&run.step);
            let (shown, word) = if run.quiet {
                (Shown::Quiet, "quiet")
            } else {
                (run.shown, run.shown.word())
            };
            let usual = match (run.usually, run.over()) {
                (Some(_), Some(r)) if r >= OVER => format!(" {}", ui::overrun(r)),
                (Some(u), _) => format!(
                    " <span class=\"pm-usual\">usually {}</span>",
                    esc(&ui::duration_text(u))
                ),
                _ => String::new(),
            };
            html.push_str(&format!(
                "<li class=\"pm-row pm-running{q}\">{glyph}<span class=\"pm-line\"><span class=\"pm-who\">{name}</span> <span class=\"pm-what\">{word}running for <span class=\"pm-for\">{since}</span>{usual}</span></span></li>",
                q = if run.quiet { " pm-quiet" } else { "" },
                glyph = ui::glyph(shown),
                name = step_link(&sref, &run.href),
                // a run that is not plainly running says how first: "quiet · running for 9d"
                word = if shown == Shown::Running {
                    String::new()
                } else {
                    format!("<span class=\"pm-word\">{}</span> · ", esc(word))
                },
                since = ui::since(&run.since),
            ));
        }
        if self.running.len() > RUNNING {
            html.push_str(&format!(
                "<li class=\"pm-row pm-more\"><a href=\"{}\">and {} more running</a></li>",
                p.href(),
                self.running.len() - RUNNING
            ));
        }
        if html.is_empty() {
            return TrustedHtml::default();
        }
        TrustedHtml::owned(format!(
            "<ul class=\"pm-rows\" aria-label=\"What needs a look and what runs\">{html}</ul>"
        ))
    }
    /// "Finished last: a-3 at 17:49, took 1h 27m · …", three at most.
    pub fn finished_line(&self) -> TrustedHtml {
        if self.finished.is_empty() {
            return TrustedHtml::default();
        }
        TrustedHtml::owned(format!(
            "<div class=\"pm-done\"><p class=\"pm-done-h\">Finished last</p>{}</div>",
            ui::latest_list(
                &format!("Finished last in {}", self.project.name),
                &self.finished[..self.finished.len().min(3)]
            )
        ))
    }
    /// Its module: its name and size, its words, its sentence and squares, its rows, what
    /// finished last and the way to its plan and its day.
    pub fn module(&self) -> TrustedHtml {
        let p = &self.project;
        let swell = match self.rank() {
            0 => ui::Swell::Ask,
            1 => ui::Swell::Look,
            _ => ui::Swell::Plain,
        };
        let size = if self.read {
            format!(
                "{} · {}",
                ui::count(self.unit_count(), "unit", "units"),
                ui::count(self.steps, "step", "steps")
            )
        } else {
            ui::count(p.counts.total(), "step", "steps")
        };
        let icon = if p.icon_url.is_empty() {
            String::new()
        } else {
            format!("<img class=\"proj-icon\" src=\"{}\" alt=\"\">", esc(&p.icon_url))
        };
        let paused = if p.paused {
            format!(
                " <span class=\"pm-paused\">{}Paused</span>",
                ui::mark(Shown::Paused)
            )
        } else {
            String::new()
        };
        TrustedHtml::owned(format!(
            "{open}<div class=\"pm-head\"><h3 class=\"pm-name{long}\"><a href=\"{href}\">{icon}{text}{name}</a></h3><p class=\"pm-size\">{state}{size}{paused}</p></div>{about}<p class=\"pm-summary\">{sentence}</p>{squares}{rows}{finished}<p class=\"pm-open\"><a href=\"{href}\">Open the {name} plan</a><a href=\"{href}/day\">Its day</a></p>{close}",
            open = ui::module_open(6, swell, &p.name),
            long = match p.name.chars().count() {
                0..=10 => "",
                11..=18 => " long",
                _ => " longer",
            },
            href = esc(&p.href()),
            text = if p.icon_text.is_empty() {
                String::new()
            } else {
                format!("<span class=\"pm-icon\">{} </span>", esc(&p.icon_text))
            },
            name = esc(&p.name),
            // how the whole project reads, its glyph named by its word
            state = ui::glyph(p.shown()),
            about = if p.description.is_empty() {
                String::new()
            } else {
                format!("<p class=\"pm-about\">{}</p>", esc(p.summary()))
            },
            sentence = self.sentence(),
            squares = self.squares(),
            rows = self.rows(),
            finished = self.finished_line(),
            close = ui::module_close(),
        ))
    }
}

/// A step named by its stage and title, linked (its id is on its page and in its Details).
fn step_link(sref: &ui::StepRef, href: &str) -> String {
    sref.link(href, 56, false).0
}
/// The page: every project, the open questions to the owner and the day, from what was read.
#[derive(Clone, Debug)]
pub struct HomeView {
    /// The live projects, what needs the owner first; then the archived.
    pub projects: Vec<HomeProject>,
    pub archived: Vec<ProjectView>,
    /// The questions to the owner as the inbox's For you draws them: open ones someone waits
    /// on, and in its place each one answered in the last ten minutes.
    pub questions: Vec<threads::MessageItem>,
    pub runner_stopped: bool,
    pub day: Option<day::DayView>,
    pub now: u64,
}
impl HomeView {
    /// Home from the summaries alone (no boards, questions or day): the order and the title.
    pub fn new(snapshot: &DashboardSnapshot) -> Self {
        Self::from_parts(snapshot, &BTreeMap::new(), vec![], None, day::now())
    }
    pub fn from_parts(
        snapshot: &DashboardSnapshot,
        boards: &BTreeMap<ProjectId, board::ProjectView>,
        questions: Vec<threads::MessageItem>,
        day: Option<day::DayView>,
        now: u64,
    ) -> Self {
        let mut projects: Vec<HomeProject> = snapshot
            .projects
            .iter()
            .filter(|p| !p.archived)
            .map(|p| HomeProject::new(p.clone(), boards.get(&p.id), now))
            .collect();
        projects.sort_by(|a, b| {
            a.rank()
                .cmp(&b.rank())
                .then(a.project.shown().cmp(&b.project.shown()))
                .then(a.project.name.cmp(&b.project.name))
        });
        Self {
            projects,
            archived: snapshot
                .projects
                .iter()
                .filter(|p| p.archived)
                .cloned()
                .collect(),
            questions,
            runner_stopped: snapshot.runner_stopped,
            day,
            now,
        }
    }
    /// The tab's words: the open questions to the owner, then what needs attention across the
    /// projects, counted ("2 questions · 2 failed · 1 quiet · Projects").
    pub fn title(&self) -> String {
        let mut all = ui::Tally::default();
        for project in &self.projects {
            all += &project.project.standing();
        }
        let asks = self.projects.iter().map(|p| p.project.asks.len()).sum();
        let mut words: Vec<String> = questions_words(asks).into_iter().collect();
        words.extend(
            all.iter()
                .filter(|(s, _)| s.spec().attention)
                .map(|(s, n)| format!("{n} {}", s.word())),
        );
        words.push("Projects".into());
        words.join(" · ")
    }
    fn live(&self) -> impl Iterator<Item = &HomeProject> {
        self.projects.iter().filter(|p| p.project.counts.total() > 0)
    }
    /// The band's sentence across the projects: its questions and where, what runs and where
    /// (quiet and past its usual time said), what stopped and where, and otherwise that all is
    /// done or waiting.
    pub fn summary(&self) -> TrustedHtml {
        let mut parts: Vec<String> = vec![];
        let asking: Vec<&HomeProject> = self
            .projects
            .iter()
            .filter(|p| !p.project.asks.is_empty())
            .collect();
        let asks: usize = asking.iter().map(|p| p.project.asks.len()).sum();
        if asks > 0 {
            let names: Vec<String> = asking.iter().map(|p| p.project.name.clone()).collect();
            parts.push(format!(
                "<a class=\"ask\" href=\"#for-you\">{} for you</a>, in {}.",
                esc(&ui::count(asks, "question", "questions")),
                esc(&day::join(&names))
            ));
        }
        let running: Vec<&HomeProject> = self
            .live()
            .filter(|p| !p.running.is_empty() || !p.project.running.is_empty())
            .collect();
        let n_running: usize = running
            .iter()
            .map(|p| p.project.running.len().max(p.running.len()))
            .sum();
        if n_running > 0 {
            let names: Vec<String> = running.iter().map(|p| p.project.name.clone()).collect();
            let quiet: usize = running
                .iter()
                .map(|p| p.project.running.iter().filter(|r| r.quiet).count())
                .sum();
            let over: usize = running
                .iter()
                .map(|p| {
                    p.running
                        .iter()
                        .filter(|r| r.over().is_some_and(|o| o >= 2.0))
                        .count()
                })
                .sum();
            let mut notes = vec![];
            if quiet > 0 {
                notes.push(format!("{quiet} quiet"));
            }
            if over > 0 {
                notes.push(format!("{over} past twice its usual time"));
            }
            parts.push(format!(
                "{} running {}{}.",
                n_running,
                if names.len() == 1 {
                    format!("in {}", esc(&names[0]))
                } else {
                    format!("across {}", esc(&day::join(&names)))
                },
                if notes.is_empty() {
                    String::new()
                } else {
                    format!(", {}", esc(&day::join(&notes)))
                }
            ));
        }
        let stopped: Vec<String> = self
            .live()
            .filter(|p| !p.project.stopped.is_empty())
            .map(|p| {
                let tally: ui::Tally = p.project.stopped.iter().map(|s| s.shown()).collect();
                let words: Vec<String> = tally
                    .iter()
                    .map(|(s, n)| format!("{n} {}", s.word()))
                    .collect();
                format!("{} in {}", day::join(&words), p.project.name)
            })
            .collect();
        if !stopped.is_empty() {
            parts.push(format!("Stopped: {}.", esc(&day::join(&stopped))));
        }
        if parts.is_empty() {
            parts.push(match self.live().count() {
                0 if self.projects.is_empty() => "No projects yet.".to_owned(),
                0 => "No project has any steps yet.".to_owned(),
                _ => {
                    let waiting = self.live().any(|p| p.project.shown().spec().band == Band::Waiting);
                    if waiting {
                        "Nothing is running; the rest waits.".to_owned()
                    } else {
                        "Every project's work is done.".to_owned()
                    }
                }
            });
        }
        TrustedHtml::owned(parts.join(" "))
    }
    /// The page's head: "All projects", the sentence across them under it, and the runner
    /// stopped when it is. What each project finished last is on its module.
    pub fn head(&self) -> TrustedHtml {
        let alert = if self.runner_stopped {
            format!(
                "<div class=\"page-alert runner-off\" role=\"status\">{}<p><strong>Runner stopped.</strong> No step starts until <code>sluice loop</code> runs; running steps carry on.</p></div>",
                icons::icon(icons::Icon::TriangleAlert, 20, "ro-icon")
            )
        } else {
            String::new()
        };
        TrustedHtml::owned(format!(
            "<div id=\"home-band\" class=\"band-wrap\">{}</div>",
            ui::page_head_with(
                &TrustedHtml::default(),
                &TrustedHtml::owned("All projects".into()),
                &TrustedHtml::default(),
                &TrustedHtml::owned(format!(
                    "<p class=\"page-line\">{}</p>{alert}",
                    self.summary()
                )),
            )
        ))
    }
    /// For you: each question to the owner swollen, answerable here; one answered in the last
    /// ten minutes as the line that says so.
    fn for_you(&self) -> Result<String, askama::Error> {
        let mut html = String::new();
        let open = self.questions.iter().filter(|q| q.answered_at.is_empty()).count();
        html.push_str(
            ui::section_head(
                "for-you",
                "For you",
                &match open {
                    0 => "Nothing is waiting on you".to_owned(),
                    1 => "The one thing waiting on you".to_owned(),
                    n => format!("{n} questions waiting on you, oldest first"),
                },
            )
            .as_str(),
        );
        if self.questions.is_empty() {
            html.push_str("<p class=\"fy-none\">No question is waiting on you. A question an agent puts to you comes here first, and to the Inbox.</p>");
        }
        for q in &self.questions {
            let id = format!("item-{}-{}", q.project, q.id());
            if !q.answered_at.is_empty() {
                html.push_str(&format!(
                    "<article class=\"mod fy-done q-done\" id=\"{id}\">{}</article>",
                    q.answered_html()
                ));
                continue;
            }
            let step = match &q.from_who {
                threads::Who::Step(s) => Some(s.clone()),
                _ => None,
            };
            let place = match &step {
                Some(s) => format!("/projects/id/{}/steps/{}", q.project, s.id),
                None => q.thread_url(),
            };
            html.push_str(&format!(
                "<article class=\"mod swell-ask fy-q item\" id=\"{id}\" aria-label=\"Question for you: {label}\"><p class=\"mod-k\"><i class=\"mk mk-ask\" aria-hidden=\"true\"></i><b>Question for you</b><span class=\"meta\">{project} · from {from} · {when}</span></p><h3 class=\"mod-t\">{title}</h3><div class=\"mod-body md fy-body\">{body}</div>{answer}<p class=\"fy-more\"><a href=\"{place}\">{open}</a></p></article>",
                label = esc(&q.title()),
                project = esc(&q.project_name),
                from = q.from_who.html(&q.project),
                when = ui::ago(&q.message.at),
                title = esc(&q.title()),
                body = q.shown_body(),
                answer = TrustedHtml::from_template(&Ask {
                    item: q,
                    c: Slot {
                        slot: "home-".into(),
                    },
                })?,
                place = esc(&place),
                open = match &step {
                    Some(_) => "Open its step".to_owned(),
                    None => "Open its thread".into(),
                },
            ));
        }
        Ok(html)
    }
    /// Today: each project's day in a line, and the way to the timetable.
    fn today(&self) -> String {
        let Some(day) = &self.day else {
            return String::new();
        };
        let mut rows = String::new();
        for p in &self.projects {
            if p.project.counts.total() == 0 {
                continue;
            }
            rows.push_str(&format!(
                "<li><a class=\"td-name\" href=\"{}/day\">{}</a><p>{}</p></li>",
                esc(&p.project.href()),
                esc(&p.project.name),
                day.project_line(&p.project.id)
            ));
        }
        format!(
            "{}<div class=\"mod swell-margin td{wide}\"><ul class=\"td-list\">{rows}</ul><p class=\"td-all\"><a href=\"/day\">The day as a timetable</a></p></div>",
            ui::section_head("today", "Today", "The last 24 hours"),
            wide = if self.questions.is_empty() { " td-wide" } else { "" },
        )
    }
    /// The index: every project's units by band, then the archived ones.
    fn index(&self) -> String {
        let mut rows = String::new();
        let mut total = [0usize; 4];
        let mut units = 0;
        for p in &self.projects {
            let n = p.bands();
            for (t, v) in total.iter_mut().zip(n) {
                *t += v;
            }
            units += p.unit_count();
            let word = if p.project.counts.total() == 0 {
                " <span class=\"meta\">· no steps yet</span>"
            } else if p.project.paused {
                " <span class=\"meta\">· paused</span>"
            } else {
                ""
            };
            rows.push_str(&format!(
                "<tr><th scope=\"row\"><a href=\"{}\">{}</a>{word}</th><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num ix-w\">{}</td><td class=\"num\">{}</td><td class=\"ix-at\">{}</td></tr>",
                esc(&p.project.href()),
                esc(&p.project.name),
                if p.read { p.unit_count().to_string() } else { "–".into() },
                n[0],
                n[1],
                n[2],
                n[3],
                ui::ago(&p.project.changed)
            ));
        }
        for p in &self.archived {
            rows.push_str(&format!(
                "<tr class=\"ix-archived\"><th scope=\"row\"><a href=\"{}\">{}</a> <span class=\"meta\">· archived</span></th><td class=\"num\" colspan=\"3\">{}</td><td class=\"num ix-w\"></td><td class=\"num\"></td><td class=\"ix-at\">{}</td></tr>",
                esc(&p.href()),
                esc(&p.name),
                esc(&ui::count(p.counts.total(), "step", "steps")),
                ui::ago(&p.changed)
            ));
        }
        format!(
            "{}<div class=\"scroll ix-wrap\" tabindex=\"0\" role=\"region\" aria-labelledby=\"index\"><table class=\"ix\"><thead><tr><th scope=\"col\">Project</th><th scope=\"col\" class=\"num\">Units</th><th scope=\"col\" class=\"num\">Running</th><th scope=\"col\" class=\"num\">Stopped</th><th scope=\"col\" class=\"num ix-w\">Waiting</th><th scope=\"col\" class=\"num\">Done</th><th scope=\"col\" class=\"ix-at\">Changed</th></tr></thead><tbody>{rows}</tbody><tfoot><tr><th scope=\"row\">All</th><td class=\"num\">{units}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num ix-w\">{}</td><td class=\"num\">{}</td><td class=\"ix-at\"></td></tr></tfoot></table></div>",
            ui::section_head("index", "Index", "Every project's units by where they stand"),
            total[0],
            total[1],
            total[2],
            total[3],
        )
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        let mut html = String::from("<div id=\"projects\" class=\"home\">");
        if self.projects.is_empty() && self.archived.is_empty() {
            html.push_str("<p class=\"empty\">No projects yet. An orchestrator creates one with <code>project_create</code>.</p>");
        } else {
            // For you beside Today while something waits on the owner; else a line over it
            let (ask, today) = if self.questions.is_empty() { (12, 12) } else { (8, 4) };
            html.push_str(ui::grid_open("home-top", 12).as_str());
            html.push_str(ui::column_open(ask).as_str());
            html.push_str(&self.for_you()?);
            html.push_str(ui::column_close().as_str());
            html.push_str(ui::column_open(today).as_str());
            html.push_str(&self.today());
            html.push_str(ui::column_close().as_str());
            html.push_str(ui::grid_close().as_str());
            let live: Vec<&HomeProject> = self.live().collect();
            if !live.is_empty() {
                html.push_str(&format!(
                    "<section class=\"home-projects\" aria-labelledby=\"projects-h\">{}<div class=\"proj-grid\">",
                    ui::section_head(
                        "projects-h",
                        "Projects",
                        &format!(
                            "{}, what needs you first",
                            ui::count(live.len(), "project", "projects")
                        )
                    )
                ));
                for p in live {
                    html.push_str(p.module().as_str());
                }
                html.push_str("</div></section>");
            }
            html.push_str(&format!(
                "<section class=\"home-index\" aria-labelledby=\"index\">{}</section>",
                self.index()
            ));
        }
        html.push_str(&format!(
            "<span class=\"page-title\" hidden data-page-title=\"{} · sluice\" data-title-src=\"/title\"></span></div>",
            esc(&self.title())
        ));
        Ok(TrustedHtml::owned(html))
    }
}
/// A question's answer area as a conversation draws it (`answer.html`), its ids under `c.slot`.
#[derive(Template)]
#[template(source = "{% include \"answer.html\" %}", ext = "html")]
struct Ask<'a> {
    item: &'a threads::MessageItem,
    c: Slot,
}
struct Slot {
    slot: String,
}

/// Everything home reads: the questions (with the nav's snapshot), then each live project's
/// board and the day, in one snapshot.
pub async fn load(
    state: &DashboardState,
    registry: Option<&board::Registry>,
    zone: i64,
) -> Result<(DashboardSnapshot, HomeView), PublicError> {
    let inbox = threads::load(&state.reads, None, sluice_model::commands::MessageView::Questions, None).await?;
    let questions: Vec<threads::MessageItem> =
        inbox.for_you_rows().into_iter().cloned().collect();
    let now = day::now();
    // each project's signatures, as its plan page reads them
    let mut providers = vec![];
    for p in inbox.nav.projects.iter().filter(|p| !p.archived && p.counts.total() > 0) {
        let exact = match registry {
            Some(r) => r.0.signatures(p.id).ok(),
            None => None,
        };
        let catalog = state.catalog.catalog(Some(p.id))?;
        providers.push((p.id, exact, catalog));
    }
    let plans = state.plans.clone();
    let functions = state.catalog.catalog(None)?;
    let (shared, boards, data) = state
        .reads
        .snapshot(move |c| {
            let shared = load_snapshot(c, functions)?;
            let mut boards = BTreeMap::new();
            for (id, exact, catalog) in &providers {
                // a plan that cannot be read leaves its project to its counts
                let read = match exact {
                    Some(exact) => board::load_board(
                        c,
                        &shared,
                        *id,
                        &plans,
                        &format!("registry:{}", exact.version),
                        exact,
                    ),
                    None => board::load_board(
                        c,
                        &shared,
                        *id,
                        &plans,
                        &format!("catalog:{}", catalog.version),
                        &board::CatalogSignatures(catalog),
                    ),
                };
                if let Ok((board, _)) = read {
                    boards.insert(*id, board);
                }
            }
            let data = day::load(c, None, now)?;
            Ok((shared, boards, data))
        })
        .await
        .map_err(|e| e.into_public(true))?;
    let day = day::DayView::new(data, None, now, zone);
    let view = HomeView::from_parts(&shared, &boards, questions, Some(day), now);
    Ok((shared, view))
}
/// Home's regions: its body, its band and the nav.
pub async fn home_batch(
    state: &DashboardState,
    registry: Option<&board::Registry>,
    viewer: &Viewer,
    zone: i64,
) -> Result<(RenderedBatch, HomeView, NavView), PublicError> {
    let (shared, view) = load(state, registry, zone).await?;
    let nav = NavView::new(&shared, None, "")?;
    let batch = RenderedBatch::new(vec![
        PatchRegion::new("projects", view.body().map_err(template_error)?),
        PatchRegion::new("home-band", view.head()),
        PatchRegion::new(
            "top-nav",
            render_nav(&nav, viewer, "/").map_err(template_error)?,
        ),
    ]);
    Ok((batch, view, nav))
}
#[derive(Clone, Debug)]
pub struct FunctionGroup {
    pub title: String,
    pub functions: Vec<FunctionView>,
}
#[derive(Template)]
#[template(path = "functions.html")]
struct FunctionsTemplate<'a> {
    nav: &'a NavView,
    groups: &'a [FunctionGroup],
}
fn function_groups(snapshot: &DashboardSnapshot, nav: &NavView) -> Vec<FunctionGroup> {
    [
        ("builtin", "Built-in"),
        ("global", "Global"),
        ("project", "Project"),
    ]
    .into_iter()
    .filter(|(scope, _)| *scope != "project" || nav.selected.is_some())
    .map(|(scope, title)| FunctionGroup {
        title: title.into(),
        // a retired fn after the live ones
        functions: {
            let mut found: Vec<FunctionView> = snapshot
                .functions
                .entries
                .iter()
                .filter(|f| f.scope == scope)
                .cloned()
                .collect();
            found.sort_by_key(FunctionView::retired);
            found
        },
    })
    .collect()
}
fn function_body(
    snapshot: &DashboardSnapshot,
    nav: &NavView,
) -> Result<TrustedHtml, askama::Error> {
    let groups = function_groups(snapshot, nav);
    TrustedHtml::from_template(&FunctionsTemplate {
        nav,
        groups: &groups,
    })
}
/// The functions' head: its name and its counts.
fn function_head(snapshot: &DashboardSnapshot, nav: &NavView) -> TrustedHtml {
    let groups = function_groups(snapshot, nav);
    let all: usize = groups.iter().map(|g| g.functions.len()).sum();
    let retired: usize = groups
        .iter()
        .flat_map(|g| &g.functions)
        .filter(|f| f.retired())
        .count();
    let broken: usize = groups
        .iter()
        .flat_map(|g| &g.functions)
        .filter(|f| !f.error.is_empty())
        .count();
    let counts: Vec<String> = groups
        .iter()
        .map(|g| format!("{} {}", g.functions.len(), g.title.to_lowercase()))
        .collect();
    let mut summary = format!(
        "{}: {}.",
        ui::count(all, "function", "functions"),
        day::join(&counts)
    );
    if retired > 0 {
        summary.push_str(&format!(" {retired} retired, kept for older plans."));
    }
    if broken > 0 {
        summary.push_str(&format!(
            " {} cannot be loaded.",
            ui::count(broken, "one", "of them")
        ));
    }
    TrustedHtml::owned(format!(
        "<div id=\"fns-band\" class=\"band-wrap\">{}</div>",
        ui::page_head_note("Functions", &TrustedHtml::owned(esc(&summary)))
    ))
}
pub fn render_functions(
    snapshot: &DashboardSnapshot,
    project: Option<ProjectId>,
    viewer: &Viewer,
) -> Result<TrustedHtml, PublicError> {
    let nav = NavView::new(snapshot, project, "functions")?;
    let drawn = batch(snapshot, project, true, viewer)?;
    let path = project
        .map(|id| format!("/fns?project={id}"))
        .unwrap_or_else(|| "/fns".into());
    let stream = project
        .map(|id| format!("/fns/stream?project={id}"))
        .unwrap_or_else(|| "/fns/stream".into());
    render_framed(
        "Functions",
        &drawn.regions[0].html,
        &nav,
        viewer,
        &stream,
        &drawn.version,
        &path,
        &Frame {
            head: drawn.regions[1].html.clone(),
            ..Frame::default()
        },
    )
    .map_err(template_error)
}
fn template_error(error: askama::Error) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}
/// The functions page's regions from one snapshot: its body, its band and the nav. Stable
/// target replacement makes replay safe even when the preceding connection applied only part
/// of a batch.
pub fn batch(
    snapshot: &DashboardSnapshot,
    project: Option<ProjectId>,
    _functions: bool,
    viewer: &Viewer,
) -> Result<RenderedBatch, PublicError> {
    let nav = NavView::new(snapshot, project, "functions")?;
    let body = function_body(snapshot, &nav).map_err(template_error)?;
    let path = project
        .map(|id| format!("/fns?project={id}"))
        .unwrap_or_else(|| "/fns".into());
    Ok(RenderedBatch::new(vec![
        PatchRegion::new("functions", body),
        PatchRegion::new("fns-band", function_head(snapshot, &nav)),
        PatchRegion::new(
            "top-nav",
            render_nav(&nav, viewer, &path).map_err(template_error)?,
        ),
    ]))
}

async fn home_handler(
    State(state): State<DashboardState>,
    registry: Option<Extension<board::Registry>>,
    headers: HeaderMap,
) -> Response {
    let page = async {
        let viewer = Viewer::from_headers(&headers);
        let (drawn, view, nav) = home_batch(
            &state,
            registry.as_ref().map(|r| &r.0),
            &viewer,
            day::zone(&headers),
        )
        .await?;
        render_framed(
            &view.title(),
            &drawn.regions[0].html,
            &nav,
            &viewer,
            "/stream",
            &drawn.version,
            "/",
            &Frame {
                head: drawn.regions[1].html.clone(),
                ..Frame::default()
            },
        )
        .map_err(template_error)
    };
    match page.await {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
async fn functions_handler(
    State(state): State<DashboardState>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Response {
    let page = async {
        let snapshot = state.snapshot(query.project).await?;
        home::render_functions(&snapshot, query.project, &Viewer::from_headers(&headers))
    };
    match page.await {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}

#[derive(Deserialize)]
struct TitleQuery {
    project: Option<ProjectId>,
}
/// A page's tab title alone, as its stream would write it: what a hidden tab asks for while its
/// stream is closed (`nav.js`). The index's by default; a project's with `?project=`.
async fn title_handler(
    State(state): State<DashboardState>,
    Query(query): Query<TitleQuery>,
) -> Response {
    let title = async {
        let snapshot = state.snapshot(None).await?;
        let words = match query.project {
            None => home::HomeView::new(&snapshot).title(),
            Some(id) => snapshot
                .projects
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| PublicError::NotFound {
                    message: "project not found".into(),
                })?
                .tab_words(),
        };
        Ok::<_, PublicError>(format!("{words} · sluice"))
    };
    match title.await {
        Ok(title) => (
            [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            title,
        )
            .into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
pub fn registration() -> PageRegistration {
    PageRegistration {
        routes: |state| {
            Router::new()
                .route("/", get(home_handler))
                .route("/title", get(title_handler))
                .route("/fns", get(functions_handler))
                .route("/stream", get(crate::streams::home_stream))
                .route("/fns/stream", get(crate::streams::functions_stream))
                .with_state(state.dashboard.clone())
        },
        nav: |project| {
            vec![NavEntry::new(
                "functions",
                project
                    .map(|id| format!("/fns?project={id}"))
                    .unwrap_or_else(|| "/fns".into()),
                "Functions",
                60,
            )]
        },
        assets: &[
            Asset {
                names: &["style.css", "dashboard.css"],
                media_type: "text/css",
                bytes: include_bytes!("../../assets/style.css"),
            },
            Asset {
                names: &["nav.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/nav.js"),
            },
            Asset {
                names: &["components.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/components.js"),
            },
            Asset {
                names: &["datastar-rocket-1.0.4.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/datastar-rocket-1.0.4.js"),
            },
            Asset {
                names: &["logo.svg"],
                media_type: "image/svg+xml",
                bytes: include_bytes!("../../assets/logo.svg"),
            },
            Asset {
                names: &["favicon.svg"],
                media_type: "image/svg+xml",
                bytes: include_bytes!("../../assets/favicon.svg"),
            },
        ],
    }
}
