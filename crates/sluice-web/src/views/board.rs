//! The v2 board. Relations retain their gate form and unit endpoints.
use super::step::{FieldView, RunTiming, StepView};
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
/// where no line shows it: on a phone, without script, or when the view leaves its source out
/// ("Waits for l-a1 (running) and l-d1").
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Wait {
    pub key: String,
    pub name: String,
    pub href: String,
    /// A source step, which opens in the drawer.
    pub opens: String,
    /// What the source is doing, when that is not just waiting itself: "running", "failed".
    pub note: String,
    /// The board draws the line from it (else the words say it is not in this view).
    pub drawn: bool,
}
/// The words of a step's waits: "Waits for a (running), b and c". `away` when a source is not
/// on the board as shown, so the words stay where the lines are drawn too. A name traces the
/// line from its source to `step`. Past three sources the rest are a count, "and 79 more",
/// which opens the step, whose page lists every gate: the words stay a line or two at any
/// fan-in.
pub fn waits_html(step: &StepView, waits: &[Wait]) -> Result<TrustedHtml, askama::Error> {
    const SHOWN: usize = 3;
    #[derive(Template)]
    #[template(
        source = "<p class=\"waits{% if away %} away{% endif %}\">Waits for {% for w in named %}{% if !loop.first %}{% if loop.last && more == 0 %} and {% else %}, {% endif %}{% endif %}<a href=\"{{ w.href }}\"{% if !w.opens.is_empty() %} data-opens=\"{{ w.opens }}\"{% endif %} data-from=\"{{ w.key }}\" data-to=\"{{ to }}\">{{ w.name }}</a>{% let off = !w.drawn && !none %}{% if !w.note.is_empty() || off %} ({{ w.note }}{% if off %}{% if !w.note.is_empty() %}, {% endif %}not in this view{% endif %}){% endif %}{% endfor %}{% if more > 0 %} and <a href=\"{{ href }}\" data-opens=\"{{ id }}\">{{ more }} more</a>{% endif %}{% if none %}, not in this view{% endif %}</p>",
        ext = "html"
    )]
    struct Words<'a> {
        to: String,
        href: String,
        id: &'a str,
        named: &'a [Wait],
        more: usize,
        away: bool,
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
        away: waits.iter().any(|w| !w.drawn),
        none: waits.iter().all(|w| !w.drawn),
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
    /// Who sent its last message.
    pub last_from: String,
    pub changed: String,
    /// The unit is one step in the plan: the board draws that step's card alone, no box.
    pub solo: bool,
    /// What each step still waits for in other units, by step id.
    pub waits: BTreeMap<String, Vec<Wait>>,
    /// A done unit a search matched in: drawn open, its matching cards in view.
    pub open: bool,
}
/// A part of the board under one label ("Stopped", "Running", "Waiting"; none under Plan
/// order): its units in layers by dependency depth, so the lines between them run down.
pub struct Band<'a> {
    pub label: &'static str,
    pub layers: Vec<Vec<&'a UnitView>>,
    /// Its units no line joins to any other: after the layers, tallest first, packed from the
    /// start of the row (centring them would leave holes and imply a wait that is not there).
    pub loose: Vec<&'a UnitView>,
}
/// The board as drawn: the bands of live and pending units, then the done units on one shelf.
pub struct Layout<'a> {
    /// Each recipe with a view: its live units as one lane matrix, before the bands.
    pub matrices: Vec<Matrix<'a>>,
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
impl Layout<'_> {
    /// The done units the shelf leaves out; "Show all" draws them (Show: Done).
    pub fn more(&self) -> usize {
        self.total - self.done.len()
    }
}
/// How many done units the shelf draws unless every one is asked for: the latest finished.
pub const SHELF: usize = 20;
impl UnitView {
    /// Its steps counted by how each reads, the states that need someone first: a quiet
    /// running step counts as quiet, not running. Zero counts are left out.
    pub fn tally(&self) -> Vec<(usize, &'static str)> {
        let order = [
            "failed",
            "cancelled",
            "stale",
            "quiet",
            "running",
            "paused",
            "pending",
            "skipped",
            "succeeded",
        ];
        let mut counts = [0usize; 9];
        for step in &self.steps {
            let word = if step.is_quiet() {
                "quiet"
            } else if step.paused && step.status != "running" {
                "paused"
            } else {
                match step.mark.as_str() {
                    "manual" => "succeeded",
                    "external" => "running",
                    m => m,
                }
            };
            if let Some(i) = order.iter().position(|o| *o == word) {
                counts[i] += 1;
            }
        }
        order
            .iter()
            .zip(counts)
            .filter(|(_, n)| *n > 0)
            .map(|(w, n)| (n, *w))
            .collect()
    }
    /// How tall its box draws, in rows of cards.
    pub fn height(&self) -> usize {
        if self.solo { 1 } else { self.rows.len() + 1 }
    }
    /// "1 running · 4 pending": every state it has, in `tally`'s order.
    pub fn tally_words(&self) -> String {
        self.tally()
            .iter()
            .map(|(n, w)| format!("{n} {w}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
    /// What in it needs someone, for its label: "1 failed", "1 quiet"; "" when nothing does.
    pub fn alarm(&self) -> String {
        self.tally()
            .iter()
            .filter(|(_, w)| ["failed", "cancelled", "stale", "quiet"].contains(w))
            .map(|(n, w)| format!("{n} {w}"))
            .collect::<Vec<_>>()
            .join(" · ")
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
    /// unit's prefix and its mark, `fork✓ work✓ land✓` (✓ succeeded, – skipped).
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
                format!("{short}{}", lane_mark(s))
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
        let skipped = self.steps.iter().filter(|s| s.status == "skipped").count();
        let steps = super::ui::count(self.steps.len(), "step", "steps");
        if skipped > 0 {
            format!("{steps} done, {skipped} skipped")
        } else {
            format!("{steps} done")
        }
    }
    pub fn body(&self) -> Result<TrustedHtml, askama::Error> {
        TrustedHtml::from_template(&UnitTemplate { unit: self })
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
    pub fn href(&self, project: &ProjectId) -> String {
        format!("/projects/id/{project}/units/{}", self.id)
    }
    /// Its step at `stage` ("land"), when it has one.
    pub fn stage_step(&self, stage: &str) -> Option<&StepView> {
        self.steps.iter().find(|s| s.stage == stage)
    }
    /// The mark that stands for the whole unit: its step that most needs someone.
    pub fn mark(&self) -> &str {
        let order = [
            "failed",
            "cancelled",
            "stale",
            "running",
            "external",
            "paused",
            "pending",
            "skipped",
            "manual",
            "succeeded",
        ];
        order
            .into_iter()
            .find(|m| self.steps.iter().any(|s| s.display_mark() == *m))
            .unwrap_or("pending")
    }
    /// Where a lane matrix puts its row: 0 needs attention (a step failed, cancelled or stale,
    /// or a quiet run), 1 running, 2 waiting.
    pub fn row_rank(&self) -> u8 {
        if self.steps.iter().any(|s| {
            s.is_quiet() || matches!(s.status.as_str(), "failed" | "stale")
        }) {
            0
        } else if self
            .steps
            .iter()
            .any(|s| s.status == "running" || s.mark == "external")
        {
            1
        } else {
            2
        }
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
    pub fn lane_links_html(&self) -> String {
        self.steps
            .iter()
            .map(|s| {
                format!(
                    "<a href=\"{}\" data-opens=\"{}\" aria-label=\"{} {}\">{}<span class=\"lm\">{}</span></a>",
                    s.href(),
                    s.id,
                    s.id,
                    super::ui::status_word(s.display_mark()),
                    super::ui::esc(if s.stage.is_empty() { s.id.as_str() } else { &s.stage }),
                    lane_mark(s)
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
    /// Its last message, drawn as message bodies are.
    pub fn last_message_html(&self) -> TrustedHtml {
        crate::markdown::render(&self.last_message)
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
    /// The band Live first draws the unit in: 0 running (a step running or outside), 1 stopped
    /// (a step failed or stale, or held up by one), 2 waiting, 3 done (the shelf).
    pub fn band(&self) -> u8 {
        if self.done {
            3
        } else if self
            .steps
            .iter()
            .any(|s| s.status == "running" || s.mark == "external")
        {
            0
        } else if self
            .steps
            .iter()
            .any(|s| s.blocked || matches!(s.status.as_str(), "failed" | "stale"))
        {
            1
        } else {
            2
        }
    }
}
/// A step's mark in a lane string: ✓ succeeded, ▶ running, ✗ failed, ■ cancelled, ~ stale,
/// – skipped, ‖ paused, · anything else.
fn lane_mark(s: &StepView) -> char {
    match s.status.as_str() {
        "succeeded" => '✓',
        "running" => '▶',
        "failed" if s.cancelled() => '■',
        "failed" => '✗',
        "stale" => '~',
        "skipped" => '–',
        _ if s.paused => '‖',
        _ => '·',
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
}
impl Matrix<'_> {
    /// "14 units · 1 failed · 3 quiet · 10 running": its rows counted by how each reads.
    pub fn tally(&self) -> String {
        let mut parts = vec![(self.rows.len(), if self.rows.len() == 1 { "unit" } else { "units" })];
        for (word, test) in [
            ("failed", "failed"),
            ("cancelled", "cancelled"),
            ("stale", "stale"),
        ] {
            parts.push((self.rows.iter().filter(|u| u.mark() == test).count(), word));
        }
        let quiet = self.rows.iter().filter(|u| u.steps.iter().any(StepView::is_quiet)).count();
        let running = self
            .rows
            .iter()
            .filter(|u| u.mark() == "running" && !u.steps.iter().any(StepView::is_quiet))
            .count();
        parts.push((quiet, "quiet"));
        parts.push((running, "running"));
        let waiting = self.rows.len() - parts[1..].iter().map(|(n, _)| n).sum::<usize>();
        parts.push((waiting, "waiting"));
        super::ui::tally(&parts)
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
/// What the board needs of the whole plan, whatever the view shows: each step's unit and
/// status, which units are done, and which units each unit comes after.
#[derive(Clone, Debug, Default)]
struct PlanFacts {
    /// By step key (`s:<id>`): its unit and status.
    steps: BTreeMap<String, (String, String)>,
    done: BTreeSet<String>,
    /// By unit: the units a relation into one of its steps comes from, while its source has
    /// not succeeded or been skipped.
    after: BTreeMap<String, BTreeSet<String>>,
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
    /// The project's board, drawn beside the plan (`docs("board")`), when it has one.
    pub panel: Option<super::panel::Panel>,
    /// Its steps' and units' names, and the recipes they came from.
    #[serde(skip)]
    pub names: std::sync::Arc<sluice_runtime::naming::ProjectNaming>,
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
                    changed: project.changed.clone(),
                    solo: unit.steps.len() == 1,
                    waits: BTreeMap::new(),
                    open: false,
                }
            })
            .collect::<Vec<_>>();
        let mut units = units;
        for (pos, unit) in units.iter_mut().enumerate() {
            unit.pos = pos;
        }
        let mut facts = PlanFacts::default();
        for unit in &units {
            if unit.done {
                facts.done.insert(unit.id.to_string());
            }
            for step in &unit.steps {
                facts
                    .steps
                    .insert(step.key(), (unit.id.to_string(), step.mark.clone()));
            }
        }
        // what each unit still waits for: a satisfied source puts no unit above another
        let through = |key: &str| {
            facts
                .steps
                .get(key)
                .is_some_and(|(_, mark)| matches!(mark.as_str(), "succeeded" | "skipped"))
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
            panel: None,
            names: Default::default(),
        };
        view.settle();
        view
    }
    /// Name its steps and units (`sluice_runtime::naming`): titles, stages, recipes and their
    /// params; a unit whose recipe has a view becomes a row of that recipe's lane matrix.
    pub fn name(&mut self, names: std::sync::Arc<sluice_runtime::naming::ProjectNaming>) {
        for unit in &mut self.units {
            let id = unit.id.to_string();
            if let Some(named) = names.naming.unit(&id) {
                unit.title = named.title.clone();
                unit.recipe = named.recipe.clone();
                unit.stages = named.stages.clone();
                unit.params = named.params.clone();
            }
            unit.matrix = names
                .recipe_of(&id)
                .is_some_and(|r| r.view_source().is_some());
            for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
                if let Some(named) = names.naming.step(step.id.as_str()) {
                    step.title = named.title.clone();
                    step.stage = named.stage.clone();
                }
            }
        }
        self.names = names;
        self.settle();
    }
    /// Mark the relations the board draws as lines, and say on each step what it still waits
    /// for in other units. Within a box every relation is a line. Between units a line is a
    /// wait: it joins a source not yet through (succeeded or skipped, or its unit done) to a
    /// step the view shows in a unit not done. A satisfied source is neither drawn nor said:
    /// the step's page lists every gate.
    fn settle(&mut self) {
        let facts = &self.facts;
        let project = self.project.id;
        let mut shown = BTreeSet::new();
        for unit in &self.units {
            shown.insert(unit.key());
            shown.extend(unit.steps.iter().map(StepView::key));
        }
        // an end a line can reach: shown and live (a plan input or output is always drawn)
        let live = |key: &str| match facts.unit_of(key) {
            Some(unit) => shown.contains(key) && !facts.done.contains(&unit),
            None => true,
        };
        // a source already through: its step succeeded or was skipped, or its unit is done
        let through = |key: &str| match key.split_once(':') {
            Some(("s", _)) => facts
                .steps
                .get(key)
                .is_some_and(|(_, mark)| matches!(mark.as_str(), "succeeded" | "skipped")),
            Some(("u", unit)) => facts.done.contains(unit),
            _ => false,
        };
        // between units, a line is a wait: from a source not through yet to a step that waits
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
                && (!relation.cross || (live(&from) && live(&to) && !through(&from)));
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
                    if waits.iter().any(|w| w.key == from) {
                        continue;
                    }
                    let (name, href, opens, doing) = match &relation.from {
                        Endpoint::Step(id) => {
                            let mark = facts.steps.get(&from).map_or("", |(_, m)| m.as_str());
                            if matches!(mark, "succeeded" | "skipped") {
                                continue;
                            }
                            let doing = match mark {
                                "running" | "failed" | "cancelled" | "stale" | "paused" => mark,
                                "external" => "outside",
                                _ => "",
                            };
                            let href = format!("/projects/id/{project}/steps/{id}");
                            (id.to_string(), href, id.to_string(), doing)
                        }
                        Endpoint::Unit(id) => {
                            if facts.done.contains(id.as_str()) {
                                continue;
                            }
                            let href = format!("/projects/id/{project}/units/{id}");
                            (format!("unit {id}"), href, String::new(), "")
                        }
                        Endpoint::Input(_) | Endpoint::Output(_) => continue,
                    };
                    waits.push(Wait {
                        drawn: live(&from),
                        key: from,
                        name,
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
    pub fn href(&self) -> String {
        self.project.href()
    }
    /// The board as drawn. Live first: the stopped units (a failure, or
    /// held up by one), then the running, then the waiting; Plan order: every unit not done in one band. Each band
    /// lays its units in layers by dependency depth among themselves, a layer ordered after the
    /// units it follows (so lines run down and seldom cross), ties in the view's order. Every
    /// done unit is on one shelf at the end.
    pub fn layout(&self) -> Layout<'_> {
        let in_matrix = |u: &UnitView| u.matrix && !u.done;
        let mut matrices: Vec<Matrix<'_>> = vec![];
        let mut by_pos: Vec<&UnitView> = self.units.iter().filter(|u| in_matrix(u)).collect();
        by_pos.sort_by_key(|u| u.pos);
        for unit in by_pos {
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
                    });
                }
            }
        }
        for m in &mut matrices {
            m.rows.sort_by_key(|u| (u.row_rank(), u.pos));
        }
        let bands: Vec<(&'static str, Vec<&UnitView>)> = if self.order == "live" {
            [("Stopped", 1), ("Running", 0), ("Waiting", 2)]
                .into_iter()
                .map(|(label, band)| {
                    let units = self
                        .units
                        .iter()
                        .filter(|u| u.band() == band && !in_matrix(u))
                        .collect();
                    (label, units)
                })
                .collect()
        } else {
            vec![(
                "",
                self.units.iter().filter(|u| !u.done && !in_matrix(u)).collect(),
            )]
        };
        let mut placed = BTreeMap::new();
        Layout {
            matrices,
            bands: bands
                .into_iter()
                .filter(|(_, units)| !units.is_empty())
                .map(|(label, units)| {
                    let (mut loose, joined): (Vec<&UnitView>, Vec<&UnitView>) =
                        units.into_iter().partition(|u| !self.joined(u));
                    // tallest first, so a row's boxes are near one height
                    loose.sort_by_key(|u| std::cmp::Reverse(u.height()));
                    Band {
                        label,
                        layers: self.layers(joined, &mut placed),
                        loose,
                    }
                })
                .collect(),
            done: self.shelf(),
            total: self.units.iter().filter(|u| u.done).count(),
            total_steps: self.units.iter().filter(|u| u.done).map(|u| u.steps.len()).sum(),
            open: self.show == "done" || !self.q.is_empty(),
        }
    }
    /// A line joins `unit` to another unit the board draws: it waits for one not done, or one
    /// not done waits for it.
    fn joined(&self, unit: &UnitView) -> bool {
        let live = |id: &str| self.units.iter().any(|u| !u.done && u.id.as_str() == id);
        let after = |id: &str| self.facts.after.get(id).into_iter().flatten();
        after(unit.id.as_str()).any(|p| p != unit.id.as_str() && live(p))
            || self.units.iter().any(|u| {
                !u.done && u.id != unit.id && after(u.id.as_str()).any(|p| p == unit.id.as_str())
            })
    }
    /// The done units the shelf draws: the latest finished first, all of them when the view
    /// asks for done units or searches, else the latest `SHELF`.
    fn shelf(&self) -> Vec<&UnitView> {
        let mut done: Vec<&UnitView> = self.units.iter().filter(|u| u.done).collect();
        done.sort_by(|a, b| b.finished().cmp(a.finished()));
        if self.show != "done" && self.q.is_empty() {
            done.truncate(SHELF);
        }
        done
    }
    /// Every done unit, under this view's order: where the shelf's "Show all" leads.
    pub fn all_done_href(&self) -> String {
        format!("{}?order={}&show=done", self.href(), self.order)
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
    /// The summary line's counts: "1139 steps · 1077 succeeded · 14 running".
    pub fn summary_counts(&self) -> String {
        let c = &self.project.counts;
        let mut words = format!(
            "{} · {} succeeded",
            super::ui::count(c.total(), "step", "steps"),
            c.succeeded
        );
        if c.running > 0 {
            words.push_str(&format!(" · {} running", c.running));
        }
        words
    }
    /// The summary line's tags, each leading to Show: Attention: failed, cancelled, quiet; and
    /// Paused.
    pub fn summary_tags(&self) -> TrustedHtml {
        use super::ui::{glyph, quiet_glyph, tag, tag_link};
        let c = &self.project.counts;
        let href = self.attention_href();
        let mut out = String::new();
        if c.failed > 0 {
            out.push_str(&tag_link(&href, &format!("{} failed", c.failed), "failed", Some(glyph("failed"))).0);
        }
        if c.cancelled > 0 {
            out.push_str(&tag_link(&href, &format!("{} cancelled", c.cancelled), "", Some(glyph("cancelled"))).0);
        }
        let quiet = self.project.quiet();
        if quiet > 0 {
            out.push_str(
                &tag_link(&href, &format!("{quiet} quiet"), "attn", Some(quiet_glyph()))
                    .0
                    .replacen(
                        "<a ",
                        "<a title=\"Running, and nothing written for 2 hours (or past its cadence)\" ",
                        1,
                    ),
            );
        }
        if self.project.paused {
            out.push_str(&tag("Paused", "", Some(glyph("paused"))).0);
        }
        TrustedHtml::owned(out)
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
    /// The board's Attention view, in the order shown: where a summary's failed, cancelled and
    /// quiet tags lead.
    pub fn attention_href(&self) -> String {
        format!("{}?order={}&show=attention", self.href(), self.order)
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
        serde_json::to_string(&relations).expect("typed relations serialize")
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
        serde_json::to_string(&relations).expect("typed relations serialize")
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
    board.name(sluice_runtime::naming::for_project(
        c,
        &super::home_of(c),
        project,
    )?);
    let mut last = last_messages(c, project, &board)?;
    // What a card shows; a step's runs, thread and submissions are its page's (load_detail).
    let mut cards = BTreeMap::new();
    let mut rows = c.prepare_cached("SELECT step_id,manual,total,done,error FROM steps WHERE project_id=?1")?;
    let mut found = rows.query([project.to_string()])?;
    while let Some(r) = found.next()? {
        let card: (bool, Option<i64>, i64, Option<String>) = (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
        cards.insert(r.get::<_, String>(0)?, card);
    }
    let finishing = sluice_store::attempts::finishing(c, project)?;
    let timings = run_timings(c, project)?;
    for unit in &mut board.units {
        if let Some((message, at, from)) = last.remove(unit.id.as_str()) {
            unit.last_message = message;
            unit.changed = at;
            unit.last_from = from;
        }
        for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
            step.finishing = finishing.get(&step.id).cloned();
            step.timing = timings.get(step.id.as_str()).cloned();
            if let Some((manual, total, done, error)) = cards.get(step.id.as_str()) {
                step.manual = *manual;
                step.total = total.map(|n| n as usize);
                step.done = *done as usize;
                if let Some(error) = error {
                    let took = step.timing.as_ref().map(|t| t.seconds);
                    step.set_failure(super::failure::Failure::parse(error, took));
                }
            }
            step.revision = revision as u64;
            if step.cancelled()
                && let Some(fact) = board.facts.steps.get_mut(&step.key())
            {
                fact.1 = "cancelled".into();
            }
        }
        // a lane's view reads its stages' live progress
        if unit.matrix && !unit.done {
            for step in unit.steps.iter_mut().filter(|s| s.status == "running") {
                super::step::load_progress(c, project, step)?;
            }
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
    // a wait on a cancelled step says so
    board.settle();
    Ok((board, plan))
}
/// Each step's current run times (its card's timer), from its current generation's runs: its
/// latest run, or for a scatter its latest round's item runs, from the first start to the last
/// end. One pass over the project's runs; a run still going is measured to the read's `now`.
fn run_timings(
    c: &Connection,
    project: ProjectId,
) -> sluice_store::Result<BTreeMap<String, RunTiming>> {
    struct Run {
        item: i64,
        work: i64,
        started: String,
        finished: Option<String>,
        from: Option<f64>,
        to: Option<f64>,
    }
    let mut runs = BTreeMap::<String, Vec<Run>>::new();
    let mut q = c.prepare_cached(
        "SELECT r.step_id,r.item_index,r.work_generation,coalesce(r.started_at,r.created_at),r.finished_at,julianday(coalesce(r.started_at,r.created_at)),julianday(coalesce(r.finished_at,'now')) FROM runs r JOIN steps s ON s.project_id=r.project_id AND s.step_id=r.step_id AND s.generation=r.generation WHERE r.project_id=?1 ORDER BY r.step_id,r.created_at,r.run_id",
    )?;
    let mut rows = q.query([project.to_string()])?;
    while let Some(r) = rows.next()? {
        runs.entry(r.get(0)?).or_default().push(Run {
            item: r.get(1)?,
            work: r.get(2)?,
            started: r.get(3)?,
            finished: r.get(4)?,
            from: r.get(5)?,
            to: r.get(6)?,
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
            let timing = RunTiming {
                started: first.started.clone(),
                finished,
                runs: all.len(),
                seconds: (to - from) * 86_400.0,
            };
            Some((step, timing))
        })
        .collect())
}
/// Each unit's last message (body and time): the newest from or to one of its steps, or in one
/// of its steps' threads. One pass over the project's messages, newest first.
fn last_messages(
    c: &Connection,
    project: ProjectId,
    board: &ProjectView,
) -> sluice_store::Result<BTreeMap<String, (String, String, String)>> {
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
                last.insert(unit.clone(), (r.get(3)?, r.get(4)?, from.clone()));
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
    let (shared, mut view, mut panel) = load(state, project, registry, None, true).await?;
    if let Some(panel) = panel.as_mut() {
        panel.set_quiet(view.project.quiet());
    }
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
    // the summary's quiet counts read the runs as observed, not as the store had them
    board.project.running = running.to_vec();
    for unit in &mut board.units {
        for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
            let run = running.iter().find(|r| r.step == step.id.as_str());
            step.quiet = run.is_some_and(|r| r.quiet);
            step.active_at = run
                .and_then(|r| r.activity)
                .map(super::rfc3339)
                .unwrap_or_default();
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
        view.units.retain(|u| {
            (match show {
                "active" => !u.done,
                // what needs a look: a failed or stale step, or one running quiet for 2h+
                "attention" => u.rank() == 0 || u.steps.iter().any(|s| s.quiet),
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
        wait.drawn = false;
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
