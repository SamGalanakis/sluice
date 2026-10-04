//! Datastar 0.4.1 patches. A version signal acknowledges all preceding HTML.
use crate::views::{DashboardState, TrustedHtml, Viewer, home};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use datastar::prelude::{PatchElements, PatchSignals};
use futures_util::{Stream, stream};
use serde::Deserialize;
use sluice_model::{error::PublicError, ids::ProjectId};
use std::{
    collections::{BTreeMap, VecDeque},
    convert::Infallible,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct PatchRegion {
    pub id: &'static str,
    pub html: TrustedHtml,
}
impl PatchRegion {
    pub fn new(id: &'static str, html: TrustedHtml) -> Self {
        Self { id, html }
    }
}
#[derive(Clone, Debug)]
pub struct RenderedBatch {
    pub version: String,
    pub regions: Vec<PatchRegion>,
}
impl RenderedBatch {
    /// A batch versioned by the HTML it draws: the same regions are the same version, so a page
    /// drawn with these regions and its stream's first batch agree whenever they show the same.
    pub fn new(regions: Vec<PatchRegion>) -> Self {
        let mut drawn = Vec::new();
        for region in &regions {
            drawn.extend_from_slice(region.id.as_bytes());
            drawn.push(0);
            drawn.extend_from_slice(region.html.as_str().as_bytes());
            drawn.push(0);
        }
        Self {
            version: sluice_store::artifacts::fingerprint(&drawn),
            regions,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum VersionSignal {
    Page,
    Step,
    Cursor,
}
impl VersionSignal {
    pub fn name(self) -> &'static str {
        match self {
            Self::Page => "ver",
            Self::Step => "sver",
            Self::Cursor => "seen",
        }
    }
}
#[derive(Clone, Debug)]
pub enum StreamEvent {
    Elements(PatchElements),
    Signals(PatchSignals),
}
impl StreamEvent {
    pub fn axum_event(&self) -> Event {
        match self {
            Self::Elements(p) => p.write_as_axum_sse_event(),
            Self::Signals(p) => p.write_as_axum_sse_event(),
        }
    }
    pub fn wire(&self) -> String {
        match self {
            Self::Elements(p) => p.as_datastar_event().to_string(),
            Self::Signals(p) => p.as_datastar_event().to_string(),
        }
    }
}
/// This state belongs to one connection. Never retain it for reconnects.
pub struct Comparison {
    client_version: String,
    current: Option<String>,
    regions: BTreeMap<&'static str, TrustedHtml>,
    signal: VersionSignal,
}
impl Comparison {
    pub fn new(client_version: String, signal: VersionSignal) -> Self {
        Self {
            client_version,
            current: None,
            regions: BTreeMap::new(),
            signal,
        }
    }
    pub fn events(&mut self, batch: RenderedBatch) -> Vec<StreamEvent> {
        if self.current.as_ref() == Some(&batch.version) {
            return vec![];
        }
        let first = self.current.is_none();
        let mut events = vec![];
        if !first || self.client_version != batch.version {
            for region in &batch.regions {
                if first || self.regions.get(region.id) != Some(&region.html) {
                    events.push(StreamEvent::Elements(
                        PatchElements::new(region.html.as_str())
                            .selector(format!("#{}", region.id)),
                    ));
                }
            }
            let signals = serde_json::json!({self.signal.name():batch.version,"stale":false});
            events.push(StreamEvent::Signals(PatchSignals::new(signals.to_string())));
        }
        if first && events.is_empty() {
            events.push(StreamEvent::Signals(PatchSignals::new(
                r#"{"stale":false}"#,
            )));
        }
        self.current = Some(batch.version);
        self.regions = batch.regions.into_iter().map(|r| (r.id, r.html)).collect();
        events
    }
}
/// Loader must finish its coherent snapshot before returning. Dropping the response
/// cancels the loader/wait; no producer task or database lease survives it.
pub fn page_events<L, F>(
    loader: L,
    client_version: String,
    signal: VersionSignal,
    stop: Arc<AtomicBool>,
) -> impl Stream<Item = Result<Event, Infallible>>
where
    L: Fn() -> F + Send + 'static,
    F: Future<Output = Result<RenderedBatch, PublicError>> + Send + 'static,
{
    stream::unfold(
        (
            loader,
            Comparison::new(client_version, signal),
            VecDeque::new(),
            false,
            false,
            stop,
        ),
        |(loader, mut comparison, mut pending, mut waited, mut ended, stop)| async move {
            loop {
                if stop.load(Ordering::Acquire) {
                    return None;
                }
                if let Some(event) = pending.pop_front() {
                    return Some((
                        Ok(event),
                        (loader, comparison, pending, waited, ended, stop),
                    ));
                }
                if ended {
                    return None;
                }
                if waited {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                waited = true;
                match loader().await {
                    Ok(batch) => {
                        pending.extend(comparison.events(batch).iter().map(StreamEvent::axum_event))
                    }
                    Err(_) => {
                        pending.push_back(
                            PatchSignals::new(r#"{"stale":true}"#).write_as_axum_sse_event(),
                        );
                        ended = true;
                    }
                }
            }
        },
    )
}
#[derive(Deserialize, Default)]
pub struct StreamQuery {
    pub project: Option<ProjectId>,
    pub datastar: Option<String>,
}
impl StreamQuery {
    pub fn version(&self, signal: VersionSignal) -> String {
        self.datastar
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| {
                v.get(signal.name())
                    .and_then(|v| v.as_str().map(str::to_owned))
            })
            .unwrap_or_default()
    }
}
pub async fn home_stream(
    State(state): State<DashboardState>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    response(state, query, headers, false).await
}
pub async fn functions_stream(
    State(state): State<DashboardState>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    response(state, query, headers, true).await
}
async fn response(
    state: DashboardState,
    query: StreamQuery,
    headers: HeaderMap,
    functions: bool,
) -> Response {
    let project = if functions { query.project } else { None };
    if let Some(project) = project {
        match state.snapshot(Some(project)).await {
            Ok(s) if s.projects.iter().any(|p| p.id == project) => {}
            Ok(_) => return StatusCode::NO_CONTENT.into_response(),
            Err(e) => return crate::http::error_response(e),
        }
    }
    let viewer = Viewer::from_headers(&headers);
    let stop = state.stop.clone();
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        async move { home::batch(&state.snapshot(project).await?, project, functions, &viewer) }
    };
    Sse::new(page_events(
        loader,
        query.version(VersionSignal::Page),
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
