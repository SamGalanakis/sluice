use super::*;
use crate::streams::{PatchRegion, RenderedBatch};
use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
};
#[derive(Clone, Debug)]
pub struct HomeView {
    pub active: Vec<ProjectView>,
    pub archived: Vec<ProjectView>,
    pub runner_stopped: bool,
}
impl HomeView {
    pub fn new(snapshot: &DashboardSnapshot) -> Self {
        Self {
            active: snapshot
                .projects
                .iter()
                .filter(|p| !p.archived)
                .cloned()
                .collect(),
            archived: snapshot
                .projects
                .iter()
                .filter(|p| p.archived)
                .cloned()
                .collect(),
            runner_stopped: snapshot.runner_stopped,
        }
    }
    pub fn title(&self) -> String {
        let failed: usize = self.active.iter().map(|p| p.counts.failed).sum();
        let quiet: usize = self
            .active
            .iter()
            .flat_map(|p| &p.running)
            .filter(|r| r.quiet)
            .count();
        format!(
            "{}{}Projects",
            if failed > 0 {
                format!("{failed} failed · ")
            } else {
                String::new()
            },
            if quiet > 0 {
                format!("{quiet} quiet · ")
            } else {
                String::new()
            }
        )
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&HomeTemplate { view: self })
    }
    pub fn render(
        &self,
        snapshot: &DashboardSnapshot,
        viewer: &Viewer,
    ) -> Result<TrustedHtml, askama::Error> {
        let nav = NavView::new(snapshot, None, "").expect("home has no project selector");
        render_layout(
            &self.title(),
            &self.body()?,
            &nav,
            viewer,
            "/stream",
            &snapshot.version(),
            "/",
        )
    }
}
#[derive(Template)]
#[template(path = "home.html")]
struct HomeTemplate<'a> {
    view: &'a HomeView,
}
/// A status's glyph (DESIGN.md, the Shape Carries It Rule): its Lucide icon, named for screen
/// readers by the status word.
pub fn glyph(status: &str) -> TrustedHtml {
    use super::icons::{Icon, icon};
    let (status, shape, class) = match status {
        "running" => ("running", Icon::LoaderCircle, "spin"),
        "succeeded" => ("succeeded", Icon::CircleCheck, ""),
        "manual" => ("manual", Icon::CircleDot, ""),
        "stale" => ("stale", Icon::RotateCw, ""),
        "failed" => ("failed", Icon::CircleX, ""),
        "paused" => ("paused", Icon::CirclePause, ""),
        "skipped" => ("skipped", Icon::CircleSlash, ""),
        "external" => ("external", Icon::SquareArrowOutUpRight, ""),
        _ => ("pending", Icon::CircleDashed, ""),
    };
    TrustedHtml::owned(format!(
        "<span class=\"g g-{status}\" role=\"img\" aria-label=\"{status}\">{}</span>",
        icon(shape, 16, class)
    ))
}
#[derive(Clone, Debug)]
pub struct FunctionGroup {
    pub title: String,
    pub functions: Vec<FunctionView>,
}
#[derive(Template)]
#[template(path = "functions.html")]
struct FunctionsTemplate<'a> {
    nav: &'a NavView,
    groups: &'a [FunctionGroup],
}
fn function_body(
    snapshot: &DashboardSnapshot,
    nav: &NavView,
) -> Result<TrustedHtml, askama::Error> {
    let groups = [
        ("builtin", "Built-in"),
        ("global", "Global"),
        ("project", "Project"),
    ]
    .into_iter()
    .filter(|(scope, _)| *scope != "project" || nav.selected.is_some())
    .map(|(scope, title)| FunctionGroup {
        title: title.into(),
        functions: snapshot
            .functions
            .entries
            .iter()
            .filter(|f| f.scope == scope)
            .cloned()
            .collect(),
    })
    .collect::<Vec<_>>();
    TrustedHtml::from_template(&FunctionsTemplate {
        nav,
        groups: &groups,
    })
}
pub fn render_functions(
    snapshot: &DashboardSnapshot,
    project: Option<ProjectId>,
    viewer: &Viewer,
) -> Result<TrustedHtml, PublicError> {
    let nav = NavView::new(snapshot, project, "functions")?;
    let body = function_body(snapshot, &nav).map_err(template_error)?;
    let path = project
        .map(|id| format!("/fns?project={id}"))
        .unwrap_or_else(|| "/fns".into());
    let stream = project
        .map(|id| format!("/fns/stream?project={id}"))
        .unwrap_or_else(|| "/fns/stream".into());
    render_layout(
        "Functions",
        &body,
        &nav,
        viewer,
        &stream,
        &snapshot.version(),
        &path,
    )
    .map_err(template_error)
}
fn template_error(error: askama::Error) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}
/// Render every target from the same snapshot. Stable target replacement makes
/// replay safe even when the preceding connection applied only part of a batch.
pub fn batch(
    snapshot: &DashboardSnapshot,
    project: Option<ProjectId>,
    functions: bool,
    viewer: &Viewer,
) -> Result<RenderedBatch, PublicError> {
    let nav = NavView::new(snapshot, project, if functions { "functions" } else { "" })?;
    let body = if functions {
        function_body(snapshot, &nav)
    } else {
        HomeView::new(snapshot).body()
    }
    .map_err(template_error)?;
    let path = if functions {
        project
            .map(|id| format!("/fns?project={id}"))
            .unwrap_or_else(|| "/fns".into())
    } else {
        "/".into()
    };
    Ok(RenderedBatch {
        version: snapshot.version(),
        regions: vec![
            PatchRegion::new(if functions { "functions" } else { "projects" }, body),
            PatchRegion::new(
                "top-nav",
                render_nav(&nav, viewer, &path).map_err(template_error)?,
            ),
        ],
    })
}

