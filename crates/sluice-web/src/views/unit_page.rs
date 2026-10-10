//! A unit's own page (DESIGN.md, A unit's page): its band carries its name huge, its id and
//! recipe, how its steps stand and its stage strip (its recipe's stages in recipe order; a unit
//! of no recipe its own steps in plan order; a one-step unit one cell). On the paper its steps
//! are modules on the grid, the one that needs the owner swelling, then its timeline and its
//! last message. Generic by rule: every stage is named as its recipe names it.
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
use std::{collections::BTreeSet, time::Duration};

/// A unit's steps in its strip's order: its recipe's stages first, in recipe order, then any
/// step no stage names; a unit of no recipe its steps in plan order.
pub fn ordered(unit: &UnitView) -> Vec<&StepView> {
    let mut out: Vec<&StepView> = unit
        .stages
        .iter()
        .filter_map(|stage| unit.stage_step(stage))
        .collect();
    for step in &unit.steps {
        if !out.iter().any(|s| s.id == step.id) {
            out.push(step);
        }
    }
    out
}
/// A step's cell in its unit's strip: its stage's name (else its id less its unit's), its
/// state, how long its run took or has run, and past its usual time by how far.
pub fn stage_of(unit: &UnitView, step: &StepView) -> Stage {
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
    if let (Some(usually), Some(t)) = (step.usually, step.timing.as_ref())
        && step.running()
        && t.finished.is_none()
        && usually > 0.0
        && t.seconds > 2.0 * usually
    {
        stage = stage.over(t.seconds / usually);
    }
    stage
}
/// The unit's stage strip's cells, in its strip's order.
pub fn stages(unit: &UnitView) -> Vec<Stage> {
    ordered(unit)
        .into_iter()
        .map(|s| stage_of(unit, s))
        .collect()
}

