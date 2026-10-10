//! The v2 board. Relations retain their gate form and unit endpoints.
use super::step::{FieldView, RunTiming, StepView};
use super::ui::{Shown, Tally};
use super::{DashboardSnapshot, DashboardState, FunctionCatalog, TrustedHtml, Viewer};
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
    ids::{ProjectId, ProjectSelector, Revision, StepId, UnitName},
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
#[derive(Clone, Debug, Serialize)]
pub struct UnitView {
    pub id: UnitName,
    /// Its title (`sluice_model::naming`): "" when it has none and its id names it.
    pub title: String,
    /// Its title whole, before `TITLE_CHARS` cut it: its own page's heading.
    pub whole: String,
    /// The recipe it was made from, its stages and the params its steps give back ("" and
    /// empty when no recipe matches it).
    pub recipe: String,
    pub stages: Vec<String>,
    pub params: indexmap::IndexMap<String, String>,
    /// Its place in the plan's order.
    pub pos: usize,
    pub tagged: bool,
    pub done: bool,
    pub settled: bool,
    pub steps: Vec<StepView>,
    pub rows: Vec<Vec<StepView>>,
    pub last_message: String,
    /// Who sent its last message, its thread and its id (its page draws it as a message).
    pub last_from: String,
    /// Its sender's stage when it is a step of another unit ("work"): its row names it so.
    pub last_from_stage: String,
    pub last_thread: String,
    pub last_id: i64,
    /// Its last message was only sent to one of its steps (a note from another unit's): its
    /// row says "Note from …".
    pub last_received: bool,
    pub changed: String,
    /// The unit is one step in the plan: the board draws that step's card alone, no box.
    pub solo: bool,
    /// What each step still waits for in other units, by step id.
    pub waits: BTreeMap<String, Vec<Wait>>,
    /// A done unit a search matched in: drawn open, its matching cards in view.
    pub open: bool,
}
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
            whole: String::new(),
            recipe: String::new(),
            stages: vec![],
            params: indexmap::IndexMap::new(),
            pos: 0,
            tagged: unit.tagged,
            done: unit.done(state),
            settled: unit.settled(plan, state),
            steps,
            rows: rows.into_values().collect(),
            last_message: String::new(),
            last_from: String::new(),
            last_from_stage: String::new(),
            last_thread: String::new(),
            last_id: 0,
            last_received: false,
            changed: changed.to_owned(),
            solo: unit.steps.len() == 1,
            waits: BTreeMap::new(),
            open: false,
        }
    }
    /// Name it and its steps (`sluice_runtime::naming`): titles, stages, its recipe and params.
    fn name(&mut self, names: &sluice_runtime::naming::ProjectNaming) {
        let id = self.id.to_string();
        if let Some(named) = names.naming.unit(&id) {
            self.title = named.title.clone();
            self.whole = named.whole.clone();
            self.recipe = named.recipe.clone();
            self.stages = named.stages.clone();
            self.params = named.params.clone();
        }
        for step in self.steps.iter_mut().chain(self.rows.iter_mut().flatten()) {
            if let Some(named) = names.naming.step(step.id.as_str()) {
                step.title = named.title.clone();
                step.whole_title = named.whole.clone();
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
            .map(StepView::standing)
            .min()
            .unwrap_or(Shown::Pending)
    }
    /// "1 running · 4 pending": every state it has, in priority order.
    pub fn tally_words(&self) -> String {
        super::ui::states_words(&self.tally())
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
    pub fn waits_of(&self, step: &StepView) -> &[Wait] {
        self.waits
            .get(step.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
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
    /// Its own page's heading: its title whole, or its id.
    pub fn whole_heading(&self) -> &str {
        if self.whole.is_empty() {
            self.heading()
        } else {
            &self.whole
        }
    }
    pub fn href(&self, project: &ProjectId) -> String {
        format!("/projects/id/{project}/units/{}", self.id)
    }
    /// Its step at `stage` ("land"), when it has one.
    pub fn stage_step(&self, stage: &str) -> Option<&StepView> {
        // a unit of one step carries no stage name of its own: its step is `<unit>-<stage>`
        let id = format!("{}-{stage}", self.id);
        self.steps
            .iter()
            .find(|s| s.stage == stage)
            .or_else(|| self.steps.iter().find(|s| s.id.as_str() == id))
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
    /// A step of it named in words: its stage, else its id without the unit's prefix.
    pub fn step_name(&self, step: &StepView) -> String {
        if !step.stage.is_empty() {
            return step.stage.clone();
        }
        let id = step.id.as_str();
        id.strip_prefix(&format!("{}-", self.id))
            .unwrap_or(id)
            .to_owned()
    }
    /// Who sent its last message, in words: a step by its stage (its own or another unit's),
    /// "orchestrator", "you" (the owner), or "another unit" for a step of no stage elsewhere;
    /// never a step's id.
    pub fn sender(&self) -> String {
        match self.last_from.as_str() {
            "" => String::new(),
            "orchestrator" => "orchestrator".into(),
            "owner" => "you".into(),
            from => self
                .steps
                .iter()
                .find(|s| s.id.as_str() == from)
                .map(|s| self.step_name(s))
                .or_else(|| Some(self.last_from_stage.clone()).filter(|s| !s.is_empty()))
                .unwrap_or_else(|| "another unit".into()),
        }
    }
    /// Its first step that put a question to the owner its run waits on: its row says so.
    pub fn asking_step(&self) -> Option<&StepView> {
        self.steps.iter().find(|s| s.asking.is_some())
    }
    /// Its last message's thread, where the unit page's "Last message" leads.
    pub fn last_thread_href(&self, project: &ProjectId) -> String {
        super::threads::thread_url(*project, &self.last_thread)
    }
    /// Its steps' records and messages, and its own, on the project's log.
    pub fn log_href(&self, project: &ProjectId) -> String {
        format!("/projects/id/{project}/log?unit={}", self.id)
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
    pub show: String,
    /// The plan's find (`q`): the words a unit's id or title, or one of its steps' id, title or
    /// doc, must all contain.
    pub q: String,
    /// With a find, how many units it matched.
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
    /// With a board, the view chosen (Plan, Both or Board: `VIEWS`): from `?view=`, else the
    /// project's `sluice_view_<id>` cookie; `None` when none was chosen, so the page's script
    /// (or, without it, the stylesheet) picks.
    #[serde(skip)]
    pub view: Option<&'static str>,
    /// Its steps' and units' names, and the recipes they came from.
    #[serde(skip)]
    pub names: std::sync::Arc<sluice_runtime::naming::ProjectNaming>,
    /// Its open questions to the owner (`project.asks`) as asked: what For you draws whole.
    pub questions: Vec<Question>,
}
/// An open question to the owner as its plan's For you draws it: its words and when it was
/// asked, by its message's id.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Question {
    pub message: i64,
    pub body: String,
    pub at: String,
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
            view: None,
            names: Default::default(),
            questions: vec![],
        };
        view.settle();
        view
    }
    /// Name its steps and units (`sluice_runtime::naming`): titles, stages, recipes and their
    /// params.
    pub fn name(&mut self, names: std::sync::Arc<sluice_runtime::naming::ProjectNaming>) {
        for unit in &mut self.units {
            unit.name(&names);
        }
        self.names = names;
        self.settle();
    }
    /// Note which units each unit still waits on, and say on each step what it still waits for
    /// in other units. A wait between units is a source not yet through (succeeded or skipped,
    /// or its unit done) and is said in words, never drawn: the plan names it on its row and
    /// traces it (`plan::Plan`). Within a unit every relation is a line on its own page. A
    /// satisfied source is neither said nor traced: the step's page lists every gate.
    fn settle(&mut self) {
        let facts = &mut self.facts;
        let names = &self.names;
        let project = self.project.id;
        let mut shown = BTreeSet::new();
        for unit in &self.units {
            shown.insert(unit.key());
            shown.extend(unit.steps.iter().map(StepView::key));
        }
        // a source already through: its step succeeded or was skipped, or its unit is done
        let through = |facts: &PlanFacts, key: &str| match key.split_once(':') {
            Some(("s", _)) => facts
                .steps
                .get(key)
                .is_some_and(|(_, shown)| shown.spec().band == Placed::Done),
            Some(("u", unit)) => facts.done.contains(unit),
            _ => false,
        };
        let mut after = BTreeMap::<String, BTreeSet<String>>::new();
        for relation in &mut self.relations {
            relation.line = !relation.cross;
            if !relation.cross || through(facts, &relation.from.key()) {
                continue;
            }
            let ends = (
                facts.unit_of(&relation.from.key()),
                facts.unit_of(&relation.to.key()),
            );
            if let (Some(from), Some(to)) = ends
                && from != to
            {
                after.entry(to).or_default().insert(from);
            }
        }
        facts.after = after;
        let facts = &self.facts;
        // an end the view shows, in a unit not done (a plan input or output is always shown)
        let live = |key: &str| match facts.unit_of(key) {
            Some(unit) => shown.contains(key) && !facts.done.contains(&unit),
            None => true,
        };
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
                        line: false,
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
    /// Each unit and a unit it still waits on (a source of one of its steps not yet through):
    /// what select-to-trace follows.
    pub fn unit_waits(&self) -> Vec<(&str, &str)> {
        self.facts
            .after
            .iter()
            .flat_map(|(unit, on)| on.iter().map(move |o| (unit.as_str(), o.as_str())))
            .collect()
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
    /// The whole plan again: the focus line's way out.
    pub fn everything_href(&self) -> String {
        self.href()
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
    /// Something is left that a pause keeps from starting: a step pending (in any of its
    /// readings) or stale.
    pub fn left_to_start(&self) -> bool {
        let c = &self.project.counts;
        c.stored(&StepStatus::Pending) + c.stored(&StepStatus::Stale) > 0
    }
    /// What the find found: "3 units match “review”", "No unit matches “x”."
    pub fn match_words(&self) -> String {
        match self.matched {
            0 => format!("No unit matches “{}”.", self.q),
            n => format!(
                "{} “{}”.",
                super::ui::count(n, "unit matches", "units match"),
                self.q
            ),
        }
    }
    /// Something in it needs the owner: a step needs attention (failed, cancelled, stale, a
    /// quiet run), or a pause holds work back. A phone then opens on the plan, not the board.
    pub fn needs_attention(&self) -> bool {
        let c = &self.project.standing();
        c.attention() + c.get(Shown::Paused) > 0 || self.project.paused
    }
    /// The plan under the same show and focus, without its find: the find's clear link.
    pub fn clear_href(&self) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("show", &self.show);
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
}
/// What a page drew is gone: one calm line where it was (`id`'s element), with a way on.
pub(crate) fn gone_html(id: &str, words: &str, href: &str, link: &str) -> TrustedHtml {
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
/// Each project's plan compiled from its rows (`compile_rows`), kept while its revision and the
/// signatures it was compiled against stay the same. A page reads the revision alone (one
/// row); only a new revision or new signatures read the plan's rows and compile them again,
/// so no page reads a document or compiles per request.
#[derive(Clone, Default)]
pub struct PlanCache(std::sync::Arc<std::sync::Mutex<BTreeMap<ProjectId, CompiledPlan>>>);
struct CompiledPlan {
    rev: Revision,
    signatures: String,
    plan: std::sync::Arc<Plan>,
}
impl PlanCache {
    /// The project's plan at its current revision, compiled against `provider` (whose version is
    /// `signatures`), with that revision; read in the caller's snapshot. A plan whose rows do
    /// not compile against the signatures is `invalid`.
    pub fn plan(
        &self,
        c: &Connection,
        project: ProjectId,
        signatures: &str,
        provider: &impl SignatureProvider,
    ) -> sluice_store::Result<(Revision, std::sync::Arc<Plan>)> {
        let rev = sluice_store::plans::plan_revision(c, project)?;
        let plan = self.compiled(project, rev, signatures, || {
            let rows = sluice_store::plans::read_plan_rows(c, project)?;
            Ok(sluice_model::plan::compile_rows(&rows, provider).map_err(|e| {
                PublicError::Invalid {
                    message: "stored plan cannot be compiled".into(),
                    errors: e.into_iter().map(|e| e.to_string()).collect(),
                }
            })?)
        })?;
        Ok((rev, plan))
    }
    /// The plan kept for `(rev, signatures)`, else `compile`'s, kept in its place.
    pub fn compiled(
        &self,
        project: ProjectId,
        rev: Revision,
        signatures: &str,
        compile: impl FnOnce() -> sluice_store::Result<Plan>,
    ) -> sluice_store::Result<std::sync::Arc<Plan>> {
        let held = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(compiled) = held.get(&project)
            && compiled.rev == rev
            && compiled.signatures == signatures
        {
            return Ok(compiled.plan.clone());
        }
        drop(held);
        let plan = std::sync::Arc::new(compile()?);
        self.0.lock().unwrap_or_else(|e| e.into_inner()).insert(
            project,
            CompiledPlan {
                rev,
                signatures: signatures.to_owned(),
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
    let (Revision(revision), plan) = plans.plan(c, project, signatures, provider)?;
    let mut state = sluice_store::plans::read_state(c, project)?;
    queue(c, project, &plan, &mut state, None)?;
    let mut board = ProjectView::new(summary.clone(), &plan, &state, revision);
    board.name(sluice_runtime::naming::for_project(
        c,
        &super::home_of(c),
        project,
    )?);
    let mut last = last_messages(c, project, &board)?;
    let cards = Cards::read(c, project)?;
    for unit in &mut board.units {
        if let Some((message, at, from, thread, id, received)) = last.remove(unit.id.as_str()) {
            unit.last_message = message;
            unit.changed = at;
            unit.last_from_stage = board
                .names
                .naming
                .step(&from)
                .map(|n| n.stage.clone())
                .unwrap_or_default();
            unit.last_from = from;
            unit.last_thread = thread;
            unit.last_id = id;
            unit.last_received = received;
        }
        for step in unit.steps.iter_mut().chain(unit.rows.iter_mut().flatten()) {
            cards.decorate(step, revision);
            // its run as observed with the store's snapshot: quiet, and when it last wrote
            let run = summary.running.iter().find(|r| r.step == step.id.as_str());
            step.quiet = run.is_some_and(|r| r.quiet);
            step.active_at = run
                .and_then(|r| r.activity)
                .map(super::rfc3339)
                .unwrap_or_default();
            step.asking = summary.ask_of(step.id.as_str()).cloned();
        }
        // a unit whose every step is done but a cancel the owner dismissed is done too: on
        // the shelf, the cancel still on its card
        if !unit.done
            && unit.steps.iter().any(|s| s.dismissed && s.cancelled())
            && unit
                .steps
                .iter()
                .all(|s| s.standing().spec().band == sluice_model::shown::Band::Done)
        {
            unit.done = true;
        }
        // a running step's live progress: its recipe's view reads it, and a long run's margin
        // module draws it
        if !unit.done {
            for step in unit.steps.iter_mut().filter(|s| s.running()) {
                super::step::load_progress(c, project, step)?;
            }
        }
    }
    // each open question to the owner as asked: its words and when
    let mut asked = c.prepare_cached("SELECT body,at FROM messages WHERE project_id=?1 AND id=?2")?;
    for ask in &summary.asks {
        if let Some((body, at)) = asked
            .query_row(rusqlite::params![project.to_string(), ask.message], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?
        {
            board.questions.push(Question {
                message: ask.message,
                body,
                at,
            });
        }
    }
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
    /// The cancels the owner dismissed.
    dismissed: BTreeSet<String>,
    /// Those dismissed in the last ten minutes, each with when.
    lately: BTreeMap<String, String>,
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
            dismissed: sluice_store::messages::dismissed(c, project)?,
            lately: sluice_store::messages::dismissed_lately(c, project, 10.0)?,
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
        step.dismissed = self.dismissed.contains(step.id.as_str());
        step.dismissed_at = self
            .lately
            .get(step.id.as_str())
            .cloned()
            .unwrap_or_default();
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
    let (Revision(revision), plan) = plans.plan(c, project, signatures, provider)?;
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
            cards.decorate(step, revision);
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
    step.timeline = unit.timeline(Some(id));
    // its unit's stages in its band: a unit of one step is the step alone
    if unit.steps.len() > 1 {
        step.lane = super::unit_page::stages(&unit);
        step.lane_unit = unit.id.to_string();
        step.lane_recipe = unit.recipe.clone();
    }
    step.chained = !plan.dependencies(id).is_empty()
        || plan.steps().keys().any(|s| plan.dependencies(s).contains(id));
    // every step after it, near or far: what a succeeded step's Retry may send round again
    let mut dependents: BTreeMap<&StepId, Vec<&StepId>> = BTreeMap::new();
    for s in plan.steps().keys() {
        for d in plan.dependencies(s) {
            dependents.entry(d).or_default().push(s);
        }
    }
    let mut after: BTreeSet<&StepId> = BTreeSet::new();
    let mut next = vec![id];
    while let Some(from) = next.pop() {
        for s in dependents.get(from).into_iter().flatten() {
            if after.insert(s) {
                next.push(s);
            }
        }
    }
    step.downstream = after.len();
    observe(c, project, &mut step)?;
    // its waits name what they wait on by how each reads, as the board's do (`known`)
    let mut shown = BTreeMap::new();
    for other in plan
        .dependencies(id)
        .iter()
        .chain(step.gates.iter().filter_map(|g| g.step.as_ref()))
    {
        let mut view = StepView::new(project, &plan, &state, other);
        cards.decorate(&mut view, revision);
        observe(c, project, &mut view)?;
        shown.insert(other.clone(), view.shown());
    }
    step.name_waits(&plan, &state, &|id| shown.get(id).copied());
    // every step it waits on is pending too: name the first one up the chain that holds it
    // (running, stopped, paused: neither pending nor done), so "why has it not started?" is one
    // look, not a hop per link
    let deps = plan.dependencies(id);
    if step.pending()
        && !deps.is_empty()
        && deps.iter().all(|d| shown.get(d) == Some(&Shown::Pending))
    {
        let mut seen: BTreeSet<&StepId> = deps.iter().collect();
        let mut queue: std::collections::VecDeque<(&StepId, &StepId)> =
            deps.iter().map(|d| (d, d)).collect();
        while let Some((at, through)) = queue.pop_front() {
            let reads = match shown.get(at) {
                Some(known) => *known,
                None => {
                    let mut view = StepView::new(project, &plan, &state, at);
                    cards.decorate(&mut view, revision);
                    observe(c, project, &mut view)?;
                    view.shown()
                }
            };
            // a done step holds nothing: the chain goes on only through what is pending
            if reads.spec().band == sluice_model::shown::Band::Done {
                continue;
            }
            if reads != Shown::Pending {
                let gate = |id: &StepId, reads: Option<Shown>| super::step::GateView {
                    entry: id.to_string(),
                    href: format!("/projects/id/{project}/steps/{id}"),
                    name: super::ui::StepRef::new(id.as_str(), names.naming.step(id.as_str())),
                    shown: reads,
                    step: Some(id.clone()),
                };
                step.held_by = Some((gate(at, Some(reads)), gate(through, shown.get(through).copied())));
                break;
            }
            if seen.len() > 512 {
                break;
            }
            for up in plan.dependencies(at) {
                if seen.insert(up) {
                    queue.push_back((up, through));
                }
            }
        }
    }
    step.runner_stopped = c.query_row(
        "SELECT scheduler_owner IS NULL FROM maintenance WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    step.name_links(&names.naming);
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
/// A unit's last message: its body, time, sender, thread and id, and whether it was only sent
/// to it (a note from another unit's step), not said in or from its own steps' threads.
type LastMessage = (String, String, String, String, i64, bool);
/// Each unit's last message: the newest from one of its steps or in one of its steps' threads;
/// a unit with none, the newest sent to one of its steps. So a note one step sends to many
/// rests on the rows of those with nothing of their own. One pass over the project's messages,
/// newest first.
fn last_messages(
    c: &Connection,
    project: ProjectId,
    board: &ProjectView,
) -> sluice_store::Result<BTreeMap<String, LastMessage>> {
    let mut unit_of = BTreeMap::new();
    let mut steps = c.prepare("SELECT step_id,coalesce(unit,step_id) FROM steps WHERE project_id=?1")?;
    let mut rows = steps.query([project.to_string()])?;
    while let Some(r) = rows.next()? {
        unit_of.insert(r.get::<_, String>(0)?, r.get::<_, String>(1)?);
    }
    let wanted: BTreeSet<&str> = board.units.iter().map(|u| u.id.as_str()).collect();
    let mut own: BTreeMap<String, LastMessage> = BTreeMap::new();
    let mut sent_to: BTreeMap<String, LastMessage> = BTreeMap::new();
    let mut messages = c.prepare(
        "SELECT \"from\",\"to\",thread,body,at,id FROM messages WHERE project_id=?1 ORDER BY id DESC",
    )?;
    let mut rows = messages.query([project.to_string()])?;
    while own.len() < wanted.len()
        && let Some(r) = rows.next()?
    {
        let (from, to, thread): (String, Option<String>, String) = (r.get(0)?, r.get(1)?, r.get(2)?);
        let mine = [
            unit_of.get(&from),
            thread.strip_prefix("step-").and_then(|step| unit_of.get(step)),
        ];
        let message = |received| -> rusqlite::Result<LastMessage> {
            Ok((r.get(3)?, r.get(4)?, from.clone(), thread.clone(), r.get(5)?, received))
        };
        for unit in mine.into_iter().flatten() {
            if wanted.contains(unit.as_str()) && !own.contains_key(unit) {
                own.insert(unit.clone(), message(false)?);
            }
        }
        if let Some(unit) = to.and_then(|to| unit_of.get(&to))
            && wanted.contains(unit.as_str())
            && !own.contains_key(unit)
            && !sent_to.contains_key(unit)
        {
            sent_to.insert(unit.clone(), message(true)?);
        }
    }
    for (unit, message) in sent_to {
        own.entry(unit).or_insert(message);
    }
    Ok(own)
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
                super::panel::gather(
                    c,
                    project,
                    Some((Revision(board.revision), &plan)),
                    &board,
                    None,
                )?
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
    pub show: Option<String>,
    pub tag: Option<String>,
    /// Words a unit's id or title, or one of its steps' id, title or doc, must all contain (any
    /// order, any case).
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
    /// The view chosen with a board: plan, both or board (`VIEWS`).
    pub view: Option<String>,
}
/// The ways a project with a board is seen: the plan alone, both side by side, the board alone.
pub const VIEWS: [&str; 3] = ["plan", "both", "board"];
/// The cookie that keeps a project's view without script.
pub fn view_cookie(project: ProjectId) -> String {
    format!("sluice_view_{project}")
}
/// The view a page is drawn in: `?view=` when it names one, else the project's cookie.
pub fn chosen_view(query: &BoardQuery, headers: &HeaderMap, project: ProjectId) -> Option<&'static str> {
    let named = |v: &str| VIEWS.iter().copied().find(|w| *w == v);
    if let Some(v) = query.view.as_deref().and_then(named) {
        return Some(v);
    }
    let name = view_cookie(project);
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .unwrap_or("")
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .and_then(|(_, v)| named(v))
}
impl BoardQuery {
    /// Filter and find in `view` as the page's query asks. The plan keeps a unit whole: a find
    /// or a chain keeps the units with a step in it, never part of one.
    pub fn apply(&self, view: &mut ProjectView) -> Result<(), PublicError> {
        let show = self.show.as_deref().unwrap_or("all");
        if !["all", "active", "attention", "done"].contains(&show)
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
            view.units
                .retain(|unit| unit.steps.iter().any(|s| chain.contains(s.id.as_str())));
            (view.root, view.up, view.down, view.depth) = (root.into(), up, down, depth);
        }
        let before = view.units.len();
        view.units.retain(shows);
        view.hidden = before - view.units.len();
        view.show = show.into();
        let q: String = self.q.as_deref().unwrap_or("").trim().chars().take(200).collect();
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("show", show);
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
        if !q.is_empty() {
            view.matched = search(&mut view.units, &q);
        }
        view.q = q;
        view.settle();
        Ok(())
    }
}
/// Keep the units whose id or title, or one of whose steps' id, title or doc, holds every word
/// of `q` (case-insensitive, in any order), each whole. Returns how many units matched.
fn search(units: &mut Vec<UnitView>, q: &str) -> usize {
    let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
    units.retain(|unit| {
        let own = format!("{} {}", unit.id, unit.title).to_lowercase();
        let hit = |text: &str| words.iter().all(|w| text.contains(w.as_str()));
        hit(&own)
            || unit.steps.iter().any(|step| {
                hit(&format!("{} {} {} {own}", step.id.as_str(), step.title, step.doc).to_lowercase())
            })
    });
    units.len()
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
/// The board's `?format=mermaid`: the `plan_view` tool's Mermaid for the project, drawn from
/// the graph index (nothing is compiled).
async fn plan_mermaid(
    state: &DashboardState,
    project: ProjectId,
    all: bool,
) -> Result<String, PublicError> {
    state
        .reads
        .snapshot(move |c| {
            sluice_runtime::dispatch_ext::render_plan_view(
                c,
                &super::home_of(c),
                project,
                sluice_model::plan_rows::PlanViewQuery {
                    project: ProjectSelector::Id(project),
                    format: sluice_model::commands::PlanViewFormat::Mermaid,
                    all,
                    units: None,
                    steps: None,
                    status: None,
                    recipe: None,
                },
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
            let text = plan_mermaid(&state, project, query.all.unwrap_or(false)).await?;
            return Ok((
                [(
                    axum::http::header::CONTENT_TYPE,
                    "text/plain; charset=utf-8",
                )],
                text,
            )
                .into_response());
        }
        view.view = chosen_view(&query, &headers, project);
        let html = super::plan::render(&view, &shared, &Viewer::from_headers(&headers))?;
        let mut response = Html(html.0).into_response();
        // a view chosen by its link (without script) is kept for the next visit
        if let Some(v) = query.view.as_deref().filter(|v| VIEWS.contains(v)) {
            let cookie = format!(
                "{}={v}; Path=/; Max-Age=34560000; SameSite=Lax",
                view_cookie(project)
            );
            response.headers_mut().append(
                axum::http::header::SET_COOKIE,
                cookie.parse().expect("validated ASCII cookie"),
            );
        }
        Ok(response)
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
    let headers = std::sync::Arc::new(headers);
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
        let headers = headers.clone();
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
            view.view = chosen_view(&query, &headers, project);
            Ok(super::plan::draw(&view, &shared, &viewer)?.2)
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
pub fn registration() -> super::PageRegistration {
    use super::{Asset, NavEntry, PageRegistration};
    use axum::routing::get;
    PageRegistration {
        routes: |state| {
            axum::Router::new()
                .route("/projects/{name}", get(project_redirect))
                .route("/projects/id/{project}", get(project_page))
                .route("/projects/id/{project}/stream", get(project_stream))
                .route("/projects/id/{project}/units/{unit}", get(super::unit_page::unit_page))
                .route(
                    "/projects/id/{project}/units/{unit}/stream",
                    get(super::unit_page::unit_stream),
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
