//! Project settings commands and id-scoped views. The host supplies its resource
//! adapter and coordinator deletion guard, and mounts this router inside its
//! Host/Origin policy. All mutations use the store's single writer transaction.
use crate::clock::ClockRead;
use crate::streams::StreamEvent;
use crate::{
    markdown,
    streams::{self, PatchRegion, RenderedBatch, StreamQuery, VersionSignal},
    views::{self, DashboardSnapshot, DashboardState, NavView, TrustedHtml, Viewer},
};
use askama::Template;
use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response, Sse, sse::KeepAlive},
    routing::{get, post},
};
use datastar::prelude::{ElementPatchMode, ExecuteScript, PatchElements};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, ProjectSelector, Revision},
};
use sluice_store::{
    RetrySafety, StoreError, Writer,
    projects::{self, DeletionGuard, Icon, Project, ResourceSettings, UpdateProject},
    resources,
};
use std::{collections::BTreeMap, sync::Arc};

/// The host wires its command service here. A committed result is the only
/// success acknowledgement; transport handlers never write SQL themselves.
pub trait SettingsCommands: Send + Sync {
    fn update(
        &self,
        id: ProjectId,
        request: UpdateProject,
    ) -> BoxFuture<'_, Result<Project, PublicError>>;
    fn delete(
        &self,
        id: ProjectId,
        request: projects::DeleteProject,
    ) -> BoxFuture<'_, Result<(), PublicError>>;
    /// Set or clear the project's board (`board_set`); returns its revision and warnings.
    fn board(
        &self,
        id: ProjectId,
        request: projects::SetBoard,
    ) -> BoxFuture<'_, Result<projects::BoardSetOutcome, PublicError>>;
}
#[derive(Clone)]
pub struct StoreCommands {
    pub writer: Writer,
    pub resources: Arc<dyn ResourceSettings + Send + Sync>,
    pub deletion_guard: Arc<dyn DeletionGuard + Send + Sync>,
}
impl SettingsCommands for StoreCommands {
    fn update(
        &self,
        id: ProjectId,
        request: UpdateProject,
    ) -> BoxFuture<'_, Result<Project, PublicError>> {
        Box::pin(async move {
            let resources = self.resources.clone();
            self.writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    projects::project_update(
                        tx,
                        &ProjectSelector::Id(id),
                        request,
                        resources.as_ref(),
                    )
                })
                .await
        })
    }
    fn delete(
        &self,
        id: ProjectId,
        request: projects::DeleteProject,
    ) -> BoxFuture<'_, Result<(), PublicError>> {
        Box::pin(async move {
            let guard = self.deletion_guard.clone();
            self.writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    projects::project_delete(tx, &ProjectSelector::Id(id), request, guard.as_ref())
                })
                .await
                .map(|_| ())
        })
    }
    fn board(
        &self,
        id: ProjectId,
        request: projects::SetBoard,
    ) -> BoxFuture<'_, Result<projects::BoardSetOutcome, PublicError>> {
        Box::pin(async move {
            self.writer
                .write(RetrySafety::NonIdempotent, move |tx| {
                    projects::board_set(tx, &ProjectSelector::Id(id), request)
                })
                .await
        })
    }
}
#[derive(Clone)]
pub struct SettingsState {
    pub dashboard: DashboardState,
    pub commands: Arc<dyn SettingsCommands>,
    pub deletion_guard: Arc<dyn DeletionGuard + Send + Sync>,
}
impl SettingsState {
    pub fn new(
        dashboard: DashboardState,
        commands: Arc<dyn SettingsCommands>,
        deletion_guard: Arc<dyn DeletionGuard + Send + Sync>,
    ) -> Self {
        Self {
            dashboard,
            commands,
            deletion_guard,
        }
    }
    pub async fn snapshot(&self, id: ProjectId) -> Result<ProjectSettingsView, PublicError> {
        let catalog = self.dashboard.catalog.clone();
        let functions = tokio::task::spawn_blocking(move || catalog.catalog(Some(id)))
            .await
            .map_err(|e| PublicError::Storage {
                message: e.to_string(),
            })??;
        let guard = self.deletion_guard.clone();
        let plans = self.dashboard.plans.clone();
        self.dashboard
            .reads
            .clock_snapshot(move |c| {
                let shared = views::load_snapshot(c, functions)?;
                let project = projects::resolve(c, &ProjectSelector::Id(id))?;
                let blocker = projects::live_work_blocker(c, id)?.or_else(|| {
                    guard
                        .check(id)
                        .err()
                        .map(|e| e.into_public(true).to_string())
                });
                let declarations = resources::declarations(c, id)?;
                let held = resources::held(c, id)?;
                let leases = resources::leases(c, id)?;
                // the steps each resource queues: none while no pending step needs a resource
                // (`steps_needs`, no declaration read); else those admission would start but
                // their resources hold back, over the plan the dashboard keeps compiled
                let needing: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM steps WHERE project_id=?1 AND status='pending' AND needs IS NOT NULL)",
                    [id.to_string()],
                    |r| r.get(0),
                )?;
                let queued = if needing {
                    let signatures = format!("settings:{}", shared.functions.version);
                    let provider = CatalogSignatures(&shared.functions);
                    match plans.plan(c, id, &signatures, &provider) {
                        Ok((rev, plan)) => Some(resources::status(c, id, rev, &plan)?),
                        Err(StoreError::Public(PublicError::Invalid { .. })) => None,
                        Err(e) => return Err(e),
                    }
                } else {
                    Some(BTreeMap::new())
                };
                let rows = declarations
                    .into_values()
                    .map(|r| ResourceView {
                        name: r.name.clone(),
                        number: match &r.declaration {
                            resources::Capacity::Fixed(n) => n.to_string(),
                            resources::Capacity::Function(_) => String::new(),
                        },
                        function: match &r.declaration {
                            resources::Capacity::Function(f) => f.to_string(),
                            resources::Capacity::Fixed(_) => String::new(),
                        },
                        value: match &r.declaration {
                            resources::Capacity::Fixed(n) => n.to_string(),
                            resources::Capacity::Function(f) => {
                                serde_json::json!({"capacity_fn":f}).to_string()
                            }
                        },
                        capacity: r
                            .capacity
                            .map_or_else(|| "unknown".into(), |n| n.to_string()),
                        dynamic: matches!(r.declaration, resources::Capacity::Function(_)),
                        held: held.get(&r.name).copied().unwrap_or(0),
                        waiting: leases
                            .iter()
                            .filter(|l| {
                                l.resource == r.name
                                    && l.state == sluice_model::commands::LeaseState::Waiting
                            })
                            .count(),
                        queued: queued
                            .as_ref()
                            .map(|rows| rows.get(&r.name).map_or(0, |r| r.queued)),
                        error: r.error.map(|e| e.to_string()).unwrap_or_default(),
                    })
                    .collect();
                let doc = projects::board_doc(c, id)?;
                let retired = projects::last_retirement(c, id)?;
                let changes = field_changes(c, id)?;
                let fallback = sluice_store::identities::fallback(c, id)?.unwrap_or_default();
                let sessions = sessions(c, id)?;
                Ok(ProjectSettingsView {
                    shared,
                    project,
                    resources: rows,
                    doc,
                    retired,
                    blocker,
                    changes,
                    fallback,
                    sessions,
                })
            })
            .await
            .map_err(|e| e.into_public(true))
    }
}
/// The settings a form edits, each with its last change: the seq of the record that made it and
/// who did. A section's form carries the last change it was drawn with (`seen`); a Save after
/// someone else changed one of its settings is refused, never written over theirs.
pub const FIELDS: [&str; 9] = [
    "name",
    "description",
    "icon",
    "fallback",
    "resources",
    "prune_done_after",
    "prune_keep",
    "paused",
    "archived",
];
fn field_changes(
    c: &rusqlite::Connection,
    id: ProjectId,
) -> sluice_store::Result<BTreeMap<&'static str, (i64, String)>> {
    let mut changes = BTreeMap::new();
    let mut q = c.prepare_cached(
        "SELECT seq,kind,payload FROM records WHERE project_id=?1 AND kind IN ('project.update','project.rename','project.pause','project.archive') ORDER BY seq DESC",
    )?;
    let mut rows = q.query([id.to_string()])?;
    while changes.len() < FIELDS.len()
        && let Some(r) = rows.next()?
    {
        let (seq, kind, payload): (i64, String, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
        let payload: serde_json::Value = serde_json::from_str(&payload)?;
        let author = payload["author"].as_str().unwrap_or("").to_owned();
        let fields: Vec<&str> = match kind.as_str() {
            "project.rename" => vec!["name"],
            "project.pause" => vec!["paused"],
            "project.archive" => vec!["archived"],
            _ => payload["fields"]
                .as_array()
                .map(|f| f.iter().filter_map(serde_json::Value::as_str).collect())
                .unwrap_or_default(),
        };
        for field in fields {
            if let Some(field) = FIELDS.iter().find(|f| **f == field) {
                changes
                    .entry(*field)
                    .or_insert_with(|| (seq, author.clone()));
            }
        }
    }
    Ok(changes)
}
/// The sessions registered in the project (SPEC §8.1: a session's readers row), by name, each
/// with when it was last seen: its reader's last heartbeat or its last message, whichever is
/// later ("" when it has neither).
fn sessions(c: &rusqlite::Connection, id: ProjectId) -> sluice_store::Result<Vec<SessionView>> {
    let mut q = c.prepare_cached(
        "SELECT r.identity, max(coalesce(r.heartbeat_at,''), coalesce(m.at,'')) FROM readers r
         LEFT JOIN (SELECT \"from\" AS who, max(at) AS at FROM messages WHERE project_id=?1 AND \"from\" LIKE '@%' GROUP BY \"from\") m ON m.who=r.identity
         WHERE r.project_id=?1 AND r.stream=?2 AND r.thread='' ORDER BY r.identity",
    )?;
    let rows = q
        .query_map(
            rusqlite::params![id.to_string(), sluice_store::identities::READER_STREAM],
            |r| {
                Ok(SessionView {
                    name: r.get(0)?,
                    seen: r.get(1)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|s| sluice_model::identity::is_session(&s.name))
        .collect())
}
/// A session registered in the project and when it was last seen (RFC 3339, "" for never).
#[derive(Clone, Debug, Serialize)]
pub struct SessionView {
    pub name: String,
    pub seen: String,
}
struct CatalogSignatures<'a>(&'a views::FunctionCatalog);
impl sluice_model::plan::SignatureProvider for CatalogSignatures<'_> {
    fn signature(&self, name: &str) -> Option<sluice_model::plan::FnSignature> {
        if name == "core.external" {
            return Some(sluice_model::plan::FnSignature {
                open: true,
                ..Default::default()
            });
        }
        let function = self
            .0
            .entries
            .iter()
            .find(|f| f.name == name && f.error.is_empty())?;
        let ports = |ports: &[views::PortView]| {
            ports
                .iter()
                .map(|p| p.ty.parse().map(|ty| (p.name.clone(), ty)))
                .collect::<Result<Vec<_>, _>>()
                .ok()
        };
        Some(sluice_model::plan::FnSignature {
            inputs: ports(&function.inputs)?.into_iter().collect(),
            outputs: ports(&function.outputs)?.into_iter().collect(),
            ..Default::default()
        })
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct ResourceView {
    pub name: String,
    pub value: String,
    /// Its fixed capacity, or its fn's name: the two halves of its editor.
    pub number: String,
    pub function: String,
    pub capacity: String,
    pub dynamic: bool,
    pub held: u64,
    pub waiting: usize,
    pub queued: Option<usize>,
    pub error: String,
}
/// The page's sections that are one form each, with the settings each edits.
const SECTIONS: [(&str, &[&str]); 3] = [
    ("details", &["name", "description", "icon", "fallback"]),
    ("resources", &["resources"]),
    ("retiring", &["prune_done_after", "prune_keep"]),
];
pub struct ProjectSettingsView {
    pub shared: DashboardSnapshot,
    pub project: Project,
    pub resources: Vec<ResourceView>,
    /// The board's document (`board_doc_write`): shown under the program, read-only.
    pub doc: projects::BoardDoc,
    /// The latest automatic retirement (SPEC §6.11), when there has been one.
    pub retired: Option<projects::Retirement>,
    pub blocker: Option<String>,
    /// Each setting's last change (`field_changes`): the record's seq and its author.
    pub changes: BTreeMap<&'static str, (i64, String)>,
    /// Who answers its unowned units: a session's `@name`, `owner`, or "" for nobody.
    pub fallback: String,
    /// Its registered sessions, by name.
    pub sessions: Vec<SessionView>,
}
impl ProjectSettingsView {
    /// The fns that can give a resource its capacity (they return `{capacity}`), with
    /// `current` kept even when the catalog no longer lists it.
    pub fn capacity_fns(&self, current: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .shared
            .functions
            .entries
            .iter()
            .filter(|f| f.error.is_empty() && f.outputs.iter().any(|o| o.name == "capacity"))
            .map(|f| f.name.clone())
            .collect();
        if !current.is_empty() && !names.iter().any(|n| n == current) {
            names.push(current.to_owned());
        }
        names.sort();
        names.dedup();
        names
    }
    /// `prune_done_after` in hours, as the field shows it: "6", "1.5", or "" when off.
    pub fn retire_hours(&self) -> String {
        self.project.prune_done_after.map(hours).unwrap_or_default()
    }
    /// The keep patterns as the field shows them: separated by a comma and a space.
    pub fn retire_keep(&self) -> String {
        self.project.prune_keep.join(", ")
    }
    /// The board's document drawn as the board draws it.
    pub fn doc_html(&self) -> TrustedHtml {
        markdown::render(&self.doc.markdown)
    }
    /// Whether the board's program draws the document (has a `Doc()`).
    pub fn doc_drawn(&self) -> bool {
        self.project
            .board
            .as_deref()
            .and_then(|p| sluice_model::openui::check_board(p).ok())
            .is_some_and(|b| b.has_doc())
    }
    pub fn path(&self) -> String {
        format!("/projects/id/{}/settings", self.project.project_id)
    }
    /// Delete project's confirmation: one button and a dialog, no typed name; dimmed while
    /// something blocks it (drawn in the page's stream, so it follows the live state).
    pub fn delete_confirm(&self) -> crate::views::ui::Confirm {
        let name = &self.project.name;
        crate::views::ui::Confirm {
            opener: "Delete project".into(),
            opener_id: "delete-button".into(),
            title: format!("Delete {name}?"),
            action: format!("{}/delete", self.path()),
            form_id: "delete-form".into(),
            hidden: vec![("expected_settings_rev", self.project.settings_rev.to_string())],
            copy: "This permanently removes the plan, messages, history, functions, secrets and artifacts. This cannot be undone.".into(),
            confirm: format!("Delete {name}"),
            keep: "Keep it",
            danger: true,
            disabled: self.blocker.is_some(),
            ..Default::default()
        }
    }
    /// Clear board's confirmation: it removes the program from the plan's page, the program
    /// sent along so the box keeps it after (Save board restores it).
    pub fn clear_confirm(&self) -> crate::views::ui::Confirm {
        crate::views::ui::Confirm {
            opener: "Clear board".into(),
            opener_id: "board-clear".into(),
            title: "Clear the board?".into(),
            action: format!("{}/board", self.path()),
            form_id: "board-clear-form".into(),
            hidden: vec![
                ("op", "clear".into()),
                ("expected_rev", self.project.board_rev.to_string()),
                ("program", self.project.board.clone().unwrap_or_default()),
            ],
            copy: format!(
                "Save board puts rev {} back until you leave this page.",
                self.project.board_rev
            ),
            confirm: "Clear board".into(),
            keep: "Keep it",
            danger: true,
            disabled: self.project.board.is_none(),
            ..Default::default()
        }
    }
    /// Remove a resource's confirmation: its steps then run with no limit on it.
    pub fn remove_confirm(&self, resource: &str) -> crate::views::ui::Confirm {
        crate::views::ui::Confirm {
            opener: format!("Remove {resource}"),
            title: format!("Remove {resource}?"),
            action: self.path(),
            form_id: format!("remove-{resource}"),
            hidden: vec![
                ("section", "remove".into()),
                ("resource", resource.to_owned()),
            ],
            copy: "Refused while a step still needs it.".into(),
            confirm: format!("Remove {resource}"),
            keep: "Keep it",
            danger: true,
            ..Default::default()
        }
    }
    /// The last change to any setting `section` edits, as drawn: what its form has seen.
    fn seen(&self, section: &str) -> i64 {
        SECTIONS
            .iter()
            .find(|(s, _)| *s == section)
            .map_or(0, |(_, fields)| {
                fields
                    .iter()
                    .filter_map(|f| self.changes.get(f).map(|c| c.0))
                    .max()
                    .unwrap_or(0)
            })
    }
    /// The opening of the Board section's form, which posts to the board's own address.
    pub fn board_form_open(&self) -> String {
        views::ui::post_form(
            "board-program-form",
            "settings-form",
            &format!("{}/board", self.path()),
        )
    }
    /// The opening of `section`'s form (`<section>-form`, a `ui::post_form` posting to
    /// `action`, with `extra` attributes), naming its section and what it has seen.
    pub fn form_open(&self, section: &str, action: &str, extra: &str) -> String {
        format!(
            "{}<input type=\"hidden\" name=\"section\" value=\"{section}\"><input type=\"hidden\" name=\"seen\" value=\"{}\">",
            views::ui::post_form_with(
                &format!("{section}-form"),
                "settings-form",
                action,
                "",
                extra
            ),
            self.seen(section)
        )
    }
    fn version(&self) -> String {
        sluice_store::artifacts::fingerprint(&serde_json::to_vec(&serde_json::json!({
            "shared":self.shared,"revision":self.project.settings_rev,"resources":self.resources,"blocker":self.blocker,
            "retired":self.retired.as_ref().map(|r| r.rev),
            "doc":self.doc.rev,"sessions":self.sessions
        })).expect("owned views serialize"))
    }
    /// The fallback field's choices, as (value, words): nobody, each registered session, and
    /// the current fallback kept when it is none of them (the owner, "you").
    pub fn fallback_choices(&self) -> Vec<(String, String)> {
        let mut out = vec![(String::new(), "none".to_owned())];
        out.extend(
            self.sessions
                .iter()
                .map(|s| (s.name.clone(), s.name.clone())),
        );
        if !self.fallback.is_empty() && !out.iter().any(|(v, _)| *v == self.fallback) {
            out.push((
                self.fallback.clone(),
                views::ui::party_words(&self.fallback),
            ));
        }
        out
    }
    pub fn render(&self, viewer: &Viewer) -> Result<TrustedHtml, PublicError> {
        let body = self.body(None).map_err(render_error)?;
        let nav = NavView::new(&self.shared, Some(self.project.project_id), "settings")?;
        views::render_framed(
            &format!("{} · Settings", self.project.name),
            &body,
            &nav,
            viewer,
            &format!("{}/stream", self.path()),
            &self.version(),
            &self.path(),
            &views::Frame {
                head: self.head(),
                ..views::Frame::default()
            },
        )
        .map_err(render_error)
    }
    /// The page's head: its name for a screen reader alone (the bar's switcher names the
    /// project, its tab the page).
    pub fn head(&self) -> TrustedHtml {
        TrustedHtml::owned(format!(
            "<div id=\"settings-band\" class=\"band-wrap\">{}</div>",
            views::ui::lead_head(
                "Settings",
                &TrustedHtml::default(),
                &TrustedHtml::default(),
                &TrustedHtml::default()
            )
        ))
    }
    /// The page's body; `board` the program its box shows instead of the board's (a cleared
    /// board's, so Save board puts it back).
    fn body(&self, board: Option<&str>) -> Result<TrustedHtml, askama::Error> {
        let icon_text = match &self.project.icon {
            Some(projects::ProjectIcon::Text(t)) => t.clone(),
            _ => String::new(),
        };
        let icon_url = self
            .project
            .icon
            .as_ref()
            .and_then(|i| i.url(self.project.project_id))
            .unwrap_or_default();
        TrustedHtml::from_template(&SettingsTemplate {
            view: self,
            icon_text,
            icon_url,
            preview: markdown::render(&self.project.description),
            board_program: board
                .or(self.project.board.as_deref())
                .unwrap_or("")
                .to_owned(),
            retire_hours: self.retire_hours(),
            retire_keep: self.retire_keep(),
        })
    }
    fn batch(&self, viewer: &Viewer) -> Result<RenderedBatch, PublicError> {
        let nav = NavView::new(&self.shared, Some(self.project.project_id), "settings")?;
        let body = self.body(None).map_err(render_error)?;
        // the forms one edits are left out: what is typed stays until it is saved
        let regions = [
            "settings-live",
            "resource-status",
            "board-doc",
            "sessions",
            "retire-status",
            "activity",
            "delete-guard",
        ]
        .into_iter()
        .map(|id| PatchRegion::new(id, TrustedHtml::owned(marked(&body, id).into())));
        Ok(RenderedBatch {
            version: self.version(),
            regions: [
                PatchRegion::new(
                    "top-nav",
                    views::render_nav(&nav, viewer, &self.path()).map_err(render_error)?,
                ),
                PatchRegion::new("settings-band", self.head()),
            ]
            .into_iter()
            .chain(regions)
            .collect(),
        })
    }
}
/// What the page's body holds between `<!--{id}-->` and `<!--/{id}-->`.
fn marked<'a>(body: &'a TrustedHtml, id: &str) -> &'a str {
    let body = body.as_str();
    let start = body.find(&format!("<!--{id}-->")).expect("owned marker") + id.len() + 7;
    let end = body[start..]
        .find(&format!("<!--/{id}-->"))
        .expect("owned closing marker")
        + start;
    &body[start..end]
}
#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate<'a> {
    view: &'a ProjectSettingsView,
    board_program: String,
    icon_text: String,
    icon_url: String,
    preview: TrustedHtml,
    retire_hours: String,
    retire_keep: String,
}
/// A form's fields as sent: url-encoded, or multipart (the Details form, for its image).
struct Sent {
    fields: Vec<(String, String)>,
    /// The image chosen in a file field, when one was.
    file: Option<Vec<u8>>,
}
impl Sent {
    fn decode(headers: &HeaderMap, body: &[u8]) -> Result<Self, PublicError> {
        let kind = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if let Some(boundary) = kind.strip_prefix("multipart/form-data").and_then(|rest| {
            rest.split(';')
                .find_map(|p| p.trim().strip_prefix("boundary="))
        }) {
            let mut sent = Sent {
                fields: Vec::new(),
                file: None,
            };
            for part in multipart(boundary.trim_matches('"'), body)? {
                match part.filename {
                    Some(name) if !name.is_empty() && !part.content.is_empty() => {
                        sent.file = Some(part.content)
                    }
                    Some(_) => {}
                    None => sent.fields.push((
                        part.name,
                        String::from_utf8(part.content).map_err(|_| bad("fields must be UTF-8"))?,
                    )),
                }
            }
            return Ok(sent);
        }
        Ok(Sent {
            fields: url::form_urlencoded::parse(body).into_owned().collect(),
            file: None,
        })
    }
    fn one(&self, name: &str) -> &str {
        self.fields
            .iter()
            .find(|(n, _)| n == name)
            .map_or("", |(_, v)| v.as_str())
    }
    fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.fields
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
    /// A text field as typed: a browser sends a form's line breaks as CRLF.
    fn text(&self, name: &str) -> String {
        self.one(name).replace("\r\n", "\n")
    }
}
struct Part {
    name: String,
    filename: Option<String>,
    content: Vec<u8>,
}
/// A `multipart/form-data` body's parts (RFC 7578), as a browser's form sends them.
fn multipart(boundary: &str, body: &[u8]) -> Result<Vec<Part>, PublicError> {
    let invalid = || bad("invalid multipart form");
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
    let delimiter = format!("--{boundary}").into_bytes();
    let between = [b"\r\n".as_slice(), &delimiter].concat();
    let mut rest = &body[find(body, &delimiter).ok_or_else(invalid)? + delimiter.len()..];
    let mut parts = Vec::new();
    while !rest.starts_with(b"--") {
        rest = rest.strip_prefix(b"\r\n").ok_or_else(invalid)?;
        let head_end = find(rest, b"\r\n\r\n").ok_or_else(invalid)?;
        let head = std::str::from_utf8(&rest[..head_end]).map_err(|_| invalid())?;
        rest = &rest[head_end + 4..];
        let end = find(rest, &between).ok_or_else(invalid)?;
        let content = rest[..end].to_vec();
        rest = &rest[end + between.len()..];
        let disposition = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-disposition:"))
            .ok_or_else(invalid)?;
        let param = |key: &str| {
            disposition.split(';').skip(1).find_map(|p| {
                p.trim()
                    .strip_prefix(key)
                    .and_then(|v| v.strip_prefix('='))
                    .map(|v| v.trim_matches('"').to_owned())
            })
        };
        parts.push(Part {
            name: param("name").ok_or_else(invalid)?,
            filename: param("filename"),
            content,
        });
    }
    Ok(parts)
}
fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn render_error(e: askama::Error) -> PublicError {
    PublicError::Storage {
        message: e.to_string(),
    }
}
/// A resource's capacity as the settings command takes it, from the form's choice: `number` a
/// whole number of 0 or more, `fn` the fn that gives it, `remove` none.
fn capacity_value(kind: &str, number: &str, function: &str) -> Result<String, PublicError> {
    match kind {
        "number" => number
            .trim()
            .parse::<u64>()
            .map(|n| n.to_string())
            .map_err(|_| bad("Enter a whole number of 0 or more.")),
        "fn" if !function.trim().is_empty() => {
            Ok(serde_json::json!({ "capacity_fn": function.trim() }).to_string())
        }
        "fn" => Err(bad("Choose the fn that gives the capacity.")),
        "remove" => Ok("null".into()),
        _ => Err(bad("capacity must be number, fn or remove")),
    }
}
/// Seconds as hours with up to three decimals and no trailing zeros: 21600 → "6".
fn hours(seconds: u64) -> String {
    let text = format!("{:.3}", seconds as f64 / 3600.0);
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}
/// The Retire field's hours ("" or "off" turns retiring off) as `prune_done_after` seconds.
fn retire_seconds(value: &str) -> Result<Option<u64>, PublicError> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("off") {
        return Ok(None);
    }
    let invalid = || {
        bad("Enter a number of hours, such as 6 or 0.5, or leave it empty to turn retiring off.")
    };
    let hours: f64 = value.parse().map_err(|_| invalid())?;
    let seconds = (hours * 3600.0).round();
    if !hours.is_finite() || seconds < 1.0 || seconds > projects::PRUNE_DONE_AFTER_MAX as f64 {
        return Err(invalid());
    }
    Ok(Some(seconds as u64))
}
/// The Keep field's patterns, separated by commas, spaces or new lines.
fn keep_patterns(value: &str) -> Vec<String> {
    value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}