async fn home_handler(State(state): State<DashboardState>, headers: HeaderMap) -> Response {
    match state.snapshot(None).await {
        Ok(Some(snapshot)) => match home::HomeView::new(&snapshot)
            .render(&snapshot, &Viewer::from_headers(&headers))
        {
            Ok(html) => Html(html.0).into_response(),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(None) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
async fn functions_handler(
    State(state): State<DashboardState>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Response {
    match state.snapshot(query.project).await {
        Ok(Some(snapshot)) => {
            match home::render_functions(&snapshot, query.project, &Viewer::from_headers(&headers))
            {
                Ok(html) => Html(html.0).into_response(),
                Err(PublicError::NotFound { .. }) => StatusCode::NOT_FOUND.into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        Ok(None) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub fn registration() -> PageRegistration {
    PageRegistration {
        routes: |state| {
            Router::new()
                .route("/", get(home_handler))
                .route("/fns", get(functions_handler))
                .route("/stream", get(crate::streams::home_stream))
                .route("/fns/stream", get(crate::streams::functions_stream))
                .with_state(state.dashboard.clone())
        },
        nav: |project| {
            vec![NavEntry::new(
                "functions",
                project
                    .map(|id| format!("/fns?project={id}"))
                    .unwrap_or_else(|| "/fns".into()),
                "Functions",
                60,
            )]
        },
        assets: &[
            Asset {
                names: &["style.css", "dashboard.css"],
                media_type: "text/css",
                bytes: include_bytes!("../../assets/style.css"),
            },
            Asset {
                names: &["nav.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/nav.js"),
            },
            Asset {
                names: &["datastar-rocket-1.0.4.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/datastar-rocket-1.0.4.js"),
            },
            Asset {
                names: &["logo.svg"],
                media_type: "image/svg+xml",
                bytes: include_bytes!("../../assets/logo.svg"),
            },
            Asset {
                names: &["favicon.svg"],
                media_type: "image/svg+xml",
                bytes: include_bytes!("../../assets/favicon.svg"),
            },
        ],
    }
}
