//! Shared dashboard facade. Load once per page or SSE batch, then render owned
//! models. URLs use immutable project IDs; labels may change independently.
use crate::clock::ClockRead;
mod registration;
pub use registration::{Asset, NavEntry, PageRegistration, PageState, page_router};
macro_rules! register_pages {
    ($($module:ident),* $(,)?) => {
        $(pub mod $module;)*
        const PAGES: &[fn() -> PageRegistration] = &[$($module::registration),*];
    };
}
register_pages! { home, board, inbox, log, project_settings, panel, gallery, day, stats, history }
pub mod activity;
pub mod failure;
pub mod icons;
pub mod missing;
pub mod plan;
pub mod prefs;
pub mod pulse;
pub mod step;
pub mod threads;
pub mod ui;
pub mod unit_page;
pub mod unit_steps;
pub mod unit_view;
pub mod write;

use askama::Template;
use axum::{
    Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, StepId},
};
use sluice_store::ReadPool;
use std::{fmt, future::Future, sync::Arc};

/// HTML emitted by an owned Askama template or the markdown renderer. There is
/// deliberately no public constructor accepting an arbitrary string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedHtml(String);
impl TrustedHtml {
    pub(crate) fn owned(html: String) -> Self {
        Self(html)
    }
    pub fn from_template(template: &impl Template) -> Result<Self, askama::Error> {
        template.render().map(Self)
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for TrustedHtml {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The one stylesheet every page loads, as served (`/static/style.css`): `assets/style.css`,
/// its font URLs fingerprinted by `build.rs`.
pub const STYLESHEET: &str = include_str!(concat!(env!("OUT_DIR"), "/style.css"));

/// The faces the stylesheet's `@font-face` rules use, vendored under `assets/fonts/` and served
/// at `/static/fonts/<file>` (`build.rs` lists them from `assets/fonts/SHA256SUMS`).
pub const FONTS: &[Asset] = include!(concat!(env!("OUT_DIR"), "/fonts.rs"));

/// Every asset the dashboard serves: each page's, then the faces.
pub fn served_assets() -> impl Iterator<Item = &'static Asset> {
    PAGES.iter().flat_map(|page| (page)().assets).chain(FONTS)
}

/// A theme (DESIGN.md, Themes): cenote's Paper (light) or Carbon (dark). Its id is
/// `data-theme` on the page and the `sluice_theme` cookie; it sets `color-scheme`, which every
/// colour token (each a `light-dark()` pair) follows.
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
}
impl Theme {
    pub fn dark(&self) -> bool {
        self.id == "carbon"
    }
}
/// The two themes the display preferences offer. With none chosen (the first open) a page is
/// Carbon unless the system asks for light, and its script keeps that choice from then on.
pub const THEMES: &[Theme] = &[
    Theme {
        id: "paper",
        name: "Paper",
    },
    Theme {
        id: "carbon",
        name: "Carbon",
    },
];
fn theme(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|t| t.id == id)
}
/// The display preferences a page is drawn with, from its cookies.
#[derive(Clone, Debug, Default)]
pub struct Viewer {
    /// The chosen theme, by id; `None` before the first choice (the page follows the system's
    /// scheme in Sluice's palette until its script keeps one).
    pub theme: Option<&'static str>,
    pub types: bool,
}
impl Viewer {
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let mut viewer = Self::default();
        for cookie in headers
            .get(header::COOKIE)
            .and_then(|c| c.to_str().ok())
            .unwrap_or("")
            .split(';')
        {
            if let Some((name, value)) = cookie.trim().split_once('=') {
                match name {
                    "sluice_theme" => viewer.theme = theme(value).map(|t| t.id),
                    "sluice_types" => viewer.types = value == "1",
                    _ => {}
                }
            }
        }
        viewer
    }
    /// The chosen theme's id, "" before the first choice.
    pub fn theme_id(&self) -> &str {
        self.theme.unwrap_or("")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunningView {
    pub step: String,
    pub started: String,
    /// Nothing written for longer than `quiet_after`: maybe stuck.
    pub quiet: bool,
    /// How long it may write nothing before it reads as quiet, in seconds (`quiet_after`).
    pub quiet_after: u64,
    pub run_id: String,
    pub activity: Option<u64>,
    /// When the step last said anything itself (its progress, or a message in its thread),
    /// RFC 3339; "" when it has not. Activity counts it with its run's files.
    #[serde(default)]
    pub said: String,
    /// A cancel was asked for and its run has not ended.
    #[serde(default)]
    pub stopping: bool,
    /// Its run has submitted: it is only finishing.
    #[serde(default)]
    pub finishing: bool,
    #[serde(default, serialize_with = "serialize_park")]
    pub parked: Option<sluice_model::park::Parked>,
}
impl RunningView {
    /// How the running step reads (`shown`).
    pub fn shown(&self) -> ui::Shown {
        sluice_model::shown::classify(&sluice_model::shown::Facts {
            quiet: self.quiet,
            stopping: self.stopping,
            finishing: self.finishing,
            parked: self.parked.is_some(),
            ..sluice_model::shown::Facts::of(sluice_model::commands::StepStatus::Running)
        })
    }
}

/// Page versions contain the park's start; the browser ticks its age from that instant.
fn serialize_park<S: serde::Serializer>(
    park: &Option<sluice_model::park::Parked>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    park.as_ref()
        .map(|p| {
            let mut park = p.clone();
            park.waited_seconds = 0.0;
            park
        })
        .serialize(serializer)
}

/// A step that stopped: failed, or cancelled by the owner.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoppedView {
    pub step: String,
    pub cancelled: bool,
}
impl StoppedView {
    pub fn shown(&self) -> ui::Shown {
        if self.cancelled {
            ui::Shown::Cancelled
        } else {
            ui::Shown::Failed
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectView {
    pub id: ProjectId,
    pub name: String,
    pub description: String,
    pub icon_text: String,
    pub icon_url: String,
    pub paused: bool,
    pub archived: bool,
    pub changed: String,
    /// Its steps by how each reads (`shown`), from what the store says of them: on the board,
    /// which also evaluates the plan, the board's own count (`board::load_board`).
    pub counts: ui::Tally,
    pub running: Vec<RunningView>,
    /// Its failed steps, then the ones the owner cancelled, in plan order.
    pub stopped: Vec<StoppedView>,
    /// Its cancelled steps the owner dismissed (`StepDismiss`): each stays on its unit, but no
    /// longer marks the project, its tab or its index row.
    #[serde(default)]
    pub dismissed: std::collections::BTreeSet<String>,
    /// Its open questions to the owner that someone still waits on, oldest first: what the
    /// nav's Inbox counts, each with the step that asked it.
    #[serde(default)]
    pub asks: Vec<AskView>,
    /// Its steps that need a look (failed, cancelled, stale or quiet) and that the owner has
    /// neither dismissed (a cancel) nor set aside in its plan's Needs you: what its tab counts.
    #[serde(default)]
    pub looks: usize,
}
/// An open question to the owner that someone waits on: its message, the step that asked it
/// ("" for a session or anyone else), and its title (its body's first line without one).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskView {
    pub message: i64,
    pub step: String,
    /// Who asked: its step, a session (`@lead`), `sluice`, or `orchestrator` before
    /// sessions named themselves.
    pub from: String,
    pub title: String,
}
impl AskView {
    /// Who asked when no step did: its sender, read as the old shared address when it has
    /// none (a question from before senders were kept).
    pub fn sender_id(&self) -> &str {
        if self.from.is_empty() {
            sluice_model::identity::LEGACY_ORCHESTRATOR
        } else {
            &self.from
        }
    }
    /// Who asked when no step did, as a sentence starts with it (`ui::Party`): the session by
    /// its name, "Sluice", or "orchestrator", muted, for a question from before sessions named
    /// themselves.
    pub fn sender_html(&self) -> String {
        ui::party_html(self.sender_id(), true)
    }
    /// Where it is answered: its step's page, whose Overview draws it whole with Answer; a
    /// question no step asked, its card in the project's inbox.
    pub fn href(&self, project: &ProjectId) -> String {
        if self.step.is_empty() {
            format!(
                "/projects/id/{project}/inbox#item-{project}-{}",
                self.message
            )
        } else {
            format!(
                "/projects/id/{project}/steps/{}#ov-message-{}",
                self.step, self.message
            )
        }
    }
}
/// "2 questions": the open questions to the owner, counted, as a tab title leads with them.
pub fn questions_words(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 question".into()),
        n => Some(format!("{n} questions")),
    }
}
impl ProjectView {
    /// Its open question to the owner from `step`, when it has one.
    pub fn ask_of(&self, step: &str) -> Option<&AskView> {
        self.asks.iter().find(|a| a.step == step)
    }
    pub fn href(&self) -> String {
        format!("/projects/id/{}", self.id)
    }
    /// Its counts as they mark the project: without the cancels the owner dismissed.
    pub fn standing(&self) -> ui::Tally {
        self.counts
            .clone()
            .without(ui::Shown::Cancelled, self.dismissed.len())
    }
    /// How the whole project reads: its first state (`Tally::first`) less what the owner
    /// dismissed, a value set by hand read as done (a project is succeeded, not "set by hand"),
    /// pending with no steps.
    pub fn shown(&self) -> ui::Shown {
        match self.standing().first() {
            Some(ui::Shown::Manual) => ui::Shown::Succeeded,
            Some(shown) => shown,
            // nothing left but dismissed cancels: done
            None if self.counts.total() > 0 => ui::Shown::Succeeded,
            None => ui::Shown::Pending,
        }
    }
    /// Its plan's tab words, its glance: its open questions to the owner counted in brackets,
    /// then a warning sign and how many of its steps need a look that the owner has not set
    /// aside (`looks`), then its name: "(1) ⚠4 · almanac", "⚠2 · almanac", "(1) almanac"; its name
    /// alone when nothing needs the owner.
    pub fn tab_words(&self) -> String {
        let mut words = String::new();
        if !self.asks.is_empty() {
            words.push_str(&format!("({}) ", self.asks.len()));
        }
        if self.looks > 0 {
            words.push_str(&format!("⚠{} · ", self.looks));
        }
        words.push_str(&self.name);
        words
    }
    /// The favicon its plan's tab wears with `tab_words`: the mark with a teal dot while a
    /// question waits on the owner, an ochre square while a step needs a look, else the mark.
    pub fn favicon(&self) -> &'static str {
        if !self.asks.is_empty() {
            "favicon-ask.svg"
        } else if self.looks > 0 {
            "favicon-look.svg"
        } else {
            "favicon.svg"
        }
    }
    /// Something waits on the owner: a question, or a step that needs a look and was not set
    /// aside. The sidebar's teal dot.
    pub fn needs_owner(&self) -> bool {
        !self.asks.is_empty() || self.looks > 0
    }
    pub fn summary(&self) -> &str {
        self.description.split("\n\n").next().unwrap_or("")
    }
    pub fn now(&self) -> &str {
        use sluice_model::shown::Band;
        let any = |band: Band| self.counts.iter().any(|(s, _)| s.spec().band == band);
        if self.paused {
            "Paused."
        } else if self.counts.total() == 0 {
            "No steps yet."
        } else if self.shown().spec().band == Band::Done {
            "Finished."
        } else if !self.stopped.is_empty() || any(Band::Running) {
            // its stopped and running rows say it
            ""
        } else if self
            .counts
            .iter()
            .all(|(s, _)| s == ui::Shown::Paused || s.spec().band == Band::Done)
        {
            // all that is left is held
            "Paused."
        } else {
            "Nothing is running."
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortView {
    pub name: String,
    pub ty: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionView {
    pub name: String,
    pub scope: String,
    pub doc: String,
    pub inputs: Vec<PortView>,
    pub outputs: Vec<PortView>,
    pub error: String,
}
impl FunctionView {
    /// Kept only so plans written before its successors still run: its doc says so first.
    pub fn retired(&self) -> bool {
        self.doc.starts_with("Retired")
    }
}
/// Registry owners return the visible entries, including errors and shadowed
/// collisions. Its token must change for every externally visible registry edit.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionCatalog {
    pub version: String,
    pub entries: Vec<FunctionView>,
}
pub trait CatalogSource: Send + Sync {
    fn catalog(&self, project: Option<ProjectId>) -> Result<FunctionCatalog, PublicError>;
    /// The catalog's version alone: what a stream's change token reads each second.
    fn version(&self, project: Option<ProjectId>) -> Result<String, PublicError> {
        Ok(self.catalog(project)?.version)
    }
}
#[derive(Default)]
pub struct EmptyCatalog;
impl CatalogSource for EmptyCatalog {
    fn catalog(&self, _: Option<ProjectId>) -> Result<FunctionCatalog, PublicError> {
        Ok(FunctionCatalog::default())
    }
}
/// One database snapshot, with each running run's activity observed once from its run files
/// after the read (best effort: those files change constantly and are never atomic with SQLite).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DashboardSnapshot {
    pub projects: Vec<ProjectView>,
    pub inbox: usize,
    /// Notes to the owner not yet read (the inbox's "Unread notes").
    pub notes: usize,
    /// Nothing holds the scheduler lease (no `sluice loop`, no `serve` without --no-runner):
    /// nothing new starts.
    pub runner_stopped: bool,
    pub functions: FunctionCatalog,
}
impl DashboardSnapshot {
    pub fn version(&self) -> String {
        sluice_store::artifacts::fingerprint(
            &serde_json::to_vec(self).expect("view models serialize"),
        )
    }
}
#[derive(Clone)]
pub struct DashboardState {
    pub reads: ReadPool,
    /// Rendering clock; set directly by tests, never loaded from home or environment.
    pub clock: crate::clock::Clock,
    pub catalog: Arc<dyn CatalogSource>,
    pub stop: Arc<std::sync::atomic::AtomicBool>,
    /// Each project's plan compiled from its rows, kept by revision and signatures: every page
    /// that draws a plan (the board, a step, home's modules, the board program, settings and
    /// the Mermaid) reads it here.
    pub plans: board::PlanCache,
    pub(crate) home_cards: home::CardCache,
    /// Claude's config home, where its session transcripts are (an agent run's activity):
    /// `CLAUDE_CONFIG_DIR`, else `~/.claude`, as Claude finds it.
    pub claude_home: Option<std::path::PathBuf>,
    /// Seconds a long run may publish nothing before the plan asks the owner to look
    /// (config.json's `long_run_quiet_h`, read when the dashboard starts).
    pub long_run_quiet: f64,
}
impl DashboardState {
    pub fn new(reads: ReadPool, catalog: Arc<dyn CatalogSource>) -> Self {
        Self {
            reads,
            clock: crate::clock::Clock::default(),
            catalog,
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            plans: board::PlanCache::default(),
            home_cards: home::CardCache::default(),
            claude_home: sluice_agents::activity::Homes::claude_from_env(),
            long_run_quiet: sluice_runtime::config::DEFAULT_LONG_RUN_QUIET_H * 3600.0,
        }
    }
    /// How long (hours) a long run may publish nothing before it needs a look.
    pub fn with_long_run_quiet(mut self, hours: f64) -> Self {
        self.long_run_quiet = hours * 3600.0;
        self
    }
    pub fn with_clock(mut self, clock: crate::clock::Clock) -> Self {
        self.clock = clock;
        self
    }
    /// The catalog, one store snapshot and the runs' activity, each read once. A page renders
    /// what this returns; nothing that moves while it renders can fail it.
    pub async fn snapshot(
        &self,
        project: Option<ProjectId>,
    ) -> Result<DashboardSnapshot, PublicError> {
        let functions = self.catalog.catalog(project)?;
        self.clock
            .scope(
                self.reads
                    .clock_snapshot(move |c| load_snapshot(c, functions)),
            )
            .await
            .map_err(|e| e.into_public(true))
    }
    /// A cheap token of everything a page reads: every store commit bumps a change version
    /// (`change_versions`, which the writer keeps for each scope and view it touches), and the
    /// function catalog has its version. What it leaves out moves only with the clock or the
    /// run files (a run going quiet), which a stream redraws within `streams::FRESH`. With
    /// `live`, a step whose run is going moves it every second: its page reads the run's
    /// transcript (its activity), which grows with no commit.
    pub async fn token(
        &self,
        project: Option<ProjectId>,
        live: Option<StepId>,
    ) -> Result<String, PublicError> {
        let catalog = self.catalog.version(project)?;
        let (sum, count, running): (i64, i64, bool) = self.clock.scope(self.reads
            .clock_snapshot(move |c| {
                let (sum, count) = c.query_row(
                    "SELECT coalesce(sum(version),0),count(*) FROM change_versions WHERE ?1 IS NULL OR project_id IS NULL OR project_id=?1 OR view IN ('projects','status','messages','questions','readers')",
                    [project.map(|p|p.to_string())],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let running = match (project, live) {
                    (Some(project), Some(step)) => c
                        .query_row(
                            "SELECT 1 FROM runs WHERE project_id=?1 AND step_id=?2 AND finished_at IS NULL LIMIT 1",
                            rusqlite::params![project.to_string(), step.as_str()],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some(),
                    _ => false,
                };
                Ok((sum, count, running))
            }))
            .await
            .map_err(|e| e.into_public(true))?;
        let tick = if running {
            self.clock
                .now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        } else {
            0
        };
        Ok(format!("{sum}:{count}:{catalog}:{tick}"))
    }
    /// `token` for `project`'s pages, as a stream's watch (`streams::page_events`).
    pub fn watch(
        &self,
        project: Option<ProjectId>,
    ) -> impl Fn() -> std::pin::Pin<Box<dyn Future<Output = Result<String, PublicError>> + Send>>
    + Send
    + 'static {
        self.watch_step(project, None)
    }
    /// `watch` for a step's page or drawer, which a run of the step going keeps moving.
    pub fn watch_step(
        &self,
        project: Option<ProjectId>,
        live: Option<StepId>,
    ) -> impl Fn() -> std::pin::Pin<Box<dyn Future<Output = Result<String, PublicError>> + Send>>
    + Send
    + 'static {
        let state = self.clone();
        move || {
            let state = state.clone();
            let live = live.clone();
            Box::pin(async move { state.token(project, live).await })
        }
    }
}
#[derive(Clone, Debug)]
pub struct NavLink {
    pub href: String,
    pub label: String,
    pub current: bool,
}
#[derive(Clone, Debug)]
pub struct NavView {
    pub projects: Vec<ProjectView>,
    pub label: String,
    pub links: Vec<NavLink>,
    pub inbox: usize,
    pub notes: usize,
    pub selected: Option<ProjectId>,
}
impl NavView {
    pub fn new(
        snapshot: &DashboardSnapshot,
        project: Option<ProjectId>,
        tab: &str,
    ) -> Result<Self, PublicError> {
        let chosen = project
            .map(|id| {
                snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or_else(|| PublicError::NotFound {
                        message: format!("project {id} not found"),
                    })
            })
            .transpose()?;
        let mut sections = PAGES
            .iter()
            .flat_map(|page| ((page)().nav)(project))
            .collect::<Vec<_>>();
        sections.sort_by_key(|entry| entry.order);
        // the sidebar lists the projects by name, so a row never moves under the pointer
        let mut projects = snapshot.projects.clone();
        projects.sort_by_key(|p| p.name.to_lowercase());
        let mut links: Vec<NavLink> = sections
            .into_iter()
            .map(|entry| NavLink {
                href: entry.href,
                label: entry.label.into(),
                current: entry.key == tab,
            })
            .collect();
        // a project's settings are its last section
        if let Some(p) = chosen {
            links.push(NavLink {
                href: format!("{}/settings", p.href()),
                label: "Settings".into(),
                current: tab == "settings",
            });
        }
        Ok(Self {
            projects,
            label: chosen
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "All projects".into()),
            links,
            inbox: snapshot.inbox,
            notes: snapshot.notes,
            selected: project,
        })
    }
}
impl NavView {
    /// The keys that go somewhere (`sluice-keys`), each "g" then its letter: the index, every
    /// section the nav links (by its word's first letter: p plan, m messages, l log, f
    /// functions) and the inbox.
    pub fn keys(&self) -> Vec<(char, String, String)> {
        let mut keys: Vec<(char, String, String)> = vec![('h', "All projects".into(), "/".into())];
        for link in &self.links {
            if let Some(k) = link.label.chars().next().map(|c| c.to_ascii_lowercase())
                && !keys.iter().any(|(have, _, _)| *have == k)
            {
                keys.push((k, link.label.clone(), link.href.clone()));
            }
        }
        if !keys.iter().any(|(k, _, _)| *k == 'i') {
            keys.push(('i', "Inbox".into(), "/inbox".into()));
        }
        keys
    }
}
/// What a page puts in the frame around it (DESIGN.md, The frame): its head on the sheet under
/// the top bar (`ui::page_head`: its name, or on the plan and home only the summary sentence),
/// and what its sidebar's find does (the plan's live find; elsewhere in a project a find that
/// goes to its plan). A page with none of it draws the frame alone.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    /// Under the top bar on the sheet: the page's lead line, or its head where its name says
    /// what the bar does not (a unit, a step, a thread).
    pub head: TrustedHtml,
    /// In the sidebar under its head: the page's find ("" for the project's default, a find on
    /// its plan; none on a page of no project).
    pub find: TrustedHtml,
    /// The page's own signals beside the frame's (`ver`, `stale`): what it sends its stream
    /// when it opens it (the plan's find and read cursor).
    pub signals: serde_json::Map<String, serde_json::Value>,
}
#[derive(Template)]
#[template(path = "layout.html")]
struct Layout<'a> {
    title: &'a str,
    frame: &'a Frame,
    body: &'a TrustedHtml,
    nav: &'a NavView,
    viewer: &'a Viewer,
    stream: &'a str,
    signals: String,
    path: &'a str,
    style_url: String,
    nav_url: String,
    components_url: String,
    datastar_url: String,
    import_map: &'static str,
    /// The build that drew the page (`streams::release`): a stream from another says so.
    release: &'static str,
}
/// The sidebar (`templates/sidebar.html`), around its project list (`side_nav`).
#[derive(Template)]
#[template(path = "sidebar.html")]
pub struct Sidebar<'a> {
    pub frame: &'a Frame,
    pub nav: &'a NavView,
    pub viewer: &'a Viewer,
    pub path: &'a str,
}
/// The top bar (`templates/bar.html`), around its sections and Inbox (`top_nav`).
#[derive(Template)]
#[template(path = "bar.html")]
pub struct Bar<'a> {
    pub nav: &'a NavView,
    /// The page's stream ("" for none: no paused line).
    pub stream: &'a str,
    pub path: &'a str,
}
#[derive(Template)]
#[template(path = "side_head.html")]
struct SideHead<'a> {
    nav: &'a NavView,
}
#[derive(Template)]
#[template(path = "side_nav.html")]
struct SideNav<'a> {
    nav: &'a NavView,
    path: &'a str,
}
#[derive(Template)]
#[template(path = "top_nav.html")]
struct TopNav<'a> {
    nav: &'a NavView,
    path: &'a str,
}
/// The sidebar's head (`#side-head`): the current project, a stream region of its own.
pub fn side_head(nav: &NavView) -> TrustedHtml {
    TrustedHtml::from_template(&SideHead { nav }).expect("the side head renders")
}
/// The sidebar's projects (`#side-nav`), a stream region of its own.
pub fn side_nav(nav: &NavView, path: &str) -> TrustedHtml {
    TrustedHtml::from_template(&SideNav { nav, path }).expect("the side nav renders")
}
/// The top bar's crumb, sections and Inbox (`#top-nav`), a stream region of its own.
pub fn top_nav(nav: &NavView, path: &str) -> TrustedHtml {
    TrustedHtml::from_template(&TopNav { nav, path }).expect("the top nav renders")
}
pub fn render_layout(
    title: &str,
    body: &TrustedHtml,
    nav: &NavView,
    viewer: &Viewer,
    stream: &str,
    version: &str,
    path: &str,
) -> Result<TrustedHtml, askama::Error> {
    render_framed(
        title,
        body,
        nav,
        viewer,
        stream,
        version,
        path,
        &Frame::default(),
    )
}
/// A page in the frame with its own head in the band and its own row under it.
#[allow(clippy::too_many_arguments)]
pub fn render_framed(
    title: &str,
    body: &TrustedHtml,
    nav: &NavView,
    viewer: &Viewer,
    stream: &str,
    version: &str,
    path: &str,
    frame: &Frame,
) -> Result<TrustedHtml, askama::Error> {
    // a project's page finds on its plan unless it has a find of its own (the plan's live one)
    let mut framed;
    let frame = match nav.selected {
        Some(project) if frame.find.as_str().is_empty() => {
            framed = frame.clone();
            framed.find = ui::find_field(
                &format!("/projects/id/{project}"),
                "",
                "Find steps",
                "Find units and steps on this project's plan",
                "",
            );
            &framed
        }
        _ => frame,
    };
    TrustedHtml::from_template(&Layout {
        title,
        frame,
        body,
        nav,
        viewer,
        stream,
        signals: {
            let mut signals = frame.signals.clone();
            signals.insert("ver".into(), version.into());
            signals.insert("stale".into(), false.into());
            // the display preferences: what <html> wears, kept by posting them to /settings
            signals.insert("theme".into(), viewer.theme_id().into());
            signals.insert("types".into(), viewer.types.into());
            serde_json::Value::Object(signals).to_string()
        },
        path,
        style_url: asset_url("style.css"),
        nav_url: asset_url("nav.js"),
        components_url: asset_url("components.js"),
        datastar_url: asset_url("datastar-rocket-1.0.4.js"),
        import_map: import_map(),
        release: crate::streams::release(),
    })
}
/// Every script module's plain URL mapped to this build's fingerprinted one, so a module one
/// imports by name (`/static/openui.js`, and what it imports in turn) is cached for good too.
fn import_map() -> &'static str {
    static MAP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let mut imports = serde_json::Map::new();
        for asset in served_assets() {
            if asset.media_type == "text/javascript" {
                for name in asset.names {
                    imports.insert(format!("/static/{name}"), asset_url(name).into());
                }
            }
        }
        // a script element's text: "</" never closes it early
        serde_json::json!({ "imports": imports })
            .to_string()
            .replace("</", "<\\/")
    })
}
/// The frame's parts a page's stream patches: the sidebar's projects and the top bar's
/// sections and Inbox, each a region of its own.
pub fn nav_regions(nav: &NavView, path: &str) -> Vec<crate::streams::PatchRegion> {
    vec![
        crate::streams::PatchRegion::new("side-head", side_head(nav)),
        crate::streams::PatchRegion::new("side-nav", side_nav(nav, path)),
        crate::streams::PatchRegion::new("top-nav", top_nav(nav, path)),
    ]
}
/// `nav_regions` as one text, for a stream's version.
pub fn nav_text(nav: &NavView, path: &str) -> String {
    format!(
        "{}{}{}",
        side_head(nav),
        side_nav(nav, path),
        top_nav(nav, path)
    )
}
/// Every embedded asset's fingerprint, folded into one: what a release's pages look and act
/// like (`streams::release`).
pub fn assets_fingerprint() -> String {
    let mut all = String::new();
    for registered in served_assets() {
        for name in registered.names {
            all.push_str(name);
            all.push_str(
                asset(name)
                    .map(|a| a.fingerprint.as_str())
                    .unwrap_or_default(),
            );
        }
    }
    let mut fingerprint = sluice_store::artifacts::fingerprint(all.as_bytes());
    fingerprint.truncate(12);
    fingerprint
}
/// An asset's URL, fingerprinted by its bytes: the page asks for exactly this build's asset,
/// which can then be cached for good.
pub fn asset_url(name: &str) -> String {
    format!(
        "/static/{name}?v={}",
        asset(name)
            .map(|a| a.fingerprint.as_str())
            .unwrap_or_default()
    )
}
struct EmbeddedAsset {
    media_type: &'static str,
    bytes: &'static [u8],
    fingerprint: String,
}
/// Each embedded asset by name, hashed once per process.
fn asset(name: &str) -> Option<&'static EmbeddedAsset> {
    static ASSETS: std::sync::OnceLock<std::collections::BTreeMap<&'static str, EmbeddedAsset>> =
        std::sync::OnceLock::new();
    ASSETS
        .get_or_init(|| {
            let mut assets = std::collections::BTreeMap::new();
            for asset in served_assets() {
                let mut fingerprint = sluice_store::artifacts::fingerprint(asset.bytes);
                fingerprint.truncate(16);
                for name in asset.names {
                    assets.entry(*name).or_insert(EmbeddedAsset {
                        media_type: asset.media_type,
                        bytes: asset.bytes,
                        fingerprint: fingerprint.clone(),
                    });
                }
            }
            assets
        })
        .get(name)
}
#[derive(Deserialize)]
struct AssetQuery {
    v: Option<String>,
}
/// A fingerprinted URL (`?v=` this build's fingerprint) never changes content, so the browser
/// keeps it without asking again; any other URL is revalidated on every use.
/// Any other URL carries the fingerprint as its ETag, so a revalidation that still matches is a
/// bodiless 304.
async fn static_asset(
    Path(name): Path<String>,
    Query(query): Query<AssetQuery>,
    headers: HeaderMap,
) -> Response {
    serve_asset(&name, &query, &headers)
}
/// A face (`/static/fonts/<file>`), served as any other asset.
async fn static_font(
    Path(name): Path<String>,
    Query(query): Query<AssetQuery>,
    headers: HeaderMap,
) -> Response {
    serve_asset(&format!("fonts/{name}"), &query, &headers)
}
fn serve_asset(name: &str, query: &AssetQuery, headers: &HeaderMap) -> Response {
    match asset(name) {
        Some(asset) => {
            let etag = format!("\"{}\"", asset.fingerprint);
            let cache = if query.v.as_ref() == Some(&asset.fingerprint) {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=0, must-revalidate"
            };
            let fresh = headers
                .get(header::IF_NONE_MATCH)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
            let head = [
                (header::CONTENT_TYPE, asset.media_type.to_owned()),
                (header::CACHE_CONTROL, cache.to_owned()),
                (header::ETAG, etag),
            ];
            if fresh {
                (StatusCode::NOT_MODIFIED, head).into_response()
            } else {
                (head, asset.bytes).into_response()
            }
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
#[derive(Deserialize, Default)]
pub struct PageQuery {
    #[serde(default, deserialize_with = "optional_project")]
    pub project: Option<ProjectId>,
}
/// Merge this router under p6-02's host/origin guard and shutdown ownership.
pub fn dashboard_router(state: DashboardState) -> Router {
    page_router(PageState::new(state))
}
impl NavView {
    /// The inbox link's name: its open questions, then its unread notes.
    pub fn inbox_label(&self) -> String {
        let mut label = "Inbox".to_owned();
        match self.inbox {
            0 => {}
            1 => label.push_str(", 1 open question"),
            n => label.push_str(&format!(", {n} open questions")),
        }
        match self.notes {
            0 => {}
            1 => label.push_str(", 1 unread note"),
            n => label.push_str(&format!(", {n} unread notes")),
        }
        label
    }
}
impl NavView {
    pub fn is_selected(&self, id: &ProjectId) -> bool {
        self.selected.as_ref() == Some(id)
    }
    /// The page's project, when it has one: the sidebar's head names it.
    pub fn current(&self) -> Option<&ProjectView> {
        let id = self.selected?;
        self.projects.iter().find(|p| p.id == id)
    }
    /// The sidebar's rows: every live project, by name.
    pub fn sidebar(&self) -> impl Iterator<Item = &ProjectView> {
        self.projects.iter().filter(|p| !p.archived)
    }
}

/// The home a read connection's database lives in.
pub fn home_of(c: &rusqlite::Connection) -> std::path::PathBuf {
    c.path()
        .and_then(|p| std::path::Path::new(p).parent())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default()
}
/// Seconds since the epoch as the store writes a time: "2026-10-07T20:47:05Z".
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}
/// UTC RFC3339 timestamps used by the store and heartbeat. Other timezone forms
/// are rejected rather than interpreted in the machine's local timezone.
pub(crate) fn timestamp(text: &str) -> Option<u64> {
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-').map(|s| s.parse::<i64>().ok());
    let year = date.next()??;
    let month = date.next()??;
    let day = date.next()??;
    let time = time.strip_suffix('Z')?;
    let mut time = time
        .split(':')
        .map(|s| s.split('.').next()?.parse::<i64>().ok());
    let hour = time.next()??;
    let minute = time.next()??;
    let second = time.next()??;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
    {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let days =
        era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + (153 * mp + 2) / 5 + day - 1 - 719468;
    u64::try_from(days * 86400 + hour * 3600 + minute * 60 + second).ok()
}
fn optional_project<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ProjectId>, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        Ok(None)
    } else {
        value.parse().map(Some).map_err(serde::de::Error::custom)
    }
}
/// Cookie-only display preferences. Mount inside p6-02's Origin/Host guards.
/// `POST /settings`: the display preferences, as the page's signals carry them (Datastar's
/// `@post` sends them as JSON): `theme` (one of `THEMES`) and `types` (whether value types
/// show), each kept in its cookie for every page drawn after.
pub async fn display_preferences(body: axum::body::Bytes) -> Response {
    if body.len() > 65_536 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let Ok(signals) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let theme = signals
        .get("theme")
        .and_then(serde_json::Value::as_str)
        .filter(|t| !t.is_empty());
    let types = signals.get("types").and_then(serde_json::Value::as_bool);
    // a theme is one of the list: there is no "follow the system" to choose, only the first
    // open's default, which the page keeps as a choice
    if theme.is_some_and(|t| self::theme(t).is_none()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    for (name, value) in [
        ("sluice_theme", theme.map(str::to_owned)),
        (
            "sluice_types",
            types.map(|t| if t { "1" } else { "0" }.to_owned()),
        ),
    ] {
        if let Some(value) = value {
            let cookie =
                format!("{name}={value}; Path=/; Max-Age=34560000; SameSite=Lax; HttpOnly");
            response.headers_mut().append(
                header::SET_COOKIE,
                cookie.parse().expect("validated ASCII cookie"),
            );
        }
    }
    response
}
/// The default time a running step may write nothing before it reads as quiet: two hours.
pub const QUIET_AFTER: u64 = 2 * 3600;
/// How long a running step may write nothing before it reads as quiet, in seconds: its own
/// cadence when its plan tags it `cadence:<n>m|h|d` (a watch loop that speaks only on news,
/// `cadence:1d`), else two hours. One threshold for the index, the board, the drawer and the
/// tab title.
pub fn quiet_after(tags: &[String]) -> u64 {
    tags.iter()
        .filter_map(|t| t.strip_prefix("cadence:"))
        .find_map(|d| {
            let (n, unit) = d.split_at(d.len().checked_sub(1)?);
            let n: u64 = n.parse().ok().filter(|n| *n > 0)?;
            Some(
                n * match unit {
                    "m" => 60,
                    "h" => 3600,
                    "d" => 86400,
                    _ => return None,
                },
            )
        })
        .unwrap_or(QUIET_AFTER)
}
/// Each running run's activity: the newest of its start and its run files' modification times,
/// in seconds; quiet once none is newer than its `quiet_after`. Read once, with the store's
/// snapshot, before any page counts or draws a step, so every surface sees the same quiet.
pub fn observe_activity(home: &std::path::Path, running: &mut [RunningView]) {
    use std::time::UNIX_EPOCH;
    let now = crate::clock::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for run in running {
        observe_run(home, run, now);
    }
}
/// One running run's activity (`observe_activity`), as of `now` (seconds).
pub fn observe_run(home: &std::path::Path, run: &mut RunningView, now: u64) {
    use std::time::UNIX_EPOCH;
    // what it said (progress, its own messages) counts as much as what its run wrote
    let mut newest = [timestamp(&run.started), timestamp(&run.said)]
        .into_iter()
        .flatten()
        .max();
    if run.run_id.parse::<sluice_model::ids::RunId>().is_ok() {
        let directory = home.join("runs").join(&run.run_id);
        for path in [
            directory.clone(),
            directory.join("stderr.txt"),
            directory.join("input.json"),
            directory.join("exit.json"),
        ] {
            if let Ok(meta) = std::fs::symlink_metadata(path)
                && !meta.file_type().is_symlink()
                && let Ok(modified) = meta.modified()
                && let Ok(age) = modified.duration_since(UNIX_EPOCH)
            {
                newest = Some(newest.unwrap_or(0).max(age.as_secs()));
            }
        }
    }
    run.activity = newest;
    run.quiet = newest.is_some_and(|at| now.saturating_sub(at) >= run.quiet_after);
}

/// Read shared navigation and home data inside a caller-owned transaction.
/// Sibling pages call this and their own projection reads in one ReadPool::snapshot.
/// A live step's columns, as `RunningView::read` reads them: its id, status, its current run's
/// start and id, its error, its tags, and when it last said anything itself.
const STEP_LIVE: &str = "step_id,status,coalesce((SELECT started_at FROM runs WHERE runs.project_id=steps.project_id AND runs.step_id=steps.step_id AND runs.generation=steps.generation AND finished_at IS NULL ORDER BY created_at DESC LIMIT 1),''),coalesce((SELECT run_id FROM runs WHERE runs.project_id=steps.project_id AND runs.step_id=steps.step_id AND runs.generation=steps.generation AND finished_at IS NULL ORDER BY created_at DESC LIMIT 1),''),error,coalesce(json_extract(declaration,'$.tags'),'[]'),max(coalesce(progress_at,''),coalesce((SELECT at FROM messages m WHERE m.project_id=steps.project_id AND m.thread='step-'||steps.step_id AND m.\"from\"=steps.step_id ORDER BY m.id DESC LIMIT 1),''))";
impl RunningView {
    fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            step: r.get(0)?,
            started: r.get(2)?,
            quiet: false,
            quiet_after: quiet_after(
                &serde_json::from_str::<Vec<String>>(&r.get::<_, String>(5)?).unwrap_or_default(),
            ),
            run_id: r.get(3)?,
            activity: None,
            said: r.get(6)?,
            stopping: false,
            finishing: false,
            parked: None,
        })
    }
    /// `step`'s run while it runs, its activity not yet observed (`observe_run`).
    pub fn of(
        c: &rusqlite::Connection,
        project: ProjectId,
        step: &str,
    ) -> sluice_store::Result<Option<Self>> {
        Ok(c.prepare_cached(&format!(
            "SELECT {STEP_LIVE} FROM steps WHERE project_id=?1 AND step_id=?2 AND status='running'"
        ))?
        .query_row((project.to_string(), step), Self::read)
        .optional()?)
    }
}
/// A project's open questions to the owner that someone still waits on, as Questions' "For
/// you" lists them: one whose asking run has stopped is under "Nobody is waiting", not here.
pub(crate) fn load_asks(
    c: &rusqlite::Connection,
    project: &str,
) -> sluice_store::Result<Vec<AskView>> {
    let mut q = c.prepare_cached("SELECT m.id,m.\"from\",m.thread,coalesce(m.title,''),m.body FROM messages m WHERE m.project_id=?1 AND m.\"to\"='owner' AND m.needs_reply=1 AND m.resolved_by IS NULL AND m.closed_at IS NULL AND NOT EXISTS (SELECT 1 FROM (SELECT coalesce((SELECT qa.run_id FROM question_attachments qa WHERE qa.project_id=m.project_id AND qa.message_id=m.id AND qa.detached_at IS NULL), m.run_id) AS run) asker WHERE asker.run IS NOT NULL AND NOT EXISTS (SELECT 1 FROM runs r JOIN attempts a ON a.attempt_id=r.attempt_id WHERE r.project_id=m.project_id AND r.run_id=asker.run AND r.finished_at IS NULL AND a.phase!='terminal' AND NOT a.cancel_requested)) ORDER BY m.id")?;
    let rows = q.query_map([project], |r| {
        let from: String = r.get(1)?;
        let thread: String = r.get(2)?;
        let title: String = r.get(3)?;
        let body: String = r.get(4)?;
        Ok(AskView {
            message: r.get(0)?,
            // a step asks on its own thread
            step: if thread.strip_prefix("step-") == Some(from.as_str()) {
                from.clone()
            } else {
                String::new()
            },
            from,
            title: if title.trim().is_empty() {
                threads::headline(&body, 90)
            } else {
                title
            },
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}
pub fn load_snapshot(
    c: &rusqlite::Connection,
    functions: FunctionCatalog,
) -> sluice_store::Result<DashboardSnapshot> {
    let mut statement = c.prepare("SELECT project_id,name,description,icon_text,icon_type,icon_generation,paused,archived,coalesce(changed_at,created_at) FROM projects WHERE deleted_at IS NULL ORDER BY archived,name")?;
    let mut projects = Vec::new();
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let id: ProjectId = raw
            .parse()
            .map_err(|_| sluice_store::StoreError::InvalidDatabase("invalid project id".into()))?;
        let image: Option<String> = row.get(4)?;
        let generation: i64 = row.get(5)?;
        let mut view = ProjectView {
            id,
            name: row.get(1)?,
            description: row.get(2)?,
            icon_text: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            icon_url: if image.is_some() {
                format!("/projects/id/{id}/icon?generation={generation}")
            } else {
                String::new()
            },
            paused: row.get(6)?,
            archived: row.get(7)?,
            changed: row.get(8)?,
            counts: ui::Tally::default(),
            running: vec![],
            stopped: vec![],
            dismissed: Default::default(),
            asks: vec![],
            looks: 0,
        };
        let mut steps = c.prepare_cached(&format!("SELECT {STEP_LIVE} FROM steps WHERE project_id=?1 AND status IN ('running','failed') ORDER BY position"))?;
        // a failed step's error says whether the owner cancelled it: listed apart
        let mut step_rows = steps.query([&raw])?;
        let mut cancels = vec![];
        let stopping = sluice_store::attempts::stopping(c, id)?;
        let finishing = sluice_store::attempts::finishing(c, id)?;
        let parked = sluice_store::resources::parked_steps(c, id)?;
        while let Some(r) = step_rows.next()? {
            let status: String = r.get(1)?;
            let step: String = r.get(0)?;
            if status == "failed" {
                let error: Option<String> = r.get(4)?;
                let failure = failure::Failure::parse(error.as_deref().unwrap_or(""), None);
                let stopped = StoppedView {
                    step,
                    cancelled: failure.cancelled,
                };
                if stopped.cancelled {
                    cancels.push(stopped);
                } else {
                    view.stopped.push(stopped);
                }
            } else {
                let mut run = RunningView::read(r)?;
                let sid = run.step.parse::<sluice_model::ids::StepId>().ok();
                run.stopping = sid.as_ref().is_some_and(|s| stopping.contains(s));
                run.finishing = sid.as_ref().is_some_and(|s| finishing.contains_key(s));
                run.parked = sid.as_ref().and_then(|s| parked.get(s)).cloned();
                view.running.push(run);
            }
        }
        // a cancel the owner dismissed is no longer listed, nor marks the project
        view.dismissed = sluice_store::messages::dismissed(c, id)?;
        cancels.retain(|s| !view.dismissed.contains(&s.step));
        view.stopped.extend(cancels);
        // its runs' activity, read once: whether each is quiet
        observe_activity(&home_of(c), &mut view.running);
        // every step by how it reads from what the store says (the board evaluates the plan
        // too: blocked, held, queued and outside work count as pending here)
        // what the owner set aside in the plan's Needs you: a stop as the store still holds it,
        // a quiet run while it has written nothing since
        let set_aside: std::collections::BTreeMap<String, i64> =
            sluice_store::messages::needs_set_aside(c, id)?
                .into_iter()
                .map(|s| (s.key, s.stamp))
                .collect();
        let mut rows = c.prepare_cached("SELECT step_id,status,paused IS NOT NULL AND paused <> 'false',manual,error FROM steps WHERE project_id=?1")?;
        let mut found = rows.query([&raw])?;
        while let Some(r) = found.next()? {
            let step: String = r.get(0)?;
            let status: sluice_model::commands::StepStatus =
                r.get::<_, String>(1)?.parse().map_err(|e| {
                    sluice_store::StoreError::InvalidDatabase(format!("step {step}: {e}"))
                })?;
            let run = view.running.iter().find(|run| run.step == step);
            let shown = match run {
                Some(run) if status == sluice_model::commands::StepStatus::Running => run.shown(),
                _ => sluice_model::shown::classify(&sluice_model::shown::Facts {
                    cancelled: r
                        .get::<_, Option<String>>(4)?
                        .is_some_and(|e| sluice_model::shown::stored_is_cancel(&e)),
                    paused: view.paused || r.get::<_, bool>(2)?,
                    manual: r.get(3)?,
                    ..sluice_model::shown::Facts::of(status)
                }),
            };
            let aside = match shown {
                ui::Shown::Quiet => run.and_then(|r| r.activity).is_some_and(|at| {
                    set_aside.get(&format!("quiet:{step}")) == Some(&(at as i64))
                }),
                ui::Shown::Cancelled if view.dismissed.contains(&step) => true,
                _ => set_aside.contains_key(&format!("stop:{step}")),
            };
            if shown.spec().attention && !aside {
                view.looks += 1;
            }
            view.counts.add(shown);
        }
        // The project's last record, by its index: records are appended in time order.
        let last: Option<String> = c
            .query_row(
                "SELECT at FROM records WHERE project_id=?1 ORDER BY seq DESC LIMIT 1",
                [&raw],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(last) = last
            && last > view.changed
        {
            view.changed = last;
        }
        view.asks = load_asks(c, &raw)?;
        projects.push(view);
    }
    // the questions put to the owner that someone still waits on, as Questions' "For you"
    // lists them: one whose asking run has stopped is under "Nobody is waiting", not counted
    let inbox = projects.iter().map(|p| p.asks.len()).sum();
    let notes: i64 = c.query_row("SELECT count(*) FROM messages m JOIN projects p USING(project_id) WHERE p.deleted_at IS NULL AND m.\"to\"='owner' AND m.needs_reply=0 AND m.id>coalesce((SELECT cursor FROM readers r WHERE r.project_id=m.project_id AND r.identity='owner' AND r.stream='owner' AND r.thread=m.thread),0)",[],|r|r.get(0))?;
    // The scheduler lease lives as long as its holder's connection to the coordinator.
    let runner_stopped: bool = c.query_row(
        "SELECT scheduler_owner IS NULL FROM maintenance WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(DashboardSnapshot {
        projects,
        inbox,
        notes: notes as usize,
        runner_stopped,
        functions,
    })
}
