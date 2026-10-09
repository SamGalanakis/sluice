//! A page that cannot be drawn, as a page: a browser that asks for HTML gets the layout with
//! what is missing (or what went wrong), why it may be gone and the ways back; a JSON client
//! keeps the JSON error.
use super::{DashboardState, NavView, TrustedHtml, Viewer};
use askama::Template;
use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use rusqlite::OptionalExtension;
use sluice_model::ids::ProjectId;

/// The browser asked for a page: a GET whose Accept names HTML (a script's fetch, a stream
/// and an MCP or tool call do not).
pub fn wants_page(method: &axum::http::Method, headers: &HeaderMap, path: &str) -> bool {
    method == axum::http::Method::GET
        && !path.starts_with("/static/")
        && !path.starts_with("/api/")
        && !path.starts_with("/mcp")
        && !path.ends_with("/stream")
        && accepts_html(headers)
}

/// Its sender reads HTML: a browser's page or form, not a script's fetch or a tool.
pub fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/html"))
}

/// What the address named, read from its path.
#[derive(Debug, Default)]
struct Named {
    project: Option<ProjectId>,
    step: Option<String>,
    unit: Option<String>,
    /// A run's file: `runs/<run>/files/<name>`.
    file: Option<(String, String)>,
    /// The address names a project by something that is not a project id (`/projects/id/nope`).
    bad_project: Option<String>,
}
fn named(path: &str) -> Named {
    let mut parts = path.trim_start_matches('/').split('/');
    let mut named = Named::default();
    if (parts.next(), parts.next()) != (Some("projects"), Some("id")) {
        return named;
    }
    let id = parts.next();
    named.project = id.and_then(|p| p.parse().ok());
    if named.project.is_none() {
        named.bad_project = id.filter(|p| !p.is_empty()).map(decode);
    }
    match (parts.next(), parts.next()) {
        (Some("steps"), Some(step)) => named.step = Some(decode(step)),
        (Some("units"), Some(unit)) => named.unit = Some(decode(unit)),
        (Some("runs"), Some(run)) => {
            if let (Some("files"), Some(file)) = (parts.next(), parts.next()) {
                named.file = Some((decode(run), decode(file)));
            }
        }
        _ => {}
    }
    named
}
fn decode(text: &str) -> String {
    url::form_urlencoded::parse(format!("x={text}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

#[derive(Template)]
#[template(path = "missing.html")]
struct Missing<'a> {
    status: u16,
    heading: &'a str,
    lead: &'a str,
    why: &'a str,
    message: &'a str,
    links: &'a [(String, String)],
}

/// The page for an error response `status` with `message` at `path`.
pub async fn page(
    state: &DashboardState,
    path: &str,
    status: StatusCode,
    message: &str,
    headers: &HeaderMap,
) -> Option<Response> {
    let snapshot = state.snapshot(None).await.ok()?;
    let mut named = named(path);
    // an id that is not one names no project: the same calm page as an id that is gone
    let status = if named.bad_project.is_some() && status == StatusCode::BAD_REQUEST {
        StatusCode::NOT_FOUND
    } else {
        status
    };
    let project = named
        .project
        .and_then(|id| snapshot.projects.iter().find(|p| p.id == id));
    if project.is_none() {
        named.step = None;
        named.unit = None;
        named.file = None;
    }
    // how long done units stay, as the project's settings set it
    let retire = match project {
        Some(p) => {
            let id = p.id.to_string();
            state
                .reads
                .snapshot(move |c| {
                    Ok(c.query_row(
                        "SELECT prune_done_after FROM projects WHERE project_id=?1",
                        [id],
                        |r| r.get::<_, Option<i64>>(0),
                    )
                    .optional()?
                    .flatten())
                })
                .await
                .ok()
                .flatten()
        }
        None => None,
    };
    let retire_words = retire.map(|s| {
        let hours = s as f64 / 3600.0;
        if hours.fract() == 0.0 {
            format!("{hours:.0}h")
        } else {
            format!("{hours:.1}h")
        }
    });
    let gone = match &retire_words {
        Some(after) => format!(
            "A plan edit may have removed it, or it was in a done unit: this project retires done units {after} after their last step finished."
        ),
        None => "A plan edit may have removed it.".to_owned(),
    };
    let mut links: Vec<(String, String)> = vec![];
    let (heading, lead, why) = match (project, &named.step, &named.unit) {
        (Some(p), Some(step), _) if status == StatusCode::NOT_FOUND => {
            links.push((p.href(), format!("Back to the {} plan", p.name)));
            links.push((
                format!("{}/log?step={}", p.href(), urlencode(step)),
                format!("Search the log for {step}"),
            ));
            (
                "No such step".to_owned(),
                format!("{} has no step {step} now.", p.name),
                gone,
            )
        }
        (Some(p), _, _) if named.file.is_some() && status == StatusCode::NOT_FOUND => {
            links.push((p.href(), format!("Back to the {} plan", p.name)));
            links.push((
                format!("{}/log?kind=run", p.href()),
                "See the runs in the log".to_owned(),
            ));
            (
                "No such run file".to_owned(),
                message.to_owned(),
                String::new(),
            )
        }
        (Some(p), _, Some(unit)) if status == StatusCode::NOT_FOUND => {
            links.push((p.href(), format!("Back to the {} plan", p.name)));
            links.push((
                format!("{}/log?kind=unit", p.href()),
                "See the units in the log".to_owned(),
            ));
            (
                "No such unit".to_owned(),
                format!("{} has no unit {unit} now.", p.name),
                gone,
            )
        }
        (Some(p), _, _) if status == StatusCode::NOT_FOUND => {
            links.push((p.href(), format!("Back to the {} plan", p.name)));
            (
                "Nothing here".to_owned(),
                format!("{} has no page at this address.", p.name),
                String::new(),
            )
        }
        (None, _, _)
            if (named.project.is_some() || named.bad_project.is_some())
                && status == StatusCode::NOT_FOUND =>
        {
            links.push(("/".into(), "All projects".into()));
            links.push(("/log".into(), "The log".into()));
            (
                "No such project".to_owned(),
                "No project has this id.".to_owned(),
                "It may have been deleted. Links keep a project's id when it is renamed."
                    .to_owned(),
            )
        }
        _ if status == StatusCode::NOT_FOUND => {
            links.push(("/".into(), "All projects".into()));
            (
                "Nothing here".to_owned(),
                "No page has this address.".to_owned(),
                String::new(),
            )
        }
        _ => {
            if let Some(p) = project {
                links.push((p.href(), format!("Back to the {} plan", p.name)));
            }
            links.push(("/".into(), "All projects".into()));
            (
                if status.is_server_error() {
                    "This page could not be drawn".to_owned()
                } else {
                    "This address cannot be shown".to_owned()
                },
                String::new(),
                String::new(),
            )
        }
    };
    let nav = NavView::new(&snapshot, project.map(|p| p.id), "").ok()?;
    let body = TrustedHtml::from_template(&Missing {
        status: status.as_u16(),
        heading: &heading,
        lead: &lead,
        why: &why,
        message,
        links: &links,
    })
    .ok()?;
    let html = super::render_layout(
        &heading,
        &body,
        &nav,
        &Viewer::from_headers(headers),
        "",
        "",
        path,
    )
    .ok()?;
    Some((status, Html(html.as_str().to_owned())).into_response())
}
fn urlencode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}
