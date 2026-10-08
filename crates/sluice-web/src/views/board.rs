//! The v2 board. Relations retain their gate form and unit endpoints.
use super::step::{FieldView, RunTiming, StepView};
use super::ui::{Shown, Tally};
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
    shown::Band as Placed,
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
    /// Its ends are not in one unit's box (a plan input or output is in none).
    pub cross: bool,
    /// The board draws it as a line: a relation within a box, or a wait between units (see
    /// `ProjectView::settle`). One whose source the view leaves out is said in words on its
    /// dependent.
    pub line: bool,
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
                    line: false,
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
                line: false,
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
            line: false,
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
/// A prerequisite in another unit that a step still waits for, said in words on its card
/// where no line shows it: on a phone, without script, in a lane matrix's row (a table draws no
/// lines), past a matrix, or when the view leaves its source out ("Waits for l-a1 (running) and
/// l-d1").
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Wait {
    pub key: String,
    /// Its source as the page names it: a step's title (its stage before it) and id; a unit's
    /// title and id (`unit` says it is one).
    pub name: super::ui::StepRef,
    pub unit: bool,
    pub href: String,
    /// A source step, which opens in the drawer.
    pub opens: String,
    /// What the source is doing, when that is not just waiting itself: "running", "failed".
    pub note: String,
    /// Its source is on the board as shown (else the words say it is not in this view).
    pub shown: bool,
    /// A line draws it; else only the words say it.
    pub line: bool,
}
/// The words of a step's waits: "Waits for a (running), b and c". `said` when a line does not
/// say one of them, so the words show where the lines are drawn too. A name traces its source.
/// Past three sources the rest are a count, "and 79 more", which opens the step, whose page
/// lists every gate: the words stay a line or two at any fan-in.
pub fn waits_html(step: &StepView, waits: &[Wait]) -> Result<TrustedHtml, askama::Error> {
    const SHOWN: usize = 3;
    #[derive(Template)]
    #[template(
        source = "<p class=\"waits{% if said %} said{% endif %}\">Waits for {% for w in named %}{% if !loop.first %}{% if loop.last && more == 0 %} and {% else %}, {% endif %}{% endif %}<a href=\"{{ w.href }}\"{% if !w.opens.is_empty() %} data-opens=\"{{ w.opens }}\"{% endif %} data-from=\"{{ w.key }}\" data-to=\"{{ to }}\">{% if w.unit %}unit {% endif %}{{ w.name.html(40)|safe }}</a>{% let off = !w.shown && !none %}{% if !w.note.is_empty() || off %} ({{ w.note }}{% if off %}{% if !w.note.is_empty() %}, {% endif %}not in this view{% endif %}){% endif %}{% endfor %}{% if more > 0 %} and <a href=\"{{ href }}\" data-opens=\"{{ id }}\">{{ more }} more</a>{% endif %}{% if none %}, not in this view{% endif %}</p>",
        ext = "html"
    )]
    struct Words<'a> {
        to: String,
        href: String,
        id: &'a str,
        named: &'a [Wait],
        more: usize,
        said: bool,
        /// No source is in this view: said once, after them all.
        none: bool,
    }
    if waits.is_empty() {
        return Ok(TrustedHtml::owned(String::new()));
    }
    let shown = if waits.len() > SHOWN + 1 { SHOWN } else { waits.len() };
    TrustedHtml::from_template(&Words {
        to: step.key(),
        href: step.href(),
        id: step.id.as_str(),
        named: &waits[..shown],
        more: waits.len() - shown,
        said: waits.iter().any(|w| !w.line),
        none: waits.iter().all(|w| !w.shown),
    })
}
#[derive(Clone, Debug, Serialize)]
pub struct UnitView {
    pub id: UnitName,
    /// Its title (`sluice_model::naming`): "" when it has none and its id names it.
    pub title: String,
    /// The recipe it was made from, its stages and the params its steps give back ("" and
    /// empty when no recipe matches it).
    pub recipe: String,
    pub stages: Vec<String>,
    pub params: indexmap::IndexMap<String, String>,
    /// Its recipe has a view: while not done it is a row of its recipe's lane matrix.
    pub matrix: bool,
    /// Its place in the plan's order.
    pub pos: usize,
    pub tagged: bool,
    pub done: bool,
    pub settled: bool,
    pub steps: Vec<StepView>,
    pub rows: Vec<Vec<StepView>>,
    pub last_message: String,
    /// Who sent its last message, and its thread.
    pub last_from: String,
    pub last_thread: String,
    pub changed: String,
    /// The unit is one step in the plan: the board draws that step's card alone, no box.
    pub solo: bool,
    /// What each step still waits for in other units, by step id.
    pub waits: BTreeMap<String, Vec<Wait>>,
    /// A done unit a search matched in: drawn open, its matching cards in view.
    pub open: bool,
}
/// A part of the board under one label ("Stopped", "Running", "Waiting"; none under Plan
/// order): first its units that are rows of a recipe's lane matrix, a matrix a recipe; then the
/// rest in layers by dependency depth, so the lines between them run down.
pub struct Band<'a> {
    pub label: &'static str,
    pub matrices: Vec<Matrix<'a>>,
    pub layers: Vec<Vec<&'a UnitView>>,
    /// Its units no line joins to any other: after the layers, tallest first, packed from the
    /// start of the row (centring them would leave holes and imply a wait that is not there).
    pub loose: Vec<&'a UnitView>,
}
/// The board as drawn: the bands of live and pending units, then the done units on one shelf.
pub struct Layout<'a> {
    pub bands: Vec<Band<'a>>,
    /// The done units the shelf draws, the latest finished first: all of them under Show: Done
    /// or a search, else the latest `SHELF`.
    pub done: Vec<&'a UnitView>,
    /// Every done unit, and their steps: what the shelf's line counts.
    pub total: usize,
    pub total_steps: usize,
    /// The shelf drawn open: the view asks for done units (Show: Done) or a search matched in it.
    pub open: bool,
}
impl Band<'_> {
    /// Its element's id, a region a patch can draw alone: "band-running", "band-plan".
    pub fn id(&self) -> String {
        band_id(self.label)
    }
}
fn band_id(label: &str) -> String {
    match label {
        "" => "band-plan".into(),
        label => format!("band-{}", label.to_lowercase()),
    }
}
impl Layout<'_> {
    /// The done units the shelf leaves out; "Show all" draws them (Show: Done).
    pub fn more(&self) -> usize {
        self.total - self.done.len()
    }
    /// How the shelf reads: its units' first state (set by hand before succeeded before
    /// skipped).
    pub fn shown(&self) -> Shown {
        self.done
            .iter()
            .map(|u| u.shown())
            .min()
            .unwrap_or(Shown::Succeeded)
    }
}
/// How many done units the shelf draws unless every one is asked for: the latest finished.
pub const SHELF: usize = 20;
/// Each step's depth in the plan (the longest chain of what it waits on), and the steps held up
/// by a stopped step (`sluice_model::status::blocked`).
struct Depths {
    depth: BTreeMap<StepId, usize>,
    blocked: indexmap::IndexSet<StepId>,
}
impl Depths {
    fn new(plan: &Plan, state: &StateSnapshot) -> Self {
        let mut depth = BTreeMap::new();
        for id in plan.topological_order() {
            let d = plan
                .dependencies(id)
                .iter()
                .filter_map(|d| depth.get(d))
                .max()
                .copied()
                .map_or(0, |d: usize| d + 1);
            depth.insert(id.clone(), d);
        }
        Self {
            depth,
            blocked: sluice_model::status::blocked(plan, state),
        }
    }
}
impl UnitView {
    /// The unit as the plan and its state have it, before it is named and its cards read.
    fn new(
        project: ProjectId,
        changed: &str,
        plan: &Plan,
        state: &StateSnapshot,
        unit: &sluice_model::units::Unit,
        depths: &Depths,
    ) -> Self {
        let steps: Vec<_> = unit
            .steps
            .iter()
            .map(|id| {
                let mut view = StepView::new(project, plan, state, id);
                view.blocked = depths.blocked.contains(id);
                view
            })
            .collect();
        let mut rows = BTreeMap::<usize, Vec<StepView>>::new();
        for step in &steps {
            rows.entry(depths.depth[&step.id]).or_default().push(step.clone());
        }
        UnitView {
            id: unit.name.clone(),
            title: String::new(),
            recipe: String::new(),
            stages: vec![],
            params: indexmap::IndexMap::new(),
            matrix: false,
            pos: 0,
            tagged: unit.tagged,
            done: unit.done(state),
            settled: unit.settled(plan, state),
            steps,
            rows: rows.into_values().collect(),
            last_message: String::new(),
            last_from: String::new(),
            last_thread: String::new(),
            changed: changed.to_owned(),
            solo: unit.steps.len() == 1,
            waits: BTreeMap::new(),
            open: false,
        }
    }
    /// Name it and its steps (`sluice_runtime::naming`): titles, stages, its recipe and params;
    /// a unit whose recipe has a view is a row of that recipe's lane matrix.
    fn name(&mut self, names: &sluice_runtime::naming::ProjectNaming) {
        let id = self.id.to_string();
        if let Some(named) = names.naming.unit(&id) {
            self.title = named.title.clone();
            self.recipe = named.recipe.clone();
            self.stages = named.stages.clone();
            self.params = named.params.clone();
        }
        self.matrix = names
            .recipe_of(&id)
            .is_some_and(|r| r.view_source().is_some());
        for step in self.steps.iter_mut().chain(self.rows.iter_mut().flatten()) {
            if let Some(named) = names.naming.step(step.id.as_str()) {
                step.title = named.title.clone();
                step.stage = named.stage.clone();
            }
        }
    }
    /// Its steps counted by how each reads (`shown`).
    pub fn tally(&self) -> Tally {
        self.steps.iter().map(StepView::shown).collect()
    }
    /// How the whole unit reads: its steps' first state in the table's order (a failed step
    /// before a running one), pending when it has none.
    pub fn shown(&self) -> Shown {
        self.steps
            .iter()
            .map(StepView::shown)
            .min()
            .unwrap_or(Shown::Pending)
    }
    /// How tall its box draws, in rows of cards.
    pub fn height(&self) -> usize {
        if self.solo { 1 } else { self.rows.len() + 1 }
    }
    /// "1 running · 4 pending": every state it has, in priority order.
    pub fn tally_words(&self) -> String {
        super::ui::states_words(&self.tally())
    }
    /// What in it needs someone, for its label: "1 failed", "1 quiet"; "" when nothing does.
    pub fn alarm(&self) -> String {
        self.tally()
            .iter()
            .filter(|(s, _)| s.spec().attention)
            .map(|(s, n)| format!("{n} {}", s.word()))
            .collect::<Vec<_>>()
            .join(" · ")
    }
    /// A step in it needs someone (Show: Attention).
    pub fn needs_attention(&self) -> bool {
        self.steps.iter().any(|s| s.shown().spec().attention)
    }
    pub fn key(&self) -> String {
        format!("u:{}", self.id)
    }
    /// When its last step's run ended (RFC 3339), "" when none did (outputs set by hand).
    pub fn finished(&self) -> &str {
        self.steps
            .iter()
            .filter_map(|s| s.timing.as_ref()?.finished.as_deref())
            .max()
            .unwrap_or("")
    }
    /// How many lanes the box lays side by side: its widest row's cards.
    pub fn lanes(&self) -> usize {
        self.rows.iter().map(Vec::len).max().unwrap_or(0)
    }
    pub fn waits_of(&self, step: &StepView) -> &[Wait] {
        self.waits
            .get(step.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
    /// A one-step unit's card names the unit too when the step's id does not already
    /// (`build / compile`); `l-a1` in unit `l-a1`, or `fig-5216-work` in `fig-5216`, says it.
    pub fn names_unit(&self, step: &StepView) -> bool {
        self.solo && !step.id.as_str().starts_with(self.id.as_str())
    }
    /// The unit's steps as the board's lane strings write them: each step's id without the
    /// unit's prefix and its state's lane mark (`shown`), `fork✓ work✓ land✓`.
    pub fn lane(&self) -> String {
        let prefix = format!("{}-", self.id);
        self.steps
            .iter()
            .map(|s| {
                // a step named as its unit is the unit: its mark alone
                let short = if s.id.as_str() == self.id.as_str() {
                    ""
                } else {
                    s.id.as_str().strip_prefix(&prefix).unwrap_or(s.id.as_str())
                };
                format!("{short}{}", s.shown().spec().lane)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    /// The lane string drawn: each mark in a span of its own (`.lm`), in ink and weight, so a
    /// ✓ never reads as the pending dot beside the muted step names.
    pub fn lane_html(&self) -> String {
        self.lane()
            .split(' ')
            .map(|step| {
                let mut chars = step.chars();
                let mark = chars.next_back().unwrap_or(' ');
                let name = chars
                    .as_str()
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                format!("{name}<span class=\"lm\">{mark}</span>")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    /// The one line's words for a screen reader: "6 steps done" ("…, 1 skipped").
    pub fn done_words(&self) -> String {
        let skipped = self.tally().get(Shown::Skipped);
        let steps = super::ui::count(self.steps.len(), "step", "steps");
        if skipped > 0 {
            format!("{steps} done, {skipped} skipped")
        } else {
            format!("{steps} done")
        }
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&UnitTemplate {
            unit: self,
            shelf: false,
        })
    }
    /// Its line on the done shelf: a unit of several steps its fold, a one-step unit a row of
    /// its own (its title, its id, when it finished), never a pill.
    pub fn shelf_body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&UnitTemplate {
            unit: self,
            shelf: true,
        })
    }
    /// It has a title apart from its id.
    pub fn titled(&self) -> bool {
        !self.title.is_empty() && self.title != self.id.as_str()
    }
    /// Its heading's words: its title, or its id.
    pub fn heading(&self) -> &str {
        if self.title.is_empty() {
            self.id.as_str()
        } else {
            &self.title
        }
    }
    /// Its heading cut to `chars`: a link's accessible name stays short, the whole title its
    /// description.
    pub fn heading_cut(&self, chars: usize) -> String {
        sluice_model::naming::cut(self.heading(), chars)
    }
    /// A matrix row's header as a screen reader names it: its title cut to 72 characters, then
    /// its id.
    pub fn row_name(&self) -> String {
        if self.titled() {
            format!("{} {}", self.heading_cut(72), self.id)
        } else {
            self.id.to_string()
        }
    }
    pub fn href(&self, project: &ProjectId) -> String {
        format!("/projects/id/{project}/units/{}", self.id)
    }
    /// Its step at `stage` ("land"), when it has one.
    pub fn stage_step(&self, stage: &str) -> Option<&StepView> {
        self.steps.iter().find(|s| s.stage == stage)
    }
    /// Its timeline: a row a step (named by its stage, else its id less the unit's prefix), a
    /// bar a run; `current` marks the step whose page shows it. None before anything ran.
    pub fn timeline(&self, current: Option<&StepId>) -> Option<super::timeline::Timeline> {
        let prefix = format!("{}-", self.id);
        let lanes: Vec<super::timeline::Lane<'_>> = self
            .steps
            .iter()
            .map(|s| super::timeline::Lane {
                label: if !s.stage.is_empty() {
                    s.stage.clone()
                } else {
                    s.id.as_str().strip_prefix(&prefix).unwrap_or(s.id.as_str()).to_owned()
                },
                title: s.name().link_name(160),
                href: s.href(),
                current: current == Some(&s.id),
                spans: s.timing.as_ref().map_or(&[][..], |t| t.spans.as_slice()),
            })
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        super::timeline::Timeline::new(&lanes, now)
    }
    /// Every wait of its steps on other units, once each, and the first step that waits.
    pub fn all_waits(&self) -> Option<(&StepView, Vec<Wait>)> {
        let mut out: Vec<Wait> = vec![];
        let mut first = None;
        for step in &self.steps {
            for wait in self.waits_of(step) {
                first.get_or_insert(step);
                if !out.iter().any(|w| w.key == wait.key) {
                    out.push(wait.clone());
                }
            }
        }
        first.map(|step| (step, out))
    }
    /// Its stages as a lane string of links (a phone's matrix row): each stage's name and mark,
    /// opening its step.
    /// Its first step (in stage order) that failed or was cancelled: its row says why.
    pub fn why_step(&self) -> Option<&StepView> {
        self.steps.iter().find(|s| !s.why().is_empty())
    }
    pub fn lane_links_html(&self) -> String {
        self.steps
            .iter()
            .map(|s| {
                // a retry says its run: "land✗ (run 3)"
                let run = s.retries().map(|(n, _)| n);
                format!(
                    "<a href=\"{}\" data-opens=\"{}\" aria-label=\"{} {}{}\">{}<span class=\"lm\">{}</span>{}</a>",
                    s.href(),
                    s.id,
                    s.id,
                    s.shown().word(),
                    run.map(|n| format!(", run {n}")).unwrap_or_default(),
                    super::ui::esc(if s.stage.is_empty() { s.id.as_str() } else { &s.stage }),
                    s.shown().spec().lane,
                    run.map(|n| format!(" <span class=\"lm-run\">(run {n})</span>"))
                        .unwrap_or_default(),
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    /// The unit's own page: a way back to the plan, its id as the page's heading, its cards
    /// (a done unit open), what it waits on and its last message.
    pub fn page_body(
        &self,
        project: &super::ProjectView,
        edges: &str,
        recipe: Option<&sluice_model::recipe::Recipe>,
    ) -> Result<TrustedHtml, askama::Error> {
        #[derive(Template)]
        #[template(path = "unit_page.html")]
        struct UnitPage<'a> {
            unit: &'a UnitView,
            project: &'a super::ProjectView,
            edges: &'a str,
            js_url: String,
            tab: String,
            view: Option<TrustedHtml>,
            view_error: Option<&'a str>,
        }
        TrustedHtml::from_template(&UnitPage {
            unit: self,
            project,
            edges,
            js_url: super::asset_url("sluice.js"),
            tab: sluice_model::naming::cut(self.heading(), 48),
            view: recipe
                .and_then(|r| r.view())
                .map(|root| super::unit_view::draw(root, self, false)),
            view_error: recipe.and_then(|r| r.view_error()),
        })
    }
    /// Its last message's thread, where the unit page's "Last message" leads.
    pub fn last_thread_href(&self, project: &ProjectId) -> String {
        super::threads::thread_url(*project, &self.last_thread)
    }
    /// Its steps' records and messages, and its own, on the project's log.
    pub fn log_href(&self, project: &ProjectId) -> String {
        format!("/projects/id/{project}/log?unit={}", self.id)
    }
    /// Its last message, drawn as message bodies are.
    pub fn last_message_html(&self) -> TrustedHtml {
        crate::markdown::render(&self.last_message)
    }
    /// The band Live first draws it in (`Band`): its first state's; a done unit is on the shelf.
    pub fn band(&self) -> Placed {
        if self.done {
            Placed::Done
        } else {
            self.shown().spec().band
        }
    }
}
/// One recipe's live units as a lane matrix: a row a unit, a column a stage; its rows the ones
/// that need attention first, then running, then waiting, plan order within each.
pub struct Matrix<'a> {
    pub recipe: String,
    pub stages: Vec<String>,
    pub rows: Vec<&'a UnitView>,
    pub view: Option<&'a sluice_model::openui::Component>,
    pub view_error: Option<&'a str>,
    /// Its head's level: an `h3` under its band's label, an `h2` where no label is drawn.
    pub level: u8,
    /// Its band's id: a recipe may have a matrix in more than one band.
    pub band: String,
}
impl Matrix<'_> {
    /// Its element's id, a region a patch can draw alone: "mx-band-running-lane".
    pub fn id(&self) -> String {
        format!("mx-{}-{}", self.band, self.recipe)
    }
    /// "14 units · 1 failed · 3 quiet · 9 running · 1 paused": its rows counted by how each
    /// reads (`UnitView::shown`), each row once.
    pub fn tally(&self) -> String {
        let mut parts = vec![(
            self.rows.len(),
            if self.rows.len() == 1 { "unit" } else { "units" },
        )];
        parts.extend(super::ui::states(
            &self.rows.iter().map(|u| u.shown()).collect(),
        ));
        super::ui::tally(&parts)
    }
    /// Every unit its recipe made, done ones too: where its head leads (the matrix itself
    /// stays live units only).
    pub fn recipe_href(&self, project: &ProjectId) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("recipe", &self.recipe).append_pair("show", "all");
        format!("/projects/id/{project}?{}", query.finish())
    }
    /// What its stage's column has that needs a look, for the column's head: "2 failed · 1
    /// quiet"; "" when nothing does.
    pub fn stage_words(&self, stage: &str) -> String {
        let tally: Tally = self
            .rows
            .iter()
            .filter_map(|u| u.stage_step(stage))
            .map(StepView::shown)
            .filter(|s| s.spec().attention)
            .collect();
        super::ui::states_words(&tally)
    }
    /// The summary column's head, from what its view shows: "Ticket · last message".
    pub fn summary_head(&self) -> String {
        self.view
            .map(super::unit_view::head)
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "Summary".into())
    }
    /// A row's summary: its recipe's view drawn for the unit.
    pub fn summary(&self, unit: &UnitView) -> TrustedHtml {
        match self.view {
            Some(root) => super::unit_view::draw(root, unit, true),
            None => TrustedHtml::owned(String::new()),
        }
    }
    pub fn html(&self, project: &ProjectId) -> Result<TrustedHtml, askama::Error> {
        #[derive(Template)]
        #[template(path = "matrix.html")]
        struct MatrixTemplate<'a> {
            m: &'a Matrix<'a>,
            project: &'a ProjectId,
        }
        TrustedHtml::from_template(&MatrixTemplate { m: self, project })
    }
}
/// What the board needs of the whole plan, whatever the view shows: each step's unit and how
/// it reads, which units are done, and which units each unit comes after.
#[derive(Clone, Debug, Default)]
struct PlanFacts {
    /// By step key (`s:<id>`): its unit and how it reads.
    steps: BTreeMap<String, (String, Shown)>,
    done: BTreeSet<String>,
    /// By unit: the units a relation into one of its steps comes from, while its source has
    /// not succeeded or been skipped.
    after: BTreeMap<String, BTreeSet<String>>,
    /// By step id: the steps it comes after (its gates, a unit's expanded to its steps).
    deps: BTreeMap<String, Vec<String>>,
}
impl PlanFacts {
    /// The unit an end is in; a plan input or output is in none.
    fn unit_of(&self, key: &str) -> Option<String> {
        match key.split_once(':') {
            Some(("s", _)) => self.steps.get(key).map(|(u, _)| u.clone()),
            Some(("u", unit)) => Some(unit.to_owned()),
            _ => None,
        }
    }
}
#[derive(Template)]
#[template(path = "unit.html")]
struct UnitTemplate<'a> {
    unit: &'a UnitView,
    /// Drawn on the done shelf.
    shelf: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProjectView {
    pub project: super::ProjectView,
    pub units: Vec<UnitView>,
    pub relations: Vec<Relation>,
    #[serde(skip)]
    facts: PlanFacts,
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
    /// Chain focus (`?root=<step>&up=1&down=1&depth=N`): the step whose ancestors (`up`) and
    /// dependents (`down`), `depth` steps away at most (0: all), the board shows; "" for none.
    pub root: String,
    pub up: bool,
    pub down: bool,
    pub depth: usize,
    /// A recipe's units, done ones too (`?recipe=lane&show=all`); "" for every unit.
    pub recipe: String,
    /// How many units the view's Show leaves out.
    pub hidden: usize,
    /// The project's board, drawn beside the plan (`docs("board")`), when it has one.
    pub panel: Option<super::panel::Panel>,
    /// Its steps' and units' names, and the recipes they came from.
    #[serde(skip)]
    pub names: std::sync::Arc<sluice_runtime::naming::ProjectNaming>,
    /// How long each recipe's stage usually takes, seconds, by recipe and stage: worked out
    /// once a read (`usual_durations`), each step looking its own up. Nothing is stored.
    #[serde(skip)]
    pub usual: BTreeMap<(String, String), f64>,
}
impl ProjectView {
    pub fn new(
        project: super::ProjectView,
        plan: &Plan,
        state: &StateSnapshot,
        revision: u64,
    ) -> Self {
        let depths = Depths::new(plan, state);
        let relations = relations(plan);
        let units = plan
            .units()
            .values()
            .map(|unit| UnitView::new(project.id, &project.changed, plan, state, unit, &depths))
            .collect::<Vec<_>>();
        let mut units = units;
        for (pos, unit) in units.iter_mut().enumerate() {
            unit.pos = pos;
        }
        let mut facts = PlanFacts::default();
        for id in plan.topological_order() {
            facts.deps.insert(
                id.to_string(),
                plan.dependencies(id).iter().map(ToString::to_string).collect(),
            );
        }
        for unit in &units {
            if unit.done {
                facts.done.insert(unit.id.to_string());
            }
            for step in &unit.steps {
                facts
                    .steps
                    .insert(step.key(), (unit.id.to_string(), step.shown()));
            }
        }
        // what each unit still waits for: a satisfied source puts no unit above another
        let through = |key: &str| {
            facts
                .steps
                .get(key)
                .is_some_and(|(_, shown)| shown.spec().band == Placed::Done)
        };
        let waits: Vec<_> = relations
            .iter()
            .filter(|r| r.cross && !through(&r.from.key()))
            .collect();
        for relation in waits {
            let ends = (
                facts.unit_of(&relation.from.key()),
                facts.unit_of(&relation.to.key()),
            );
            if let (Some(from), Some(to)) = ends
                && from != to
            {
                facts.after.entry(to).or_default().insert(from);
            }
        }
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
                        "A plan input",
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
            facts,
            revision,
            query: String::new(),
            order: "live".into(),
            show: "all".into(),
            q: String::new(),
            matched: 0,
            root: String::new(),
            up: false,
            down: false,
            depth: 0,
            recipe: String::new(),
            hidden: 0,
            panel: None,
            names: Default::default(),
            usual: BTreeMap::new(),
        };
        view.settle();
        view
    }
    /// Name its steps and units (`sluice_runtime::naming`): titles, stages, recipes and their
    /// params; a unit whose recipe has a view becomes a row of that recipe's lane matrix.
    pub fn name(&mut self, names: std::sync::Arc<sluice_runtime::naming::ProjectNaming>) {
        for unit in &mut self.units {
            unit.name(&names);
        }
        self.names = names;
        self.settle();
    }
    /// Where the board draws a unit not done, top down: its band's matrix (even) or the band's
    /// layers after it (odd); a done unit is on the shelf (`None`). Live first draws Stopped,
    /// Running, Waiting; Plan order one band.
    fn place(&self, unit: &UnitView) -> Option<i32> {
        if unit.done {
            return None;
        }
        let band = if self.order == "live" {
            unit.band() as i32
        } else {
            0
        };
        Some(band * 2 + i32::from(!unit.matrix))
    }
    /// Mark the relations the board draws as lines, and say on each step what it still waits
    /// for in other units. Within a box every relation is a line. Between units a line is a
    /// wait: it joins a source not yet through (succeeded or skipped, or its unit done) to a
    /// step the view shows in a unit not done, drawn above it or beside it (a line never runs
    /// up). A lane matrix is a table, not a graph: no line leaves, enters or passes one. A wait
    /// no line draws (to or from a matrix's row, across a matrix, or up the board) is said in
    /// words on the step that waits. A satisfied source is neither drawn nor said: the step's
    /// page lists every gate.
    fn settle(&mut self) {
        let facts = &self.facts;
        let names = &self.names;
        let project = self.project.id;
        let mut shown = BTreeSet::new();
        let mut places = BTreeMap::new();
        for unit in &self.units {
            shown.insert(unit.key());
            shown.extend(unit.steps.iter().map(StepView::key));
            if let Some(place) = self.place(unit) {
                places.insert(unit.id.to_string(), place);
            }
        }
        // the places a matrix is drawn at
        let matrices: BTreeSet<i32> = places.values().copied().filter(|p| p % 2 == 0).collect();
        // an end a line can reach: shown and live (a plan input or output is always drawn)
        let live = |key: &str| match facts.unit_of(key) {
            Some(unit) => shown.contains(key) && !facts.done.contains(&unit),
            None => true,
        };
        // where an end is drawn: its unit's place; a plan input above every band, a plan
        // output under them
        let at = |key: &str| match facts.unit_of(key) {
            Some(unit) => places.get(&unit).copied(),
            None if key.starts_with("i:") => Some(-1),
            None => Some(i32::MAX),
        };
        // two ends a line may join: neither a matrix's row, the source not drawn under the
        // step that waits for it (a line never runs up: a stopped unit waiting on a running
        // one says so in words), and no matrix drawn between them
        let clear = |from: &str, to: &str| match (at(from), at(to)) {
            (Some(a), Some(b)) => {
                a <= b
                    && !matrices.contains(&a)
                    && !matrices.contains(&b)
                    && !matrices.iter().any(|m| a < *m && *m < b)
            }
            _ => false,
        };
        // a source already through: its step succeeded or was skipped, or its unit is done
        let through = |key: &str| match key.split_once(':') {
            Some(("s", _)) => facts
                .steps
                .get(key)
                .is_some_and(|(_, shown)| shown.spec().band == Placed::Done),
            Some(("u", unit)) => facts.done.contains(unit),
            _ => false,
        };
        // a matrix row's stages are its columns: the order inside it needs no line
        let rows: BTreeSet<String> = self
            .units
            .iter()
            .filter(|u| u.matrix && !u.done)
            .map(|u| u.id.to_string())
            .collect();
        for relation in &mut self.relations {
            let (from, to) = (relation.from.key(), relation.to.key());
            let in_row = !relation.cross
                && facts
                    .unit_of(&from)
                    .is_some_and(|unit| rows.contains(&unit));
            relation.line = !in_row
                && (!relation.cross
                    || (live(&from) && live(&to) && !through(&from) && clear(&from, &to)));
        }
        for unit in &mut self.units {
            unit.waits.clear();
            if unit.done {
                continue;
            }
            for step in &unit.steps {
                let key = step.key();
                let mut waits: Vec<Wait> = vec![];
                for relation in self.relations.iter().filter(|r| r.cross && r.to.key() == key) {
                    let from = relation.from.key();
                    if let Some(wait) = waits.iter_mut().find(|w| w.key == from) {
                        wait.line |= relation.line;
                        continue;
                    }
                    let (name, unit, href, opens, doing) = match &relation.from {
                        Endpoint::Step(id) => {
                            let shown = facts.steps.get(&from).map(|(_, s)| *s);
                            if shown.is_some_and(|s| s.spec().band == Placed::Done) {
                                continue;
                            }
                            // what it is doing, unless it only waits itself
                            let doing = match shown {
                                Some(Shown::Pending) | None => "",
                                Some(shown) => shown.word(),
                            };
                            let href = format!("/projects/id/{project}/steps/{id}");
                            let name =
                                super::ui::StepRef::new(id.as_str(), names.naming.step(id.as_str()));
                            (name, false, href, id.to_string(), doing)
                        }
                        Endpoint::Unit(id) => {
                            if facts.done.contains(id.as_str()) {
                                continue;
                            }
                            let href = format!("/projects/id/{project}/units/{id}");
                            let name = super::ui::StepRef {
                                id: id.to_string(),
                                title: names
                                    .naming
                                    .unit(id.as_str())
                                    .map(|u| u.title.clone())
                                    .unwrap_or_default(),
                                stage: String::new(),
                            };
                            (name, true, href, String::new(), "")
                        }
                        Endpoint::Input(_) | Endpoint::Output(_) => continue,
                    };
                    waits.push(Wait {
                        shown: live(&from),
                        line: relation.line,
                        key: from,
                        name,
                        unit,
                        href,
                        opens,
                        note: doing.to_owned(),
                    });
                }
                if !waits.is_empty() {
                    unit.waits.insert(step.id.to_string(), waits);
                }
            }
        }
    }
    /// Once every fact of every step is read (its failure, its run's quiet, a cancel asked
    /// for): how each step reads, for the whole plan's facts, its waits' words (a wait on a
    /// cancelled step says "cancelled"), the project's counts and the board's lines.
    fn known(&mut self, plan: &Plan, state: &StateSnapshot) {
        let shown: BTreeMap<StepId, Shown> = self
            .units
            .iter()
            .flat_map(|u| &u.steps)
            .map(|s| (s.id.clone(), s.shown()))
            .collect();
        for (key, fact) in &mut self.facts.steps {
            if let Some(now) = key
                .strip_prefix("s:")
                .and_then(|id| id.parse::<StepId>().ok())
                .and_then(|id| shown.get(&id))
            {
                fact.1 = *now;
            }
        }
        let name = |id: &StepId| shown.get(id).copied();
        for unit in &mut self.units {
            for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
                step.name_waits(plan, state, &name);
            }
        }
        self.project.counts = shown.values().copied().collect();
        self.settle();
    }
    pub fn href(&self) -> String {
        self.project.href()
    }
    /// The board as drawn. Live first: the stopped units (a failure, or held up by one), then
    /// the running, then the waiting; Plan order: every unit not done in one band. Each band
    /// draws its units that are rows of a recipe's lane matrix first, a matrix a recipe (rows
    /// that need someone first under Live first, plan order under Plan order); then lays its
    /// other units in layers by dependency depth among themselves, a layer ordered after the
    /// units it follows (so lines run down and seldom cross), ties in the view's order. Every
    /// done unit is on one shelf at the end.
    pub fn layout(&self) -> Layout<'_> {
        let bands: Vec<(&'static str, Vec<&UnitView>)> = if self.order == "live" {
            [Placed::Stopped, Placed::Running, Placed::Waiting]
                .into_iter()
                .map(|band| {
                    let units = self.units.iter().filter(|u| u.band() == band).collect();
                    (band.label(), units)
                })
                .collect()
        } else {
            vec![("", self.units.iter().filter(|u| !u.done).collect())]
        };
        let mut placed = BTreeMap::new();
        Layout {
            bands: bands
                .into_iter()
                .filter(|(_, units)| !units.is_empty())
                .map(|(label, units)| {
                    let (rows, units): (Vec<&UnitView>, Vec<&UnitView>) =
                        units.into_iter().partition(|u| u.matrix);
                    let (mut loose, joined): (Vec<&UnitView>, Vec<&UnitView>) =
                        units.into_iter().partition(|u| !self.joined(u));
                    // tallest first, so a row's boxes are near one height
                    loose.sort_by_key(|u| std::cmp::Reverse(u.height()));
                    Band {
                        label,
                        matrices: self.matrices(rows, band_id(label)),
                        layers: self.layers(joined, &mut placed),
                        loose,
                    }
                })
                .collect(),
            done: self.shelf(),
            total: self.units.iter().filter(|u| u.done).count(),
            total_steps: self.units.iter().filter(|u| u.done).map(|u| u.steps.len()).sum(),
            open: self.show == "done" || !self.q.is_empty() || !self.recipe.is_empty(),
        }
    }
    /// A band's matrix rows as one lane matrix a recipe, in the plan's order of their first
    /// rows; under Live first each matrix's rows that need someone first.
    fn matrices<'a>(&'a self, mut rows: Vec<&'a UnitView>, band: String) -> Vec<Matrix<'a>> {
        rows.sort_by_key(|u| u.pos);
        let live = self.order == "live";
        let mut matrices: Vec<Matrix<'a>> = vec![];
        for unit in rows {
            match matrices.iter_mut().find(|m| m.recipe == unit.recipe) {
                Some(m) => m.rows.push(unit),
                None => {
                    let recipe = self.names.recipes.get(&unit.recipe);
                    matrices.push(Matrix {
                        recipe: unit.recipe.clone(),
                        stages: unit.stages.clone(),
                        rows: vec![unit],
                        view: recipe.and_then(|r| r.view()),
                        view_error: recipe.and_then(|r| r.view_error()),
                        level: if live { 3 } else { 2 },
                        band: band.clone(),
                    });
                }
            }
        }
        if live {
            for m in &mut matrices {
                m.rows.sort_by_key(|u| (u.shown(), u.pos));
            }
        }
        matrices
    }
    /// A line joins `unit` to another unit the board draws: a wait between them is a line
    /// (`settle`), one way or the other.
    fn joined(&self, unit: &UnitView) -> bool {
        let id = unit.id.as_str();
        self.relations.iter().any(|r| {
            let (from, to) = (
                self.facts.unit_of(&r.from.key()),
                self.facts.unit_of(&r.to.key()),
            );
            r.cross
                && r.line
                && from.is_some()
                && to.is_some()
                && from != to
                && (from.as_deref() == Some(id) || to.as_deref() == Some(id))
        })
    }
    /// The done units the shelf draws: the latest finished first, all of them when the view
    /// asks for done units or searches, else the latest `SHELF`.
    fn shelf(&self) -> Vec<&UnitView> {
        let mut done: Vec<&UnitView> = self.units.iter().filter(|u| u.done).collect();
        done.sort_by(|a, b| b.finished().cmp(a.finished()));
        if self.show != "done" && self.q.is_empty() && self.recipe.is_empty() {
            done.truncate(SHELF);
        }
        done
    }
    /// Every done unit, under this view's order: where the shelf's "Show all" leads.
    pub fn all_done_href(&self) -> String {
        format!("{}?order={}&show=done{}", self.href(), self.order, self.focus_query())
    }
    /// The chain focus and recipe the view keeps across its links: "&root=…&up=1", "" for none.
    fn focus_query(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in self.focus_pairs() {
            query.append_pair(name, &value);
        }
        match query.finish() {
            q if q.is_empty() => q,
            q => format!("&{q}"),
        }
    }
    fn focus_pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = vec![];
        if !self.recipe.is_empty() {
            pairs.push(("recipe", self.recipe.clone()));
        }
        if !self.root.is_empty() {
            pairs.push(("root", self.root.clone()));
            if self.up {
                pairs.push(("up", "1".into()));
            }
            if self.down {
                pairs.push(("down", "1".into()));
            }
            if self.depth > 0 {
                pairs.push(("depth", self.depth.to_string()));
            }
        }
        pairs
    }
    /// The board tools' hidden fields that keep the focus as the form applies.
    pub fn focus_inputs(&self) -> TrustedHtml {
        TrustedHtml::owned(
            self.focus_pairs()
                .into_iter()
                .map(|(name, value)| {
                    format!(
                        "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
                        super::ui::esc(&value)
                    )
                })
                .collect(),
        )
    }
    /// The whole board again, in this view's order: the focus line's way out.
    pub fn everything_href(&self) -> String {
        if self.order == "live" {
            self.href()
        } else {
            format!("{}?order={}", self.href(), self.order)
        }
    }
    /// The line over a focused board: whose chain it shows, or which recipe's units and how they
    /// stand, then "Show everything"; empty without a focus.
    pub fn focus_html(&self) -> TrustedHtml {
        use super::ui::esc;
        let mut out = String::new();
        if !self.recipe.is_empty() {
            // the live units each once under the state it reads as (`UnitView::shown`)
            let live: Tally = self
                .units
                .iter()
                .filter(|u| !u.done)
                .map(UnitView::shown)
                .collect();
            let done = self.units.iter().filter(|u| u.done).count();
            let mut parts = vec![(
                self.units.len(),
                if self.units.len() == 1 { "unit" } else { "units" },
            )];
            parts.extend(super::ui::states(&live));
            parts.push((done, "done"));
            out.push_str(&format!(
                "Every unit of recipe <code>{}</code>: {}.",
                esc(&self.recipe),
                esc(&super::ui::tally(&parts))
            ));
        }
        if !self.root.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            let root = self
                .units
                .iter()
                .flat_map(|u| &u.steps)
                .find(|s| s.id.as_str() == self.root);
            match root {
                Some(step) => {
                    let name = step.name().link(&step.href(), 72, true).0;
                    let what = match (self.up, self.down) {
                        (true, false) => format!("Showing what {name} comes after"),
                        (false, true) => format!("Showing what comes after {name}"),
                        _ => format!("Showing the chain of {name}"),
                    };
                    out.push_str(&what);
                    if self.depth > 0 {
                        out.push_str(&format!(
                            ", {} each way",
                            super::ui::count(self.depth, "step", "steps")
                        ));
                    }
                    out.push('.');
                }
                None => out.push_str(&format!(
                    "No step <code>{}</code> is in the plan now.",
                    esc(&self.root)
                )),
            }
        }
        if !out.is_empty() {
            out.push_str(&format!(
                " <a href=\"{}\">Show everything</a>",
                esc(&self.everything_href())
            ));
        }
        TrustedHtml::owned(out)
    }
    /// The steps in `root`'s chain: itself, what it comes after (`up`) and what comes after it
    /// (`down`), at most `depth` steps away (0: any); none when it is not in the plan.
    fn chain(&self, root: &str, up: bool, down: bool, depth: usize) -> BTreeSet<String> {
        let mut kept = BTreeSet::new();
        if !self.facts.deps.contains_key(root) {
            return kept;
        }
        let mut dependents = BTreeMap::<&str, Vec<&str>>::new();
        for (id, deps) in &self.facts.deps {
            for dep in deps {
                dependents.entry(dep.as_str()).or_default().push(id.as_str());
            }
        }
        kept.insert(root.to_owned());
        for (on, way) in [(up, true), (down, false)] {
            if !on {
                continue;
            }
            let mut seen = BTreeSet::from([root]);
            let mut frontier = vec![root];
            let mut hops = 0;
            while !frontier.is_empty() && (depth == 0 || hops < depth) {
                hops += 1;
                let mut next = vec![];
                for id in frontier {
                    let near: Vec<&str> = if way {
                        self.facts.deps.get(id).into_iter().flatten().map(String::as_str).collect()
                    } else {
                        dependents.get(id).cloned().unwrap_or_default()
                    };
                    for n in near {
                        if seen.insert(n) {
                            kept.insert(n.to_owned());
                            next.push(n);
                        }
                    }
                }
                frontier = next;
            }
        }
        kept
    }
    /// `units` in layers by the longest chain of them each comes after (a cycle between units
    /// counts once), each layer ordered by where the units it follows were placed. `placed`
    /// holds every placed unit's place across its layer, 0 to 1.
    fn layers<'a>(
        &self,
        units: Vec<&'a UnitView>,
        placed: &mut BTreeMap<String, f64>,
    ) -> Vec<Vec<&'a UnitView>> {
        fn depth(
            i: usize,
            before: &[Vec<usize>],
            memo: &mut [Option<usize>],
            open: &mut [bool],
        ) -> usize {
            if let Some(d) = memo[i] {
                return d;
            }
            if open[i] {
                return 0;
            }
            open[i] = true;
            let d = before[i]
                .iter()
                .map(|&j| depth(j, before, memo, open) + 1)
                .max()
                .unwrap_or(0);
            open[i] = false;
            memo[i] = Some(d);
            d
        }
        let index: BTreeMap<&str, usize> = units
            .iter()
            .enumerate()
            .map(|(i, u)| (u.id.as_str(), i))
            .collect();
        let after = |u: &UnitView| -> Vec<String> {
            self.facts
                .after
                .get(u.id.as_str())
                .into_iter()
                .flatten()
                .cloned()
                .collect()
        };
        let before: Vec<Vec<usize>> = units
            .iter()
            .map(|u| after(u).iter().filter_map(|p| index.get(p.as_str()).copied()).collect())
            .collect();
        let (mut memo, mut open) = (vec![None; units.len()], vec![false; units.len()]);
        let mut layers = BTreeMap::<usize, Vec<usize>>::new();
        for i in 0..units.len() {
            layers
                .entry(depth(i, &before, &mut memo, &mut open))
                .or_default()
                .push(i);
        }
        layers
            .into_values()
            .map(|layer| {
                let n = layer.len() as f64;
                let mut keyed: Vec<(f64, usize)> = layer
                    .iter()
                    .enumerate()
                    .map(|(k, &i)| {
                        let at: Vec<f64> = after(units[i])
                            .iter()
                            .filter_map(|p| placed.get(p).copied())
                            .collect();
                        let own = (k as f64 + 0.5) / n;
                        let key = if at.is_empty() {
                            own
                        } else {
                            at.iter().sum::<f64>() / at.len() as f64
                        };
                        (key, i)
                    })
                    .collect();
                keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                for (k, &(_, i)) in keyed.iter().enumerate() {
                    placed.insert(units[i].id.to_string(), (k as f64 + 0.5) / n);
                }
                keyed.into_iter().map(|(_, i)| units[i]).collect()
            })
            .collect()
    }
    /// The plan has no steps at all: nothing to find, order or filter.
    pub fn plan_empty(&self) -> bool {
        self.facts.steps.is_empty()
    }
    /// When its Show leaves units out: "12 units hidden by Show: Attention."; "" otherwise.
    pub fn hidden_words(&self) -> String {
        let label = match self.show.as_str() {
            "active" => "Active",
            "attention" => "Attention",
            "done" => "Done",
            _ => return String::new(),
        };
        if self.hidden == 0 {
            return String::new();
        }
        format!(
            "{} hidden by Show: {label}.",
            super::ui::count(self.hidden, "unit", "units")
        )
    }
    /// The board as it is, with Show: All (its order, tag and search kept).
    pub fn show_all_href(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in url::form_urlencoded::parse(self.query.as_bytes()) {
            if key == "show" {
                query.append_pair("show", "all");
            } else {
                query.append_pair(&key, &value);
            }
        }
        format!("{}?{}", self.href(), query.finish())
    }
    /// What the board says when it shows no unit, for the view's Show.
    pub fn empty_words(&self) -> &'static str {
        if self.plan_empty() {
            return "The plan has no steps yet.";
        }
        match self.show.as_str() {
            "attention" => "Nothing needs attention.",
            "active" => "Every unit is done.",
            "done" => "No unit is done yet.",
            _ => "No units match this view.",
        }
    }
    /// The summary line's counts: its steps, then every state that needs no attention, as its
    /// bar orders them ("1139 steps · 1077 succeeded · 14 running · 6 paused · 40 pending"); the
    /// states that do are its tags.
    pub fn summary_counts(&self) -> String {
        let c = &self.project.counts;
        let mut parts = vec![(c.total(), if c.total() == 1 { "step" } else { "steps" })];
        parts.extend(
            super::ui::progress(c)
                .into_iter()
                .filter(|(s, _)| !s.spec().attention)
                .map(|(s, n)| (n, s.word())),
        );
        super::ui::tally(&parts)
    }
    /// The summary line's tags: each state that needs attention (failed, cancelled, stale,
    /// quiet), its glyph and count, leading to Show: Attention; and Paused for a paused project.
    pub fn summary_tags(&self) -> TrustedHtml {
        use super::ui::{glyph, mark, tag, tag_link};
        let href = self.attention_href();
        let mut out = String::new();
        for (shown, n) in self.project.counts.iter().filter(|(s, _)| s.spec().attention) {
            let tone = match shown.spec().tone {
                sluice_model::shown::Tone::Ink => "failed",
                sluice_model::shown::Tone::Attention => "attn",
                sluice_model::shown::Tone::Muted
                | sluice_model::shown::Tone::Active
                | sluice_model::shown::Tone::Paused
                | sluice_model::shown::Tone::Idle
                | sluice_model::shown::Tone::Success => "",
            };
            out.push_str(
                &tag_link(&href, &format!("{n} {}", shown.word()), tone, Some(mark(shown)))
                    .0
                    .replacen(
                        "<a ",
                        &format!("<a title=\"{}\" ", super::ui::esc(shown.spec().help)),
                        1,
                    ),
            );
        }
        if self.project.paused {
            out.push_str(&tag("Paused", "", Some(glyph(Shown::Paused))).0);
        }
        TrustedHtml::owned(out)
    }
    /// Something is left that a pause keeps from starting: a step pending (in any of its
    /// readings) or stale.
    pub fn left_to_start(&self) -> bool {
        let c = &self.project.counts;
        c.stored(&StepStatus::Pending) + c.stored(&StepStatus::Stale) > 0
    }
    /// The summary line has tags to draw.
    pub fn has_tags(&self) -> bool {
        self.project.counts.attention() > 0 || self.project.paused
    }
    /// What the search found: "12 steps match “land”", "No step matches “x”."
    pub fn match_words(&self) -> String {
        match self.matched {
            0 => format!("No step matches “{}”.", self.q),
            n => format!(
                "{} “{}”.",
                super::ui::count(n, "step matches", "steps match"),
                self.q
            ),
        }
    }
    /// Something in it needs the owner: a step needs attention (failed, cancelled, stale, a
    /// quiet run), or a pause holds work back. A phone then opens on the plan, not the board.
    pub fn needs_attention(&self) -> bool {
        let c = &self.project.counts;
        c.attention() + c.get(Shown::Paused) > 0 || self.project.paused
    }
    /// The board's Attention view, in the order shown: where a summary's failed, cancelled and
    /// quiet tags lead.
    pub fn attention_href(&self) -> String {
        format!("{}?order={}&show=attention{}", self.href(), self.order, self.focus_query())
    }
    /// The board under the same order and show, without its search: the search's clear link.
    pub fn clear_href(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query
            .append_pair("order", &self.order)
            .append_pair("show", &self.show);
        for (name, value) in self.focus_pairs() {
            query.append_pair(name, &value);
        }
        format!("{}?{}", self.href(), query.finish())
    }
    /// The project's description: its first block (a heading takes the block after it too),
    /// and the rest, which the page folds.
    pub fn about(&self) -> (TrustedHtml, Option<TrustedHtml>) {
        crate::markdown::render_folded(&self.project.description)
    }
    /// The relations among what the page draws: the live units and the done units on the
    /// shelf, with the plan's inputs and outputs. A relation inside a done unit the shelf
    /// leaves out has no card to join, so it is not sent (on lash, most of the plan).
    pub fn edges_json(&self) -> String {
        let drawn: BTreeSet<&str> = self
            .units
            .iter()
            .filter(|u| !u.done)
            .chain(self.shelf())
            .map(|u| u.id.as_str())
            .collect();
        let on_page = |key: &str| match self.facts.unit_of(key) {
            Some(unit) => drawn.contains(unit.as_str()),
            None => true,
        };
        let relations: Vec<&Relation> = self
            .relations
            .iter()
            .filter(|r| on_page(&r.from.key()) && on_page(&r.to.key()))
            .collect();
        script_json(&relations)
    }
    /// The lines inside one unit, for its own page.
    pub fn unit_edges_json(&self, unit: &UnitView) -> String {
        let inside = |key: &str| self.facts.unit_of(key).is_some_and(|u| u == unit.id.as_str());
        // on its own page a unit is its box: every relation inside it is a line, a lane's too
        let relations: Vec<Relation> = self
            .relations
            .iter()
            .filter(|r| inside(&r.from.key()) && inside(&r.to.key()))
            .map(|r| Relation {
                line: true,
                ..r.clone()
            })
            .collect();
        script_json(&relations)
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&ProjectTemplate {
            view: self,
            js_url: super::asset_url("sluice.js"),
            board_js_url: super::asset_url("board.js"),
        })
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
            &self.project.tab_words(),
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
/// Relations as the text of a `<script type="application/json">` the board reads its lines from:
/// JSON with `<`, `>` and `&` written as escapes, so no text in it can end or open an element.
fn script_json(relations: &[impl Serialize]) -> String {
    serde_json::to_string(relations)
        .expect("typed relations serialize")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}
