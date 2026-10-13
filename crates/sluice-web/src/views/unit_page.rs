//! A unit's own page (DESIGN.md, A unit's page): its head carries its name, what identifies it
//! behind its ⋯ and how its steps stand. On the paper its recipe's view when it says more than a
//! line, then its step view (`unit_steps`), the whole unit step by step, and under it a module
//! for each step that needs the owner: a question on the coral rule, a stop on the sand with why
//! it stopped. Generic by rule: every stage is named as its recipe names it.
use super::board::{self, Registry, UnitView, render_error};
use super::step::StepView;
use super::ui::{self, Shown, Stage, Swell};
use super::{NavView, TrustedHtml, Viewer};
use crate::streams::{self, PatchRegion, RenderedBatch, StreamQuery, VersionSignal};
use askama::Template;
use axum::{
    extract::{Extension, Path, Query, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Response, Sse, sse::KeepAlive},
};
use sluice_model::{
    error::PublicError,
    ids::{ProjectId, UnitName},
};
use std::time::Duration;

/// A step's cell in its unit's strip: its stage's name (else its id less its unit's), its
/// state, and how long its run took or has run.
fn stage_of(unit: &UnitView, step: &StepView) -> Stage {
    let prefix = format!("{}-", unit.id);
    let name = if !step.stage.is_empty() {
        step.stage.clone()
    } else if step.id.as_str() == unit.id.as_str() {
        step.id.to_string()
    } else {
        step.id
            .as_str()
            .strip_prefix(&prefix)
            .unwrap_or(step.id.as_str())
            .to_owned()
    };
    let mut stage = Stage::new(name, Some(step.shown())).href(step.href());
    if let Some(t) = step.shown_timing() {
        stage = if t.finished.is_none() {
            stage.running_since(t.started.clone(), t.seconds)
        } else {
            stage.took(t.seconds)
        };
    }
    stage
}
/// The unit's stage strip's cells, in its strip's order.
pub fn stages(unit: &UnitView) -> Vec<Stage> {
    super::unit_steps::ordered(unit)
        .into_iter()
        .map(|s| stage_of(unit, s))
        .collect()
}

