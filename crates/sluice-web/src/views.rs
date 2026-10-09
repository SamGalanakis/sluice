//! Shared dashboard facade. Load once per page or SSE batch, then render owned
//! models. URLs use immutable project IDs; labels may change independently.
mod registration;
pub use registration::{Asset, NavEntry, PageRegistration, PageState, page_router};
macro_rules! register_pages {
    ($($module:ident),* $(,)?) => {
        $(pub mod $module;)*
        const PAGES: &[fn() -> PageRegistration] = &[$($module::registration),*];
    };
}
register_pages! { home, board, inbox, log, project_settings, panel, gallery }
pub mod activity;
pub mod failure;
pub mod icons;
pub mod missing;
pub mod step;
pub mod threads;
pub mod timeline;
pub mod ui;
pub mod unit_view;

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

/// The two themes: cream paper and navy paper (DESIGN.md). With neither chosen a page follows
/// the system.
pub const THEMES: [(&str, &str); 2] = [("light", "Light"), ("dark", "Dark")];
#[derive(Clone, Debug, Default)]
pub struct Viewer {
    pub theme: Option<String>,
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
                    "sluice_theme" if THEMES.iter().any(|(id, _)| *id == value) => {
                        viewer.theme = Some(value.into())
                    }
                    "sluice_types" => viewer.types = value == "1",
                    _ => {}
                }
            }
        }
        viewer
    }
    pub fn theme_id(&self) -> &str {
        self.theme.as_deref().unwrap_or("")
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
}
impl RunningView {
    /// How the running step reads (`shown`).
    pub fn shown(&self) -> ui::Shown {
        sluice_model::shown::classify(&sluice_model::shown::Facts {
            quiet: self.quiet,
            stopping: self.stopping,
            finishing: self.finishing,
            ..sluice_model::shown::Facts::of(sluice_model::commands::StepStatus::Running)
        })
    }
}

