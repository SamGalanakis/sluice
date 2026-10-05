//! The v2 board. Relations retain their gate form and unit endpoints.
use super::step::{FieldView, StepView};
use super::{DashboardSnapshot, DashboardState, FunctionCatalog, NavView, TrustedHtml, Viewer};
use crate::streams::{self, PatchRegion, RenderedBatch, StreamQuery, VersionSignal};
use askama::Template;
use axum::{
    Extension,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response, Sse, sse::KeepAlive},
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sluice_model::{
    commands::StepStatus,
    error::PublicError,
    gates::{Gate, GateDecision, StateSnapshot, evaluate_step},
    ids::{ProjectId, StepId, UnitName},
    plan::{FnSignature, Plan, SignatureProvider},
    types::Type,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Endpoint {
    Step(StepId),
    Unit(UnitName),
    Input(String),
    Output(String),
}
impl Endpoint {
    pub fn key(&self) -> String {
        match self {
            Self::Step(id) => format!("s:{id}"),
            Self::Unit(id) => format!("u:{id}"),
            Self::Input(id) => format!("i:{id}"),
            Self::Output(id) => format!("o:{id}"),
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Handoff,
    Ordering,
    Condition,
    NegatedCondition,
    Unit,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Relation {
    pub from: Endpoint,
    pub to: Endpoint,
    pub kind: RelationKind,
    pub label: String,
    pub tolerant: bool,
    /// Its ends are not in one unit's box (a plan input or output is in none): the board shows
    /// it as a chip on the dependent (`Xref`), not as an edge drawn across other boxes.
    pub cross: bool,
}
impl Endpoint {
    /// The unit whose box holds this end; a plan input or output is in no box.
    fn unit<'a>(&'a self, unit_of: &BTreeMap<&StepId, &'a UnitName>) -> Option<&'a UnitName> {
        match self {
            Self::Step(id) => unit_of.get(id).copied(),
            Self::Unit(name) => Some(name),
            Self::Input(_) | Self::Output(_) => None,
        }
    }
}
pub fn relations(plan: &Plan) -> Vec<Relation> {
    let unit_of: BTreeMap<&StepId, &UnitName> = plan
        .units()
        .values()
        .flat_map(|u| u.steps.iter().map(move |s| (s, &u.name)))
        .collect();
    let mut out = relation_ends(plan);
    for relation in &mut out {
        let (from, to) = (
            relation.from.unit(&unit_of),
            relation.to.unit(&unit_of),
        );
        relation.cross = from.is_none() || from != to;
    }
    out
}
fn relation_ends(plan: &Plan) -> Vec<Relation> {
    let mut out = vec![];
    let endpoint = |reference: &sluice_model::gates::ValueRef| {
        let parts = reference.parts().expect("compiled reference");
        let label = std::iter::once(parts.name.clone())
            .chain(parts.fields)
            .collect::<Vec<_>>()
            .join(".");
        (
            parts
                .step
                .map(Endpoint::Step)
                .unwrap_or(Endpoint::Input(parts.name)),
            label,
        )
    };
    for (id, step) in plan.steps() {
        let to = Endpoint::Step(id.clone());
        for (input, binding) in &step.bindings {
            for reference in binding.references() {
                let (from, label) = endpoint(reference);
                out.push(Relation {
                    from,
                    to: to.clone(),
                    kind: RelationKind::Handoff,
                    label: format!("{label} → {input}"),
                    tolerant: false,
                    cross: false,
                });
            }
        }
        for gate in &step.after {
            let (from, kind, label, tolerant) = match gate {
                Gate::Step { id, accept_skip } => (
                    Endpoint::Step(id.clone()),
                    RelationKind::Ordering,
                    if *accept_skip { "?" } else { "after" }.into(),
                    *accept_skip,
                ),
                Gate::Unit { name, accept_skip } => (
                    Endpoint::Unit(name.clone()),
                    RelationKind::Unit,
                    format!("unit:{name}{}", if *accept_skip { "?" } else { "" }),
                    *accept_skip,
                ),
                Gate::Bool { reference, negate } => {
                    let (from, label) = endpoint(reference);
                    (
                        from,
                        if *negate {
                            RelationKind::NegatedCondition
                        } else {
                            RelationKind::Condition
                        },
                        format!("{}{label}", if *negate { "not " } else { "" }),
                        false,
                    )
                }
            };
            out.push(Relation {
                from,
                to: to.clone(),
                kind,
                label,
                tolerant,
                cross: false,
            });
        }
    }
    for (name, reference) in plan.outputs() {
        let (from, label) = endpoint(reference);
        out.push(Relation {
            from,
            to: Endpoint::Output(name.clone()),
            kind: RelationKind::Handoff,
            label,
            tolerant: false,
            cross: false,
        });
    }
    out
}
impl RelationKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::Ordering => "ordering",
            Self::Condition => "condition",
            Self::NegatedCondition => "negated_condition",
            Self::Unit => "unit",
        }
    }
}
/// A relation between boxes, shown on its dependent as a chip above its card ("after source")
/// that links to the source and lights it when hovered or focused. Its words say the kind, as
/// an input to the card: `after` for an ordering entry (`after unit:` for a unit gate), `if`
/// (and `if not`) for a condition, `from` and the output for a handoff, `?` when a skip counts
/// (dashed, as that edge is).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Xref {
    /// The source's and the dependent's node keys (`s:<step>`, `u:<unit>`, `i:<input>`, ...).
    pub from: String,
    pub to: String,
    pub kind: RelationKind,
    pub tolerant: bool,
    /// Said before the rest by a screen reader, not shown: "Handoff " (the other kinds say
    /// theirs in the shown words).
    pub said: &'static str,
    /// Muted before the source: "from ", "after ", "after unit:", "if ", "if not ".
    pub lead: String,
    /// The source: a step, a unit or a plan input.
    pub name: String,
    /// Muted after the source: a handoff's or condition's output ("/text"), or "?".
    pub tail: String,
    /// The whole relation, in words.
    pub title: String,
    pub href: String,
    /// A source step, which opens in the drawer.
    pub opens: String,
    /// Where a source step is when its card is not on the board as drawn ("in done unit
    /// build", "not shown in this view"), said after the title; empty when it is shown.
    pub away: String,
}
impl Xref {
    fn new(project: ProjectId, relation: &Relation) -> Self {
        let skip = if relation.tolerant { "?" } else { "" };
        let skips = if relation.tolerant {
            ", even if it is skipped"
        } else {
            ""
        };
        let (name, href, opens, step) = match &relation.from {
            Endpoint::Step(id) => (
                id.to_string(),
                format!("/projects/id/{project}/steps/{id}"),
                id.to_string(),
                true,
            ),
            Endpoint::Unit(id) => (
                id.to_string(),
                format!("/projects/id/{project}/units/{id}"),
                String::new(),
                false,
            ),
            Endpoint::Input(id) | Endpoint::Output(id) => {
                (id.to_string(), format!("#in-{id}"), String::new(), false)
            }
        };
        // A step's output follows its id; a plan input's reference is its own name.
        let output = |path: &str| {
            if step {
                (name.clone(), format!("/{path}"))
            } else {
                (path.to_owned(), String::new())
            }
        };
        let (said, lead, (name, tail), title) = match relation.kind {
            RelationKind::Handoff => {
                let (path, into) = relation
                    .label
                    .split_once(" → ")
                    .map_or((relation.label.as_str(), None), |(p, i)| (p, Some(i)));
                let (shown, tail) = output(path);
                let source = if step {
                    format!("{shown}{tail}")
                } else {
                    format!("plan input {shown}")
                };
                let title = match (into, &relation.to) {
                    (Some(input), _) => format!("Handoff: {source} → {input}"),
                    (None, Endpoint::Output(out)) => format!("Plan output {out}: {source}"),
                    (None, _) => format!("Handoff: {source}"),
                };
                // "from", as the drawer says of an input's source
                let said = if step { "Handoff " } else { "Plan input handoff " };
                (said, "from ".to_owned(), (shown, tail), title)
            }
            RelationKind::Ordering => (
                "",
                "after ".to_owned(),
                (name.clone(), skip.to_owned()),
                format!("After {name}{skips}"),
            ),
            RelationKind::Unit => (
                "",
                "after unit:".to_owned(),
                (name.clone(), skip.to_owned()),
                format!("After unit {name}{skips}"),
            ),
            RelationKind::Condition | RelationKind::NegatedCondition => {
                let negated = relation.kind == RelationKind::NegatedCondition;
                let path = if negated {
                    relation
                        .label
                        .strip_prefix("not ")
                        .unwrap_or(&relation.label)
                } else {
                    &relation.label
                };
                let (shown, tail) = output(path);
                let truth = if negated { "false" } else { "true" };
                let title = format!("If {shown}{tail} is {truth}");
                let lead = if negated { "if not " } else { "if " };
                ("", lead.to_owned(), (shown, tail), title)
            }
        };
        Self {
            from: relation.from.key(),
            to: relation.to.key(),
            kind: relation.kind.clone(),
            tolerant: relation.tolerant,
            said,
            lead,
            name,
            tail,
            title,
            href,
            opens,
            away: String::new(),
        }
    }
    pub fn html(&self) -> Result<TrustedHtml, askama::Error> {
        #[derive(Template)]
        #[template(
            source = "<a class=\"xref k-{{ x.kind.name() }}{% if x.tolerant %} tolerant{% endif %}\" href=\"{{ x.href }}\"{% if !x.opens.is_empty() %} data-opens=\"{{ x.opens }}\"{% endif %} data-from=\"{{ x.from }}\" data-to=\"{{ x.to }}\" title=\"{{ x.title }}{% if !x.away.is_empty() %} ({{ x.away }}){% endif %}\">{% if !x.said.is_empty() %}<span class=\"vh\">{{ x.said }}</span>{% endif %}{% if !x.lead.is_empty() %}<span class=\"xm\">{{ x.lead }}</span>{% endif %}<span class=\"xn\">{{ x.name }}</span>{% if !x.tail.is_empty() %}<span class=\"xm\">{{ x.tail }}</span>{% endif %}</a>",
            ext = "html"
        )]
        struct Chip<'a> {
            x: &'a Xref,
        }
        TrustedHtml::from_template(&Chip { x: self })
    }
}
/// On a source card (or a unit's label): how many dependents it has in other boxes, "→ n".
pub fn feeds_html(n: impl std::borrow::Borrow<usize>) -> String {
    let n = *n.borrow();
    format!(
        "<span class=\"xout\" title=\"{n} in other units after it\"><span aria-hidden=\"true\">→ {n}</span><span class=\"vh\">, {n} in other units after it</span></span>"
    )
}
/// The chips of the relations between boxes, by their dependent's node key (one chip for the
/// relations a dependent shows alike), and how many dependents each source has in other boxes.
type Crossings = (
    BTreeMap<String, Vec<Xref>>,
    BTreeMap<String, BTreeSet<String>>,
);
fn crossings(project: ProjectId, relations: &[Relation]) -> Crossings {
    let mut chips = BTreeMap::<String, Vec<Xref>>::new();
    let mut feeds = BTreeMap::<String, BTreeSet<String>>::new();
    for relation in relations.iter().filter(|r| r.cross) {
        let chip = Xref::new(project, relation);
        feeds
            .entry(chip.from.clone())
            .or_default()
            .insert(chip.to.clone());
        let on = chips.entry(chip.to.clone()).or_default();
        match on.iter_mut().find(|c| {
            (&c.from, &c.kind, c.tolerant, &c.lead, &c.name, &c.tail)
                == (
                    &chip.from,
                    &chip.kind,
                    chip.tolerant,
                    &chip.lead,
                    &chip.name,
                    &chip.tail,
                )
        }) {
            Some(same) => {
                same.title.push_str("; ");
                same.title.push_str(&chip.title);
            }
            None => on.push(chip),
        }
    }
    (chips, feeds)
}
#[derive(Clone, Debug, Serialize)]
pub struct UnitView {
    pub id: UnitName,
    pub tagged: bool,
    pub done: bool,
    pub settled: bool,
    pub steps: Vec<StepView>,
    pub rows: Vec<Vec<StepView>>,
    pub blocked: Vec<String>,
    pub last_message: String,
    pub changed: String,
    /// Each step's chips for its relations to other boxes, by step id.
    pub xrefs: BTreeMap<String, Vec<Xref>>,
    /// How many dependents in other boxes each step has, by step id, and the unit itself.
    pub feeds: BTreeMap<String, usize>,
    pub unit_feeds: usize,
}
impl UnitView {
    pub fn key(&self) -> String {
        format!("u:{}", self.id)
    }
    /// How many lanes the box lays side by side: its widest row's cards. A box of one lane
    /// takes a cell of the board's grid; a wider one spans the grid's row.
    pub fn lanes(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }
    pub fn xrefs_of(&self, step: &StepView) -> &[Xref] {
        self.xrefs
            .get(step.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    pub fn feeds_of(&self, step: &StepView) -> usize {
        self.feeds.get(step.id.as_str()).copied().unwrap_or(0)
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&UnitTemplate { unit: self })
    }
    pub fn page_body(&self) -> Result<TrustedHtml, askama::Error> {
        #[derive(Template)]
        #[template(
            source = "<div id=\"unit-detail\" class=\"board\"><div class=\"boxes boxed\">{{ unit.body()?|safe }}</div>{% if !unit.blocked.is_empty() %}<dl class=\"facts\"><div><dt>Waits on</dt><dd>{% for wait in unit.blocked %}<p>{{ wait }}</p>{% endfor %}</dd></div></dl>{% endif %}{% if !unit.last_message.is_empty() %}<section class=\"d-sec\"><h2>Last message</h2><p>{{ unit.last_message }}</p><p class=\"meta\">{{ unit.changed }}</p></section>{% endif %}</div>",
            ext = "html"
        )]
        struct UnitPage<'a> {
            unit: &'a UnitView,
        }
        TrustedHtml::from_template(&UnitPage { unit: self })
    }
    pub fn rank(&self) -> u8 {
        if self
            .steps
            .iter()
            .any(|s| matches!(s.status.as_str(), "failed" | "stale"))
        {
            0
        } else if self
            .steps
            .iter()
            .any(|s| s.status == "running" || s.mark == "external")
        {
            1
        } else if self.steps.iter().any(|s| s.ready && !s.paused) {
            2
        } else if self.done {
            4
        } else {
            3
        }
    }
}
#[derive(Template)]
#[template(path = "unit.html")]
struct UnitTemplate<'a> {
    unit: &'a UnitView,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProjectView {
    pub project: super::ProjectView,
    pub units: Vec<UnitView>,
    pub relations: Vec<Relation>,
    /// Each plan output's chips for the relations that feed it, by output name.
    pub output_xrefs: BTreeMap<String, Vec<Xref>>,
    pub inputs: Vec<FieldView>,
    pub outputs: Vec<FieldView>,
    pub revision: u64,
    pub query: String,
    pub order: String,
    pub show: String,
    /// The board's search (`q`): the words a shown step's id, doc or unit must all contain.
    pub q: String,
    /// With a search, how many steps it matched.
    pub matched: usize,
    /// The project's board, drawn beside the plan (`docs("board")`), when it has one.
    pub panel: Option<super::panel::Panel>,
}
impl ProjectView {
    pub fn new(
        project: super::ProjectView,
        plan: &Plan,
        state: &StateSnapshot,
        revision: u64,
    ) -> Self {
        let mut depth = BTreeMap::new();
        let mut blocked = BTreeSet::new();
        for id in plan.topological_order() {
            let d = plan
                .dependencies(id)
                .iter()
                .filter_map(|d| depth.get(d))
                .max()
                .copied()
                .map_or(0, |d: usize| d + 1);
            depth.insert(id.clone(), d);
            if matches!(state.status(id), StepStatus::Failed | StepStatus::Stale)
                || (state.status(id) == StepStatus::Pending
                    && plan.dependencies(id).iter().any(|d| blocked.contains(d)))
            {
                blocked.insert(id.clone());
            }
        }
        let relations = relations(plan);
        let (mut chips, feeds) = crossings(project.id, &relations);
        let units = plan
            .units()
            .values()
            .map(|unit| {
                let steps: Vec<_> = unit
                    .steps
                    .iter()
                    .map(|id| {
                        let mut view = StepView::new(project.id, plan, state, id);
                        view.blocked = view.status == "pending"
                            && view.mark != "paused"
                            && blocked.contains(id);
                        view
                    })
                    .collect();
                let mut rows = BTreeMap::<usize, Vec<StepView>>::new();
                for step in &steps {
                    rows.entry(depth[&step.id]).or_default().push(step.clone());
                }
                let xrefs = steps
                    .iter()
                    .filter_map(|s| Some((s.id.to_string(), chips.remove(&s.key())?)))
                    .collect();
                let feeds_of = |key: &str| feeds.get(key).map_or(0, BTreeSet::len);
                let step_feeds = steps
                    .iter()
                    .map(|s| (s.id.to_string(), feeds_of(&s.key())))
                    .filter(|(_, n)| *n > 0)
                    .collect();
                UnitView {
                    id: unit.name.clone(),
                    tagged: unit.tagged,
                    done: unit.done(state),
                    settled: unit.settled(plan, state),
                    blocked: steps.iter().flat_map(|s| s.waits.clone()).collect(),
                    steps,
                    rows: rows.into_values().collect(),
                    last_message: String::new(),
                    changed: project.changed.clone(),
                    xrefs,
                    feeds: step_feeds,
                    unit_feeds: feeds_of(&format!("u:{}", unit.name)),
                }
            })
            .collect::<Vec<_>>();
        let output_xrefs = plan
            .outputs()
            .keys()
            .filter_map(|n| Some((n.clone(), chips.remove(&format!("o:{n}"))?)))
            .collect();
        let mut view = Self {
            inputs: plan
                .inputs()
                .iter()
                .map(|(n, d)| {
                    FieldView::new(
                        n,
                        &d.ty.to_string(),
                        d.doc.as_deref().unwrap_or(""),
                        state.inputs.0.get(n).map(|v| v.as_value()),
                        "Plan input",
                    )
                })
                .collect(),
            outputs: plan
                .outputs()
                .iter()
                .map(|(n, r)| {
                    FieldView::reference(
                        n,
                        &plan
                            .reference_type(r)
                            .map(|t| t.to_string())
                            .unwrap_or_default(),
                        r,
                        plan,
                        state,
                    )
                })
                .collect(),
            project,
            units,
            relations,
            output_xrefs,
            revision,
            query: String::new(),
            order: "live".into(),
            show: "all".into(),
            q: String::new(),
            matched: 0,
            panel: None,
        };
        view.mark_away();
        view
    }
    /// Say on each chip where its source is when the board as drawn does not show its card: in
    /// a done unit (folded to its line) or left out by the view's filter or search.
    fn mark_away(&mut self) {
        let mut shown = BTreeMap::<String, Option<String>>::new();
        for unit in &self.units {
            shown.insert(unit.key(), None);
            for step in &unit.steps {
                let done = unit.done.then(|| unit.id.to_string());
                shown.insert(step.key(), done);
            }
        }
        let chips = self
            .units
            .iter_mut()
            .flat_map(|u| u.xrefs.values_mut())
            .chain(self.output_xrefs.values_mut())
            .flatten();
        for chip in chips {
            if !(chip.from.starts_with("s:") || chip.from.starts_with("u:")) {
                continue;
            }
            chip.away = match shown.get(&chip.from) {
                None => "not shown in this view".into(),
                Some(Some(unit)) => format!("in done unit {unit}"),
                Some(None) => String::new(),
            };
        }
    }
    pub fn href(&self) -> String {
        self.project.href()
    }
    /// The board under the same order and show, without its search: the search's clear link.
    pub fn clear_href(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("order", &self.order)
            .append_pair("show", &self.show);
        format!("{}?{}", self.href(), query.finish())
    }
    /// The project's description: its first block (a heading takes the block after it too),
    /// and the rest, which the page folds.
    pub fn about(&self) -> (TrustedHtml, Option<TrustedHtml>) {
        crate::markdown::render_folded(&self.project.description)
    }
    pub fn output_xrefs_of(&self, output: &FieldView) -> &[Xref] {
        self.output_xrefs
            .get(&output.name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    pub fn edges_json(&self) -> String {
        serde_json::to_string(&self.relations).expect("typed relations serialize")
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&ProjectTemplate {
            view: self,
            js_url: super::asset_url("sluice.js"),
            board_js_url: super::asset_url("board.js"),
        })
    }
    /// The part of the body a stream patches: everything before the drawer.
    pub fn region(&self) -> Result<TrustedHtml, askama::Error> {
        Ok(Self::board_region(&self.body()?))
    }
    fn board_region(body: &TrustedHtml) -> TrustedHtml {
        let end = body
            .as_str()
            .find("<sluice-drawer")
            .expect("owned template drawer boundary");
        TrustedHtml::owned(body.as_str()[..end].trim().to_owned())
    }
    /// The page body and the batch its stream patches: the board region and the nav,
    /// versioned by that HTML, so the page and its stream agree while nothing shown changes.
    pub fn draw(
        &self,
        shared: &DashboardSnapshot,
        viewer: &Viewer,
    ) -> Result<(TrustedHtml, RenderedBatch), PublicError> {
        let nav = NavView::new(shared, Some(self.project.id), "plan")?;
        let body = self.body().map_err(render_error)?;
        let batch = RenderedBatch::new(vec![
            PatchRegion::new("project-board", Self::board_region(&body)),
            PatchRegion::new(
                "top-nav",
                super::render_nav(&nav, viewer, &self.href()).map_err(render_error)?,
            ),
        ]);
        Ok((body, batch))
    }
    pub fn render(
        &self,
        shared: &DashboardSnapshot,
        viewer: &Viewer,
    ) -> Result<TrustedHtml, PublicError> {
        let nav = NavView::new(shared, Some(self.project.id), "plan")?;
        let (body, batch) = self.draw(shared, viewer)?;
        super::render_layout(
            &self.project.name,
            &body,
            &nav,
            viewer,
            &format!("{}/stream?{}", self.href(), self.query),
            &batch.version,
            &self.href(),
        )
        .map_err(render_error)
    }
}
#[derive(Template)]
#[template(path = "project.html")]
struct ProjectTemplate<'a> {
    view: &'a ProjectView,
    js_url: String,
    board_js_url: String,
}
pub(crate) fn render_error(error: askama::Error) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}
pub(crate) struct CatalogSignatures<'a>(pub(crate) &'a FunctionCatalog);
impl SignatureProvider for CatalogSignatures<'_> {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        let entry = self
            .0
            .entries
            .iter()
            .rev()
            .find(|f| f.name == name && f.error.is_empty())?;
        let mut sig = FnSignature {
            open: name == "core.external",
            ..FnSignature::default()
        };
        for p in &entry.inputs {
            sig.inputs.insert(
                p.name.clone(),
                Type::parse(&serde_json::Value::String(p.ty.clone())).ok()?,
            );
        }
        for p in &entry.outputs {
            sig.outputs.insert(
                p.name.clone(),
                Type::parse(&serde_json::Value::String(p.ty.clone())).ok()?,
            );
        }
        Some(sig)
    }
}
/// Each project's compiled plan, kept while its stored document and the signatures it was
/// compiled against stay the same: compiling a large plan costs more than the rest of its page.
#[derive(Clone, Default)]
pub struct PlanCache(std::sync::Arc<std::sync::Mutex<BTreeMap<ProjectId, CompiledPlan>>>);
struct CompiledPlan {
    signatures: String,
    doc: String,
    plan: std::sync::Arc<Plan>,
}
impl PlanCache {
    /// The plan `doc` compiled against `provider`, whose version is `signatures`.
    fn compile(
        &self,
        project: ProjectId,
        doc: String,
        signatures: &str,
        provider: &impl SignatureProvider,
    ) -> Result<std::sync::Arc<Plan>, PublicError> {
        let held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(compiled) = held.get(&project)
            && compiled.signatures == signatures
            && compiled.doc == doc
        {
            return Ok(compiled.plan.clone());
        }
        drop(held);
        let plan = std::sync::Arc::new(Plan::parse_json(doc.as_bytes(), provider).map_err(
            |e| PublicError::Invalid {
                message: "stored plan cannot be compiled".into(),
                errors: e.into_iter().map(|e| e.to_string()).collect(),
            },
        )?);
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(
            project,
            CompiledPlan {
                signatures: signatures.to_owned(),
                doc,
                plan: plan.clone(),
            },
        );
        Ok(plan)
    }
}
/// Load the board and its compiled plan in a single caller-owned transaction. The signature
/// provider must be the registry's exact compiled signatures, `signatures` its version. Every
/// step gets what its card shows; `detail` also gets its runs, thread and submissions (its
/// page).
pub fn load_board(
    c: &Connection,
    shared: &DashboardSnapshot,
    project: ProjectId,
    plans: &PlanCache,
    signatures: &str,
    provider: &impl SignatureProvider,
    detail: Option<&StepId>,
) -> sluice_store::Result<(ProjectView, std::sync::Arc<Plan>)> {
    let summary = shared
        .projects
        .iter()
        .find(|p| p.id == project)
        .ok_or_else(|| PublicError::NotFound {
            message: "project not found".into(),
        })?;
    let (revision, doc): (i64, String) = c.query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let plan = plans.compile(project, doc, signatures, provider)?;
    let mut state = sluice_store::plans::read_state(c, project)?;
    for (id, step) in plan.steps() {
        if state.status(id) == StepStatus::Pending
            && !step.is_external()
            && evaluate_step(&plan, &state, step) == GateDecision::Ready
        {
            let needs = step.needs.iter().map(|(n, a)| (n.clone(), *a)).collect();
            let fit = sluice_store::resources::fits(c, project, &needs)?;
            if !fit.blocked.is_empty() {
                state
                    .steps
                    .entry(id.clone())
                    .or_default()
                    .queued
                    .push(fit.reason);
            }
        }
    }
    let mut board = ProjectView::new(summary.clone(), &plan, &state, revision as u64);
    let mut last = last_messages(c, project, &board)?;
    // What a card shows; a step's runs, thread and submissions are its page's (load_detail).
    let mut cards = BTreeMap::new();
    let mut rows = c.prepare_cached("SELECT step_id,manual,total,done FROM steps WHERE project_id=?1")?;
    let mut found = rows.query([project.to_string()])?;
    while let Some(r) = found.next()? {
        let (manual, total, done): (bool, Option<i64>, i64) = (r.get(1)?, r.get(2)?, r.get(3)?);
        cards.insert(r.get::<_, String>(0)?, (manual, total, done));
    }
    let finishing = sluice_store::attempts::finishing(c, project)?;
    for unit in &mut board.units {
        if let Some((message, at)) = last.remove(unit.id.as_str()) {
            unit.last_message = message;
            unit.changed = at;
        }
        for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
            if let Some((manual, total, done)) = cards.get(step.id.as_str()) {
                step.manual = *manual;
                step.total = total.map(|n| n as usize);
                step.done = *done as usize;
            }
            step.finishing = finishing.get(&step.id).cloned();
            step.revision = revision as u64;
        }
        if let Some(id) = detail
            && let Some(step) = unit.steps.iter_mut().find(|s| &s.id == id)
        {
            super::step::load_detail(c, project, step)?;
            for drawn in unit.rows.iter_mut().flatten().filter(|s| &s.id == id) {
                *drawn = step.clone();
            }
        }
    }
    Ok((board, plan))
}
/// Each unit's last message (body and time): the newest from or to one of its steps, or in one
/// of its steps' threads. One pass over the project's messages, newest first.
fn last_messages(
    c: &Connection,
    project: ProjectId,
    board: &ProjectView,
) -> sluice_store::Result<BTreeMap<String, (String, String)>> {
    let mut unit_of = BTreeMap::new();
    let mut steps = c.prepare("SELECT step_id,coalesce(unit,step_id) FROM steps WHERE project_id=?1")?;
    let mut rows = steps.query([project.to_string()])?;
    while let Some(r) = rows.next()? {
        unit_of.insert(r.get::<_, String>(0)?, r.get::<_, String>(1)?);
    }
    let wanted: BTreeSet<&str> = board.units.iter().map(|u| u.id.as_str()).collect();
    let mut last = BTreeMap::new();
    let mut messages = c.prepare(
        "SELECT \"from\",\"to\",thread,body,at FROM messages WHERE project_id=?1 ORDER BY id DESC",
    )?;
    let mut rows = messages.query([project.to_string()])?;
    while last.len() < wanted.len()
        && let Some(r) = rows.next()?
    {
        let (from, to, thread): (String, Option<String>, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
        let units = [
            unit_of.get(&from),
            to.and_then(|to| unit_of.get(&to)),
            thread.strip_prefix("step-").and_then(|step| unit_of.get(step)),
        ];
        for unit in units.into_iter().flatten() {
            if wanted.contains(unit.as_str()) && !last.contains_key(unit) {
                last.insert(unit.clone(), (r.get(3)?, r.get(4)?));
            }
        }
    }
    Ok(last)
}
/// Exact registry signatures, including open/submitted ports, supplied by the
/// application. Display catalog ports alone cannot describe arbitrary open fns.
#[derive(Clone, Debug, PartialEq)]
pub struct RegistrySnapshot {
    pub version: String,
    pub functions: Vec<(String, FnSignature)>,
}
impl SignatureProvider for RegistrySnapshot {
    fn signature(&self, name: &str) -> Option<FnSignature> {
        self.functions
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, sig)| sig.clone())
    }
}
pub trait RegistrySource: Send + Sync {
    fn signatures(&self, project: ProjectId) -> Result<RegistrySnapshot, PublicError>;
}
#[derive(Clone)]
pub struct Registry(pub std::sync::Arc<dyn RegistrySource>);
/// The shared nav and the project's board from one store snapshot, with the runs' activity
/// observed once after it. Nothing that moves while the page renders can fail it.
pub async fn snapshot(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
) -> Result<(DashboardSnapshot, ProjectView), PublicError> {
    let (shared, view, _) = load(state, project, registry, None, false).await?;
    Ok((shared, view))
}
/// The project's page: the board with its panel (the project's board program, drawn), both
/// read in the one store snapshot.
pub async fn page_snapshot(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
    cache: Option<&super::panel::QueryCache>,
) -> Result<(DashboardSnapshot, ProjectView), PublicError> {
    let (shared, mut view, panel) = load(state, project, registry, None, true).await?;
    view.panel = super::panel::draw(state, project, panel, cache).await?;
    Ok((shared, view))
}
/// The board with one step's full detail (its runs, frozen inputs, thread and submissions), for
/// the step's page and drawer.
pub async fn step_snapshot(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
    step: &StepId,
) -> Result<(DashboardSnapshot, ProjectView, StepView), PublicError> {
    let (shared, view, _) = load(state, project, registry, Some(step.clone()), false).await?;
    let detail = view
        .units
        .iter()
        .flat_map(|u| &u.steps)
        .find(|s| &s.id == step)
        .cloned()
        .ok_or_else(|| PublicError::NotFound {
            message: "step not found".into(),
        })?;
    Ok((shared, view, detail))
}
async fn load(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
    detail: Option<StepId>,
    panel: bool,
) -> Result<
    (
        DashboardSnapshot,
        ProjectView,
        Option<super::panel::Loaded>,
    ),
    PublicError,
