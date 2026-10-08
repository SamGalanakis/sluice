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
register_pages! { home, board, inbox, log, project_settings, panel }
pub mod failure;
pub mod icons;
pub mod missing;
pub mod step;
pub mod threads;

use askama::Template;
use axum::{
    Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sluice_model::{error::PublicError, ids::ProjectId};
use sluice_store::ReadPool;
use std::{fmt, sync::Arc};

/// HTML emitted by an owned Askama template or the markdown renderer. There is
/// deliberately no public constructor accepting an arbitrary string.
#[derive(Clone, Debug, PartialEq, Eq)]
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

pub const THEMES: [(&str, &str); 7] = [
    ("light", "Sluice Light"),
    ("dark", "Sluice Dark"),
    ("canyon", "Canyon"),
    ("ranger", "Ranger"),
    ("diner", "Diner"),
    ("night-sky", "Night Sky"),
    ("wood-panel", "Wood Panel"),
];
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
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Counts {
    pub pending: usize,
    pub running: usize,
    pub succeeded: usize,
    /// Failed, not counting the ones the owner cancelled.
    pub failed: usize,
    /// Failed because the owner cancelled them (`failure::is_cancel`).
    pub cancelled: usize,
    pub stale: usize,
    pub skipped: usize,
    pub paused: usize,
}
impl Counts {
    pub fn total(&self) -> usize {
        self.pending
            + self.running
            + self.succeeded
            + self.failed
            + self.cancelled
            + self.stale
            + self.skipped
    }
    pub fn status(&self) -> &str {
        if self.failed > 0 {
            "failed"
        } else if self.running > 0 {
            "running"
        } else if self.stale > 0 {
            "stale"
        } else if self.cancelled > 0 {
            "cancelled"
        } else if self.total() > 0 && self.succeeded + self.skipped == self.total() {
            "succeeded"
        } else {
            "pending"
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunningView {
    pub step: String,
    pub title: String,
    pub started: String,
    pub quiet: bool,
    /// Quiet for two hours or more: what the board's Attention view shows.
    pub long_quiet: bool,
    pub run_id: String,
    pub activity: Option<u64>,
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
    pub counts: Counts,
    pub running: Vec<RunningView>,
    pub failed_steps: Vec<String>,
    pub cancelled_steps: Vec<String>,
}
impl ProjectView {
    pub fn href(&self) -> String {
        format!("/projects/id/{}", self.id)
    }
    /// Its running steps quiet for two hours or more.
    pub fn long_quiet(&self) -> usize {
        self.running.iter().filter(|r| r.long_quiet).count()
    }
    pub fn summary(&self) -> &str {
        self.description.split("\n\n").next().unwrap_or("")
    }
    pub fn now(&self) -> &str {
        if self.paused {
            "Paused."
        } else if self.counts.total() == 0 {
            "No steps yet."
        } else if self.counts.succeeded + self.counts.skipped == self.counts.total() {
            "Finished."
        } else if self.counts.failed > 0 {
            "Stopped: nothing is running."
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
/// Registry owners return the visible entries, including errors and shadowed
/// collisions. Its token must change for every externally visible registry edit.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionCatalog {
    pub version: String,
    pub entries: Vec<FunctionView>,
}
pub trait CatalogSource: Send + Sync {
    fn catalog(&self, project: Option<ProjectId>) -> Result<FunctionCatalog, PublicError>;
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
}
impl DashboardState {
    pub fn new(reads: ReadPool, catalog: Arc<dyn CatalogSource>) -> Self {
        Self {
            reads,
            catalog,
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            plans: board::PlanCache::default(),
        }
    }
    /// The catalog, one store snapshot and the runs' activity, each read once. A page renders
    /// what this returns; nothing that moves while it renders can fail it.
    pub async fn snapshot(
        &self,
        project: Option<ProjectId>,
    ) -> Result<DashboardSnapshot, PublicError> {
        let functions = self.catalog.catalog(project)?;
        let snapshot = self
            .reads
            .snapshot(move |c| load_snapshot(c, functions))
            .await
            .map_err(|e| e.into_public(true))?;
        self.observe(snapshot).await
    }
    /// Observe each running run's activity from its run files, once.
    pub async fn observe(
        &self,
        mut snapshot: DashboardSnapshot,
    ) -> Result<DashboardSnapshot, PublicError> {
        let home = self.reads.home().to_owned();
        tokio::task::spawn_blocking(move || {
            observe_activity(&home, &mut snapshot);
            snapshot
        })
        .await
        .map_err(|e| PublicError::Storage {
            message: e.to_string(),
        })
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
        Ok(Self {
            projects: snapshot.projects.clone(),
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
            settings_href: chosen
                .map(|p| format!("{}/settings", p.href()))
                .unwrap_or_default(),
            selected: project,
        })
    }
}
#[derive(Template)]
#[template(path = "layout.html")]
struct Layout<'a> {
    title: &'a str,
    body: &'a TrustedHtml,
    nav: &'a NavView,
    viewer: &'a Viewer,
    stream: &'a str,
    signals: String,
    themes: &'a [(&'a str, &'a str)],
    path: &'a str,
    style_url: String,
    nav_url: String,
    datastar_url: String,
    import_map: &'static str,
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
    TrustedHtml::from_template(&Layout {
        title,
        body,
        nav,
        viewer,
        stream,
        signals: serde_json::json!({"ver":version,"stale":false}).to_string(),
        themes: &THEMES,
        path,
        style_url: asset_url("style.css"),
        nav_url: asset_url("nav.js"),
        datastar_url: asset_url("datastar-rocket-1.0.4.js"),
        import_map: import_map(),
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
    pub fn has_archived(&self) -> bool {
        self.projects.iter().any(|p| p.archived)
    }
}
impl NavView {
    pub fn is_selected(&self, id: &ProjectId) -> bool {
        self.selected.as_ref() == Some(id)
    }
}

/// A stored time as every page draws it before its script reads it: "2026-10-07 20:47 UTC".
/// Nothing renders a raw RFC 3339 time; `nav.js` turns a `<time>` into the page's words.
pub fn when(at: &str) -> String {
    match (at.get(..10), at.get(10..11), at.get(11..16)) {
        (Some(day), Some("T"), Some(time)) => format!("{day} {time} UTC"),
        _ => at.to_owned(),
    }
}
/// A past time as a page says it: "12m ago", "3d 12h ago" once its script reads it, the UTC
/// day and minute before (and in its title, always).
pub fn ago_html(at: &str) -> TrustedHtml {
    time_html(at, "data-ago")
}
/// How long since a time, ticking: "45s", "12m", "2h 14m", "3d 12h".
pub fn since_html(at: &str) -> TrustedHtml {
    time_html(at, "data-since")
}
fn time_html(at: &str, mode: &str) -> TrustedHtml {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let shown = esc(&when(at));
    TrustedHtml::owned(format!(
        "<time {mode}=\"{at}\" datetime=\"{at}\" title=\"{shown}\">{shown}</time>",
        at = esc(at)
    ))
}

/// UTC RFC3339 timestamps used by the store and heartbeat. Other timezone forms
/// are rejected rather than interpreted in the machine's local timezone.
fn timestamp(text: &str) -> Option<u64> {
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
/// Each running run's activity: the newest of its start and its run files' modification times,
/// in seconds; quiet after 15 minutes without any.
pub fn observe_activity(home: &std::path::Path, snapshot: &mut DashboardSnapshot) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for run in snapshot.projects.iter_mut().flat_map(|p| &mut p.running) {
        let mut newest = timestamp(&run.started);
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
        run.quiet = newest.is_some_and(|at| now.saturating_sub(at) >= 900);
        run.long_quiet = newest.is_some_and(|at| now.saturating_sub(at) >= 7200);
    }
}

/// Read shared navigation and home data inside a caller-owned transaction.
/// Sibling pages call this and their own projection reads in one ReadPool::snapshot.
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
            counts: Counts::default(),
            running: vec![],
            failed_steps: vec![],
            cancelled_steps: vec![],
        };
        let mut counts = c.prepare_cached("SELECT status,count(*),sum(paused IS NOT NULL AND paused <> 'false') FROM steps WHERE project_id=?1 GROUP BY status")?;
        let mut count_rows = counts.query([&raw])?;
        while let Some(r) = count_rows.next()? {
            let status: String = r.get(0)?;
            let n = r.get::<_, i64>(1)? as usize;
            view.counts.paused += r.get::<_, i64>(2)? as usize;
            match status.as_str() {
                "pending" => view.counts.pending = n,
                "running" => view.counts.running = n,
                "succeeded" => view.counts.succeeded = n,
                "failed" => view.counts.failed = n,
                "stale" => view.counts.stale = n,
                "skipped" => view.counts.skipped = n,
                _ => {}
            }
        }
        let mut steps = c.prepare_cached("SELECT step_id,coalesce(json_extract(declaration,'$.doc'),step_id),status,coalesce((SELECT started_at FROM runs WHERE runs.project_id=steps.project_id AND runs.step_id=steps.step_id AND runs.generation=steps.generation AND finished_at IS NULL ORDER BY created_at DESC LIMIT 1),''),coalesce((SELECT run_id FROM runs WHERE runs.project_id=steps.project_id AND runs.step_id=steps.step_id AND runs.generation=steps.generation AND finished_at IS NULL ORDER BY created_at DESC LIMIT 1),''),error FROM steps WHERE project_id=?1 AND status IN ('running','failed') ORDER BY position")?;
        // a failed step's error says whether the owner cancelled it: counted apart
        let mut step_rows = steps.query([&raw])?;
        while let Some(r) = step_rows.next()? {
            let status: String = r.get(2)?;
            if status == "failed" {
                let error: Option<String> = r.get(5)?;
                let cancelled = error
                    .and_then(|e| serde_json::from_str::<PublicError>(&e).ok())
                    .is_some_and(|e| failure::is_cancel(&e));
                if cancelled {
                    view.counts.failed -= 1;
                    view.counts.cancelled += 1;
                    view.cancelled_steps.push(r.get(0)?);
                } else {
                    view.failed_steps.push(r.get(0)?);
                }
            } else {
                view.running.push(RunningView {
                    step: r.get(0)?,
                    title: r.get(1)?,
                    started: r.get(3)?,
                    quiet: false,
                    long_quiet: false,
                    run_id: r.get(4)?,
                    activity: None,
                });
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
        projects.push(view);
    }
    let inbox: i64 = c.query_row("SELECT count(*) FROM messages m JOIN projects p USING(project_id) WHERE p.deleted_at IS NULL AND m.\"to\"='owner' AND needs_reply=1 AND resolved_by IS NULL AND closed_at IS NULL",[],|r|r.get(0))?;
    // The scheduler lease lives as long as its holder's connection to the coordinator.
    let runner_stopped: bool = c.query_row(
        "SELECT scheduler_owner IS NULL FROM maintenance WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(DashboardSnapshot {
        projects,
        inbox: inbox as usize,
        runner_stopped,
        functions,
    })
}