/// A step that stopped: failed, or cancelled by the owner, with its failure's one sentence
/// (`failure::Failure`'s headline: "Stopped at its wall-clock cap after 10h 0m.").
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoppedView {
    pub step: String,
    pub cancelled: bool,
    pub headline: String,
    /// The log record that says it failed (its latest `step.status` to failed), when kept.
    #[serde(default)]
    pub record: Option<i64>,
}
impl StoppedView {
    /// Its failure's own record on its project's log, that record in view and marked
    /// (`step::failure_log_href`); "" when the log no longer keeps it.
    /// Where its reason leads: its failure's own log record, else its step's records on the
    /// log, so every reason on the index is a link.
    pub fn log_href(&self, project: &ProjectId) -> String {
        self.record.map_or_else(
            || format!("/projects/id/{project}/log?step={}", self.step),
            |seq| step::failure_log_href(project, &self.step, seq),
        )
    }
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
    /// How the index names its running, failed and cancelled steps (`ui::StepRef`), by id.
    #[serde(default)]
    pub names: std::collections::BTreeMap<String, ui::StepRef>,
    /// Its cancelled steps the owner dismissed (`StepDismiss`): each stays on its unit, but no
    /// longer marks the project, its tab or its index row.
    #[serde(default)]
    pub dismissed: std::collections::BTreeSet<String>,
    /// Its open questions to the owner that someone still waits on, oldest first: what the
    /// nav's Inbox counts, each with the step that asked it.
    #[serde(default)]
    pub asks: Vec<AskView>,
}
/// An open question to the owner that someone waits on: its message, the step that asked it
/// ("" for the orchestrator or anyone else), and its title (its body's first line without one).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AskView {
    pub message: i64,
    pub step: String,
    pub title: String,
}
impl AskView {
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
    /// How a page names one of its listed steps: its title and id, or its id alone.
    pub fn step_ref(&self, id: &str) -> ui::StepRef {
        self.names.get(id).cloned().unwrap_or_else(|| ui::StepRef {
            id: id.to_owned(),
            ..Default::default()
        })
    }
    pub fn href(&self) -> String {
        format!("/projects/id/{}", self.id)
    }
    /// How many stopped steps the index lists by name; the rest it counts.
    pub const STOPPED_ROWS: usize = 4;
    /// The stopped steps the index lists, failures first.
    pub fn stopped_rows(&self) -> &[StoppedView] {
        &self.stopped[..self.stopped.len().min(Self::STOPPED_ROWS)]
    }
    /// The stopped steps past `STOPPED_ROWS`.
    pub fn stopped_more(&self) -> usize {
        self.stopped.len().saturating_sub(Self::STOPPED_ROWS)
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
    /// Its pages' tab words, what needs a look first, as the index's tab counts it
    /// (`home::HomeView::title`): "1 question · 2 failed · 1 quiet · almanac", its open questions
    /// to the owner, then the states that need attention.
    pub fn tab_words(&self) -> String {
        let mut words: Vec<String> = questions_words(self.asks.len()).into_iter().collect();
        words.extend(
            self.standing()
                .iter()
                .filter(|(s, _)| s.spec().attention)
                .map(|(s, n)| format!("{n} {}", s.word())),
        );
        words.push(self.name.clone());
        words.join(" · ")
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
    pub catalog: Arc<dyn CatalogSource>,
    pub stop: Arc<std::sync::atomic::AtomicBool>,
    pub plans: board::PlanCache,
    /// Claude's config home, where its session transcripts are (an agent run's activity):
    /// `CLAUDE_CONFIG_DIR`, else `~/.claude`, as Claude finds it.
    pub claude_home: Option<std::path::PathBuf>,
}
impl DashboardState {
    pub fn new(reads: ReadPool, catalog: Arc<dyn CatalogSource>) -> Self {
        Self {
            reads,
            catalog,
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            plans: board::PlanCache::default(),
            claude_home: sluice_agents::activity::Homes::claude_from_env(),
        }
    }
    /// The catalog, one store snapshot and the runs' activity, each read once. A page renders
    /// what this returns; nothing that moves while it renders can fail it.
    pub async fn snapshot(
        &self,
        project: Option<ProjectId>,
    ) -> Result<DashboardSnapshot, PublicError> {
        let functions = self.catalog.catalog(project)?;
        self.reads
            .snapshot(move |c| load_snapshot(c, functions))
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
        let (sum, count, running): (i64, i64, bool) = self
            .reads
            .snapshot(move |c| {
                let (sum, count) = c.query_row(
                    "SELECT coalesce(sum(version),0),count(*) FROM change_versions",
                    [],
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
            })
            .await
            .map_err(|e| e.into_public(true))?;
        let tick = if running {
            std::time::SystemTime::now()
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
    pub settings_href: String,
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
        // the switcher lists projects as the index does: most urgent first
        let mut projects = snapshot.projects.clone();
        projects.sort_by_key(|p| (p.counts.total() == 0, p.shown()));
        Ok(Self {
            projects,
            label: chosen
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "All projects".into()),
            links: sections
                .into_iter()
                .map(|entry| NavLink {
                    href: entry.href,
                    label: entry.label.into(),
                    current: entry.key == tab,
                })
                .collect(),
            inbox: snapshot.inbox,
            notes: snapshot.notes,
            settings_href: chosen
                .map(|p| format!("{}/settings", p.href()))
                .unwrap_or_default(),
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
/// What a page puts in the frame around it (DESIGN.md, The band): its head in the title band
/// (`ui::band_head`, then any strip such as `ui::recent_strip`), and on the paper row under the
/// band, after its sections, a meta line ("12 units · 40 steps") and its tools (a find, the
/// grid switch). A page with none of it gets the band's top row alone.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub head: TrustedHtml,
    pub meta: TrustedHtml,
    pub tools: TrustedHtml,
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
    themes: &'a [(&'a str, &'a str)],
    path: &'a str,
    style_url: String,
    nav_url: String,
    components_url: String,
    datastar_url: String,
    import_map: &'static str,
    /// The build that drew the page (`streams::release`): a stream from another says so.
    release: &'static str,
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
    TrustedHtml::from_template(&Layout {
        title,
        frame,
        body,
        nav,
        viewer,
        stream,
        signals: serde_json::json!({"ver":version,"stale":false}).to_string(),
        themes: &THEMES,
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
        for asset in PAGES.iter().flat_map(|page| (page)().assets) {
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
/// Render the shared stable nav target for a sibling page's patch batch.
pub fn render_nav(
    nav: &NavView,
    viewer: &Viewer,
    path: &str,
) -> Result<TrustedHtml, askama::Error> {
    let layout = render_layout(
        "",
        &TrustedHtml::owned(String::new()),
        nav,
        viewer,
        "",
        "",
        path,
    )?;
    let start = layout
        .as_str()
        .find("<nav id=\"top-nav\"")
        .expect("owned layout has nav");
    let end = start
        + layout.as_str()[start..]
            .find("</nav>")
            .expect("owned layout closes nav")
        + 6;
    Ok(TrustedHtml::owned(layout.as_str()[start..end].into()))
}
/// Every embedded asset's fingerprint, folded into one: what a release's pages look and act
/// like (`streams::release`).
pub fn assets_fingerprint() -> String {
    let mut all = String::new();
    for registered in PAGES.iter().flat_map(|page| (page)().assets) {
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
            for asset in PAGES.iter().flat_map(|page| (page)().assets) {
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
    match asset(&name) {
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
    pub fn has_archived(&self) -> bool {
        self.projects.iter().any(|p| p.archived)
    }
}
impl NavView {
    pub fn is_selected(&self, id: &ProjectId) -> bool {
        self.selected.as_ref() == Some(id)
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
pub async fn display_preferences(body: axum::body::Bytes) -> Response {
    if body.len() > 4096 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let values: url::form_urlencoded::Parse<'_> = url::form_urlencoded::parse(&body);
    let mut theme = None;
    let mut types = None;
    let mut next = "/".to_owned();
    for (name, value) in values {
        match name.as_ref() {
            "theme" => theme = Some(value.into_owned()),
            "types" => types = Some(value.into_owned()),
            "next" => next = value.into_owned(),
            _ => {}
        }
    }
    // an empty theme is "Match system": the cookie goes, and the page follows the OS
    if theme
        .as_deref()
        .is_some_and(|t| !t.is_empty() && !THEMES.iter().any(|(id, _)| *id == t))
        || types.as_deref().is_some_and(|t| t != "0" && t != "1")
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if !next.starts_with('/')
        || next.starts_with("//")
        || next.contains('\\')
        || next.chars().any(char::is_control)
    {
        next = "/".into();
    }
    let mut response = axum::response::Redirect::to(&next).into_response();
    for (name, value) in [("sluice_theme", theme), ("sluice_types", types)] {
        if let Some(value) = value {
            let age = if value.is_empty() { 0 } else { 34_560_000 };
            let cookie = format!("{name}={value}; Path=/; Max-Age={age}; SameSite=Lax; HttpOnly");
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
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
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
                from
            } else {
                String::new()
            },
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
            names: Default::default(),
            dismissed: Default::default(),
            asks: vec![],
        };
        let mut steps = c.prepare_cached(&format!("SELECT {STEP_LIVE} FROM steps WHERE project_id=?1 AND status IN ('running','failed') ORDER BY position"))?;
        // a failed step's error says whether the owner cancelled it: listed apart
        let mut step_rows = steps.query([&raw])?;
        let mut cancels = vec![];
        let stopping = sluice_store::attempts::stopping(c, id)?;
        let finishing = sluice_store::attempts::finishing(c, id)?;
        while let Some(r) = step_rows.next()? {
            let status: String = r.get(1)?;
            let step: String = r.get(0)?;
            if status == "failed" {
                let error: Option<String> = r.get(4)?;
                let failure = failure::Failure::parse(error.as_deref().unwrap_or(""), None);
                let stopped = StoppedView {
                    step,
                    cancelled: failure.cancelled,
                    headline: failure.headline,
                    record: None,
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
                view.running.push(run);
            }
        }
        // a cancel the owner dismissed is no longer listed, nor marks the project
        view.dismissed = sluice_store::messages::dismissed(c, id)?;
        cancels.retain(|s| !view.dismissed.contains(&s.step));
        view.stopped.extend(cancels);
        // each stopped step's failure as its log records it, so its row links that record
        if !view.stopped.is_empty() {
            let records = step::failure_records(c, id)?;
            for stopped in &mut view.stopped {
                stopped.record = records.get(&stopped.step).copied();
            }
        }
        // its runs' activity, read once: whether each is quiet
        observe_activity(&home_of(c), &mut view.running);
        // every step by how it reads from what the store says (the board evaluates the plan
        // too: blocked, held, queued and outside work count as pending here)
        let mut rows = c.prepare_cached("SELECT step_id,status,paused IS NOT NULL AND paused <> 'false',manual,error FROM steps WHERE project_id=?1")?;
        let mut found = rows.query([&raw])?;
        while let Some(r) = found.next()? {
            let step: String = r.get(0)?;
            let status: sluice_model::commands::StepStatus =
                r.get::<_, String>(1)?.parse().map_err(|e| {
                    sluice_store::StoreError::InvalidDatabase(format!("step {step}: {e}"))
                })?;
            let shown = match view.running.iter().find(|run| run.step == step) {
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
            view.counts.add(shown);
        }
        // the steps the index lists, named (`sluice_model::naming`)
        if !(view.running.is_empty() && view.stopped.is_empty()) {
            let names = sluice_runtime::naming::for_project(c, &home_of(c), id)?;
            for step in view
                .running
                .iter()
                .map(|r| r.step.clone())
                .chain(view.stopped.iter().map(|s| s.step.clone()))
                .collect::<Vec<_>>()
            {
                let named = ui::StepRef::new(&step, names.naming.step(&step));
                view.names.insert(step, named);
            }
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_time_reads_back_as_the_store_writes_it() {
        for at in [
            "2026-10-07T20:47:05Z",
            "2024-02-29T00:00:00Z",
            "1999-12-31T23:59:59Z",
        ] {
            assert_eq!(super::rfc3339(super::timestamp(at).unwrap()), at);
        }
    }
}
