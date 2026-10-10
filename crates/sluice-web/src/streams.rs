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
    time::{Duration, Instant},
};

/// A part of a page a stream patches, by its element's id. Its HTML may mark parts of it as
/// regions of their own, `<!--r:id-->` and `<!--/r:id-->` around the element with that id: a
/// batch then sends only the smallest regions that changed (`Comparison::events`).
#[derive(Clone, Debug)]
pub struct PatchRegion {
    pub id: String,
    pub html: TrustedHtml,
}
impl PatchRegion {
    pub fn new(id: impl Into<String>, html: TrustedHtml) -> Self {
        Self {
            id: id.into(),
            html,
        }
    }
}
/// `html` split at the regions it marks: its skeleton, each marked region's HTML left out (its
/// markers kept, so where each one sits is part of the skeleton), and those regions in order.
/// A marked region's own regions stay inside it.
fn split(html: &str) -> (String, Vec<(&str, &str)>) {
    const OPEN: &str = "<!--r:";
    let mut skeleton = String::with_capacity(html.len().min(4096));
    let mut children = vec![];
    let mut rest = html;
    while let Some(at) = rest.find(OPEN) {
        let after = &rest[at + OPEN.len()..];
        let Some(close) = after.find("-->") else {
            break;
        };
        let id = &after[..close];
        let body = &after[close + 3..];
        let end_marker = format!("<!--/r:{id}-->");
        let Some(end) = body.find(&end_marker) else {
            break;
        };
        skeleton.push_str(&rest[..at + OPEN.len() + close + 3]);
        skeleton.push_str(&end_marker);
        children.push((id, &body[..end]));
        rest = &body[end + end_marker.len()..];
    }
    skeleton.push_str(rest);
    (skeleton, children)
}
#[derive(Clone, Debug)]
pub struct RenderedBatch {
    pub version: String,
    pub regions: Vec<PatchRegion>,
}
impl RenderedBatch {
    /// A batch versioned by the HTML it draws: the same regions are the same version, so a page
    /// drawn with these regions and its stream's first batch agree whenever they show the same.
    /// A ticking time's text (`<time data-since=…>…</time>`, a running card's timer) is the
    /// clock's, not the page's: the version leaves it out, so the clock alone never patches.
    pub fn new(regions: Vec<PatchRegion>) -> Self {
        let mut drawn = Vec::new();
        for region in &regions {
            drawn.extend_from_slice(region.id.as_bytes());
            drawn.push(0);
            without_clock(region.html.as_str(), &mut drawn);
            drawn.push(0);
        }
        Self {
            version: sluice_store::artifacts::fingerprint(&drawn),
            regions,
        }
    }
}
/// `html` with the text of each ticking `<time data-since=…>` and `<time data-ago=…>` left out
/// (its tag kept).
fn without_clock(html: &str, out: &mut Vec<u8>) {
    let mut rest = html;
    while let Some(at) = ["<time data-since=", "<time data-ago="]
        .iter()
        .filter_map(|open| rest.find(open))
        .min()
    {
        let Some(tag) = rest[at..].find('>') else {
            break;
        };
        let (kept, after) = rest.split_at(at + tag + 1);
        out.extend_from_slice(kept.as_bytes());
        rest = after.find("</time>").map_or(after, |end| &after[end..]);
    }
    out.extend_from_slice(rest.as_bytes());
}
/// A region's skeleton as the comparison keeps it: hashed, its clock left out.
fn skeleton_hash(skeleton: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut bytes = Vec::with_capacity(skeleton.len());
    without_clock(skeleton, &mut bytes);
    let mut hasher = std::hash::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// The signal that says this stream is behind: the page's banner reads `stale`; a step
    /// drawn in the drawer has its own, so the drawer never speaks for the page.
    pub fn stale(self) -> &'static str {
        match self {
            Self::Step => "sstale",
            Self::Page | Self::Cursor => "stale",
        }
    }
}
/// The running build's release: its installed release's name and its assets' fingerprint. A
/// stream's first signals carry it (`rel`), and a page drawn by another build says so.
pub fn release() -> &'static str {
    static RELEASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RELEASE.get_or_init(|| {
        format!(
            "{}:{}",
            sluice_runtime::install::release_id("working-tree"),
            crate::views::assets_fingerprint()
        )
    })
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
    /// Each region drawn last, by id: its skeleton's hash.
    drawn: BTreeMap<String, u64>,
    signal: VersionSignal,
}
impl Comparison {
    pub fn new(client_version: String, signal: VersionSignal) -> Self {
        Self {
            client_version,
            current: None,
            drawn: BTreeMap::new(),
            signal,
        }
    }
    /// The events that bring the client from what this connection last sent (or, first, from
    /// the version it says it has) to `batch`: nothing for the same version; otherwise each
    /// smallest region that changed (a region whose skeleton is the same sends only its own
    /// regions that changed), then the version. The first signals carry the release.
    pub fn events(&mut self, batch: RenderedBatch) -> Vec<StreamEvent> {
        if self.current.as_ref() == Some(&batch.version) {
            return vec![];
        }
        let first = self.current.is_none();
        let mut events = vec![];
        let mut drawn = BTreeMap::new();
        let send = !first || self.client_version != batch.version;
        for region in &batch.regions {
            if first {
                record(&region.id, region.html.as_str(), &mut drawn);
                if send {
                    events.push(patch(&region.id, region.html.as_str()));
                }
            } else {
                self.diff(&region.id, region.html.as_str(), &mut drawn, &mut events);
            }
        }
        let mut signals = serde_json::Map::new();
        if send {
            signals.insert(self.signal.name().into(), batch.version.clone().into());
        }
        signals.insert(self.signal.stale().into(), false.into());
        if first {
            signals.insert("rel".into(), release().into());
        }
        if send || first {
            events.push(StreamEvent::Signals(PatchSignals::new(
                serde_json::Value::Object(signals).to_string(),
            )));
        }
        self.current = Some(batch.version);
        self.drawn = drawn;
        events
    }
    fn diff(
        &self,
        id: &str,
        html: &str,
        drawn: &mut BTreeMap<String, u64>,
        events: &mut Vec<StreamEvent>,
    ) {
        let (skeleton, children) = split(html);
        let hash = skeleton_hash(&skeleton);
        if self.drawn.get(id) != Some(&hash) {
            record(id, html, drawn);
            events.push(patch(id, html));
            return;
        }
        drawn.insert(id.to_owned(), hash);
        for (child, html) in children {
            self.diff(child, html, drawn, events);
        }
    }
}
/// Note `html`'s skeleton and each of its regions' as drawn.
fn record(id: &str, html: &str, drawn: &mut BTreeMap<String, u64>) {
    let (skeleton, children) = split(html);
    drawn.insert(id.to_owned(), skeleton_hash(&skeleton));
    for (child, html) in children {
        record(child, html, drawn);
    }
}
fn patch(id: &str, html: &str) -> StreamEvent {
    StreamEvent::Elements(PatchElements::new(html).selector(format!("#{id}")))
}
/// A stream renders its page again only when what it reads may have changed (its `watch`
/// token moved), and at least this often: what the clock alone changes (a run going quiet,
/// "Read today", a board query over the time) is redrawn within it.
pub const FRESH: Duration = Duration::from_secs(30);
/// Loader must finish its coherent snapshot before returning. Dropping the response
/// cancels the loader/wait; no producer task or database lease survives it. Each second the
/// stream reads `watch`, a cheap token of everything its page reads (`DashboardState::token`),
/// and runs `loader` only when it moved or the last render is older than `FRESH`.
pub fn page_events<W, G, L, F>(
    watch: W,
    loader: L,
    client_version: String,
    signal: VersionSignal,
    stop: Arc<AtomicBool>,
) -> impl Stream<Item = Result<Event, Infallible>>
where
    W: Fn() -> G + Send + 'static,
    G: Future<Output = Result<String, PublicError>> + Send + 'static,
    L: Fn() -> F + Send + 'static,
    F: Future<Output = Result<RenderedBatch, PublicError>> + Send + 'static,
{
    struct Gate {
        token: Option<String>,
        rendered: Option<Instant>,
    }
    stream::unfold(
        (
            (watch, loader),
            Comparison::new(client_version, signal),
            VecDeque::new(),
            Gate {
                token: None,
                rendered: None,
            },
            false,
            stop,
        ),
        move |((watch, loader), mut comparison, mut pending, mut gate, mut ended, stop)| async move {
            loop {
                if stop.load(Ordering::Acquire) {
                    return None;
                }
                if let Some(event) = pending.pop_front() {
                    return Some((
                        Ok(event),
                        ((watch, loader), comparison, pending, gate, ended, stop),
                    ));
                }
                if ended {
                    return None;
                }
                if gate.rendered.is_some() {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                // read before the loader's snapshot: a change after it moves the next token
                let token = watch().await.ok();
                if token.is_some()
                    && token == gate.token
                    && gate.rendered.is_some_and(|at| at.elapsed() < FRESH)
                {
                    continue;
                }
                gate.token = token;
                gate.rendered = Some(Instant::now());
                match loader().await {
                    Ok(batch) => {
                        pending.extend(comparison.events(batch).iter().map(StreamEvent::axum_event))
                    }
                    Err(_) => {
                        let stale = serde_json::json!({ signal.stale(): true }).to_string();
                        pending.push_back(PatchSignals::new(stale).write_as_axum_sse_event());
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
    /// A signal the page holds, as text ("" when it holds none): the step's chosen `tab`.
    pub fn signal(&self, name: &str) -> String {
        self.datastar
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v.get(name).and_then(|v| v.as_str().map(str::to_owned)))
            .unwrap_or_default()
    }
}
/// Home's stream: its body, its band and the nav, drawn again as its pages are.
pub async fn home_stream(
    State(state): State<DashboardState>,
    registry: Option<axum::Extension<crate::views::board::Registry>>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let viewer = Viewer::from_headers(&headers);
    let zone = crate::views::day::zone(&headers);
    let registry = registry.map(|r| r.0);
    let stop = state.stop.clone();
    let watch = state.watch(None);
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        let registry = registry.clone();
        async move {
            home::home_batch(&state, registry.as_ref(), &viewer, zone)
                .await
                .map(|(batch, _, _)| batch)
        }
    };
    Sse::new(page_events(
        watch,
        loader,
        query.version(VersionSignal::Page),
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
pub async fn functions_stream(
    State(state): State<DashboardState>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let project = query.project;
    if let Some(project) = project {
        match state.snapshot(Some(project)).await {
            Ok(s) if s.projects.iter().any(|p| p.id == project) => {}
            Ok(_) => return StatusCode::NO_CONTENT.into_response(),
            Err(e) => return crate::http::error_response(e),
        }
    }
    let viewer = Viewer::from_headers(&headers);
    let stop = state.stop.clone();
    let watch = state.watch(project);
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        async move { home::batch(&state.snapshot(project).await?, project, true, &viewer) }
    };
    Sse::new(page_events(
        watch,
        loader,
        query.version(VersionSignal::Page),
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