/// The unit's page as drawn: its band and its detail, the regions its stream patches.
struct Drawn {
    band: TrustedHtml,
    detail: TrustedHtml,
}
#[derive(Template)]
#[template(path = "unit_page.html")]
struct UnitPage<'a> {
    unit: &'a UnitView,
    project: &'a super::ProjectView,
    tab: String,
    view: Option<TrustedHtml>,
    /// The view is short enough for the band's line, not a module of its own.
    view_short: bool,
    view_error: Option<&'a str>,
    /// The view's outputs that identify (a merged hash), which its Details hold.
    held: Vec<(String, String)>,
    /// Its step view (`unit_steps`).
    steps: TrustedHtml,
    /// The open questions to the owner, each its words whole.
    questions: &'a [board::Question],
    /// Drawing the band (else the detail).
    band: bool,
}
impl UnitPage<'_> {
    /// What identifies it, behind its head's "⋯": its id, its recipe, its params and the
    /// view's outputs that identify (`unit_view::held`).
    fn details(&self) -> TrustedHtml {
        let mut details = ui::Details::new()
            .id("Unit", self.unit.id.as_str())
            .text("Recipe", &self.unit.recipe);
        for (name, value) in &self.unit.params {
            details = match ui::ValueSet::of_text(value) {
                ui::ValueSet::Detail => details.id(name, value),
                _ => details.text(name, value),
            };
        }
        for (name, value) in &self.held {
            details = details.id(name, value);
        }
        details.menu(self.unit.whole_heading())
    }
    /// The steps that need the owner, in the view's order: one that asks a question, one
    /// stopped (failed, cancelled, stale).
    fn needs(&self) -> Vec<&StepView> {
        super::unit_steps::ordered(self.unit)
            .into_iter()
            .filter(|s| s.asking.is_some() || Self::stopped(s))
            .collect()
    }
    fn stopped(step: &StepView) -> bool {
        step.shown().spec().attention && step.shown() != Shown::Quiet
    }
    /// A step's question to the owner whole, its title over its words, Answer under them;
    /// nothing when it asks none.
    fn question(&self, step: &StepView) -> TrustedHtml {
        let Some(ask) = step.asking.as_ref() else {
            return TrustedHtml::default();
        };
        let body = self
            .questions
            .iter()
            .find(|q| q.message == ask.message)
            .map(|q| crate::markdown::render(&q.body).0)
            .unwrap_or_default();
        TrustedHtml::owned(format!(
            "<p class=\"mod-body u-ask\">{}</p><div class=\"mod-body md\">{body}</div><div class=\"mod-actions\"><a class=\"primary\" href=\"{}\">Answer{}</a></div>",
            ui::esc(&ask.title),
            ui::esc(&ask.href(&step.project)),
            super::icons::icon(super::icons::Icon::ArrowRight, 16, "")
        ))
    }
    /// A step's module: the question to the owner swells on the coral rule, a stop on the sand.
    fn module(&self, step: &StepView) -> TrustedHtml {
        let swell = if step.asking.is_some() {
            Swell::Ask
        } else {
            Swell::Look
        };
        ui::module_open_as(6, swell, &format!("us-{}", step.id), "u-step")
    }
}
fn draw(view: &board::ProjectView, unit: &UnitView) -> Result<Drawn, askama::Error> {
    let recipe = view.names.recipe_of(unit.id.as_str()).map(|r| r.as_ref());
    let drawn_view = recipe
        .and_then(|r| r.view())
        .map(|root| super::unit_view::draw(root, unit, false))
        .filter(|drawn| !super::unit_view::empty(drawn));
    let held = recipe
        .and_then(|r| r.view())
        .map(|root| super::unit_view::held(root, unit))
        .unwrap_or_default();
    // its owner is its head's to say, once
    let mut steps = super::unit_steps::of_unit(unit, &view.relations, false, "");
    steps.owner.clear();
    let steps = steps.html();
    let page = |band| UnitPage {
        unit,
        project: &view.project,
        tab: sluice_model::naming::cut(unit.heading(), 48),
        view_short: drawn_view.as_ref().is_some_and(super::unit_view::short),
        view: drawn_view.clone(),
        view_error: recipe.and_then(|r| r.view_error()),
        held: held.clone(),
        steps: steps.clone(),
        questions: &view.questions,
        band,
    };
    Ok(Drawn {
        band: TrustedHtml::from_template(&page(true))?,
        detail: TrustedHtml::from_template(&page(false))?,
    })
}
/// The unit with its steps open, as its page draws it.
fn unit_of(view: &board::ProjectView, unit: &UnitName) -> Result<UnitView, PublicError> {
    view.units
        .iter()
        .find(|u| &u.id == unit)
        .cloned()
        .ok_or_else(|| PublicError::NotFound {
            message: format!("unit {unit} not found"),
        })
}
fn batch(
    shared: &super::DashboardSnapshot,
    view: &board::ProjectView,
    unit: &UnitName,
    viewer: &Viewer,
) -> Result<RenderedBatch, PublicError> {
    let unit = unit_of(view, unit)?;
    let drawn = draw(view, &unit).map_err(render_error)?;
    let nav = NavView::new(shared, Some(view.project.id), "plan")?;
    Ok(RenderedBatch::new(vec![
        PatchRegion::new("unit-band", drawn.band),
        PatchRegion::new("unit-detail", drawn.detail),
        PatchRegion::new(
            "top-nav",
            super::render_nav(&nav, viewer, &format!("{}/units/{}", view.href(), unit.id))
                .map_err(render_error)?,
        ),
    ]))
}
pub async fn unit_page(
    State(state): State<super::DashboardState>,
    registry: Option<Extension<Registry>>,
    Path((project, unit)): Path<(ProjectId, UnitName)>,
    headers: HeaderMap,
) -> Response {
    let page = async {
        let (shared, view) =
            board::snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?;
        let viewer = Viewer::from_headers(&headers);
        let drawn = batch(&shared, &view, &unit, &viewer)?;
        let nav = NavView::new(&shared, Some(project), "plan")?;
        let frame = super::Frame {
            head: drawn.regions[0].html.clone(),
            inner: true,
            ..Default::default()
        };
        super::render_framed(
            &format!(
                "{} · {}",
                sluice_model::naming::cut(view.names.naming.unit_title(unit.as_str()), 48),
                view.project.name
            ),
            &drawn.regions[1].html,
            &nav,
            &viewer,
            &format!("{}/units/{}/stream", view.href(), unit),
            &drawn.version,
            &format!("{}/units/{}", view.href(), unit),
            &frame,
        )
        .map_err(render_error)
    };
    match page.await {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => board::error_response(e),
    }
}
/// What a page drew is gone: one calm line where it was (`id`'s element), with a way on.
fn gone(id: &str, unit: &UnitName, href: &str) -> Result<TrustedHtml, askama::Error> {
    #[derive(Template)]
    #[template(
        source = "{% if band %}<div id=\"unit-band\" class=\"unit-band\" data-gone><div class=\"page-head\"><div class=\"ph-name\"><h1>{{ unit }}</h1></div></div></div>{% else %}<div id=\"unit-detail\" class=\"gone\" data-gone><p class=\"d-gone\">This unit left the plan; it may have retired. <a href=\"{{ href }}\">Search the log for it</a></p></div>{% endif %}",
        ext = "html"
    )]
    struct Gone<'a> {
        band: bool,
        unit: &'a UnitName,
        href: &'a str,
    }
    TrustedHtml::from_template(&Gone {
        band: id == "unit-band",
        unit,
        href,
    })
}
pub async fn unit_stream(
    State(state): State<super::DashboardState>,
    registry: Option<Extension<Registry>>,
    Path((project, id)): Path<(ProjectId, UnitName)>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let stop = state.stop.clone();
    let version = query.version(VersionSignal::Page);
    let viewer = Viewer::from_headers(&headers);
    let watch = state.watch(Some(project));
    let loader = move || {
        let state = state.clone();
        let registry = registry.clone();
        let id = id.clone();
        let viewer = viewer.clone();
        async move {
            let (shared, view) =
                board::snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?;
            // a unit that left the plan (retired, or edited out) is said where it was drawn,
            // and the stream stays open and quiet: its page never reconnects for it
            if !view.units.iter().any(|u| u.id == id) {
                let href = format!("{}/log?unit={id}", view.href());
                return Ok(RenderedBatch::new(vec![
                    PatchRegion::new(
                        "unit-band",
                        gone("unit-band", &id, &href).map_err(render_error)?,
                    ),
                    PatchRegion::new(
                        "unit-detail",
                        gone("unit-detail", &id, &href).map_err(render_error)?,
                    ),
                ]));
            }
            batch(&shared, &view, &id, &viewer)
        }
    };
    Sse::new(streams::page_events(
        watch,
        loader,
        version,
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