/// What a page drew is gone: one calm line where it was (`id`'s element), with a way on.
fn gone_html(id: &str, words: &str, href: &str, link: &str) -> TrustedHtml {
    #[derive(Template)]
    #[template(
        source = "<div id=\"{{ id }}\" class=\"gone\" data-gone><p class=\"d-gone\">{{ words }} <a href=\"{{ href }}\">{{ link }}</a></p></div>",
        ext = "html"
    )]
    struct Gone<'a> {
        id: &'a str,
        words: &'a str,
        href: &'a str,
        link: &'a str,
    }
    TrustedHtml::from_template(&Gone {
        id,
        words,
        href,
        link,
    })
    .expect("owned template renders")
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
/// step gets what its card shows; a step's runs, thread and submissions are its page's
/// (`load_step`).
pub fn load_board(
    c: &Connection,
    shared: &DashboardSnapshot,
    project: ProjectId,
    plans: &PlanCache,
    signatures: &str,
    provider: &impl SignatureProvider,
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
    queue(c, project, &plan, &mut state, None)?;
    let mut board = ProjectView::new(summary.clone(), &plan, &state, revision as u64);
    board.name(sluice_runtime::naming::for_project(
        c,
        &super::home_of(c),
        project,
    )?);
    let mut last = last_messages(c, project, &board)?;
    let cards = Cards::read(c, project)?;
    for unit in &mut board.units {
        if let Some((message, at, from, thread)) = last.remove(unit.id.as_str()) {
            unit.last_message = message;
            unit.changed = at;
            unit.last_from = from;
            unit.last_thread = thread;
        }
        for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
            cards.decorate(step, revision as u64);
            // its run as observed with the store's snapshot: quiet, and when it last wrote
            let run = summary.running.iter().find(|r| r.step == step.id.as_str());
            step.quiet = run.is_some_and(|r| r.quiet);
            step.active_at = run
                .and_then(|r| r.activity)
                .map(super::rfc3339)
                .unwrap_or_default();
        }
        // a lane's view reads its stages' live progress
        if unit.matrix && !unit.done {
            for step in unit.steps.iter_mut().filter(|s| s.running()) {
                super::step::load_progress(c, project, step)?;
            }
        }
    }
    // how long each recipe's stage usually takes, for its steps' pages and running cards
    let usual = usual_durations(&board.units);
    for unit in board.units.iter_mut().filter(|u| !u.recipe.is_empty()) {
        for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
            step.usually = usual
                .get(&(unit.recipe.clone(), step.stage.clone()))
                .copied();
        }
    }
    board.usual = usual;
    // every fact read: how each step reads, its waits' words, the counts and the lines
    board.known(&plan, &state);
    Ok((board, plan))
}
/// Note on each pending step the plan would start but its resources hold back why it is queued
/// (`only`: that step alone).
fn queue(
    c: &Connection,
    project: ProjectId,
    plan: &Plan,
    state: &mut StateSnapshot,
    only: Option<&StepId>,
) -> sluice_store::Result<()> {
    for (id, step) in plan.steps() {
        if only.is_none_or(|only| only == id)
            && state.status(id) == StepStatus::Pending
            && !step.is_external()
            && evaluate_step(plan, state, step) == GateDecision::Ready
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
    Ok(())
}
/// What a step's card shows beyond the plan's state, read for the whole project:
/// its progress counts, error, finishing run, asked cancel and current run times. A step's runs, thread and
/// submissions are its page's (`step::load_detail`).
struct Cards {
    rows: BTreeMap<String, (bool, Option<i64>, i64, Option<String>)>,
    finishing: BTreeMap<StepId, sluice_model::attempt::Finishing>,
    stopping: BTreeSet<StepId>,
    timings: BTreeMap<String, RunTiming>,
}
impl Cards {
    fn read(c: &Connection, project: ProjectId) -> sluice_store::Result<Self> {
        let mut rows = BTreeMap::new();
        let mut q = c.prepare_cached("SELECT step_id,manual,total,done,error FROM steps WHERE project_id=?1")?;
        let mut found = q.query([project.to_string()])?;
        while let Some(r) = found.next()? {
            rows.insert(r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?));
        }
        Ok(Self {
            rows,
            finishing: sluice_store::attempts::finishing(c, project)?,
            stopping: sluice_store::attempts::stopping(c, project)?,
            timings: run_timings(c, project)?,
        })
    }
    fn decorate(&self, step: &mut StepView, revision: u64) {
        step.finishing = self.finishing.get(&step.id).cloned();
        step.stopping = self.stopping.contains(&step.id);
        step.timing = self.timings.get(step.id.as_str()).cloned();
        if let Some((manual, total, done, error)) = self.rows.get(step.id.as_str()) {
            step.manual = *manual;
            step.total = total.map(|n| n as usize);
            step.done = *done as usize;
            if let Some(error) = error {
                let took = step.timing.as_ref().map(|t| t.seconds);
                step.set_failure(super::failure::Failure::parse(error, took));
            }
        }
        step.revision = revision;
    }
}
/// One step as its page and the drawer draw it, read without drawing the rest of the board:
/// what its card shows, its title and stage, and its runs, thread and submissions; with the
/// project's name and, when the step is in a tagged unit, that unit's id and title. None when
/// the plan has no such step.
pub struct StepDetail {
    pub project: String,
    pub step: StepView,
    pub unit: Option<(String, String)>,
}
/// A step's run while it runs, observed now (`super::observe_run`): whether it is quiet, and
/// when it last wrote.
fn observe(c: &Connection, project: ProjectId, step: &mut StepView) -> sluice_store::Result<()> {
    if let Some(mut run) = super::RunningView::of(c, project, step.id.as_str())? {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        super::observe_run(&super::home_of(c), &mut run, now);
        step.quiet = run.quiet;
        step.active_at = run.activity.map(super::rfc3339).unwrap_or_default();
    }
    Ok(())
}
pub fn load_step(
    c: &Connection,
    project: ProjectId,
    plans: &PlanCache,
    signatures: &str,
    provider: &impl SignatureProvider,
    id: &StepId,
) -> sluice_store::Result<Option<StepDetail>> {
    let name: String = c
        .query_row(
            "SELECT name FROM projects WHERE project_id=?1 AND deleted_at IS NULL",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| PublicError::NotFound {
            message: "project not found".into(),
        })?;
    let (revision, doc): (i64, String) = c.query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let plan = plans.compile(project, doc, signatures, provider)?;
    if !plan.steps().contains_key(id) {
        return Ok(None);
    }
    let mut state = sluice_store::plans::read_state(c, project)?;
    queue(c, project, &plan, &mut state, Some(id))?;
    let depths = Depths::new(&plan, &state);
    let names = sluice_runtime::naming::for_project(c, &super::home_of(c), project)?;
    let cards = Cards::read(c, project)?;
    let unit_of = |unit: &sluice_model::units::Unit| {
        let mut view = UnitView::new(project, "", &plan, &state, unit, &depths);
        view.name(&names);
        for step in &mut view.steps {
            cards.decorate(step, revision as u64);
        }
        view
    };
    let Some(home) = plan.units().values().find(|u| u.steps.contains(id)) else {
        return Ok(None);
    };
    let unit = unit_of(home);
    let mut step = unit
        .steps
        .iter()
        .find(|s| &s.id == id)
        .cloned()
        .expect("the unit holds the step");
    // how long its stage usually takes, over its recipe's done units (as the board works it out)
    if !unit.recipe.is_empty() {
        let peers: Vec<UnitView> = plan
            .units()
            .values()
            .filter(|u| {
                u.done(&state)
                    && names
                        .naming
                        .unit(u.name.as_str())
                        .is_some_and(|n| n.recipe == unit.recipe)
            })
            .map(unit_of)
            .collect();
        step.usually = usual_durations(&peers)
            .get(&(unit.recipe.clone(), step.stage.clone()))
            .copied();
    }
    step.timeline = unit.timeline(Some(id));
    step.chained = !plan.dependencies(id).is_empty()
        || plan.steps().keys().any(|s| plan.dependencies(s).contains(id));
    observe(c, project, &mut step)?;
    // its waits name what they wait on by how each reads, as the board's do (`known`)
    let mut shown = BTreeMap::new();
    for other in plan
        .dependencies(id)
        .iter()
        .chain(step.gates.iter().filter_map(|g| g.step.as_ref()))
    {
        let mut view = StepView::new(project, &plan, &state, other);
        cards.decorate(&mut view, revision as u64);
        observe(c, project, &mut view)?;
        shown.insert(other.clone(), view.shown());
    }
    step.name_waits(&plan, &state, &|id| shown.get(id).copied());
    super::step::load_detail(c, project, &mut step)?;
    let unit = home.tagged.then(|| {
        let title = names.naming.unit_title(home.name.as_str());
        let titled = !title.is_empty() && title != home.name.as_str();
        (home.name.to_string(), if titled { title.to_owned() } else { String::new() })
    });
    Ok(Some(StepDetail {
        project: name,
        step,
        unit,
    }))
}
/// How long each recipe's stage usually takes: the median, over the recipe's done units in
/// the plan, of how long the stage's step took when it succeeded (its last run, a scatter's last
/// round; never a value set by hand). A stage with fewer than three such runs has none.
pub fn usual_durations(units: &[UnitView]) -> BTreeMap<(String, String), f64> {
    let mut samples = BTreeMap::<(String, String), Vec<f64>>::new();
    for unit in units.iter().filter(|u| u.done && !u.recipe.is_empty()) {
        for step in &unit.steps {
            if let Some(t) = &step.timing
                && t.finished.is_some()
                && step.status == StepStatus::Succeeded
                && !step.manual
                && !step.stage.is_empty()
            {
                samples
                    .entry((unit.recipe.clone(), step.stage.clone()))
                    .or_default()
                    .push(t.seconds);
            }
        }
    }
    samples
        .into_iter()
        .filter_map(|(key, s)| Some((key, super::timeline::median(s)?)))
        .collect()
}
/// Each step's current run times (its card's timer) and every run of its current generation
/// (its unit's timeline, and how its earlier runs ended for its card's "run 3"): its latest run, or for a scatter its latest round's item runs, from the
/// first start to the last end. One pass over the project's runs; a run still going is measured
/// to the read's `now` for the timer, and has no end in its span.
fn run_timings(
    c: &Connection,
    project: ProjectId,
) -> sluice_store::Result<BTreeMap<String, RunTiming>> {
    let mut runs = BTreeMap::<String, Vec<Run>>::new();
    let mut q = c.prepare_cached(
        "SELECT r.step_id,r.item_index,r.work_generation,coalesce(r.started_at,r.created_at),r.finished_at,julianday(coalesce(r.started_at,r.created_at)),julianday(coalesce(r.finished_at,'now')),julianday(r.finished_at),json_extract(r.result,'$.status'),CASE WHEN r.finished_at IS NOT NULL AND coalesce(json_extract(r.result,'$.status'),'')<>'succeeded' THEN json_extract(r.result,'$.error') END FROM runs r JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id AND s.generation=r.generation WHERE r.project_id=?1 ORDER BY r.step_id,r.created_at,r.run_id",
    )?;
    let mut rows = q.query([project.to_string()])?;
    while let Some(r) = rows.next()? {
        let finished: Option<String> = r.get(4)?;
        let status: Option<String> = r.get(8)?;
        let error: Option<String> = r.get(9)?;
        // how it ended, as the status table reads its result (the step page's Runs read it
        // the same way); none without a result
        let outcome = match (&finished, status.map(|s| s.parse::<StepStatus>())) {
            (None, _) => Some(Shown::Running),
            (Some(_), Some(Ok(status))) => Some(sluice_model::shown::classify(
                &sluice_model::shown::Facts {
                    cancelled: error
                        .as_deref()
                        .is_some_and(sluice_model::shown::stored_is_cancel),
                    ..sluice_model::shown::Facts::of(status)
                },
            )),
            (Some(_), Some(Err(_)) | None) => None,
        };
        runs.entry(r.get(0)?).or_default().push(Run {
            item: r.get(1)?,
            work: r.get(2)?,
            started: r.get(3)?,
            finished,
            from: r.get(5)?,
            to: r.get(6)?,
            ended: r.get(7)?,
            outcome,
        });
    }
    Ok(runs
        .into_iter()
        .filter_map(|(step, all)| {
            let last = all.last()?;
            let round: Vec<&Run> = if last.item < 0 {
                vec![last]
            } else {
                all.iter()
                    .filter(|r| r.item >= 0 && r.work == last.work)
                    .collect()
            };
            let first = round
                .iter()
                .min_by(|a, b| a.from.partial_cmp(&b.from).unwrap_or(std::cmp::Ordering::Equal))?;
            let from = first.from?;
            let to = round
                .iter()
                .map(|r| r.to)
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .fold(from, f64::max);
            let finished = round
                .iter()
                .map(|r| r.finished.clone())
                .collect::<Option<Vec<_>>>()
                .and_then(|ends| ends.into_iter().max());
            // its runs (a scatter's rounds), each as its timeline draws it; the earlier ones'
            // ends for its card's "run 3"
            let spans = spans(&all);
            let earlier = spans[..spans.len().saturating_sub(1)]
                .iter()
                .filter_map(|s| s.outcome)
                .collect();
            let timing = RunTiming {
                started: first.started.clone(),
                finished,
                runs: spans.len(),
                earlier,
                seconds: (to - from) * 86_400.0,
                spans,
            };
            Some((step, timing))
        })
        .collect())
}
/// One run as `run_timings` reads it: its item (a scatter's, else -1), its work generation,
/// its start and end (RFC 3339, and julian days: `to` measured to the read's `now`, `ended` none
/// while it runs) and how it ended.
struct Run {
    item: i64,
    work: i64,
    started: String,
    finished: Option<String>,
    from: Option<f64>,
    to: Option<f64>,
    ended: Option<f64>,
    outcome: Option<Shown>,
}
/// A step's runs as its timeline draws them, oldest first: a run a span, a scatter's round of
/// item runs one span from its first start to its last end (running while any item is).
fn spans(all: &[Run]) -> Vec<super::timeline::RunSpan> {
    use super::timeline::{RunSpan, from_julian};
    let mut out: Vec<(Option<i64>, RunSpan)> = vec![];
    for run in all {
        let Some(from) = run.from.map(from_julian) else {
            continue;
        };
        let to = run.ended.map(from_julian);
        let round = (run.item >= 0).then_some(run.work);
        if let Some((_, span)) = out.iter_mut().find(|(r, _)| round.is_some() && *r == round) {
            span.items += 1;
            if from < span.from {
                span.from = from;
                span.started = run.started.clone();
            }
            span.to = span.to.zip(to).map(|(a, b)| a.max(b));
            if span.to.is_none() {
                span.finished = None;
            } else if run.finished > span.finished {
                span.finished = run.finished.clone();
            }
            // a round reads as the first state among its items (`shown`); an item with no
            // result says nothing of it
            span.outcome = match (span.outcome, run.outcome) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            continue;
        }
        out.push((
            round,
            RunSpan {
                started: run.started.clone(),
                finished: run.finished.clone(),
                outcome: run.outcome,
                items: usize::from(round.is_some()),
                from,
                to,
            },
        ));
    }
    out.into_iter().map(|(_, s)| s).collect()
}
/// Each unit's last message (body and time): the newest from or to one of its steps, or in one
/// of its steps' threads. One pass over the project's messages, newest first.
fn last_messages(
    c: &Connection,
    project: ProjectId,
    board: &ProjectView,
) -> sluice_store::Result<BTreeMap<String, (String, String, String, String)>> {
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
                last.insert(unit.clone(), (r.get(3)?, r.get(4)?, from.clone(), thread.clone()));
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
/// observed once inside it. Nothing that moves while the page renders can fail it.
pub async fn snapshot(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
) -> Result<(DashboardSnapshot, ProjectView), PublicError> {
    let (shared, view, _) = load(state, project, registry, false).await?;
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
    let (shared, mut view, panel) = load(state, project, registry, true).await?;
    view.panel = super::panel::draw(state, project, panel, cache).await?;
    Ok((shared, view))
}
/// One step's detail (`load_step`) from one store snapshot, its run's activity observed: what
/// the step's page and the drawer draw, without drawing the rest of the board. With `nav`, the
/// nav's snapshot from the same read. None when the plan has no such step.
pub async fn step_detail(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
    step: &StepId,
    nav: bool,
) -> Result<(Option<DashboardSnapshot>, Option<StepDetail>), PublicError> {
    let exact = registry.map(|r| r.0.signatures(project)).transpose()?;
    let catalog = state.catalog.catalog(Some(project))?;
    let plans = state.plans.clone();
    let id = step.clone();
    let (shared, detail) = state
        .reads
        .snapshot(move |c| {
            let detail = match &exact {
                Some(exact) => {
                    let version = format!("registry:{}", exact.version);
                    load_step(c, project, &plans, &version, exact, &id)?
                }
                None => {
                    let version = format!("catalog:{}", catalog.version);
                    load_step(c, project, &plans, &version, &CatalogSignatures(&catalog), &id)?
                }
            };
            let shared = nav.then(|| super::load_snapshot(c, catalog)).transpose()?;
            Ok((shared, detail))
        })
        .await
        .map_err(|e| e.into_public(true))?;
    Ok((shared, detail))
}
async fn load(
    state: &DashboardState,
    project: ProjectId,
    registry: Option<&Registry>,
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
    let (shared, board, loaded) = state
        .reads
        .snapshot(move |c| {
            let shared = super::load_snapshot(c, catalog)?;
            let (board, plan) = if let Some(exact) = exact {
                let version = format!("registry:{}", exact.version);
                load_board(c, &shared, project, &plans, &version, &exact)?
            } else {
                let version = format!("catalog:{}", shared.functions.version);
                let provider = CatalogSignatures(&shared.functions);
                load_board(c, &shared, project, &plans, &version, &provider)?
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
    /// Chain focus: the step whose chain the board shows, `up` its ancestors and `down` its
    /// dependents ("1"; both when neither is given), `depth` steps away at most.
    pub root: Option<String>,
    pub up: Option<String>,
    pub down: Option<String>,
    pub depth: Option<usize>,
    /// Only the units this recipe made.
    pub recipe: Option<String>,
}
impl BoardQuery {
    /// Filter, order and search `view` as the page's query asks.
    pub fn apply(&self, view: &mut ProjectView) -> Result<(), PublicError> {
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
        let shows = |u: &UnitView| match show {
            "active" => !u.done,
            // what needs a look: a step whose state needs attention (`shown`)
            "attention" => u.needs_attention(),
            "done" => u.done,
            _ => true,
        };
        view.units.retain(|u| {
            self.tag.as_deref().is_none_or(|tag| {
                tag.is_empty() || u.steps.iter().any(|s| s.tags.iter().any(|t| t == tag))
            })
        });
        if let Some(recipe) = self.recipe.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            view.units.retain(|u| u.recipe == recipe);
            view.recipe = recipe.into();
        }
        if let Some(root) = self.root.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
            let on = |flag: &Option<String>| {
                flag.as_deref()
                    .is_some_and(|f| !matches!(f, "" | "0" | "false"))
            };
            let (up, down) = match (on(&self.up), on(&self.down)) {
                (false, false) => (true, true),
                flags => flags,
            };
            let depth = self.depth.unwrap_or(0);
            let chain = view.chain(root, up, down, depth);
            view.units.retain_mut(|unit| {
                unit.steps.retain(|s| chain.contains(s.id.as_str()));
                for row in &mut unit.rows {
                    row.retain(|s| chain.contains(s.id.as_str()));
                }
                unit.rows.retain(|row| !row.is_empty());
                !unit.steps.is_empty()
            });
            (view.root, view.up, view.down, view.depth) = (root.into(), up, down, depth);
        }
        let before = view.units.len();
        view.units.retain(shows);
        view.hidden = before - view.units.len();
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
        for (name, value) in view.focus_pairs() {
            query.append_pair(name, &value);
        }
        view.query = query.finish();
        if order == "live" {
            view.units.sort_by_key(UnitView::shown);
        }
        if !q.is_empty() {
            view.matched = search(&mut view.units, &q);
            // a done unit with a match opens to it, as does its shelf
            for unit in &mut view.units {
                unit.open = unit.done;
            }
        }
        view.q = q;
        view.settle();
        Ok(())
    }
}
/// Keep the steps whose id, title, doc or unit id holds every word of `q` (case-insensitive, in any
/// order), and the units with one; rows left empty go. Returns how many steps matched.
fn search(units: &mut Vec<UnitView>, q: &str) -> usize {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    let mut matched = 0;
    units.retain_mut(|unit| {
        let unit_id = unit.id.as_str().to_lowercase();
        let hit = |step: &StepView| {
            let text =
                format!("{} {} {} {unit_id}", step.id.as_str(), step.title, step.doc).to_lowercase();
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
    let watch = state.watch(Some(project));
    let loader = move || {
        let state = state.clone();
        let viewer = viewer.clone();
        let query = query.clone();
        let registry = registry.clone();
        let cache = cache.clone();
        async move {
            let registry = registry.as_ref().map(|r| &r.0);
            // a deleted project is said where its plan was, and the stream stays open and quiet
            let (shared, mut view) = match page_snapshot(&state, project, registry, Some(&cache)).await {
                Err(PublicError::NotFound { .. }) => {
                    return Ok(RenderedBatch::new(vec![PatchRegion::new(
                        "project-board",
                        gone_html("project-board", "This project was deleted.", "/", "All projects"),
                    )]));
                }
                read => read?,
            };
            query.apply(&mut view)?;
            Ok(view.draw(&shared, &viewer)?.1)
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
    let mut unit = view
        .units
        .iter()
        .find(|u| &u.id == unit)
        .cloned()
        .ok_or_else(|| PublicError::NotFound {
            message: format!("unit {unit} not found"),
        })?;
    unit.open = unit.done;  // on its own page a done unit shows its cards
    // a wait on another unit is said in words under its card: the page draws only the
    // lines inside the unit
    for wait in unit.waits.values_mut().flatten() {
        wait.line = false;
    }
    let nav = NavView::new(shared, Some(view.project.id), "plan")?;
    Ok(RenderedBatch::new(vec![
        PatchRegion::new(
            "unit-detail",
            unit.page_body(
                &view.project,
                &view.unit_edges_json(&unit),
                view.names.recipe_of(unit.id.as_str()).map(|r| r.as_ref()),
            )
            .map_err(render_error)?,
        ),
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
            &format!(
                "{} · {}",
                sluice_model::naming::cut(view.names.naming.unit_title(unit.as_str()), 48),
                view.project.name
            ),
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
    let watch = state.watch(Some(project));
    let loader = move || {
        let state = state.clone();
        let registry = registry.clone();
        let id = id.clone();
        let viewer = viewer.clone();
        async move {
            let (shared, view) =
                snapshot(&state, project, registry.as_ref().map(|r| &r.0)).await?;
            // a unit that left the plan (retired, or edited out) is said where it was drawn,
            // and the stream stays open and quiet: its page never reconnects for it
            if !view.units.iter().any(|u| u.id == id) {
                return Ok(RenderedBatch::new(vec![PatchRegion::new(
                    "unit-detail",
                    gone_html(
                        "unit-detail",
                        "This unit left the plan; it may have retired.",
                        &format!("{}/log?unit={id}", view.href()),
                        "Search the log for it",
                    ),
                )]));
            }
            unit_batch(&shared, &view, &id, &viewer)
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
                .route(
                    "/projects/id/{project}/runs/{run}/files/{name}",
                    get(super::step::run_file_page),
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