/// What a section's Save asks of the store: every setting the form holds, the store leaving
/// alone what it already says. `remove` is a resource's Remove, `activity` a switch.
fn command(
    section: &str,
    sent: &Sent,
    view: &ProjectSettingsView,
) -> Result<UpdateProject, PublicError> {
    let mut command = UpdateProject {
        expected_settings_rev: Some(view.project.settings_rev),
        author: "owner".into(),
        ..Default::default()
    };
    match section {
        "details" => {
            command.new_name = Some(sent.one("name").trim().parse().map_err(|_| {
                bad("Use lowercase letters, numbers, underscores or hyphens for the name.")
            })?);
            command.description = Some(sent.text("description"));
            command.fallback = Some(Some(sent.one("fallback").trim().to_owned()));
            let text = sent.one("icon").trim();
            let now = match &view.project.icon {
                Some(projects::ProjectIcon::Text(t)) => t.as_str(),
                _ => "",
            };
            command.icon = match &sent.file {
                Some(image) => Some(Icon::image(image.clone()).map_err(|e| e.into_public(false))?),
                None if text != now => Some(Icon::text(text).map_err(|e| e.into_public(false))?),
                None => None,
            };
        }
        "resources" => {
            let mut patch = serde_json::Map::new();
            for resource in sent.all("resource") {
                let value = capacity_value(
                    sent.one(&format!("capacity.{resource}")),
                    sent.one(&format!("number.{resource}")),
                    sent.one(&format!("capacity_fn.{resource}")),
                )
                .map_err(|e| bad(format!("{resource}: {e}")))?;
                if view
                    .resources
                    .iter()
                    .any(|r| r.name == resource && r.value != value)
                {
                    patch.insert(
                        resource.to_owned(),
                        serde_json::from_str(&value).map_err(|_| bad("invalid capacity"))?,
                    );
                }
            }
            let new = sent.one("new").trim();
            if !new.is_empty() {
                let value = capacity_value(
                    sent.one("capacity.:new"),
                    sent.one("number.:new"),
                    sent.one("capacity_fn.:new"),
                )?;
                patch.insert(
                    new.to_owned(),
                    serde_json::from_str(&value).map_err(|_| bad("invalid capacity"))?,
                );
            }
            if patch.is_empty() {
                return Ok(command);
            }
            command.resources = Some(patch.into());
        }
        "remove" => {
            command.resources = Some(serde_json::json!({ sent.one("resource"): null }));
        }
        "retiring" => {
            command.prune_done_after = Some(retire_seconds(sent.one("prune_done_after"))?);
            command.prune_keep = Some(keep_patterns(sent.one("prune_keep")));
        }
        "activity" => {
            let switch = |name: &str| match sent.one(name) {
                "" => Ok(None),
                "true" => Ok(Some(true)),
                "false" => Ok(Some(false)),
                _ => Err(bad("a switch is true or false")),
            };
            command.paused = switch("paused")?;
            command.archived = switch("archived")?;
        }
        _ => return Err(bad("unknown settings section")),
    }
    Ok(command)
}
/// A setting as the page names it, in a sentence.
fn setting_words(field: &str) -> &'static str {
    match field {
        "name" => "the name",
        "description" => "the description",
        "icon" => "the icon",
        "fallback" => "the fallback",
        "resources" => "the resources",
        "prune_done_after" => "when done units retire",
        "prune_keep" => "the units never retired",
        _ => "a setting",
    }
}
/// The answer to a section's write the store took: the section drawn anew from what it now
/// says (its form, `<section>-edit`, or the switches), with `words` in its notice; a
/// confirmation it came from closes. A client that is not a page's form gets 204.
async fn saved(
    state: &SettingsState,
    id: ProjectId,
    headers: &HeaderMap,
    section: &str,
    board: Option<&str>,
    words: &str,
) -> Response {
    if views::write::form(headers).is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let region = match section {
        "activity" => "activity",
        "remove" => "edit:resources",
        "board" => "edit:board",
        "details" => "edit:details",
        "resources" => "edit:resources",
        _ => "edit:retiring",
    };
    let form = match section {
        "activity" => "activity-form",
        "remove" | "resources" => "resources-form",
        "board" => "board-program-form",
        "details" => "details-form",
        _ => "retiring-form",
    };
    let body = match state
        .snapshot(id)
        .await
        .and_then(|v| v.body(board).map_err(render_error))
    {
        Ok(body) => body,
        Err(e) => return views::write::failed(headers, e),
    };
    // the confirmation closes once the answer is in (`sluice-dialog`), its form back in place
    // unless the section's patch drew a new one
    views::write::events([
        StreamEvent::Elements(PatchElements::new(marked(&body, region))),
        views::write::status(form, words),
    ])
}
/// The answer to a write the store refused: what the form says, as an alert.
fn refused(headers: &HeaderMap, error: PublicError) -> Response {
    match &error {
        // the store's revision check: a setting changed between the page's look and the write
        PublicError::Conflict { message, .. } if message == "project settings changed" => {
            views::write::refused(
                headers,
                "Not saved: a setting changed just now. Save again.",
                error,
            )
        }
        _ => views::write::failed(headers, error),
    }
}
/// Merge with dashboard_router under the host's Host/Origin guard. This module
/// neither starts a coordinator nor executes cleanup jobs; the host owns those.
pub fn router(state: SettingsState) -> Router {
    Router::new()
        .route("/projects/id/{id}/settings", get(page).post(change))
        .route("/projects/{name}/settings", get(named_page))
        .route("/projects/id/{id}/settings/preview", post(preview))
        .route("/projects/id/{id}/settings/board", post(board_change))
        .route(
            "/projects/id/{id}/settings/board/preview",
            post(board_preview),
        )
        .route("/projects/id/{id}/settings/delete", post(delete))
        .route("/projects/id/{id}/settings/stream", get(settings_stream))
        .route("/projects/id/{id}/icon", get(image))
        .layer(DefaultBodyLimit::max(512 * 1024))
        .with_state(state)
}
async fn page(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    headers: HeaderMap,
) -> Response {
    match state
        .snapshot(id)
        .await
        .and_then(|v| v.render(&Viewer::from_headers(&headers)))
    {
        Ok(html) => Html(html.to_string()).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
async fn named_page(State(state): State<SettingsState>, Path(name): Path<String>) -> Response {
    let Ok(name) = name.parse() else {
        return crate::http::error_response(PublicError::NotFound {
            message: "project not found".into(),
        });
    };
    match state
        .dashboard
        .reads
        .clock_snapshot(move |c| projects::resolve(c, &ProjectSelector::Name(name)))
        .await
    {
        Ok(p) => {
            Redirect::permanent(&format!("/projects/id/{}/settings", p.project_id)).into_response()
        }
        Err(e) => crate::http::error_response(e.into_public(true)),
    }
}
/// A section's Save (`section`: details, resources, retiring), a resource's Remove or an
/// Activity switch. A Save whose section someone else changed since its form was drawn (`seen`)
/// is refused: it would write over what they did.
async fn change(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let sent = match Sent::decode(&headers, &body) {
        Ok(sent) => sent,
        Err(e) => return views::write::failed(&headers, e),
    };
    let view = match state.snapshot(id).await {
        Ok(view) => view,
        Err(e) => return views::write::failed(&headers, e),
    };
    let section = sent.one("section");
    if let Some((_, fields)) = SECTIONS.iter().find(|(s, _)| *s == section) {
        let seen: i64 = sent.one("seen").parse().unwrap_or(0);
        if let Some((field, (_, by))) = fields
            .iter()
            .filter_map(|f| view.changes.get(f).map(|c| (*f, c)))
            .find(|(_, (seq, _))| *seq > seen)
        {
            let by = match by.as_str() {
                "" => "someone".to_owned(),
                "owner" => "you, elsewhere".to_owned(),
                by => by.to_owned(),
            };
            return views::write::refused(
                &headers,
                &format!(
                    "Not saved: {} changed since this page was drawn (by {by}). Reload to see it, then save again.",
                    setting_words(field)
                ),
                PublicError::Conflict {
                    message: "project settings changed".into(),
                    current_rev: Some(view.project.settings_rev),
                },
            );
        }
    }
    let command = match command(section, &sent, &view) {
        Ok(command) => command,
        Err(e) => return views::write::failed(&headers, e),
    };
    match state.commands.update(id, command).await {
        Ok(_) => saved(&state, id, &headers, section, None, "Saved.").await,
        Err(e) => refused(&headers, e),
    }
}
/// The Board section's Save (`op=save`, `program`) and Clear (`op=clear`, from its
/// confirmation), each fenced by `expected_rev`; answered as the other sections are.
async fn board_change(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let mut fields = BTreeMap::new();
    for (key, value) in url::form_urlencoded::parse(&body) {
        fields.insert(key.into_owned(), value.into_owned());
    }
    let program = fields
        .remove("program")
        .unwrap_or_default()
        .replace("\r\n", "\n");
    let request = (|| {
        let expected_rev = fields
            .get("expected_rev")
            .ok_or_else(|| bad("board revision required"))?
            .parse()
            .map(Revision)
            .map_err(|_| bad("invalid board revision"))?;
        let program = match fields.get("op").map(String::as_str) {
            Some("clear") => None,
            Some("save") if program.trim().is_empty() => {
                return Err(bad(
                    "Write a program, or use Clear board to remove the board.",
                ));
            }
            Some("save") => Some(program.clone()),
            _ => return Err(bad("op must be save or clear")),
        };
        Ok(projects::SetBoard {
            program,
            expected_rev: Some(expected_rev),
            reason: Some("set in project settings".into()),
            author: "owner".into(),
        })
    })();
    let clear = request.as_ref().is_ok_and(|r| r.program.is_none());
    let result = match request {
        Ok(request) => state.commands.board(id, request).await,
        Err(e) => Err(e),
    };
    match result {
        Ok(outcome) if clear => {
            // the box keeps the cleared program, so Save board puts it back
            let words = format!("Cleared (rev {}). Save board restores it.", outcome.rev);
            saved(&state, id, &headers, "board", Some(&program), &words).await
        }
        Ok(outcome) => {
            let words = std::iter::once(format!("Saved (rev {}).", outcome.rev))
                .chain(outcome.warnings.iter().map(|w| format!("Warning: {w}")))
                .collect::<Vec<_>>()
                .join("\n");
            saved(&state, id, &headers, "board", None, &words).await
        }
        Err(PublicError::Invalid { message, errors }) => {
            let words = std::iter::once(format!("Not saved: {message}"))
                .chain(errors.iter().cloned())
                .collect::<Vec<_>>()
                .join("\n");
            views::write::refused(&headers, &words, PublicError::Invalid { message, errors })
        }
        Err(e) => views::write::failed(&headers, e),
    }
}
/// The Board section's live preview: the board form's draft (`program`) drawn with the
/// project's data, or its problems, patched into `#board-preview` as it is typed.
async fn board_preview(
    State(state): State<SettingsState>,
    registry: Option<axum::Extension<views::board::Registry>>,
    Path(id): Path<ProjectId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let program = match Sent::decode(&headers, &body) {
        Ok(sent) => sent.text("program"),
        Err(e) => return crate::http::error_response(e),
    };
    let html = if program.trim().is_empty() {
        "<p class=\"setting-help\">No board.</p>".to_owned()
    } else {
        let registry = registry.map(|r| r.0);
        let result = async {
            let (_, view) = views::board::snapshot(&state.dashboard, id, registry.as_ref()).await?;
            views::panel::load(
                &state.dashboard,
                id,
                registry.as_ref(),
                &view,
                Some(program),
                None,
            )
            .await
        }
        .await;
        match result {
            Ok(Some(panel)) => format!("{}{}", panel.head(false).as_str(), panel.html),
            Ok(None) => String::new(),
            Err(e) => format!(
                "<p class=\"setting-error\">{}</p>",
                views::ui::esc(&e.to_string())
            ),
        }
    };
    inner("#board-preview", html)
}
/// The description as it reads: the Details form's `description`, patched into
/// `#description-preview` as it is typed.
async fn preview(Path(_id): Path<ProjectId>, headers: HeaderMap, body: Bytes) -> Response {
    match Sent::decode(&headers, &body) {
        Ok(sent) => inner(
            "#description-preview",
            markdown::render(&sent.text("description")).to_string(),
        ),
        Err(e) => crate::http::error_response(e),
    }
}
/// A Datastar answer that draws `html` inside the element `selector` names.
fn inner(selector: &str, html: String) -> Response {
    views::write::events([StreamEvent::Elements(
        PatchElements::new(html)
            .selector(selector)
            .mode(ElementPatchMode::Inner),
    )])
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    expected_settings_rev: Revision,
}
async fn delete(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let confirmation: Result<Confirmation, _> = if headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/json"))
    {
        serde_json::from_slice(&body)
    } else {
        let fields: BTreeMap<_, _> = url::form_urlencoded::parse(&body)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        serde_json::from_value(
            serde_json::json!({"expected_settings_rev":fields.get("expected_settings_rev").and_then(|s| s.parse::<u64>().ok())}),
        )
    };
    let confirmation = match confirmation {
        Ok(c) => c,
        Err(_) => return views::write::failed(&headers, bad("settings revision required")),
    };
    let view = match state.snapshot(id).await {
        Ok(view) => view,
        Err(e) => return views::write::failed(&headers, e),
    };
    if view.project.settings_rev != confirmation.expected_settings_rev {
        return views::write::failed(
            &headers,
            PublicError::Conflict {
                message: "project settings changed".into(),
                current_rev: Some(view.project.settings_rev),
            },
        );
    }
    if let Err(e) = state.deletion_guard.check(id) {
        return views::write::failed(&headers, e.into_public(true));
    }
    if let Some(reason) = view.blocker {
        return views::write::failed(&headers, bad(reason));
    }
    let project = if view.project.archived {
        view.project
    } else {
        match state
            .commands
            .update(
                id,
                UpdateProject {
                    archived: Some(true),
                    expected_settings_rev: Some(confirmation.expected_settings_rev),
                    author: "owner".into(),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(project) => project,
            Err(e) => return views::write::failed(&headers, e),
        }
    };
    match state
        .commands
        .delete(
            id,
            projects::DeleteProject {
                confirm_name: project.name.to_string(),
                expected_settings_rev: project.settings_rev,
                author: "owner".into(),
            },
        )
        .await
    {
        // the project is gone: its page goes home
        Ok(_) if views::write::form(&headers).is_some() => {
            views::write::events([StreamEvent::Script(ExecuteScript::new(
                "window.location.assign('/')",
            ))])
        }
        Ok(_) => Redirect::to("/").into_response(),
        Err(e) => views::write::failed(&headers, e),
    }
}
#[derive(Deserialize)]
struct ImageQuery {
    generation: i64,
}
async fn image(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    Query(query): Query<ImageQuery>,
) -> Response {
    match state
        .dashboard
        .reads
        .clock_snapshot(move |c| {
            projects::icon_image(c, &ProjectSelector::Id(id), query.generation)
        })
        .await
    {
        Ok((ty, bytes, hash)) => (
            [
                (header::CONTENT_TYPE, ty),
                (header::ETAG, format!("\"{hash}\"")),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable".into(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => crate::http::error_response(e.into_public(true)),
    }
}
async fn settings_stream(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = state.snapshot(id).await {
        return crate::http::error_response(e);
    }
    let viewer = Viewer::from_headers(&headers);
    let stop = state.dashboard.stop.clone();
    let watch = state.dashboard.watch(Some(id));
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        async move {
            match state.snapshot(id).await {
                Ok(v) => v.batch(&viewer),
                // deleted: its page goes home
                Err(PublicError::NotFound {..}) => Ok(RenderedBatch { version:"deleted".into(), regions:vec![PatchRegion::new("settings-live", TrustedHtml::owned("<header id=\"settings-live\" data-init=\"window.location.assign('/')\">Project deleted.</header>".into()))] }),
                Err(e) => Err(e),
            }
        }
    };
    Sse::new(streams::page_events(
        watch,
        loader,
        query.version(VersionSignal::Page),
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::default())
    .into_response()
}
/// Two names the same: for the template, whose values arrive as `String` and `&str` alike.
pub fn same(a: &str, b: &str) -> bool {
    a == b
}
