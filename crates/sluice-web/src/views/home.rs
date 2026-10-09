use super::*;
use crate::streams::{PatchRegion, RenderedBatch};
use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
};
#[derive(Clone, Debug)]
pub struct HomeView {
    /// The live projects with steps, most urgent first: by how each reads (`ProjectView::shown`,
    /// the status table's order), by name within each.
    pub active: Vec<ProjectView>,
    /// The live projects with no steps yet, folded under the rest.
    pub empty: Vec<ProjectView>,
    pub archived: Vec<ProjectView>,
    pub runner_stopped: bool,
}
impl HomeView {
    pub fn new(snapshot: &DashboardSnapshot) -> Self {
        let live = snapshot.projects.iter().filter(|p| !p.archived);
        let mut active: Vec<ProjectView> =
            live.clone().filter(|p| p.counts.total() > 0).cloned().collect();
        active.sort_by_key(ProjectView::shown); // stable: by name within each
        Self {
            active,
            empty: live.filter(|p| p.counts.total() == 0).cloned().collect(),
            archived: snapshot
                .projects
                .iter()
                .filter(|p| p.archived)
                .cloned()
                .collect(),
            runner_stopped: snapshot.runner_stopped,
        }
    }
    /// The tab's words: the open questions to the owner, then what needs attention across the
    /// projects, counted ("2 questions · 2 failed · 1 quiet · Projects").
    pub fn title(&self) -> String {
        let mut all = ui::Tally::default();
        for project in &self.active {
            all += &project.standing();
        }
        let asks = self
            .active
            .iter()
            .chain(&self.empty)
            .map(|p| p.asks.len())
            .sum();
        let mut words: Vec<String> = questions_words(asks).into_iter().collect();
        words.extend(
            all.iter()
                .filter(|(s, _)| s.spec().attention)
                .map(|(s, n)| format!("{n} {}", s.word())),
        );
        words.push("Projects".into());
        words.join(" · ")
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&HomeTemplate { view: self })
    }
    pub fn render(
        &self,
        snapshot: &DashboardSnapshot,
        viewer: &Viewer,
    ) -> Result<TrustedHtml, PublicError> {
        let nav = NavView::new(snapshot, None, "")?;
        let drawn = batch(snapshot, None, false, viewer)?;
        render_layout(
            &self.title(),
            &drawn.regions[0].html,
            &nav,
            viewer,
            "/stream",
            &drawn.version,
            "/",
        )
        .map_err(template_error)
    }
}
#[derive(Template)]
#[template(path = "home.html")]
struct HomeTemplate<'a> {
    view: &'a HomeView,
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
        // a retired fn after the live ones
        functions: {
            let mut found: Vec<FunctionView> = snapshot
                .functions
                .entries
                .iter()
                .filter(|f| f.scope == scope)
                .cloned()
                .collect();
            found.sort_by_key(FunctionView::retired);
            found
        },
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
    let drawn = batch(snapshot, project, true, viewer)?;
    let path = project
        .map(|id| format!("/fns?project={id}"))
        .unwrap_or_else(|| "/fns".into());
    let stream = project
        .map(|id| format!("/fns/stream?project={id}"))
        .unwrap_or_else(|| "/fns/stream".into());
    render_layout(
        "Functions",
        &drawn.regions[0].html,
        &nav,
        viewer,
        &stream,
        &drawn.version,
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
/// replay safe even when the preceding connection applied only part of a batch. The page
/// draws its first region and carries the batch's version.
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
    Ok(RenderedBatch::new(vec![
        PatchRegion::new(if functions { "functions" } else { "projects" }, body),
        PatchRegion::new(
            "top-nav",
            render_nav(&nav, viewer, &path).map_err(template_error)?,
        ),
    ]))
}

async fn home_handler(State(state): State<DashboardState>, headers: HeaderMap) -> Response {
    let page = async {
        let snapshot = state.snapshot(None).await?;
        home::HomeView::new(&snapshot).render(&snapshot, &Viewer::from_headers(&headers))
    };
    match page.await {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
async fn functions_handler(
    State(state): State<DashboardState>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Response {
    let page = async {
        let snapshot = state.snapshot(query.project).await?;
        home::render_functions(&snapshot, query.project, &Viewer::from_headers(&headers))
    };
    match page.await {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => crate::http::error_response(e),
    }
}

#[derive(Deserialize)]
struct TitleQuery {
    project: Option<ProjectId>,
}
/// A page's tab title alone, as its stream would write it: what a hidden tab asks for while its
/// stream is closed (`nav.js`). The index's by default; a project's with `?project=`.
async fn title_handler(
    State(state): State<DashboardState>,
    Query(query): Query<TitleQuery>,
) -> Response {
    let title = async {
        let snapshot = state.snapshot(None).await?;
        let words = match query.project {
            None => home::HomeView::new(&snapshot).title(),
            Some(id) => snapshot
                .projects
                .iter()
                .find(|p| p.id == id)
                .ok_or_else(|| PublicError::NotFound {
                    message: "project not found".into(),
                })?
                .tab_words(),
        };
        Ok::<_, PublicError>(format!("{words} · sluice"))
    };
    match title.await {
        Ok(title) => (
            [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            title,
        )
            .into_response(),
        Err(e) => crate::http::error_response(e),
    }
}
pub fn registration() -> PageRegistration {
    PageRegistration {
        routes: |state| {
            Router::new()
                .route("/", get(home_handler))
                .route("/title", get(title_handler))
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
                names: &["components.js"],
                media_type: "text/javascript",
                bytes: include_bytes!("../../assets/components.js"),
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