> {
    let exact = registry.map(|r| r.0.signatures(project)).transpose()?;
    let catalog = state.catalog.catalog(Some(project))?;
    let plans = state.plans.clone();
    let (shared, mut board, loaded) = state
        .reads
        .snapshot(move |c| {
            let shared = super::load_snapshot(c, catalog)?;
            let detail = detail.as_ref();
            let (board, plan) = if let Some(exact) = exact {
                let version = format!("registry:{}", exact.version);
                load_board(c, &shared, project, &plans, &version, &exact, detail)?
            } else {
                let version = format!("catalog:{}", shared.functions.version);
                let provider = CatalogSignatures(&shared.functions);
                load_board(c, &shared, project, &plans, &version, &provider, detail)?
            };
            let loaded = if panel {
                super::panel::gather(c, project, Some(&plan), &board, None)?
            } else {
                None
            };
            Ok((shared, board, loaded))
        })
        .await
        .map_err(|e| e.into_public(true))?;
    let shared = state.observe(shared).await?;
    let running = shared
        .projects
        .iter()
        .find(|p| p.id == project)
        .map(|p| p.running.as_slice())
        .unwrap_or_default();
    for unit in &mut board.units {
        for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
            step.quiet = running
                .iter()
                .find(|r| r.step == step.id.as_str())
                .is_some_and(|r| r.quiet);
        }
    }
    Ok((shared, board, loaded))
}
#[derive(Clone, Debug, Deserialize, Default)]
pub struct BoardQuery {
    pub order: Option<String>,
    pub show: Option<String>,
    pub tag: Option<String>,
    /// Words a step's id, doc or unit must all contain (any order, any case).
    pub q: Option<String>,
    pub format: Option<String>,
    pub all: Option<bool>,
    pub datastar: Option<String>,
}
impl BoardQuery {
    fn apply(&self, view: &mut ProjectView) -> Result<(), PublicError> {
        let order = self.order.as_deref().unwrap_or("live");
        let show = self.show.as_deref().unwrap_or("all");
        if !["live", "plan"].contains(&order)
            || !["all", "active", "attention", "done"].contains(&show)
            || self
                .format
                .as_deref()
                .is_some_and(|s| !["html", "mermaid"].contains(&s))
        {
            return Err(PublicError::BadRequest {
                message: "invalid board filter".into(),
            });
        }
        view.units.retain(|u| {
            (match show {
                "active" => !u.done,
                "attention" => u.rank() == 0,
                "done" => u.done,
                _ => true,
            }) && self.tag.as_deref().is_none_or(|tag| {
                tag.is_empty() || u.steps.iter().any(|s| s.tags.iter().any(|t| t == tag))
            })
        });
        view.order = order.into();
        view.show = show.into();
        let q: String = self.q.as_deref().unwrap_or("").trim().chars().take(200).collect();
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("order", order).append_pair("show", show);
        if let Some(tag) = &self.tag {
            query.append_pair("tag", tag);
        }
        if !q.is_empty() {
            query.append_pair("q", &q);
        }
        view.query = query.finish();
        if order == "live" {
            view.units.sort_by_key(UnitView::rank);
        }
        if !q.is_empty() {
            view.matched = search(&mut view.units, &q);
        }
        view.q = q;
        view.mark_away();
        Ok(())
    }
}
/// Keep the steps whose id, doc or unit id holds every word of `q` (case-insensitive, in any
/// order), and the units with one; rows left empty go. Returns how many steps matched.
fn search(units: &mut Vec<UnitView>, q: &str) -> usize {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    let mut matched = 0;
    units.retain_mut(|unit| {
        let unit_id = unit.id.as_str().to_lowercase();
        let hit = |step: &StepView| {
            let text = format!("{} {} {unit_id}", step.id.as_str(), step.doc).to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        };
        unit.steps.retain(|s| hit(s));
        for row in &mut unit.rows {
            row.retain(|s| hit(s));
        }
        unit.rows.retain(|row| !row.is_empty());
        matched += unit.steps.len();
        !unit.steps.is_empty()
    });
    matched
}
pub(crate) fn error_response(e: PublicError) -> Response {
    let code = match e {
        PublicError::NotFound { .. } => StatusCode::NOT_FOUND,
        PublicError::BadRequest { .. } | PublicError::Invalid { .. } => StatusCode::BAD_REQUEST,
        PublicError::Conflict { .. } => StatusCode::CONFLICT,
        PublicError::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (code, e.to_string()).into_response()
}
/// The board's `?format=mermaid`: the `plan_view` tool's Mermaid for the project.
async fn plan_mermaid(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
    all: bool,
) -> Result<String, PublicError> {
    let exact = registry.map(|r| r.0.signatures(project)).transpose()?;
    let catalog = state.catalog.catalog(Some(project))?;
    state
        .reads
        .snapshot(move |c| {
            let doc: String = c.query_row(
                "SELECT doc FROM plans WHERE project_id=?1",
                [project.to_string()],
                |r| r.get(0),
            )?;
            let plan = match &exact {
                Some(exact) => Plan::parse_json(doc.as_bytes(), exact),
                None => Plan::parse_json(doc.as_bytes(), &CatalogSignatures(&catalog)),
            }
            .map_err(|e| PublicError::Invalid {
                message: "stored plan cannot be compiled".into(),
                errors: e.into_iter().map(|e| e.to_string()).collect(),
            })?;
            sluice_runtime::dispatch_ext::render_plan_view(
                c,
                project,
                &plan,
                sluice_model::commands::PlanViewFormat::Mermaid,
                all,
            )
        })
        .await
        .map_err(|e| e.into_public(true))
}
pub async fn project_page(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path(project): Path<ProjectId>,
    Query(query): Query<BoardQuery>,
    headers: HeaderMap,
) -> Response {
    let registry = registry.as_ref().map(|r| &r.0);
    let mermaid = query.format.as_deref() == Some("mermaid");
    let page = async {
        let (shared, mut view) = if mermaid {
            snapshot(&state, project, registry).await?
        } else {
            page_snapshot(&state, project, registry, None).await?
        };
        query.apply(&mut view)?;
        if mermaid {
            let text = plan_mermaid(&state, project, registry, query.all.unwrap_or(false)).await?;
            return Ok((
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                text,
            )
                .into_response());
        }
        let html = view.render(&shared, &Viewer::from_headers(&headers))?;
        Ok(Html(html.0).into_response())
    };
    page.await.unwrap_or_else(error_response)
}
pub async fn project_stream(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path(project): Path<ProjectId>,
    Query(query): Query<BoardQuery>,
    headers: HeaderMap,
) -> Response {
    let viewer = Viewer::from_headers(&headers);
    let stop = state.stop.clone();
    let version = StreamQuery {
        project: Some(project),
        datastar: query.datastar.clone(),
    }
    .version(VersionSignal::Page);
    let cache = super::panel::SharedCache::default();
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        let query = query.clone();
        let registry = registry.clone();
        let cache = cache.clone();
        async move {
            let registry = registry.as_ref().map(|r| &r.0);
            let (shared, mut view) = page_snapshot(&state, project, registry, Some(&cache)).await?;
            query.apply(&mut view)?;
            Ok(view.draw(&shared, &viewer)?.1)
        }
    };
    Sse::new(streams::page_events(
        loader,
        version,
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}
pub async fn project_redirect(
    State(state): State<DashboardState>,
    Path(name): Path<String>,
) -> Response {
    let result = state
        .reads
        .snapshot(move |c| {
            let raw: Option<String> = c
                .query_row(
                    "SELECT project_id FROM projects WHERE name=?1 AND deleted_at IS NULL",
                    [name],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(raw)
        })
        .await;
    match result {
        Ok(Some(id)) => Redirect::temporary(&format!("/projects/id/{id}")).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => error_response(e.into_public(true)),
    }
}
/// One unit's page body and the batch its stream patches, versioned by that HTML.
fn unit_batch(
    shared: &DashboardSnapshot,
    view: &ProjectView,
    unit: &UnitName,
    viewer: &Viewer,
) -> Result<RenderedBatch, PublicError> {
    let unit = view
        .units
        .iter()
        .find(|u| &u.id == unit)
        .ok_or_else(|| PublicError::NotFound {
            message: "unit not found".into(),
        })?;
    let nav = NavView::new(shared, Some(view.project.id), "plan")?;
    Ok(RenderedBatch::new(vec![
        PatchRegion::new("unit-detail", unit.page_body().map_err(render_error)?),
        PatchRegion::new(
            "top-nav",
            super::render_nav(&nav, viewer, &format!("{}/units/{}", view.href(), unit.id))
                .map_err(render_error)?,
        ),
    ]))
}
pub async fn unit_page(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path((project, unit)): Path<(ProjectId, UnitName)>,
    headers: HeaderMap,
) -> Response {
    let page = async {
        let (shared, view) = snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?;
        let viewer = Viewer::from_headers(&headers);
        let drawn = unit_batch(&shared, &view, &unit, &viewer)?;
        let nav = NavView::new(&shared, Some(project), "plan")?;
        super::render_layout(
            unit.as_str(),
            &drawn.regions[0].html,
            &nav,
            &viewer,
            &format!("{}/units/{}/stream", view.href(), unit),
            &drawn.version,
            &format!("{}/units/{}", view.href(), unit),
        )
        .map_err(render_error)
    };
    match page.await {
        Ok(html) => Html(html.0).into_response(),
        Err(e) => error_response(e),
    }
}

pub async fn unit_stream(
    State(state): State<DashboardState>,
    registry: Option<Extension<Registry>>,
    Path((project, id)): Path<(ProjectId, UnitName)>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    let stop = state.stop.clone();
    let version = query.version(VersionSignal::Page);
    let viewer = Viewer::from_headers(&headers);
    let loader = move || {
        let state = state.clone();
        let registry = registry.clone();
        let id = id.clone();
        let viewer = viewer.clone();
        async move {
            let (shared, view) =
                snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?;
            unit_batch(&shared, &view, &id, &viewer)
        }
    };
    Sse::new(streams::page_events(
        loader,
        version,
        VersionSignal::Page,
        stop,
    ))
    .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
    .into_response()
}

pub fn registration() -> super::PageRegistration {
    use super::{Asset, NavEntry, PageRegistration};
    use axum::routing::get;
    PageRegistration {
        routes: |state| {
            axum::Router::new()
                .route("/projects/{name}", get(project_redirect))
                .route("/projects/id/{project}", get(project_page))
                .route("/projects/id/{project}/stream", get(project_stream))
                .route("/projects/id/{project}/units/{unit}", get(unit_page))
                .route(
                    "/projects/id/{project}/units/{unit}/stream",
                    get(unit_stream),
                )
                .route(
                    "/projects/id/{project}/steps/{step}",
                    get(super::step::step_page),
                )
                .route(
                    "/projects/id/{project}/steps/{step}/stream",
                    get(super::step::step_stream),
                )
                .route(
                    "/projects/id/{project}/steps/{step}/actions",
                    axum::routing::post(super::step::action),
                )
                .with_state(state.dashboard.clone())
        },
        nav: |project| {
            project
                .map(|id| {
                    vec![NavEntry::new(
                        "plan",
                        format!("/projects/id/{id}"),
                        "Plan",
                        10,
                    )]
                })
                .unwrap_or_default()
        },
        assets: &[Asset {
            names: &["sluice.js"],
            media_type: "text/javascript",
            bytes: include_bytes!("../../assets/sluice.js"),
        }],
    }
}
