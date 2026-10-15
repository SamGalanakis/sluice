//! Home (`/`, DESIGN.md, Pages: Home): every project as a cover and an index. The band holds
//! "sluice" and one sentence across the projects, the runner stopped when it is, and what
//! finished most recently; under it For you (every open question to the owner, swollen and
//! answerable in place) beside the day in a line a project; then each project as a module,
//! the ones that need the owner first: its own summary sentence, a square a unit, its questions,
//! its stopped steps, its running steps with how long each has run and what it finished last;
//! then the index of every project, archived ones too.
use crate::clock::ClockRead;
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

/// One project as home draws it: its summary and, when its plan was read, the plan in brief
/// (`plan::Plan`): its sentence, what needs the owner counted, At work's groups in a line each,
/// what finished last.
#[derive(Clone, Debug)]
pub struct HomeProject {
    pub project: ProjectView,
    /// Each unit's state, as the index counts them.
    pub units: Vec<ui::UnitFact>,
    /// How many steps it has.
    pub steps: usize,
    /// Its board was read: a project whose plan cannot be read says only its counts.
    pub read: bool,
    /// The plan's head sentence.
    pub sentence: String,
    /// How many items of its Needs you stand.
    pub needs: usize,
    /// At work's groups, a line each.
    pub groups: String,
    /// What finished last, as the plan's Finished draws it.
    pub finished: String,
}
impl HomeProject {
    /// What its board says, as home reads it.
    pub fn new(project: ProjectView, board: Option<&board::ProjectView>) -> Self {
        let Some(board) = board else {
            return Self {
                steps: project.counts.total(),
                project,
                units: vec![],
                read: false,
                sentence: String::new(),
                needs: 0,
                groups: String::new(),
                finished: String::new(),
            };
        };
        let plan = super::plan::Plan::new(board);
        Self {
            units: board
                .units
                .iter()
                .map(|u| ui::UnitFact {
                    name: u.id.to_string(),
                    recipe: u.recipe.clone(),
                    shown: Some(u.shown()),
                    quiet: None,
                    quiet_since: String::new(),
                })
                .collect(),
            steps: board.units.iter().map(|u| u.steps.len()).sum(),
            read: true,
            sentence: plan.sentence_html(),
            needs: plan.needs_count(),
            groups: plan.group_lines(),
            finished: plan.finished_recent(FINISHED),
            project,
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
    /// Its plan's sentence, or its counts when its plan was not read.
    pub fn sentence(&self) -> TrustedHtml {
        if self.read {
            return TrustedHtml::owned(self.sentence.clone());
        }
        let words = ui::states_words(&self.project.standing());
        TrustedHtml::owned(if words.is_empty() {
            "No steps yet.".into()
        } else {
            format!("{}.", esc(&words))
        })
    }
    /// Its card: its name and size, its words, its plan's sentence, what needs the owner
    /// counted, At work's groups, what finished last, the way to its plan and its day.
    pub fn module(&self) -> TrustedHtml {
        let p = &self.project;
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
        let needs = match self.needs {
            0 => String::new(),
            n => format!(
                "<p class=\"pm-needs\"><a href=\"{}#needs\">{}</a></p>",
                esc(&p.href()),
                ui::count(n, "item needs you", "items need you")
            ),
        };
        let finished = if self.finished.is_empty() {
            String::new()
        } else {
            format!(
                "<div class=\"pm-done\"><p class=\"pm-done-h\">Finished lately</p>{}</div>",
                self.finished
            )
        };
        // a card, not a well: its sentence says what needs the owner, the coral only on its
        // questions
        TrustedHtml::owned(format!(
            "<article class=\"pm card\" aria-label=\"{name}\"><div class=\"pm-head\"><h3 class=\"pm-name\"><a href=\"{href}\">{icon}{text}{name}</a></h3><p class=\"pm-size\">{state} {size}{paused}</p></div>{about}<p class=\"pm-summary\">{sentence}</p>{needs}{groups}{finished}<p class=\"pm-open\"><a href=\"{href}\">Open the {name} plan</a><a href=\"{href}/day\">Its day</a></p></article>",
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
            groups = self.groups,
        ))
    }
}
/// How many of what finished last a project's card shows.
const FINISHED: usize = 3;
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
        mut questions: Vec<threads::MessageItem>,
        day: Option<day::DayView>,
        now: u64,
    ) -> Self {
        // a question the owner set aside in its plan's Needs you is set aside here too
        questions.retain(|q| {
            boards.get(&q.project).is_none_or(|b| {
                !b.set_aside
                    .iter()
                    .any(|s| s.key == format!("ask:{}", q.id()))
            })
        });
        let mut projects: Vec<HomeProject> = snapshot
            .projects
            .iter()
            .filter(|p| !p.archived)
            .map(|p| HomeProject::new(p.clone(), boards.get(&p.id)))
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
    /// The band's sentence across the projects, from counts alone: the questions for the owner
    /// (linked to For you), each state that needs a look by its word, what runs and in how many
    /// projects: "3 questions for you, 2 failed, 1 quiet; 63 running in 4 projects." Which
    /// project is each card's to say.
    pub fn summary(&self) -> TrustedHtml {
        let asks: usize = self.projects.iter().map(|p| p.project.asks.len()).sum();
        let mut first: Vec<String> = vec![];
        if asks > 0 {
            first.push(format!(
                "<a class=\"ask\" href=\"#for-you\">{}</a>",
                esc(&ui::count(asks, "question for you", "questions for you"))
            ));
        }
        let mut all = ui::Tally::default();
        for project in self.live() {
            all += &project.project.standing();
        }
        first.extend(
            all.iter()
                .filter(|(s, _)| s.spec().attention)
                .map(|(s, n)| format!("{n} {}", esc(s.word()))),
        );
        let running: Vec<&HomeProject> = self
            .live()
            .filter(|p| !p.project.running.is_empty())
            .collect();
        let n_running: usize = running.iter().map(|p| p.project.running.len()).sum();
        let work = match (n_running, running.len()) {
            (0, _) => String::new(),
            (n, 1) => format!("{n} running in 1 project"),
            (n, k) => format!("{n} running in {k} projects"),
        };
        let sentence = match (first.is_empty(), work.is_empty()) {
            (false, false) => format!("{}; {work}.", first.join(", ")),
            (false, true) => format!("{}.", first.join(", ")),
            (true, false) => format!("{work}."),
            (true, true) => match self.live().count() {
                0 if self.projects.is_empty() => "No projects yet.".to_owned(),
                0 => "No project has any steps yet.".to_owned(),
                _ if self.live().any(|p| p.project.shown().spec().band == Band::Waiting) => {
                    "Nothing is running; the rest waits.".to_owned()
                }
                _ => "Every project's work is done.".to_owned(),
            },
        };
        TrustedHtml::owned(sentence)
    }
    /// The page's head: "All projects", the sentence across them under it, and the runner
    /// stopped when it is. What each project finished last is on its module.
    pub fn head(&self) -> TrustedHtml {
        let alert = if self.runner_stopped {
            format!(
                "<div class=\"page-alert runner-off\" role=\"status\">{}<p><strong>Runner stopped.</strong> No step starts until <code>sluice loop</code> runs.</p></div>",
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
    /// For you: each open question to the owner as a line of the Needs-you list (its project
    /// and who asked, the question, Answer the way to it and × to set it aside); one answered
    /// in the last ten minutes as the line that says so.
    fn for_you(&self) -> Result<String, askama::Error> {
        let mut html = String::new();
        let open = self.questions.iter().filter(|q| q.answered_at.is_empty()).count();
        html.push_str(
            ui::section_head(
                "for-you",
                "For you",
                &match open {
                    0 => String::new(),
                    n => ui::count(n, "question", "questions"),
                },
            )
            .as_str(),
        );
        if self.questions.is_empty() {
            html.push_str("<p class=\"fy-none\">Nothing waiting on you.</p>");
            return Ok(html);
        }
        html.push_str("<ul class=\"nd-list fy-list\">");
        for q in &self.questions {
            let id = format!("item-{}-{}", q.project, q.id());
            if !q.answered_at.is_empty() {
                html.push_str(&format!(
                    "<li class=\"nd-i fy-done\" id=\"{id}\">{}</li>",
                    q.answered_html()
                ));
                continue;
            }
            let place = match &q.from_who {
                threads::Who::Step(s) => format!("/projects/id/{}/steps/{}", q.project, s.id),
                _ => q.thread_url(),
            };
            let project = format!("/projects/id/{}", q.project);
            html.push_str(&ui::need_line(
                ui::Mark::Ask,
                &id,
                "",
                &format!(
                    "<span class=\"fy-where\">{} ·</span> {} asks: <b><a href=\"{}\">{}</a></b>",
                    esc(&q.project_name),
                    q.from_who.html(&q.project),
                    esc(&place),
                    esc(&q.title())
                ),
                "",
                &format!("<a class=\"btn primary\" href=\"{}\">Answer</a>", esc(&place)),
                &{
                    let form = format!("aside-{}-{}", q.project, q.id());
                    format!(
                        "{open}<input type=\"hidden\" name=\"action\" value=\"dismiss\"><input type=\"hidden\" name=\"items\" value=\"ask:{m}\"><button class=\"icon\" aria-label=\"Set aside {t}\" title=\"Set aside {t}\">×</button>{said}</form>",
                        open = ui::post_form(&form, "nd-form", &format!("{project}/needs")),
                        said = ui::form_said(&form),
                        m = q.id(),
                        t = esc(&q.title()),
                    )
                },
            ));
        }
        html.push_str("</ul>");
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
            "{}<div class=\"mod swell-margin td{wide}\"><ul class=\"td-list\">{rows}</ul><p class=\"td-all\"><a href=\"/day\">Timetable</a></p></div>",
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
            ui::section_head("index", "Index", ""),
            total[0],
            total[1],
            total[2],
            total[3],
        )
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        let mut html = String::from("<div id=\"projects\" class=\"home\">");
        if self.projects.is_empty() && self.archived.is_empty() {
            html.push_str("<p class=\"empty\">No projects yet.</p>");
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
                        &ui::count(live.len(), "project", "projects")
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
        .clock_snapshot(move |c| {
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
        summary.push_str(&format!(" {retired} retired."));
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
/// stream is closed (`nav.js`). The index's by default; a project's plan's glance with
/// `?project=`, its favicon's address in `x-sluice-favicon`.
async fn title_handler(
    State(state): State<DashboardState>,
    Query(query): Query<TitleQuery>,
) -> Response {
    let title = async {
        let snapshot = state.snapshot(None).await?;
        let (words, favicon) = match query.project {
            None => (home::HomeView::new(&snapshot).title(), "favicon.svg"),
            Some(id) => {
                let project = snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or_else(|| PublicError::NotFound {
                        message: "project not found".into(),
                    })?;
                (project.tab_words(), project.favicon())
            }
        };
        Ok::<_, PublicError>((format!("{words} · sluice"), super::asset_url(favicon)))
    };
    match title.await {
        Ok((title, favicon)) => (
            [
                (
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8".to_owned(),
                ),
                (
                    axum::http::header::HeaderName::from_static("x-sluice-favicon"),
                    favicon,
                ),
            ],
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
                bytes: include_bytes!(concat!(env!("OUT_DIR"), "/style.css")),
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
            Asset {
                names: &["favicon-ask.svg"],
                media_type: "image/svg+xml",
                bytes: include_bytes!("../../assets/favicon-ask.svg"),
            },
            Asset {
                names: &["favicon-look.svg"],
                media_type: "image/svg+xml",
                bytes: include_bytes!("../../assets/favicon-look.svg"),
            },
        ],
    }
}
