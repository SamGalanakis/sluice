//! Step detail and owner actions. P6-02 injects the command service adapter.
use super::board::{self, render_error};
use super::{DashboardState, NavView, TrustedHtml, Viewer};
use crate::streams::{self, PatchRegion, RenderedBatch, StreamQuery, VersionSignal};
use askama::Template;
use axum::{
    Extension,
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response, Sse, sse::KeepAlive},
};
use serde::{Deserialize, Serialize};
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    gates::{GateDecision, StateSnapshot, ValueRef, evaluate_step, resolve_reference},
    ids::{ProjectId, RunId, StepId},
    plan::{Binding, Plan},
    types::BoundValue,
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

#[derive(Clone, Debug, Serialize)]
pub struct FieldView {
    pub name: String,
    pub ty: String,
    pub doc: String,
    pub value: String,
    pub available: bool,
    pub kind: String,
    pub source: String,
}
impl FieldView {
    pub fn new(
        name: &str,
        ty: &str,
        doc: &str,
        value: Option<&serde_json::Value>,
        source: &str,
    ) -> Self {
        Self {
            name: name.into(),
            ty: ty.into(),
            doc: doc.into(),
            value: value
                .map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    _ => serde_json::to_string_pretty(v).expect("JSON value"),
                })
                .unwrap_or_default(),
            available: value.is_some(),
            kind: match value {
                Some(serde_json::Value::Null) => "null",
                Some(serde_json::Value::Bool(true)) => "true",
                Some(serde_json::Value::Bool(false)) => "false",
                Some(serde_json::Value::Number(_)) => "number",
                Some(serde_json::Value::String(_)) => "text",
                _ => "json",
            }
            .into(),
            source: source.into(),
        }
    }
    pub fn reference(
        name: &str,
        ty: &str,
        reference: &ValueRef,
        plan: &Plan,
        state: &StateSnapshot,
    ) -> Self {
        let resolved = resolve_reference(plan, state, reference);
        let value = match &resolved {
            BoundValue::Ready(v) => Some(v.as_value()),
            _ => None,
        };
        Self::new(name, ty, "", value, &reference.0)
    }
    pub fn long(&self) -> bool {
        self.value.contains('\n') || self.value.chars().count() > 90
    }
    pub fn source_href(&self, project: &ProjectId) -> String {
        ValueRef::parse(&self.source)
            .ok()
            .and_then(|r| r.parts().ok())
            .and_then(|p| p.step)
            .map(|id| format!("/projects/id/{project}/steps/{id}"))
            .unwrap_or_default()
    }
    pub fn key(&self, prefix: &str) -> String {
        format!("{prefix}:{}", self.name)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct RunView {
    pub id: RunId,
    pub started: String,
    pub finished: String,
    pub result: String,
    pub session: String,
    pub engine: String,
    pub inputs: Vec<FieldView>,
}
#[derive(Clone, Debug, Serialize)]
pub struct StepView {
    pub project: ProjectId,
    pub id: StepId,
    pub function: String,
    pub doc: String,
    pub status: String,
    pub mark: String,
    pub tags: Vec<String>,
    pub paused: bool,
    pub ready: bool,
    pub blocked: bool,
    pub quiet: bool,
    pub waits: Vec<String>,
    pub gates: Vec<String>,
    pub queued: Vec<String>,
    pub skipped: Vec<String>,
    pub error: String,
    pub inputs: Vec<FieldView>,
    pub outputs: Vec<FieldView>,
    pub runs: Vec<RunView>,
    pub messages: usize,
    pub awaiting: usize,
    pub total: Option<usize>,
    pub done: usize,
    pub manual: bool,
    pub revision: u64,
}
pub fn status_name(status: &StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Stale => "stale",
        StepStatus::Skipped => "skipped",
    }
}
impl StepView {
    pub fn new(project: ProjectId, plan: &Plan, state: &StateSnapshot, id: &StepId) -> Self {
        let step = &plan.steps()[id];
        let entry = state.steps.get(id).cloned().unwrap_or_default();
        let decision = evaluate_step(plan, state, step);
        let ready = decision == GateDecision::Ready;
        let waits = match decision {
            GateDecision::Wait(w) => w,
            GateDecision::Invalid(e) => e.into_iter().map(|e| e.to_string()).collect(),
            _ => vec![],
        };
        let paused = step.paused.is_paused();
        let held = paused || state.paused.is_paused();
        let status = status_name(&entry.status).to_owned();
        let mark = if entry.status == StepStatus::Pending && held {
            "paused"
        } else if entry.status == StepStatus::Pending && ready && step.is_external() {
            "external"
        } else {
            &status
        }
        .to_owned();
        let inputs = step
            .bindings
            .iter()
            .map(|(n, b)| {
                let ty = step
                    .signature
                    .inputs
                    .get(n)
                    .or_else(|| step.extra_inputs.get(n))
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                match b {
                    Binding::Default(v) => {
                        FieldView::new(n, &ty, "", Some(v.as_value()), "Default")
                    }
                    Binding::Source(r) => FieldView::reference(n, &ty, r, plan, state),
                    Binding::Sources(rs) => {
                        let fields: Vec<_> = rs
                            .iter()
                            .map(|r| FieldView::reference(n, &ty, r, plan, state))
                            .collect();
                        let values: Option<Vec<_>> = rs
                            .iter()
                            .map(|r| match resolve_reference(plan, state, r) {
                                BoundValue::Ready(v) => Some(v.as_value().clone()),
                                _ => None,
                            })
                            .collect();
                        let value = values.map(serde_json::Value::Array);
                        FieldView::new(
                            n,
                            &ty,
                            "",
                            value.as_ref(),
                            &fields
                                .iter()
                                .map(|f| f.source.clone())
                                .collect::<Vec<_>>()
                                .join(", "),
                        )
                    }
                    Binding::File(path) => FieldView::new(
                        n,
                        &ty,
                        "",
                        Some(&serde_json::json!({"file":path})),
                        "File, resolved when the run starts",
                    ),
                }
            })
            .collect();
        let mut outputs = vec![];
        for (name, ty, doc) in step
            .signature
            .outputs
            .iter()
            .map(|(n, t)| (n, t, ""))
            .chain(
                step.declared_outputs
                    .iter()
                    .map(|(n, d)| (n, &d.ty, d.doc.as_deref().unwrap_or(""))),
            )
        {
            outputs.push(FieldView::new(
                name,
                &ty.to_string(),
                doc,
                entry.outputs.0.get(name).map(|v| v.as_value()),
                "",
            ));
        }
        for (n, v) in &entry.outputs.0 {
            if !outputs.iter().any(|f| &f.name == n) {
                outputs.push(FieldView::new(n, "", "", Some(v.as_value()), ""));
            }
        }
        Self {
            project,
            id: id.clone(),
            function: step.run.clone(),
            doc: step.doc.clone().unwrap_or_default(),
            status,
            mark,
            tags: step.tags.clone(),
            paused,
            ready,
            blocked: false,
            quiet: false,
            waits,
            gates: step.after.iter().map(|g| g.entry()).collect(),
            queued: entry.queued,
            skipped: entry.skipped.iter().map(|s| s.to_string()).collect(),
            error: entry.error.unwrap_or_default(),
            inputs,
            outputs,
            runs: vec![],
            messages: 0,
            awaiting: 0,
            total: None,
            done: 0,
            manual: false,
            revision: 0,
        }
    }
    pub fn href(&self) -> String {
        format!("/projects/id/{}/steps/{}", self.project, self.id)
    }
    pub fn thread_href(&self) -> String {
        format!(
            "/projects/id/{}/thread?thread=step-{}",
            self.project, self.id
        )
    }
    pub fn key(&self) -> String {
        format!("s:{}", self.id)
    }
    pub fn display_mark(&self) -> &str {
        if self.manual && self.status == "succeeded" {
            "manual"
        } else {
            &self.mark
        }
    }
    pub fn card_class(&self) -> String {
        format!(
            "node {} is-{}{}{}",
            if self.function.starts_with("core.") && self.function != "core.external" {
                "chip"
            } else {
                "card"
            },
            self.mark,
            if self.blocked { " is-blocked" } else { "" },
            if self.ready { " is-next" } else { "" }
        )
    }
    pub fn caption(&self) -> String {
        if self.blocked {
            "blocked".into()
        } else if !self.queued.is_empty() {
            "queued".into()
        } else if self.mark == "external" {
            "outside".into()
        } else if let Some(total) = self.total {
            format!("{}/{total}", self.done)
        } else {
            String::new()
        }
    }
    pub fn retryable(&self) -> bool {
        matches!(self.status.as_str(), "succeeded" | "failed" | "stale")
    }
    pub fn pausable(&self) -> bool {
        self.paused || matches!(self.status.as_str(), "pending" | "failed" | "stale")
    }
    pub fn last_run(&self) -> Option<&RunView> {
        self.runs.last()
    }
    pub fn cancellable(&self) -> bool {
        matches!(self.status.as_str(), "pending" | "running")
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&StepTemplate { step: self })
    }
    pub fn page_body(&self) -> Result<TrustedHtml, askama::Error> {
        #[derive(Template)]
        #[template(
            source = "{{ body|safe }}<script type=\"module\" src=\"{{ js_url }}\"></script>",
            ext = "html"
        )]
        struct Page {
            body: TrustedHtml,
            js_url: String,
        }
        TrustedHtml::from_template(&Page {
            body: self.body()?,
            js_url: super::asset_url("sluice.js"),
        })
    }
    pub fn version(&self) -> String {
        sluice_store::artifacts::fingerprint(&serde_json::to_vec(self).expect("view serializes"))
    }
}
#[derive(Template)]
#[template(path = "step.html")]
struct StepTemplate<'a> {
    step: &'a StepView,
}
/// Run history is current-generation only; a reused step id never inherits an
/// old declaration's runs. Frozen attempted inputs come from durable results.
pub fn load_detail(
    c: &rusqlite::Connection,
    project: ProjectId,
    step: &mut StepView,
) -> sluice_store::Result<()> {
    let (manual,total,done,revision): (bool,Option<i64>,i64,i64) = c.query_row("SELECT manual,total,done,(SELECT rev FROM plans WHERE project_id=?1) FROM steps WHERE project_id=?1 AND step_id=?2", (project.to_string(),step.id.as_str()), |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    step.manual = manual;
    step.total = total.map(|n| n as usize);
    step.done = done as usize;
    step.revision = revision as u64;
    let mut q = c.prepare("SELECT r.run_id,coalesce(r.started_at,r.created_at),coalesce(r.finished_at,''),coalesce(r.result,''),coalesce(s.session_id,''),coalesce(s.engine,''),a.request FROM runs r JOIN attempts a USING(attempt_id) LEFT JOIN sessions s USING(run_id) JOIN steps st ON st.project_id=r.project_id AND st.step_id=r.step_id AND st.generation=r.generation WHERE r.project_id=?1 AND r.step_id=?2 ORDER BY r.created_at,r.run_id")?;
    let mut rows = q.query((project.to_string(), step.id.as_str()))?;
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let request: String = r.get(6)?;
        let request: serde_json::Value = serde_json::from_str(&request)?;
        let inputs = request
            .get("inputs")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .map(|(n, v)| {
                        FieldView::new(
                            n,
                            step.inputs
                                .iter()
                                .find(|f| &f.name == n)
                                .map(|f| f.ty.as_str())
                                .unwrap_or(""),
                            "",
                            Some(v),
                            "Frozen run input",
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        step.runs.push(RunView {
            id: id
                .parse()
                .map_err(|e| sluice_store::StoreError::InvalidDatabase(format!("{e}")))?,
            started: r.get(1)?,
            finished: r.get(2)?,
            result: r.get(3)?,
            session: r.get(4)?,
            engine: r.get(5)?,
            inputs,
        });
    }
    if let Some(last) = step.runs.last() {
        for frozen in &last.inputs {
            if let Some(field) = step.inputs.iter_mut().find(|f| f.name == frozen.name) {
                *field = frozen.clone();
            }
        }
    }
    (step.messages,step.awaiting) = c.query_row("SELECT count(*),coalesce(sum(needs_reply=1 AND resolved_by IS NULL AND closed_at IS NULL),0) FROM messages WHERE project_id=?1 AND thread=?2", (project.to_string(),format!("step-{}",step.id)), |r| Ok((r.get::<_, i64>(0)? as usize,r.get::<_, i64>(1)? as usize)))?;
    let mut q = c.prepare("SELECT sub.outputs FROM submissions sub JOIN runs r USING(run_id) JOIN steps st ON st.project_id=r.project_id AND st.step_id=r.step_id AND st.generation=r.generation WHERE r.project_id=?1 AND r.step_id=?2 AND r.finished_at IS NULL ORDER BY r.created_at DESC LIMIT 1")?;
    use rusqlite::OptionalExtension;
    let submitted: Option<String> = q
        .query_row((project.to_string(), step.id.as_str()), |r| r.get(0))
        .optional()?;
    if let Some(submitted) = submitted {
        let outputs: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&submitted)?;
        for (n, v) in outputs {
            if let Some(field) = step.outputs.iter_mut().find(|f| f.name == n) {
                let source = field.clone();
                *field = FieldView::new(&n, &source.ty, &source.doc, Some(&v), "Submitted so far");
            } else {
                step.outputs
                    .push(FieldView::new(&n, "", "", Some(&v), "Submitted so far"));
            }
        }
    }
    Ok(())
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Pause,
    Unpause,
    Retry,
    Cancel,
    PauseProject,
    ResumeProject,
    ArchiveProject,
    RestoreProject,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct OwnerCommand {
    pub project: ProjectId,
    pub step: Option<StepId>,
    pub action: Action,
    pub revision: u64,
    pub message: String,
    pub author: &'static str,
}
pub trait CommandService: Send + Sync {
    fn execute(
        &self,
        command: OwnerCommand,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublicError>> + Send + '_>>;
}
#[derive(Clone)]
pub struct Commands(pub Arc<dyn CommandService>);
#[derive(Deserialize)]
pub struct ActionForm {
    pub action: Action,
    pub revision: u64,
    #[serde(default)]
    pub message: String,
}
pub async fn action(
    State(state): State<DashboardState>,
    registry: Option<Extension<board::Registry>>,
    commands: Option<Extension<Commands>>,
    Path((project, id)): Path<(ProjectId, StepId)>,
    Form(form): Form<ActionForm>,
) -> Response {
    execute_action(state, commands, registry, project, Some(id), form).await
}
pub async fn project_action(
    State(state): State<DashboardState>,
    registry: Option<Extension<board::Registry>>,
    commands: Option<Extension<Commands>>,
    Path(project): Path<ProjectId>,
    Form(form): Form<ActionForm>,
) -> Response {
    execute_action(state, commands, registry, project, None, form).await
}
async fn execute_action(
    state: DashboardState,
    commands: Option<Extension<Commands>>,
    registry: Option<Extension<board::Registry>>,
    project: ProjectId,
    id: Option<StepId>,
    form: ActionForm,
) -> Response {
    let Some(Extension(commands)) = commands else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Command service unavailable",
        )
            .into_response();
    };
    let view = match board::snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await {
        Ok(Some((_, view))) => view,
        Ok(None) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(e) => return board::error_response(e),
    };
    if view.revision != form.revision {
        return (
            StatusCode::CONFLICT,
            "The plan changed. Reload before acting.",
        )
            .into_response();
    }
    if form.message.len() > 16_384 {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let valid = if let Some(id) = &id {
        let Some(step) = view
            .units
            .iter()
            .flat_map(|u| &u.steps)
            .find(|s| &s.id == id)
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match form.action {
            Action::Retry => step.retryable(),
            Action::Cancel => step.cancellable(),
            Action::Pause => step.pausable(),
            Action::Unpause => step.paused,
            _ => false,
        }
    } else {
        matches!(
            form.action,
            Action::PauseProject
                | Action::ResumeProject
                | Action::ArchiveProject
                | Action::RestoreProject
        )
    };
    if !valid {
        return (
            StatusCode::BAD_REQUEST,
            "Action does not apply to this state",
        )
            .into_response();
    }
    let next = id
        .as_ref()
        .map(|id| format!("{}/steps/{id}", view.href()))
        .unwrap_or_else(|| view.href());
    match commands
        .0
        .execute(OwnerCommand {
            project,
            step: id,
            action: form.action,
            revision: form.revision,
            message: form.message,
            author: "owner",
        })
        .await
    {
        Ok(()) => Redirect::to(&next).into_response(),
        Err(e) => board::error_response(e),
    }
}
pub async fn step_page(
    State(state): State<DashboardState>,
    registry: Option<Extension<board::Registry>>,
    Path((project, id)): Path<(ProjectId, StepId)>,
    headers: HeaderMap,
) -> Response {
    match board::snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await {
        Ok(Some((shared, view))) => {
            let Some(step) = view
                .units
                .iter()
                .flat_map(|u| &u.steps)
                .find(|s| s.id == id)
            else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let nav = match NavView::new(&shared, Some(project), "plan") {
                Ok(n) => n,
                Err(e) => return board::error_response(e),
            };
            let html = step.page_body().and_then(|body| {
                super::render_layout(
                    id.as_str(),
                    &body,
                    &nav,
                    &Viewer::from_headers(&headers),
                    &format!("{}/stream", step.href()),
                    "",
                    &step.href(),
                )
            });
            match html {
                Ok(html) => Html(html.as_str().to_owned()).into_response(),
                Err(e) => board::error_response(render_error(e)),
            }
        }
        Ok(None) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(e) => board::error_response(e),
    }
}
pub async fn step_stream(
    State(state): State<DashboardState>,
    registry: Option<Extension<board::Registry>>,
    Path((project, id)): Path<(ProjectId, StepId)>,
    Query(query): Query<StreamQuery>,
) -> Response {
    let stop = state.stop.clone();
    let version = query.version(VersionSignal::Step);
    let loader = move || {
        let state = state.clone();
        let id = id.clone();
        let registry = registry.clone();
        async move {
            let Some((_, view)) =
                board::snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?
            else {
                return Ok(None);
            };
            let step = view
                .units
                .iter()
                .flat_map(|u| &u.steps)
                .find(|s| s.id == id)
                .ok_or_else(|| PublicError::NotFound {
                    message: "step not found".into(),
                })?;
            Ok(Some(RenderedBatch {
                version: step.version(),
                regions: vec![PatchRegion::new(
                    "step-detail",
                    step.body().map_err(render_error)?,
                )],
            }))
        }
    };
    Sse::new(streams::page_events(
        loader,
        version,
        VersionSignal::Step,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
