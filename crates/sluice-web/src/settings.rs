//! Project settings commands and id-scoped views. The host supplies its resource
//! adapter and coordinator deletion guard, and mounts this router inside its
//! Host/Origin policy. All mutations use the store's single writer transaction.
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
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, ProjectSelector, Revision},
};
use sluice_store::{
    RetrySafety, Writer,
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
    /// Set or clear the project's board (`board_set`); returns its revision.
    fn board(
        &self,
        id: ProjectId,
        request: projects::SetBoard,
    ) -> BoxFuture<'_, Result<Revision, PublicError>>;
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
    ) -> BoxFuture<'_, Result<Revision, PublicError>> {
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
        self.dashboard
            .reads
            .snapshot(move |c| {
                let shared = views::load_snapshot(c, functions)?;
                let project = projects::resolve(c, &ProjectSelector::Id(id))?;
                let blocker =
                    projects::deletion_blocker(c, &ProjectSelector::Id(id))?.or_else(|| {
                        guard
                            .check(id)
                            .err()
                            .map(|e| e.into_public(true).to_string())
                    });
                let declarations = resources::declarations(c, id)?;
                let held = resources::held(c, id)?;
                let leases = resources::leases(c, id)?;
                let doc: String = c.query_row(
                    "SELECT doc FROM plans WHERE project_id=?1",
                    [id.to_string()],
                    |r| r.get(0),
                )?;
                let raw = serde_json::from_str(&doc)?;
                let queued =
                    sluice_model::plan::Plan::parse(&raw, &CatalogSignatures(&shared.functions))
                        .ok()
                        .map(|plan| resources::status(c, id, &plan))
                        .transpose()?;
                let rows = declarations
                    .into_values()
                    .map(|r| ResourceView {
                        name: r.name.clone(),
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
                            .and_then(|rows| rows.get(&r.name))
                            .map(|r| r.queued),
                        error: r.error.map(|e| e.to_string()).unwrap_or_default(),
                    })
                    .collect();
                Ok(ProjectSettingsView {
                    shared,
                    project,
                    resources: rows,
                    blocker,
                })
            })
            .await
            .map_err(|e| e.into_public(true))
    }
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
    pub capacity: String,
    pub dynamic: bool,
    pub held: u64,
    pub waiting: usize,
    pub queued: Option<usize>,
    pub error: String,
}
pub struct ProjectSettingsView {
    pub shared: DashboardSnapshot,
    pub project: Project,
    pub resources: Vec<ResourceView>,
    pub blocker: Option<String>,
}
impl ProjectSettingsView {
    pub fn path(&self) -> String {
        format!("/projects/id/{}/settings", self.project.project_id)
    }
    fn version(&self) -> String {
        sluice_store::artifacts::fingerprint(&serde_json::to_vec(&serde_json::json!({
            "shared":self.shared,"revision":self.project.settings_rev,"resources":self.resources,"blocker":self.blocker
        })).expect("owned views serialize"))
    }
    pub fn render(&self, viewer: &Viewer, feedback: &Feedback) -> Result<TrustedHtml, PublicError> {
        let body = self.body(feedback).map_err(render_error)?;
        let nav = NavView::new(&self.shared, Some(self.project.project_id), "settings")?;
        views::render_layout(
            &format!("{} · Settings", self.project.name),
            &body,
            &nav,
            viewer,
            &format!("{}/stream", self.path()),
            &self.version(),
            &self.path(),
        )
        .map_err(render_error)
    }
    fn body(&self, feedback: &Feedback) -> Result<TrustedHtml, askama::Error> {
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
        let description = feedback.value("description", &self.project.description);
        TrustedHtml::from_template(&SettingsTemplate {
            view: self,
            feedback,
            icon_text,
            icon_url,
            preview: markdown::render(description),
            css: views::asset_url("settings.css"),
        })
    }
    fn batch(&self, viewer: &Viewer) -> Result<RenderedBatch, PublicError> {
        let nav = NavView::new(&self.shared, Some(self.project.project_id), "settings")?;
        let body = self.body(&Feedback::default()).map_err(render_error)?;
        let regions = ["settings-live", "resource-status", "delete-guard"]
            .into_iter()
            .map(|id| {
                // These owned template markers keep all editable forms out of stream patches.
                let start = body
                    .as_str()
                    .find(&format!("<!--{id}-->"))
                    .expect("owned marker")
                    + id.len()
                    + 7;
                let end = body.as_str()[start..]
                    .find(&format!("<!--/{id}-->"))
                    .expect("owned closing marker")
                    + start;
                PatchRegion::new(id, TrustedHtml::owned(body.as_str()[start..end].into()))
            });
        Ok(RenderedBatch {
            version: self.version(),
            regions: std::iter::once(PatchRegion::new(
                "top-nav",
                views::render_nav(&nav, viewer, &self.path()).map_err(render_error)?,
            ))
            .chain(regions)
            .collect(),
        })
    }
}
#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate<'a> {
    view: &'a ProjectSettingsView,
    feedback: &'a Feedback,
    icon_text: String,
    icon_url: String,
    preview: TrustedHtml,
    css: String,
}
#[derive(Default)]
pub struct Feedback {
    pub field: String,
    pub value: String,
    pub resource: String,
    pub message: String,
    pub saved: bool,
}
impl Feedback {
    pub fn value<'a>(&'a self, field: &str, fallback: &'a str) -> &'a str {
        if self.field == field && !self.saved {
            &self.value
        } else {
            fallback
        }
    }
    pub fn error(&self, field: &str) -> bool {
        self.field == field && !self.saved && !self.message.is_empty()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldChange {
    pub field: String,
    pub value: String,
    #[serde(default)]
    pub resource: String,
    pub expected_settings_rev: Revision,
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
fn status(e: &PublicError) -> StatusCode {
    match e {
        PublicError::NotFound { .. } => StatusCode::NOT_FOUND,
        PublicError::Conflict { .. } => StatusCode::CONFLICT,
        PublicError::Invalid { .. } | PublicError::BadRequest { .. } => StatusCode::BAD_REQUEST,
        PublicError::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
fn error_response(e: PublicError) -> Response {
    (status(&e), axum::Json(e)).into_response()
}
fn decode(headers: &HeaderMap, body: &[u8]) -> Result<FieldChange, PublicError> {
    if headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|h| h.as_bytes().starts_with(b"application/json"))
    {
        serde_json::from_slice(body).map_err(|_| bad("invalid settings command"))
    } else {
        let mut fields = BTreeMap::new();
        for (key, value) in url::form_urlencoded::parse(body) {
            if fields
                .insert(key.into_owned(), value.into_owned())
                .is_some()
            {
                return Err(bad("duplicate settings field"));
            }
        }
        let rev = fields
            .remove("expected_settings_rev")
            .ok_or_else(|| bad("settings revision required"))?
            .parse()
            .map_err(|_| bad("invalid settings revision"))?;
        let request = FieldChange {
            field: fields
                .remove("field")
                .ok_or_else(|| bad("field required"))?,
            value: fields.remove("value").unwrap_or_default(),
            resource: fields.remove("resource").unwrap_or_default(),
            expected_settings_rev: Revision(rev),
        };
        if !fields.is_empty() {
            return Err(bad("unknown settings field"));
        }
        Ok(request)
    }
}
fn update(request: &FieldChange) -> Result<UpdateProject, PublicError> {
    let mut command = UpdateProject {
        expected_settings_rev: Some(request.expected_settings_rev),
        author: "owner".into(),
        ..Default::default()
    };
    match request.field.as_str() {
        "name" => {
            command.new_name = Some(request.value.parse().map_err(|_| {
                bad("Use lowercase letters, numbers, underscores or hyphens for the name.")
            })?)
        }
        "description" => command.description = Some(request.value.clone()),
        "icon" => {
            command.icon = Some(Icon::text(&request.value).map_err(|e| e.into_public(false))?)
        }
        "resources" => {
            let value: serde_json::Value = serde_json::from_str(&request.value).map_err(|_| {
                bad("Enter a nonnegative capacity, {\"capacity_fn\":\"fn\"}, or null to remove.")
            })?;
            command.resources = Some(serde_json::json!({ request.resource.clone():value }));
        }
        "paused" | "archived" => {
            let value = match request.value.as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(bad("switch value must be true or false")),
            };
            if request.field == "paused" {
                command.paused = Some(value);
            } else {
                command.archived = Some(value);
            }
        }
        _ => return Err(bad("unknown project setting")),
    }
    Ok(command)
}
/// One field per command. The store performs revision and uniqueness checks in
/// the same transaction as authored records and resource validation.
pub async fn apply(
    state: &SettingsState,
    id: ProjectId,
    request: &FieldChange,
) -> Result<Project, PublicError> {
    let command = update(request)?;
    state.commands.update(id, command).await
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
        .route("/projects/id/{id}/settings/icon", post(upload))
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
        .and_then(|v| v.render(&Viewer::from_headers(&headers), &Feedback::default()))
    {
        Ok(html) => Html(html.to_string()).into_response(),
        Err(e) => error_response(e),
    }
}
async fn named_page(State(state): State<SettingsState>, Path(name): Path<String>) -> Response {
    let Ok(name) = name.parse() else {
        return error_response(PublicError::NotFound {
            message: "project not found".into(),
        });
    };
    match state
        .dashboard
        .reads
        .snapshot(move |c| projects::resolve(c, &ProjectSelector::Name(name)))
        .await
    {
        Ok(p) => {
            Redirect::permanent(&format!("/projects/id/{}/settings", p.project_id)).into_response()
        }
        Err(e) => error_response(e.into_public(true)),
    }
}
async fn change(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = match decode(&headers, &body) {
        Ok(r) => r,
        Err(e) => return error_response(e),
    };
    let result = apply(&state, id, &request).await;
    let status = result.as_ref().err().map(status).unwrap_or(StatusCode::OK);
    let feedback = Feedback {
        field: request.field,
        value: request.value,
        resource: request.resource,
        saved: result.is_ok(),
        message: result
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "Saved".into()),
    };
    match state
        .snapshot(id)
        .await
        .and_then(|v| v.render(&Viewer::from_headers(&headers), &feedback))
    {
        Ok(html) => (status, Html(html.to_string())).into_response(),
        Err(e) => error_response(e),
    }
}
/// The Board section's Save (`op=save`, `program`) and Clear (`op=clear`), each fenced by
/// `expected_rev`; the page comes back with the outcome, as the other settings do.
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
    let program = fields.remove("program").unwrap_or_default();
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
            Some("save") => Some(program.replace("\r\n", "\n")),
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
    let status = result.as_ref().err().map(status).unwrap_or(StatusCode::OK);
    let feedback = Feedback {
        field: "board".into(),
        value: program,
        resource: String::new(),
        saved: result.is_ok(),
        message: match &result {
            Ok(rev) if clear => format!("Cleared (rev {rev})"),
            Ok(rev) => format!("Saved (rev {rev})"),
            Err(PublicError::Invalid { message, errors }) => std::iter::once(message.clone())
                .chain(errors.iter().cloned())
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => e.to_string(),
        },
    };
    match state
        .snapshot(id)
        .await
        .and_then(|v| v.render(&Viewer::from_headers(&headers), &feedback))
    {
        Ok(html) => (status, Html(html.to_string())).into_response(),
        Err(e) => error_response(e),
    }
}
/// The Board section's live preview: the draft drawn with the project's data, or its problems.
async fn board_preview(
    State(state): State<SettingsState>,
    registry: Option<axum::Extension<views::board::Registry>>,
    Path(id): Path<ProjectId>,
    body: Bytes,
) -> Response {
    let Ok(program) = std::str::from_utf8(&body) else {
        return error_response(bad("the program must be UTF-8"));
    };
    if program.trim().is_empty() {
        return Html(
            "<p class=\"setting-help\">No board: the plan keeps the whole page.</p>".to_owned(),
        )
        .into_response();
    }
    let registry = registry.map(|r| r.0);
    let result = async {
        let (_, view) = views::board::snapshot(&state.dashboard, id, registry.as_ref())
            .await?
            .ok_or_else(|| PublicError::Busy {
                message: "the dashboard is refreshing; try again".into(),
                retryable: true,
            })?;
        views::panel::load(
            &state.dashboard,
            id,
            registry.as_ref(),
            &view,
            Some(program.to_owned()),
            None,
        )
        .await
    }
    .await;
    match result {
        Ok(Some(panel)) => Html(panel.html).into_response(),
        Ok(None) => Html(String::new()).into_response(),
        Err(e) => error_response(e),
    }
}
async fn preview(Path(_id): Path<ProjectId>, body: Bytes) -> Response {
    match std::str::from_utf8(&body) {
        Ok(text) => Html(markdown::render(text).to_string()).into_response(),
        Err(_) => error_response(bad("description must be UTF-8")),
    }
}
#[derive(Deserialize)]
struct RevisionQuery {
    expected_settings_rev: Revision,
}
async fn upload(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    Query(query): Query<RevisionQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let icon = match Icon::image(body.to_vec()) {
        Ok(i) => i,
        Err(e) => return error_response(e.into_public(false)),
    };
    match state
        .commands
        .update(
            id,
            UpdateProject {
                icon: Some(icon),
                expected_settings_rev: Some(query.expected_settings_rev),
                author: "owner".into(),
                ..Default::default()
            },
        )
        .await
    {
        Ok(_) => page(State(state), Path(id), headers).await,
        Err(e) => error_response(e),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    confirm_name: String,
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
            serde_json::json!({"confirm_name":fields.get("confirm_name"),"expected_settings_rev":fields.get("expected_settings_rev").and_then(|s| s.parse::<u64>().ok())}),
        )
    };
    let confirmation = match confirmation {
        Ok(c) => c,
        Err(_) => return error_response(bad("exact name and settings revision required")),
    };
    match state
        .commands
        .delete(
            id,
            projects::DeleteProject {
                confirm_name: confirmation.confirm_name,
                expected_settings_rev: confirmation.expected_settings_rev,
                author: "owner".into(),
            },
        )
        .await
    {
        Ok(_) => Redirect::to("/").into_response(),
        Err(e) => error_response(e),
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
        .snapshot(move |c| projects::icon_image(c, &ProjectSelector::Id(id), query.generation))
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
        Err(e) => error_response(e.into_public(true)),
    }
}
async fn settings_stream(
    State(state): State<SettingsState>,
    Path(id): Path<ProjectId>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(e) = state.snapshot(id).await {
        return error_response(e);
    }
    let viewer = Viewer::from_headers(&headers);
    let stop = state.dashboard.stop.clone();
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        async move {
            match state.snapshot(id).await {
                Ok(v) => v.batch(&viewer).map(Some),
                Err(PublicError::NotFound {..}) => Ok(Some(RenderedBatch { version:"deleted".into(), regions:vec![PatchRegion::new("settings-live", TrustedHtml::owned("<header id=\"settings-live\" data-deleted=\"true\">Project deleted.</header>".into()))] })),
                Err(e) => Err(e),
            }
        }
    };
    Sse::new(streams::page_events(
        loader,
        query.version(VersionSignal::Page),
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::default())
    .into_response()
}