/// A stage strip's place on a band's grid of `room` columns: two columns a stage while half the
/// room holds them all, else a column a stage, wrapping past the room, so its cells sit on the
/// frame's columns. Its style: `--lane`, the
/// columns the strip spans, and `--lane-n`, its cells a row.
pub fn lane_style(stages: usize, room: usize) -> String {
    let stages = stages.max(1);
    let (span, cells) = if stages * 2 <= room {
        (stages * 2, stages)
    } else {
        (stages.min(room), stages.min(room))
    };
    format!("--lane:{span};--lane-n:{cells}")
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
    last: Option<&'a super::threads::Conversation>,
    tab: String,
    view: Option<TrustedHtml>,
    /// The view is short enough for the band's line, not a module of its own.
    view_short: bool,
    view_error: Option<&'a str>,
    /// Drawing the band (else the detail).
    band: bool,
}
impl UnitPage<'_> {
    fn ordered(&self) -> Vec<&StepView> {
        ordered(self.unit)
    }
    fn lane_style(&self) -> String {
        lane_style(self.unit.steps.len(), 12)
    }
    fn strip(&self) -> TrustedHtml {
        ui::stage_strip(&format!("Stages of {}", self.unit.id), &stages(self.unit))
    }
    /// Its name's size in the band, by how long its heading runs.
    fn title_size(&self) -> &'static str {
        match self.unit.whole_heading().chars().count() {
            0..=6 => "",
            7..=16 => " long",
            17..=40 => " longer",
            _ => " longest",
        }
    }
    /// A step's name inside its unit, the unit's title being in the band: its stage, else its
    /// id past the unit's ("draft" for a-12-draft), else its id.
    fn stage_name(&self, step: &StepView) -> String {
        if !step.stage.is_empty() {
            return step.stage.clone();
        }
        step.id
            .as_str()
            .strip_prefix(&format!("{}-", self.unit.id))
            .unwrap_or(step.id.as_str())
            .to_owned()
    }
    /// A step's own title, when it says something the unit's does not: "" when it has none
    /// or it is the unit's title again.
    fn own_title(&self, step: &StepView) -> String {
        let unit = [self.unit.title.as_str(), self.unit.whole.as_str()];
        if !step.titled()
            || unit.contains(&step.title.as_str())
            || unit.contains(&step.whole_heading())
        {
            String::new()
        } else {
            step.whole_heading().to_owned()
        }
    }
    /// A step's module: the question to the owner swells, a stop needs a look, the rest plain.
    fn module(&self, step: &StepView) -> TrustedHtml {
        let swell = if step.asking.is_some() {
            Swell::Ask
        } else if step.shown().spec().attention && step.shown() != Shown::Quiet {
            Swell::Look
        } else {
            Swell::Plain
        };
        let class = if step.running() {
            "u-step is-running"
        } else {
            "u-step"
        };
        ui::module_open_as(4, swell, &format!("us-{}", step.id), class)
    }
    /// What a step waits for in other units, in words, each linked.
    fn waits(&self, step: &StepView) -> TrustedHtml {
        let waits = self.unit.waits_of(step);
        if waits.is_empty() {
            return TrustedHtml::default();
        }
        let named: Vec<String> = waits
            .iter()
            .map(|w| {
                format!(
                    "<a href=\"{}\">{}{}</a>{}",
                    ui::esc(&w.href),
                    if w.unit { "unit " } else { "" },
                    w.name.id_first_html(40).as_str(),
                    if w.note.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", ui::esc(&w.note))
                    }
                )
            })
            .collect();
        let words = match named.as_slice() {
            [one] => one.clone(),
            [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
            [] => String::new(),
        };
        TrustedHtml::owned(format!("<p class=\"u-waits\">Waits for {words}</p>"))
    }
}
fn draw(
    view: &board::ProjectView,
    unit: &UnitView,
    last: Option<&super::threads::Conversation>,
) -> Result<Drawn, askama::Error> {
    let recipe = view.names.recipe_of(unit.id.as_str()).map(|r| r.as_ref());
    let drawn_view = recipe
        .and_then(|r| r.view())
        .map(|root| super::unit_view::draw(root, unit, false));
    let page = |band| UnitPage {
        unit,
        project: &view.project,
        last,
        tab: sluice_model::naming::cut(unit.heading(), 48),
        view_short: drawn_view.as_ref().is_some_and(super::unit_view::short),
        view: drawn_view.clone(),
        view_error: recipe.and_then(|r| r.view_error()),
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
    last: Option<&super::threads::Conversation>,
) -> Result<RenderedBatch, PublicError> {
    let unit = unit_of(view, unit)?;
    let drawn = draw(view, &unit, last).map_err(render_error)?;
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
/// A unit's last message as its page draws it: one message of the shared conversation, read
/// from the store by its id (none when the unit has no message).
async fn last_message(
    state: &super::DashboardState,
    view: &board::ProjectView,
    unit: &UnitName,
) -> Result<Option<super::threads::Conversation>, PublicError> {
    let Some(id) = view
        .units
        .iter()
        .find(|u| &u.id == unit)
        .map(|u| u.last_id)
        .filter(|id| *id > 0)
    else {
        return Ok(None);
    };
    let (project, name) = (view.project.id, view.project.name.clone());
    state
        .reads
        .snapshot(move |c| {
            let message =
                sluice_store::messages::message(c, project, sluice_model::ids::MessageId(id))?;
            let steps = super::threads::step_names(c, project)?;
            // a note sent to several within a minute is drawn as its thread draws it, once,
            // "to 6 steps": every copy of it is read
            let mut ids = vec![];
            if !message.is_question() {
                let mut q = c.prepare_cached("SELECT id FROM messages WHERE project_id=?1 AND thread=?2 AND \"from\"=?3 AND body=?4 AND coalesce(title,'')=?5 AND needs_reply=0 AND abs(julianday(at)-julianday(?6))*86400<=60 AND id!=?7 ORDER BY id")?;
                let rows = q.query_map(
                    rusqlite::params![
                        project.to_string(),
                        message.thread,
                        message.from,
                        message.body,
                        message.title.clone().unwrap_or_default(),
                        message.at,
                        id
                    ],
                    |r| r.get::<_, i64>(0),
                )?;
                ids = rows.collect::<Result<Vec<_>, _>>()?;
            }
            ids.push(id);
            ids.sort_unstable();
            let mut items = vec![];
            for each in ids {
                let message = if each == id {
                    message.clone()
                } else {
                    sluice_store::messages::message(c, project, sluice_model::ids::MessageId(each))?
                };
                items.push(super::threads::item(c, project, &name, &steps, message)?);
            }
            Ok(Some(super::threads::Conversation::build(
                super::threads::Build {
                    project,
                    subject: None,
                    steps: &steps,
                    unread: &BTreeSet::new(),
                    most: None,
                    excerpt: false,
                    href: &|m| super::threads::thread_url(project, &m.message.thread),
                },
                items,
            )))
        })
        .await
        .map_err(|e| e.into_public(true))
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
        let last = last_message(&state, &view, &unit).await?;
        let drawn = batch(&shared, &view, &unit, &viewer, last.as_ref())?;
        let nav = NavView::new(&shared, Some(project), "plan")?;
        let frame = super::Frame {
            head: drawn.regions[0].html.clone(),
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
        source = "{% if band %}<div id=\"unit-band\" class=\"unit-band\" data-gone><div class=\"band-head\"><h1>{{ unit }}</h1></div></div>{% else %}<div id=\"unit-detail\" class=\"gone\" data-gone><p class=\"d-gone\">This unit left the plan; it may have retired. <a href=\"{{ href }}\">Search the log for it</a></p></div>{% endif %}",
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
            let last = last_message(&state, &view, &id).await?;
            batch(&shared, &view, &id, &viewer, last.as_ref())
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
