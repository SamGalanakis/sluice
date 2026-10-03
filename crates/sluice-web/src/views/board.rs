//! The v2 board. Relations retain their gate form and unit endpoints.
use super::step::{FieldView, StepView};
use super::{DashboardSnapshot, DashboardState, FunctionCatalog, NavView, TrustedHtml, Viewer};
use crate::streams::{self, PatchRegion, RenderedBatch, StreamQuery, VersionSignal};
use askama::Template;
use axum::{
    Extension,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response, Sse, sse::KeepAlive},
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    gates::{Gate, GateDecision, StateSnapshot, evaluate_step},
    ids::{ProjectId, StepId, UnitName},
    plan::{FnSignature, Plan, SignatureProvider},
    types::Type,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Endpoint {
    Step(StepId),
    Unit(UnitName),
    Input(String),
    Output(String),
}
impl Endpoint {
    pub fn key(&self) -> String {
        match self {
            Self::Step(id) => format!("s:{id}"),
            Self::Unit(id) => format!("u:{id}"),
            Self::Input(id) => format!("i:{id}"),
            Self::Output(id) => format!("o:{id}"),
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Handoff,
    Ordering,
    Condition,
    NegatedCondition,
    Unit,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Relation {
    pub from: Endpoint,
    pub to: Endpoint,
    pub kind: RelationKind,
    pub label: String,
    pub tolerant: bool,
}
pub fn relations(plan: &Plan) -> Vec<Relation> {
    let mut out = vec![];
    let endpoint = |reference: &sluice_model::gates::ValueRef| {
        let parts = reference.parts().expect("compiled reference");
        let label = std::iter::once(parts.name.clone())
            .chain(parts.fields)
            .collect::<Vec<_>>()
            .join(".");
        (
            parts
                .step
                .map(Endpoint::Step)
                .unwrap_or(Endpoint::Input(parts.name)),
            label,
        )
    };
    for (id, step) in plan.steps() {
        let to = Endpoint::Step(id.clone());
        for (input, binding) in &step.bindings {
            for reference in binding.references() {
                let (from, label) = endpoint(reference);
                out.push(Relation {
                    from,
                    to: to.clone(),
                    kind: RelationKind::Handoff,
                    label: format!("{label} → {input}"),
                    tolerant: false,
                });
            }
        }
        for gate in &step.after {
            let (from, kind, label, tolerant) = match gate {
                Gate::Step { id, accept_skip } => (
                    Endpoint::Step(id.clone()),
                    RelationKind::Ordering,
                    if *accept_skip { "?" } else { "after" }.into(),
                    *accept_skip,
                ),
                Gate::Unit { name, accept_skip } => (
                    Endpoint::Unit(name.clone()),
                    RelationKind::Unit,
                    format!("unit:{name}{}", if *accept_skip { "?" } else { "" }),
                    *accept_skip,
                ),
                Gate::Bool { reference, negate } => {
                    let (from, label) = endpoint(reference);
                    (
                        from,
                        if *negate {
                            RelationKind::NegatedCondition
                        } else {
                            RelationKind::Condition
                        },
                        format!("{}{label}", if *negate { "not " } else { "" }),
                        false,
                    )
                }
            };
            out.push(Relation {
                from,
                to: to.clone(),
                kind,
                label,
                tolerant,
            });
        }
    }
    for (name, reference) in plan.outputs() {
        let (from, label) = endpoint(reference);
        out.push(Relation {
            from,
            to: Endpoint::Output(name.clone()),
            kind: RelationKind::Handoff,
            label,
            tolerant: false,
        });
    }
    out
}
#[derive(Clone, Debug, Serialize)]
pub struct UnitView {
    pub id: UnitName,
    pub tagged: bool,
    pub done: bool,
    pub settled: bool,
    pub steps: Vec<StepView>,
    pub rows: Vec<Vec<StepView>>,
    pub blocked: Vec<String>,
    pub last_message: String,
    pub changed: String,
}
impl UnitView {
    pub fn key(&self) -> String {
        format!("u:{}", self.id)
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&UnitTemplate { unit: self })
    }
    pub fn page_body(&self) -> Result<TrustedHtml, askama::Error> {
        #[derive(Template)]
        #[template(
            source = "<div id=\"unit-detail\" class=\"board\"><div class=\"boxes boxed\">{{ unit.body()?|safe }}</div>{% if !unit.blocked.is_empty() %}<dl class=\"facts\"><div><dt>Waits on</dt><dd>{% for wait in unit.blocked %}<p>{{ wait }}</p>{% endfor %}</dd></div></dl>{% endif %}{% if !unit.last_message.is_empty() %}<section class=\"d-sec\"><h2>Last message</h2><p>{{ unit.last_message }}</p><p class=\"meta\">{{ unit.changed }}</p></section>{% endif %}</div>",
            ext = "html"
        )]
        struct UnitPage<'a> {
            unit: &'a UnitView,
        }
        TrustedHtml::from_template(&UnitPage { unit: self })
    }
    pub fn rank(&self) -> u8 {
        if self
            .steps
            .iter()
            .any(|s| matches!(s.status.as_str(), "failed" | "stale"))
        {
            0
        } else if self
            .steps
            .iter()
            .any(|s| s.status == "running" || s.mark == "external")
        {
            1
        } else if self.steps.iter().any(|s| s.ready && !s.paused) {
            2
        } else if self.done {
            4
        } else {
            3
        }
    }
}
#[derive(Template)]
#[template(path = "unit.html")]
struct UnitTemplate<'a> {
    unit: &'a UnitView,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProjectView {
    pub project: super::ProjectView,
    pub units: Vec<UnitView>,
    pub relations: Vec<Relation>,
    pub inputs: Vec<FieldView>,
    pub outputs: Vec<FieldView>,
    pub revision: u64,
    pub query: String,
    pub order: String,
    pub show: String,
}
impl ProjectView {
    pub fn new(
        project: super::ProjectView,
        plan: &Plan,
        state: &StateSnapshot,
        revision: u64,
    ) -> Self {
        let mut depth = BTreeMap::new();
        let mut blocked = BTreeSet::new();
        for id in plan.topological_order() {
            let d = plan
                .dependencies(id)
                .iter()
                .filter_map(|d| depth.get(d))
                .max()
                .copied()
                .map_or(0, |d: usize| d + 1);
            depth.insert(id.clone(), d);
            if matches!(state.status(id), StepStatus::Failed | StepStatus::Stale)
                || (state.status(id) == StepStatus::Pending
                    && plan.dependencies(id).iter().any(|d| blocked.contains(d)))
            {
                blocked.insert(id.clone());
            }
        }
        let units = plan
            .units()
            .values()
            .map(|unit| {
                let steps: Vec<_> = unit
                    .steps
                    .iter()
                    .map(|id| {
                        let mut view = StepView::new(project.id, plan, state, id);
                        view.blocked = view.status == "pending"
                            && view.mark != "paused"
                            && blocked.contains(id);
                        view
                    })
                    .collect();
                let mut rows = BTreeMap::<usize, Vec<StepView>>::new();
                for step in &steps {
                    rows.entry(depth[&step.id]).or_default().push(step.clone());
                }
                UnitView {
                    id: unit.name.clone(),
                    tagged: unit.tagged,
                    done: unit.done(state),
                    settled: unit.settled(plan, state),
                    blocked: steps.iter().flat_map(|s| s.waits.clone()).collect(),
                    steps,
                    rows: rows.into_values().collect(),
                    last_message: String::new(),
                    changed: project.changed.clone(),
                }
            })
            .collect();
        Self {
            inputs: plan
                .inputs()
                .iter()
                .map(|(n, d)| {
                    FieldView::new(
                        n,
                        &d.ty.to_string(),
                        d.doc.as_deref().unwrap_or(""),
                        state.inputs.0.get(n).map(|v| v.as_value()),
                        "Plan input",
                    )
                })
                .collect(),
            outputs: plan
                .outputs()
                .iter()
                .map(|(n, r)| {
                    FieldView::reference(
                        n,
                        &plan
                            .reference_type(r)
                            .map(|t| t.to_string())
                            .unwrap_or_default(),
                        r,
                        plan,
                        state,
                    )
                })
                .collect(),
            project,
            units,
            relations: relations(plan),
            revision,
            query: String::new(),
            order: "live".into(),
            show: "all".into(),
        }
    }
    pub fn href(&self) -> String {
        self.project.href()
    }
    pub fn edges_json(&self) -> String {
        serde_json::to_string(&self.relations).expect("typed relations serialize")
    }
    pub fn version(&self, shared: &DashboardSnapshot) -> String {
        sluice_store::artifacts::fingerprint(
            &serde_json::to_vec(&(self, shared)).expect("views serialize"),
        )
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&ProjectTemplate {
            view: self,
            js_url: super::asset_url("sluice.js"),
        })
    }
    pub fn region(&self) -> Result<TrustedHtml, askama::Error> {
        let body = self.body()?;
        let end = body
            .as_str()
            .find("<sluice-drawer")
            .expect("owned template drawer boundary");
        Ok(TrustedHtml::owned(body.as_str()[..end].trim().to_owned()))
    }
    pub fn render(
        &self,
        shared: &DashboardSnapshot,
        viewer: &Viewer,
    ) -> Result<TrustedHtml, PublicError> {
        let nav = NavView::new(shared, Some(self.project.id), "plan")?;
        super::render_layout(
            &self.project.name,
            &self.body().map_err(render_error)?,
            &nav,
            viewer,
            &format!("{}/stream?{}", self.href(), self.query),
            &self.version(shared),
            &self.href(),
        )
        .map_err(render_error)
    }
    /// Mermaid consumes the same typed relations as the browser. Stable synthetic
    /// node ids keep user labels out of Mermaid syntax.
    pub fn mermaid(&self, all: bool) -> String {
        let mut out = String::from("flowchart TD\n");
        let mut ids = BTreeMap::new();
        for unit in &self.units {
            if unit.done && !all {
                continue;
            }
            let uid = format!("u{}", ids.len());
            ids.insert(unit.key(), uid.clone());
            out.push_str(&format!(
                "  subgraph {uid}[\"{}\"]\n",
                mermaid_text(unit.id.as_str())
            ));
            for step in &unit.steps {
                let sid = format!("n{}", ids.len());
                ids.insert(format!("s:{}", step.id), sid.clone());
                out.push_str(&format!(
                    "    {sid}[\"{} · {}\"]\n",
                    mermaid_text(step.id.as_str()),
                    step.mark
                ));
            }
            out.push_str("  end\n");
        }
        for (prefix, fields) in [("i", &self.inputs), ("o", &self.outputs)] {
            for field in fields {
                let id = format!("n{}", ids.len());
                ids.insert(format!("{prefix}:{}", field.name), id.clone());
                out.push_str(&format!("  {id}([\"{}\"])\n", mermaid_text(&field.name)));
            }
        }
        for edge in &self.relations {
            if let (Some(a), Some(b)) = (ids.get(&edge.from.key()), ids.get(&edge.to.key())) {
                out.push_str(&format!(
                    "  {a} {}|{}| {b}\n",
                    if edge.tolerant { "-.->" } else { "-->" },
                    mermaid_text(&edge.label)
                ));
            }
        }
        let folded = self.units.iter().filter(|u| u.done).count();
        if !all && folded > 0 {
            out.push_str(&format!(
                "  %% {folded} done units omitted; all=true shows them\n"
            ));
        }
        out
    }
}
fn mermaid_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace(['\n', '\r', '|', '<', '>'], " ")
}
#[derive(Template)]
#[template(path = "project.html")]
struct ProjectTemplate<'a> {
    view: &'a ProjectView,
    js_url: String,
}
pub(crate) fn render_error(error: askama::Error) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}
struct CatalogSignatures<'a>(&'a FunctionCatalog);
impl SignatureProvider for CatalogSignatures<'_> {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        let entry = self
            .0
            .entries
            .iter()
            .rev()
            .find(|f| f.name == name && f.error.is_empty())?;
        let mut sig = FnSignature {
            open: name == "core.external",
            ..FnSignature::default()
        };
        for p in &entry.inputs {
            sig.inputs.insert(
                p.name.clone(),
                Type::parse(&serde_json::Value::String(p.ty.clone())).ok()?,
            );
        }
        for p in &entry.outputs {
            sig.outputs.insert(
                p.name.clone(),
                Type::parse(&serde_json::Value::String(p.ty.clone())).ok()?,
            );
        }
        Some(sig)
    }
}
/// Load the shared nav and board in a single caller-owned transaction. The
/// signature provider must be the registry's exact compiled signatures.
pub fn load_board(
    c: &Connection,
    shared: &DashboardSnapshot,
    project: ProjectId,
    signatures: &impl SignatureProvider,
) -> sluice_store::Result<ProjectView> {
    let summary = shared
        .projects
        .iter()
        .find(|p| p.id == project)
        .ok_or_else(|| PublicError::NotFound {
            message: "project not found".into(),
        })?;
    let (revision, doc): (i64, String) = c.query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let plan = Plan::parse_json(doc.as_bytes(), signatures).map_err(|e| PublicError::Invalid {
        message: "stored plan cannot be compiled".into(),
        errors: e.into_iter().map(|e| e.to_string()).collect(),
    })?;
    let mut state = sluice_store::plans::read_state(c, project)?;
    for (id, step) in plan.steps() {
        if state.status(id) == StepStatus::Pending
            && !step.is_external()
            && evaluate_step(&plan, &state, step) == GateDecision::Ready
        {
            let needs = step.needs.iter().map(|(n, a)| (n.clone(), *a)).collect();
            let fit = sluice_store::resources::fits(c, project, &needs)?;
            if !fit.blocked.is_empty() {
                state
                    .steps
                    .entry(id.clone())
                    .or_default()
                    .queued
                    .push(fit.reason);
            }
        }
    }
    let mut board = ProjectView::new(summary.clone(), &plan, &state, revision as u64);
    for unit in &mut board.units {
        let last: Option<(String, String)> = c.query_row("SELECT body,at FROM messages WHERE project_id=?1 AND (\"from\" IN (SELECT step_id FROM steps WHERE project_id=?1 AND coalesce(unit,step_id)=?2) OR \"to\" IN (SELECT step_id FROM steps WHERE project_id=?1 AND coalesce(unit,step_id)=?2) OR thread IN (SELECT 'step-'||step_id FROM steps WHERE project_id=?1 AND coalesce(unit,step_id)=?2)) ORDER BY id DESC LIMIT 1", (project.to_string(), unit.id.as_str()), |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((message, at)) = last {
            unit.last_message = message;
            unit.changed = at;
        }
        for step in &mut unit.steps {
            super::step::load_detail(c, project, step)?;
        }
        for row in &mut unit.rows {
            for step in row {
                if let Some(detail) = unit.steps.iter().find(|s| s.id == step.id) {
                    *step = detail.clone();
                }
            }
        }
    }
    Ok(board)
}
/// Exact registry signatures, including open/submitted ports, supplied by the
/// application. Display catalog ports alone cannot describe arbitrary open fns.
#[derive(Clone, Debug, PartialEq)]
pub struct RegistrySnapshot {
    pub version: String,
    pub functions: Vec<(String, FnSignature)>,
}
impl SignatureProvider for RegistrySnapshot {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        self.functions
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, sig)| sig.clone())
    }
}
pub trait RegistrySource: Send + Sync {
    fn signatures(&self, project: ProjectId) -> Result<RegistrySnapshot, PublicError>;
}
#[derive(Clone)]
pub struct Registry(pub std::sync::Arc<dyn RegistrySource>);
pub async fn snapshot(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
) -> Result<Option<(DashboardSnapshot, ProjectView)>, PublicError> {
    let exact = registry.map(|r| r.0.signatures(project)).transpose()?;
    let exact_before = exact.clone();
    let stopped = super::runner_stale(state.reads.home());
    let catalog = state.catalog.catalog(Some(project))?;
    let before = catalog.clone();
    let (mut shared, mut board) = state
        .reads
        .snapshot(move |c| {
            let shared = super::load_snapshot(c, catalog, stopped)?;
            let board = if let Some(exact) = exact {
                load_board(c, &shared, project, &exact)?
            } else {
                load_board(c, &shared, project, &CatalogSignatures(&shared.functions))?
            };
            Ok((shared, board))
        })
        .await
        .map_err(|e| e.into_public(true))?;
    let home = state.reads.home().to_owned();
    let (updated, stable) = tokio::task::spawn_blocking(move || {
        super::observe_activity(&home, &mut shared);
        let before = shared.version();
        super::observe_activity(&home, &mut shared);
        let stable = before == shared.version();
        (shared, stable)
    })
    .await
    .map_err(|e| PublicError::Storage {
        message: e.to_string(),
    })?;
    shared = updated;
    for unit in &mut board.units {
        for step in &mut unit.steps {
            step.quiet = shared
                .projects
                .iter()
                .find(|p| p.id == project)
                .into_iter()
                .flat_map(|p| &p.running)
                .find(|r| r.step == step.id.as_str())
                .is_some_and(|r| r.quiet);
        }
        for row in &mut unit.rows {
            for step in row {
                if let Some(updated) = unit.steps.iter().find(|s| s.id == step.id) {
                    *step = updated.clone();
                }
            }
        }
    }
    Ok((stable
        && stopped == super::runner_stale(state.reads.home())
        && before == state.catalog.catalog(Some(project))?
        && exact_before == registry.map(|r| r.0.signatures(project)).transpose()?)
    .then_some((shared, board)))
}
#[derive(Clone, Debug, Deserialize, Default)]
pub struct BoardQuery {
    pub order: Option<String>,
    pub show: Option<String>,
    pub tag: Option<String>,
    pub format: Option<String>,
    pub all: Option<bool>,
    pub datastar: Option<String>,
}
impl BoardQuery {
    fn apply(&self, view: &mut ProjectView) -> Result<(), PublicError> {
        let order = self.order.as_deref().unwrap_or("live");
        let show = self.show.as_deref().unwrap_or("all");
        if !["live", "plan"].contains(&order)
            || !["all", "active", "attention", "done"].contains(&show)
            || self
                .format
                .as_deref()
                .is_some_and(|s| !["html", "mermaid"].contains(&s))
        {
            return Err(PublicError::BadRequest {
                message: "invalid board filter".into(),
            });
        }
        view.units.retain(|u| {
            (match show {
                "active" => !u.done,
                "attention" => u.rank() == 0,
                "done" => u.done,
                _ => true,
            }) && self.tag.as_deref().is_none_or(|tag| {
                tag.is_empty() || u.steps.iter().any(|s| s.tags.iter().any(|t| t == tag))
            })
        });
        view.order = order.into();
        view.show = show.into();
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("order", order).append_pair("show", show);
        if let Some(tag) = &self.tag {
            query.append_pair("tag", tag);
        }
        view.query = query.finish();
        if order == "live" {
            view.units.sort_by_key(UnitView::rank);
        }
        Ok(())
    }
}
pub(crate) fn error_response(e: PublicError) -> Response {
    let code = match e {
        PublicError::NotFound { .. } => StatusCode::NOT_FOUND,
        PublicError::BadRequest { .. } | PublicError::Invalid { .. } => StatusCode::BAD_REQUEST,
        PublicError::Conflict { .. } => StatusCode::CONFLICT,
        PublicError::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, e.to_string()).into_response()
}
pub async fn project_page(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path(project): Path<ProjectId>,
    Query(query): Query<BoardQuery>,
    headers: HeaderMap,
) -> Response {
    match snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await {
        Ok(Some((shared, mut view))) => {
            if let Err(e) = query.apply(&mut view) {
                return error_response(e);
            }
            if query.format.as_deref() == Some("mermaid") {
                return (
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "text/plain; charset=utf-8",
                    )],
                    view.mermaid(query.all.unwrap_or(false)),
                )
                    .into_response();
            }
            match view.render(&shared, &Viewer::from_headers(&headers)) {
                Ok(html) => Html(html.as_str().to_owned()).into_response(),
                Err(e) => error_response(e),
            }
        }
        Ok(None) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(e) => error_response(e),
    }
}
pub async fn project_stream(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path(project): Path<ProjectId>,
    Query(query): Query<BoardQuery>,
    headers: HeaderMap,
) -> Response {
    let viewer = Viewer::from_headers(&headers);
    let stop = state.stop.clone();
    let version = StreamQuery {
        project: Some(project),
        datastar: query.datastar.clone(),
    }
    .version(VersionSignal::Page);
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        let query = query.clone();
        let registry = registry.clone();
        async move {
            let Some((shared, mut view)) =
                snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?
            else {
                return Ok(None);
            };
            query.apply(&mut view)?;
            let nav = NavView::new(&shared, Some(project), "plan")?;
            Ok(Some(RenderedBatch {
                version: view.version(&shared),
                regions: vec![
                    PatchRegion::new("project-board", view.region().map_err(render_error)?),
                    PatchRegion::new(
                        "top-nav",
                        super::render_nav(&nav, &viewer, &view.href()).map_err(render_error)?,
                    ),
                ],
            }))
        }
    };
    Sse::new(streams::page_events(
        loader,
        version,
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
pub async fn project_redirect(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
) -> Response {
    let result = state
        .reads
        .snapshot(move |c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT project_id FROM projects WHERE name=?1 AND deleted_at IS NULL",
                    [name],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(raw)
        })
        .await;
    match result {
        Ok(Some(id)) => Redirect::temporary(&format!("/projects/id/{id}")).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => error_response(e.into_public(true)),
    }
}
pub async fn unit_page(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path((project, unit)): Path<(ProjectId, UnitName)>,
    headers: HeaderMap,
) -> Response {
    match snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await {
        Ok(Some((shared, view))) => {
            let Some(unit) = view.units.iter().find(|u| u.id == unit) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let nav = match NavView::new(&shared, Some(project), "plan") {
                Ok(n) => n,
                Err(e) => return error_response(e),
            };
            let page = unit.page_body().and_then(|body| {
                super::render_layout(
                    unit.id.as_str(),
                    &body,
                    &nav,
                    &Viewer::from_headers(&headers),
                    &format!("{}/units/{}/stream", view.href(), unit.id),
                    &view.version(&shared),
                    &format!("{}/units/{}", view.href(), unit.id),
                )
            });
            match page {
                Ok(html) => Html(html.as_str().to_owned()).into_response(),
                Err(e) => error_response(render_error(e)),
            }
        }
        Ok(None) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(e) => error_response(e),
    }
}

pub async fn unit_stream(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path((project, id)): Path<(ProjectId, UnitName)>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let stop = state.stop.clone();
    let version = query.version(VersionSignal::Page);
    let viewer = Viewer::from_headers(&headers);
    let loader = move || {
        let state = state.clone();
        let registry = registry.clone();
        let id = id.clone();
        let viewer = viewer.clone();
        async move {
            let Some((shared, view)) =
                snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?
            else {
                return Ok(None);
            };
            let unit =
                view.units
                    .iter()
                    .find(|u| u.id == id)
                    .ok_or_else(|| PublicError::NotFound {
                        message: "unit not found".into(),
                    })?;
            let nav = NavView::new(&shared, Some(project), "plan")?;
            Ok(Some(RenderedBatch {
                version: view.version(&shared),
                regions: vec![
                    PatchRegion::new("unit-detail", unit.page_body().map_err(render_error)?),
                    PatchRegion::new(
                        "top-nav",
                        super::render_nav(&nav, &viewer, &format!("{}/units/{id}", view.href()))
                            .map_err(render_error)?,
                    ),
                ],
            }))
        }
    };
    Sse::new(streams::page_events(
        loader,
        version,
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
