//! Finished units, in bounded pages, with a browser-local read cursor.
use super::board::{ProjectView, UnitView};
use super::ui::{self, Shown, esc};
use super::{DashboardState, Frame, NavView, TrustedHtml, Viewer};
use crate::streams::{PatchRegion, RenderedBatch, StreamQuery, VersionSignal};
use axum::Extension;
use super::board::Registry;
use axum::{
    extract::{Form, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response, Sse},
};
use serde::Deserialize;
use sluice_model::{error::PublicError, ids::ProjectId, shown::Band};
use std::fmt::Write;

pub const PAGE_SIZE: usize = 50;
/// The cookie that keeps how far this browser has looked at a project's finished units.
pub fn seen_cookie(project: ProjectId) -> String {
    format!("sluice_seen_{project}")
}
/// What a request's cookie says it has seen of a project: the finish time (seconds since the
/// epoch) of the last finished unit it marked seen.
pub fn seen_of(headers: &HeaderMap, project: ProjectId) -> Option<u64> {
    let name = seen_cookie(project);
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .unwrap_or("")
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| v.parse().ok())
        // 0 is the look undone: everything finished lately again
        .filter(|seen| *seen > 0)
}

#[derive(Clone, Default, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    page: usize,
}
#[derive(Deserialize)]
pub struct SeenForm {
    pub through: u64,
}
/// A stopped unit belongs here once all its work is terminal; a retry takes it back out.
fn finished(unit: &UnitView) -> bool {
    !unit.finished().is_empty()
        && (unit.done
            || unit.steps.iter().all(|s| {
                s.standing().spec().band == Band::Done
                    || matches!(s.shown(), Shown::Failed | Shown::Cancelled)
            }))
}
fn local_day(at: u64, zone: i64) -> i64 {
    (at as i64 + zone * 60).div_euclid(86_400)
}
pub fn finished_today(view: &ProjectView) -> usize {
    let today = local_day(super::plan::now() as u64, view.zone);
    view.units
        .iter()
        .filter(|u| {
            finished(u)
                && super::timestamp(u.finished()).is_some_and(|t| local_day(t, view.zone) == today)
        })
        .count()
}
fn day_label(at: u64, now: u64, zone: i64) -> String {
    let day = local_day(at, zone);
    match local_day(now, zone) - day {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => {
            let date = super::rfc3339((day * 86_400).max(0) as u64);
            let month: usize = date[5..7].parse().unwrap_or(1);
            let n: usize = date[8..10].parse().unwrap_or(1);
            let months = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let year = &date[..4];
            let current = super::rfc3339((local_day(now, zone) * 86_400).max(0) as u64);
            format!(
                "{} {n}{}",
                months[month.saturating_sub(1).min(11)],
                if year == &current[..4] {
                    String::new()
                } else {
                    format!(", {year}")
                }
            )
        }
    }
}
pub fn row(
    at: &str,
    took: f64,
    href: &str,
    title: &str,
    shown: Shown,
    engine: &str,
    unseen: bool,
) -> String {
    format!(
        "<li class=\"history-row\"><a href=\"{}\">{}<span class=\"engine\">{}</span><span class=\"history-title\">{}</span><span class=\"history-new\"{}>{}</span><span class=\"history-when\">{}{}</span></a></li>",
        esc(href),
        ui::glyph(shown),
        esc(engine),
        esc(title),
        if unseen {
            " aria-label=\"Finished since you looked\""
        } else {
            " aria-hidden=\"true\""
        },
        if unseen { "·" } else { "" },
        ui::clock(at),
        if took > 0.0 {
            format!(" · {}", esc(&ui::duration_text(took)))
        } else {
            String::new()
        }
    )
}
fn body(view: &ProjectView, page: usize) -> TrustedHtml {
    let href = format!("{}/history", view.href());
    let mut units: Vec<_> = view.units.iter().filter(|u| finished(u)).collect();
    units.sort_by(|a, b| b.finished().cmp(a.finished()).then(a.pos.cmp(&b.pos)));
    let page = page.min(units.len().saturating_sub(1) / PAGE_SIZE);
    let start = page * PAGE_SIZE;
    let through = units
        .get(start)
        .and_then(|u| super::timestamp(u.finished()))
        .unwrap_or(0)
        .max(view.seen.unwrap_or(0));
    let mut out = format!(
        "<div id=\"history-view\" data-reopens-stream data-on-signal-patch=\"{}\" data-on-signal-patch-filter=\"{{include: /^seen$/}}\">",
        esc(&ui::stream_reopen(&format!("{href}/stream?page={page}")))
    );
    if !units.is_empty() {
        let _ = write!(
            out,
            "<div class=\"history-seen\">{}<input type=\"hidden\" name=\"through\" value=\"{through}\"><button class=\"quiet\">Mark as seen</button>{}</form>",
            ui::post_form("mark-seen", "seen-form", &format!("{href}/seen")),
            ui::form_said("mark-seen")
        );
        if let Some(seen) = view.seen {
            let _ = write!(
                out,
                "{}<input type=\"hidden\" name=\"through\" value=\"0\"><span class=\"meta\">Seen through {} · <button class=\"quiet\">Undo</button></span>{}</form>",
                ui::post_form("undo-seen", "seen-form", &format!("{href}/seen")),
                ui::clock(&super::rfc3339(seen)),
                ui::form_said("undo-seen")
            );
        }
        out.push_str("</div>");
    } else {
        out.push_str(&ui::state(ui::Sketch::Lines, "Nothing finished yet", "", false).0);
    }
    let now = super::plan::now() as u64;
    let mut last_day = None;
    for unit in units.iter().skip(start).take(PAGE_SIZE) {
        let at = super::timestamp(unit.finished()).unwrap_or(0);
        let day = local_day(at, view.zone);
        if last_day != Some(day) {
            if last_day.is_some() {
                out.push_str("</ul></section>");
            }
            let _ = write!(
                out,
                "<section class=\"history-day\">{}<ul class=\"history-list\">",
                ui::SectionHead::new("", &day_label(at, now, view.zone)).html()
            );
            last_day = Some(day);
        }
        let step = super::plan::current(unit);
        let engine = step.map(|s| s.badge()).unwrap_or_default();
        out.push_str(&row(
            unit.finished(),
            super::plan::took(unit),
            &super::plan::unit_href(view, unit),
            unit.heading(),
            unit.steps.iter().map(|s| s.shown()).min().unwrap_or(Shown::Succeeded),
            &engine,
            view.seen.is_some_and(|seen| at > seen),
        ));
    }
    if last_day.is_some() {
        out.push_str("</ul></section>");
    }
    if units.len() > PAGE_SIZE {
        out.push_str("<nav class=\"history-pages\" aria-label=\"History pages\">");
        if page > 0 {
            let _ = write!(out, "<a href=\"{href}?page={}\">← Newer</a>", page - 1);
        }
        let _ = write!(
            out,
            "<span class=\"meta\">{}–{} of {}</span>",
            start + 1,
            (start + PAGE_SIZE).min(units.len()),
            units.len()
        );
        if start + PAGE_SIZE < units.len() {
            let _ = write!(out, "<a href=\"{href}?page={}\">Older →</a>", page + 1);
        }
        out.push_str("</nav>");
    }
    out.push_str("</div>");
    TrustedHtml::owned(out)
}
async fn draw(
    state: &DashboardState,
    project: ProjectId,
    page: usize,
    headers: &HeaderMap,
    seen: Option<u64>,
    registry: Option<&Registry>,
) -> Result<(super::DashboardSnapshot, ProjectView, RenderedBatch), PublicError> {
    let (shared, mut view) = super::board::snapshot(state, project, registry).await?;
    view.seen = seen
        .or_else(|| seen_of(headers, project))
        .filter(|s| *s > 0);
    view.zone = super::day::zone(headers);
    let nav = NavView::new(&shared, Some(project), "history")?;
    let mut regions = vec![PatchRegion::new("history-view", body(&view, page))];
    regions.extend(super::nav_regions(
        &nav,
        &format!("{}/history", view.href()),
    ));
    Ok((shared, view, RenderedBatch::new(regions)))
}
async fn page(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path(project): Path<ProjectId>,
    Query(query): Query<HistoryQuery>,
    headers: HeaderMap,
) -> Response {
    let drawn = async {
        let (shared, view, batch) = draw(&state, project, query.page, &headers, None, registry.as_ref().map(|r| &r.0)).await?;
        let href = format!("{}/history", view.href());
        let nav = NavView::new(&shared, Some(project), "history")?;
        let frame = Frame {
            head: ui::lead_head(
                "History",
                &TrustedHtml::default(),
                &TrustedHtml::default(),
                &TrustedHtml::default(),
            ),
            signals: serde_json::Map::from_iter([("seen".into(), view.seen.unwrap_or(0).into())]),
            ..Frame::default()
        };
        super::render_framed(
            &format!("History · {} · sluice", view.project.name),
            &batch.regions[0].html,
            &nav,
            &Viewer::from_headers(&headers),
            &format!("{href}/stream?page={}", query.page),
            &batch.version,
            &href,
            &frame,
        )
        .map_err(super::board::render_error)
    }
    .await;
    match drawn {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
async fn stream(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path(project): Path<ProjectId>,
    Query(page): Query<HistoryQuery>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let version = query.version(VersionSignal::Page);
    let seen = query.number("seen");
    let watch = state.watch(Some(project));
    let stop = state.stop.clone();
    let loader = move || {
        let state = state.clone();
        let headers = headers.clone();
        let registry = registry.clone();
        async move { Ok(draw(&state, project, page.page, &headers, seen, registry.as_ref().map(|r| &r.0)).await?.2) }
    };
    Sse::new(crate::streams::page_events(
        watch,
        loader,
        version,
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(axum::response::sse::KeepAlive::default())
    .into_response()
}
pub fn registration() -> super::PageRegistration {
    super::PageRegistration {
        routes: |state| {
            axum::Router::new()
                .route("/projects/id/{project}/history", axum::routing::get(page))
                .route(
                    "/projects/id/{project}/history/stream",
                    axum::routing::get(stream),
                )
                .route(
                    "/projects/id/{project}/history/seen",
                    axum::routing::post(mark_seen),
                )
                .with_state(state.dashboard.clone())
        },
        nav: |project| {
            project
                .map(|id| {
                    vec![super::NavEntry::new(
                        "history",
                        format!("/projects/id/{id}/history"),
                        "History",
                        11,
                    )]
                })
                .unwrap_or_default()
        },
        assets: &[],
    }
}

pub async fn mark_seen(
    Path(project): Path<ProjectId>,
    headers: axum::http::HeaderMap,
    Form(form): Form<SeenForm>,
) -> Response {
    let mut response = match super::write::form(&headers) {
        Some(id) => super::write::events([
            super::write::notice(&id, ""),
            crate::streams::StreamEvent::Signals(datastar::prelude::PatchSignals::new(
                serde_json::json!({ "seen": form.through }).to_string(),
            )),
        ]),
        None => StatusCode::NO_CONTENT.into_response(),
    };
    let cookie = format!(
        "{}={}; Path=/; Max-Age=34560000; SameSite=Lax",
        seen_cookie(project),
        form.through
    );
    response.headers_mut().append(
        axum::http::header::SET_COOKIE,
        cookie.parse().expect("an ASCII cookie"),
    );
    response
}
