use super::*;
use crate::streams::{PatchRegion, RenderedBatch};
#[derive(Clone, Debug)]
pub struct HomeView {
    pub active: Vec<ProjectView>,
    pub archived: Vec<ProjectView>,
    pub runner_stale: bool,
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
            runner_stale: snapshot.runner_stale,
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
pub fn glyph(status: &str) -> TrustedHtml {
    let (status, paths) = match status {
        "pending" => (
            "pending",
            "<circle cx=\"8\" cy=\"8\" r=\"5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-dasharray=\"2.6 2.2\"/>",
        ),
        "running" => (
            "running",
            "<circle cx=\"8\" cy=\"8\" r=\"5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" opacity=\".25\"/><path class=\"spin\" d=\"M8 2.5a5.5 5.5 0 0 1 5.5 5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/>",
        ),
        "succeeded" => (
            "succeeded",
            "<circle cx=\"8\" cy=\"8\" r=\"6.5\" fill=\"currentColor\"/><path d=\"M5.2 8.3l1.9 1.9 3.7-4.1\" fill=\"none\" stroke=\"var(--card)\" stroke-width=\"1.6\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",
        ),
        "manual" => (
            "manual",
            "<circle cx=\"8\" cy=\"8\" r=\"5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><circle cx=\"8\" cy=\"8\" r=\"2.6\" fill=\"currentColor\"/>",
        ),
        "stale" => (
            "stale",
            "<path d=\"M12.6 5.6A5.1 5.1 0 1 0 13.1 8.6\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/><path d=\"M13.2 2.7v3.4H9.8\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",
        ),
        "failed" => (
            "failed",
            "<circle cx=\"8\" cy=\"8\" r=\"6.5\" fill=\"currentColor\"/><path d=\"M5.9 5.9l4.2 4.2M10.1 5.9l-4.2 4.2\" fill=\"none\" stroke=\"var(--card)\" stroke-width=\"1.6\" stroke-linecap=\"round\"/>",
        ),
        "paused" => (
            "paused",
            "<circle cx=\"8\" cy=\"8\" r=\"5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\"/><path d=\"M6.6 5.9v4.2M9.4 5.9v4.2\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/>",
        ),
        "skipped" => (
            "skipped",
            "<circle cx=\"8\" cy=\"8\" r=\"5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-dasharray=\"2.6 2.2\"/><path d=\"M5.3 10.7l5.4-5.4\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\"/>",
        ),
        "external" => (
            "external",
            "<path d=\"M7 3.2H4.6a1.4 1.4 0 0 0-1.4 1.4v6.8a1.4 1.4 0 0 0 1.4 1.4h6.8a1.4 1.4 0 0 0 1.4-1.4V9M9.6 3.2h3.2v3.2M12.6 3.4L8 8\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>",
        ),
        _ => (
            "pending",
            "<circle cx=\"8\" cy=\"8\" r=\"5.5\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.5\" stroke-dasharray=\"2.6 2.2\"/>",
        ),
    };
    TrustedHtml::owned(format!(
        "<span class=\"g g-{status}\" role=\"img\" aria-label=\"{status}\"><svg viewBox=\"0 0 16 16\" width=\"16\" height=\"16\" aria-hidden=\"true\">{paths}</svg></span>"
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
