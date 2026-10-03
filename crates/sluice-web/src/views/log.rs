//! Bounded, snapshot-coherent log pages with kind/thread filters and keyset paging.
use super::{DashboardSnapshot, DashboardState, FunctionCatalog, NavView, TrustedHtml, Viewer};
use askama::Template;
use axum::{
    Router,
    extract::{Path, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Response, Sse, sse::KeepAlive},
    routing::get,
};
use rusqlite::{params_from_iter, types::Value};
use sluice_model::{
    error::PublicError,
    events::{Event, Record},
    ids::{ProjectId, RecordSeq},
};
use sluice_store::{
    ReadPool,
    records::{self, RecordFilter},
};
use std::time::Duration;
const PAGE_SIZE: usize = 50;
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogQuery {
    pub kinds: Vec<String>,
    pub threads: Vec<String>,
    pub before: Option<i64>,
    pub after: Option<i64>,
}
impl LogQuery {
    pub fn parse(query: &str) -> Result<Self, PublicError> {
        let mut result = Self::default();
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "kind" | "kinds" => split(&value, &mut result.kinds),
                "thread" => split(&value, &mut result.threads),
                "before" | "after" => {
                    if value.is_empty() || value == "0" {
                        continue;
                    }
                    let seq = value
                        .parse::<i64>()
                        .ok()
                        .filter(|n| *n > 0)
                        .ok_or_else(|| bad("page cursor must be a positive integer"))?;
                    if key == "before" {
                        result.before = Some(seq);
                    } else {
                        result.after = Some(seq);
                    }
                }
                _ => {}
            }
        }
        if result.before.is_some() && result.after.is_some() {
            return Err(bad("give before or after, not both"));
        }
        Ok(result)
    }
    pub fn query(&self, before: Option<i64>, after: Option<i64>) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for kind in &self.kinds {
            query.append_pair("kind", kind);
        }
        if !self.threads.is_empty() {
            query.append_pair("thread", &self.threads.join(","));
        }
        if let Some(seq) = before {
            query.append_pair("before", &seq.to_string());
        }
        if let Some(seq) = after {
            query.append_pair("after", &seq.to_string());
        }
        query.finish()
    }
    pub fn kinds_text(&self) -> String {
        self.kinds.join(",")
    }
    pub fn threads_text(&self) -> String {
        self.threads.join(",")
    }
}
fn bad(message: &str) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn split(text: &str, into: &mut Vec<String>) {
    for item in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !into.iter().any(|s| s == item) {
            into.push(item.into());
        }
    }
}
pub const KIND_OPTIONS: &[&str] = &[
    "plan",
    "plan.edit",
    "plan.input",
    "step",
    "step.output",
    "step.retry",
    "step.cancel",
    "step.submit",
    "step.status",
    "step.lease",
    "step.queued",
    "call",
    "message",
    "project",
    "project.pause",
    "project.archive",
    "project.update",
    "project.rename",
    "project.delete",
    "project.capacity",
    "project.notify",
    "run",
    "run.adopt",
    "run.orphan",
    "run.completion_action",
    "run.completion_action.register",
    "unit",
    "unit.settled",
];
#[derive(Clone, Debug)]
pub struct LogRow {
    pub seq: i64,
    pub at: String,
    pub kind: String,
    pub summary: String,
    pub json: String,
}
#[derive(Clone, Debug)]
pub struct LogView {
    pub nav: DashboardSnapshot,
    pub project: Option<ProjectId>,
    pub query: LogQuery,
    pub rows: Vec<LogRow>,
    pub older: String,
    pub newer: String,
}
impl LogView {
    pub fn kinds(&self) -> &'static [&'static str] {
        KIND_OPTIONS
    }
    pub fn selected(&self, kind: &str) -> bool {
        self.query.kinds.iter().any(|k| k == kind)
    }
    pub fn base(&self) -> String {
        self.project
            .map(|id| format!("/projects/id/{id}/log"))
            .unwrap_or_else(|| "/log".into())
    }
    pub fn body(&self) -> Result<TrustedHtml, PublicError> {
        TrustedHtml::from_template(&LogTemplate { view: self })
            .map_err(super::threads::render_error)
    }
    pub fn version(&self) -> String {
        sluice_store::artifacts::fingerprint(
            format!("{}{:?}{:?}", self.nav.version(), self.query, self.rows).as_bytes(),
        )
    }
    pub fn render(&self, viewer: &Viewer) -> Result<TrustedHtml, PublicError> {
        let nav = NavView::new(&self.nav, self.project, "log")?;
        let stream = if self.query.before.is_none() && self.query.after.is_none() {
            format!("{}/stream?{}", self.base(), self.query.query(None, None))
        } else {
            String::new()
        };
        super::render_layout(
            "Log",
            &self.body()?,
            &nav,
            viewer,
            &stream,
            &self.version(),
            &self.base(),
        )
        .map_err(super::threads::render_error)
    }
}
#[derive(Template)]
#[template(path = "log.html")]
struct LogTemplate<'a> {
    view: &'a LogView,
}
pub async fn load(
    reads: &ReadPool,
    project: Option<ProjectId>,
    query: LogQuery,
) -> Result<LogView, PublicError> {
    reads.snapshot(move |sql| {
        if let Some(project) = project { sluice_store::messages::resolve_project(sql, &sluice_model::ids::ProjectSelector::Id(project))?; }
        let nav = super::load_snapshot(sql, FunctionCatalog::default(), false)?;
        // The store owns the closed set of valid event kinds.
        records::read_records(sql, project, &RecordFilter { kinds: query.kinds.clone(), threads: query.threads.clone(), limit: 1, ..Default::default() })?;
        let mut condition = "project_id IS ?".to_owned();
        let mut args = vec![project.map_or(Value::Null, |id| Value::Text(id.to_string()))];
        let mut kinds = query.kinds.clone();
        if kinds.is_empty() && !query.threads.is_empty() { kinds.push("message".into()); }
        if !kinds.is_empty() {
            condition.push_str(" AND (");
            for (index, kind) in kinds.iter().enumerate() {
                if index > 0 { condition.push_str(" OR "); }
                if ["plan", "step", "project", "run", "unit"].contains(&kind.as_str()) { condition.push_str("kind GLOB ?"); args.push(Value::Text(format!("{kind}.*"))); }
                else { condition.push_str("kind=?"); args.push(Value::Text(kind.clone())); }
            }
            condition.push(')');
        }
        if !query.threads.is_empty() {
            condition.push_str(" AND (kind!='message' OR thread IN ("); condition.push_str(&vec!["?"; query.threads.len()].join(",")); condition.push_str("))"); args.extend(query.threads.iter().cloned().map(Value::Text));
        }
        if !query.kinds.iter().any(|k| k == "call") { condition.push_str(" AND NOT (kind='call' AND json_extract(payload,'$.fn')='message.post' AND json_extract(payload,'$.status')!='failed')"); }
        let base_args = args.clone();
        let base_condition = condition.clone();
        if let Some(before) = query.before { condition.push_str(" AND seq<?"); args.push(Value::Integer(before)); }
        if let Some(after) = query.after { condition.push_str(" AND seq>?"); args.push(Value::Integer(after)); }
        let order = if query.after.is_some() { "ASC" } else { "DESC" };
        let mut stmt = sql.prepare(&format!("SELECT seq,at,payload,payload_version FROM records WHERE {condition} ORDER BY seq {order} LIMIT {PAGE_SIZE}"))?;
        let raw = stmt.query_map(params_from_iter(args), |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?, r.get::<_,String>(2)?, r.get::<_,i64>(3)?)))?.collect::<Result<Vec<_>,_>>()?;
        let mut rows = vec![];
        for (seq, at, payload, version) in raw {
            if version != sluice_store::schema::RECORD_PAYLOAD_VERSION { return Err(sluice_store::StoreError::InvalidDatabase("unsupported record payload version".into())); }
            let event: Event = serde_json::from_str(&payload)?;
            let json = serde_json::to_value(&event)?;
            let kind = json.get("kind").and_then(|v| v.as_str()).unwrap_or("").into();
            let summary = summary(&event);
            rows.push(LogRow { seq, at: at.clone(), kind, summary, json: serde_json::to_string_pretty(&Record { seq: RecordSeq(seq), at, project, event })? });
        }
        rows.sort_by_key(|r| std::cmp::Reverse(r.seq));
        let base = project.map(|id| format!("/projects/id/{id}/log")).unwrap_or_else(|| "/log".into());
        let exists = |seq, comparator: &str| -> sluice_store::Result<bool> {
            let mut args = base_args.clone(); args.push(Value::Integer(seq));
            Ok(sql.query_row(&format!("SELECT EXISTS(SELECT 1 FROM records WHERE {base_condition} AND seq{comparator}?)"), params_from_iter(args), |r| r.get(0))?)
        };
        let older = if let Some(last) = rows.last() { if exists(last.seq, "<")? { format!("{base}?{}", query.query(Some(last.seq), None)) } else { String::new() } } else { String::new() };
        let newer = if let Some(first) = rows.first() { if exists(first.seq, ">")? { format!("{base}?{}", query.query(None, Some(first.seq))) } else { String::new() } } else { String::new() };
        Ok(LogView { nav, project, query, rows, older, newer })
    }).await.map_err(|e| e.into_public(true))
}
fn summary(event: &Event) -> String {
    match event {
        Event::Message(m) => format!(
            "{} from {} to {}: {}",
            m.thread,
            m.from,
            m.to.as_deref().unwrap_or("anyone"),
            m.body.chars().take(160).collect::<String>()
        ),
        Event::StepStatus {
            step,
            from,
            to,
            error,
            ..
        } => format!(
            "{step} {} → {}{}",
            from.as_ref()
                .map(|s| serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned())
                .unwrap_or_else(|| "new".into()),
            serde_json::to_value(to).unwrap().as_str().unwrap(),
            error.as_ref().map(|e| format!(": {e}")).unwrap_or_default()
        ),
        Event::PlanEdit {
            rev,
            author,
            reason,
            ops,
        } => format!("rev {} by {author}: {reason} ({} ops)", rev.0, ops.len()),
        Event::PlanInput {
            name,
            value,
            author,
            ..
        } => format!("{name} = {value:?} by {author}"),
        Event::Call { name, status, .. } => format!(
            "{name} {}",
            serde_json::to_value(status).unwrap().as_str().unwrap()
        ),
        _ => serde_json::to_string(event)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
    }
}
pub fn router(state: DashboardState) -> Router {
    Router::new()
        .route("/log", get(global_page))
        .route("/log/stream", get(global_stream))
        .route("/projects/id/{project}/log", get(project_page))
        .route("/projects/id/{project}/log/stream", get(project_stream))
        .with_state(state)
}
async fn global_page(
    State(state): State<DashboardState>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    page(state, None, uri.query().unwrap_or(""), headers).await
}
async fn project_page(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    page(state, Some(project), uri.query().unwrap_or(""), headers).await
}
async fn page(
    state: DashboardState,
    project: Option<ProjectId>,
    raw: &str,
    headers: HeaderMap,
) -> Response {
    let result = async {
        load(&state.reads, project, LogQuery::parse(raw)?)
            .await?
            .render(&Viewer::from_headers(&headers))
    }
    .await;
    match result {
        Ok(html) => Html(html.as_str().to_owned()).into_response(),
        Err(error) => super::inbox::error_response(error),
    }
}
async fn global_stream(
    State(state): State<DashboardState>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream(state, None, uri.query().unwrap_or(""), headers)
}
async fn project_stream(
    State(state): State<DashboardState>,
    Path(project): Path<ProjectId>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
) -> Response {
    stream(state, Some(project), uri.query().unwrap_or(""), headers)
}
fn stream(
    state: DashboardState,
    project: Option<ProjectId>,
    raw: &str,
    headers: HeaderMap,
) -> Response {
    use crate::streams::{PatchRegion, RenderedBatch, StreamQuery, VersionSignal, page_events};
    let query = match LogQuery::parse(raw) {
        Ok(q) => q,
        Err(e) => return super::inbox::error_response(e),
    };
    let datastar = url::form_urlencoded::parse(raw.as_bytes())
        .find(|(k, _)| k == "datastar")
        .map(|(_, v)| v.into_owned());
    let version = StreamQuery { project, datastar }.version(VersionSignal::Page);
    let stop = state.stop.clone();
    let viewer = Viewer::from_headers(&headers);
    let events = page_events(
        move || {
            let state = state.clone();
            let query = query.clone();
            let viewer = viewer.clone();
            async move {
                let page = load(&state.reads, project, query).await?;
                let nav = NavView::new(&page.nav, project, "log")?;
                Ok(Some(RenderedBatch {
                    version: page.version(),
                    regions: vec![
                        PatchRegion::new("log-view", page.body()?),
                        PatchRegion::new(
                            "top-nav",
                            super::render_nav(&nav, &viewer, &page.base())
                                .map_err(super::threads::render_error)?,
                        ),
                    ],
                }))
            }
        },
        version,
        VersionSignal::Page,
        stop,
    );
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}
