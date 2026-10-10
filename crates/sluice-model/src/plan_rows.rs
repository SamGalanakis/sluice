//! Schema-3 plan contracts: the wire types of the scoped reads, the atomic `plan_edit`, the
//! logged row changes and the store's row projections. `docs/design/plan-rows.md` is the
//! contract; every JSON example there is a fixture under `tests/fixtures/plan_rows/` that
//! these types round-trip.
//!
//! These types sit beside the schema-1 ones they replace (`commands::EditPreview`,
//! `commands::EditResult`, `events::Event::PlanEdit`'s `ops`, `PatchOperation`): the
//! implementing lanes switch the commands, events and store over to them and delete the old
//! ones. Nothing here is wired into dispatch yet, and nothing here reads or writes state.

use crate::{
    commands::{KeptUnit, PlanViewFormat, ProjectIdentity, StepStatus, UnsupportedInput},
    error::PublicError,
    ids::{ProjectId, ProjectSelector, RecordSeq, Revision, StepId, UnitName, WorkGeneration},
    rpc::{JsonMap, JsonValue},
};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

fn yes() -> bool {
    true
}
fn is_false(value: &bool) -> bool {
    !value
}
fn default_limit() -> u32 {
    DEFAULT_LIMIT
}
/// A field where absent (`None`) and null (`Some(None)`) differ: in `StepChanges`, absent
/// leaves a field as it is and null removes it.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// The page size `plan_read` and `plan_history` use when `limit` is left out.
pub const DEFAULT_LIMIT: u32 = 200;
/// The largest page: a larger `limit` reads as this one; 0 is `bad_request`.
pub const MAX_LIMIT: u32 = 1000;

// ---- tokens ---------------------------------------------------------------------------

/// The project's execution-state witness (`plans.state_epoch`): the writer adds one in every
/// transaction that changes something an edit's preparation reads besides the plan itself
/// (input values, step statuses, outputs and results, the project's pause, resource
/// capacities and leases). Never decreases; progress and messages leave it alone.
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct StateEpoch(pub u64);

/// The recipe files a project sees, as a token compared for equality only: the first 16 hex
/// digits of the SHA-256 of the project's and the home's recipe listing (each file's name,
/// size and modification time). Recipe files change outside sluice, so there is no counter.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RecipeGeneration(pub String);

/// The coordinator's fn catalog publication a preparation compiled against (in memory, one
/// more per catalog publication; it restarts with the coordinator, which also drops every
/// prepared edit).
#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct CatalogGeneration(pub u64);

/// What a prepared edit was worked out from. The writer commits it only while every token
/// still holds; otherwise the edit is prepared again (§ edit transaction).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidationTokens {
    pub plan_rev: Revision,
    pub state_epoch: StateEpoch,
    pub catalog_generation: CatalogGeneration,
    pub recipe_generation: RecipeGeneration,
    pub board_rev: Revision,
}

// ---- rows ------------------------------------------------------------------------------

/// A root section of the plan document, in the order `plans.root_order` keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RootSection {
    Inputs,
    Outputs,
    Steps,
}
impl RootSection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inputs => "inputs",
            Self::Outputs => "outputs",
            Self::Steps => "steps",
        }
    }
}

/// The `plans` row: the authored revision, the present root sections in document order and
/// the execution-state witness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanHeader {
    pub rev: Revision,
    pub root_order: Vec<RootSection>,
    pub state_epoch: StateEpoch,
}
/// An `inputs` row's authored part: the declaration exactly as written (`"string"`,
/// `{"type": …}` or `{"type": …, "doc": …}`). The runtime value is not authored.
#[derive(Debug, Clone, PartialEq)]
pub struct InputRow {
    pub name: String,
    pub position: u64,
    pub declaration: JsonValue,
}
/// A `plan_outputs` row: the binding exactly as written (`{"source": "<ref>"}`).
#[derive(Debug, Clone, PartialEq)]
pub struct OutputRow {
    pub name: String,
    pub position: u64,
    pub binding: JsonMap,
}
/// A `steps` row's authored part: the declaration exactly as written.
#[derive(Debug, Clone, PartialEq)]
pub struct StepRow {
    pub step: StepId,
    pub position: u64,
    pub declaration: JsonMap,
}
/// Every authored row of one plan, each collection in position order: what `export_plan`
/// assembles the document from and `compile_rows` compiles.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanRows {
    pub header: PlanHeader,
    pub inputs: Vec<InputRow>,
    pub outputs: Vec<OutputRow>,
    pub steps: Vec<StepRow>,
}
/// `export_plan`'s result: the document assembled from rows, never compiled.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedPlan {
    pub rev: Revision,
    pub document: JsonMap,
}

/// Which steps a row read selects. `None` in a dimension is unrestricted, an empty list
/// matches nothing; lists are OR within a dimension and dimensions AND together. `units` is
/// already resolved: a `recipe` filter is turned into its matching units by the caller,
/// which owns recipe matching.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowSelection {
    pub units: Option<Vec<UnitName>>,
    pub steps: Option<Vec<StepId>>,
    pub status: Option<Vec<StepStatus>>,
    /// Keyset: only rows after this `(position, step)`.
    pub after: Option<(u64, StepId)>,
    /// At most this many rows (one more is read to set `StepRows::more`).
    pub limit: Option<u32>,
}
/// How much of each step row a read decodes. `Compact` never selects `declaration`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepProjection {
    Compact,
    Full,
}
/// One step row as a read projects it. `declaration` is `Some` exactly for `Full`.
#[derive(Debug, Clone, PartialEq)]
pub struct StepRowView {
    pub step: StepId,
    pub position: u64,
    pub run: String,
    pub unit: UnitName,
    pub priority: i64,
    pub paused: PauseValue,
    pub status: StepStatus,
    pub declaration: Option<JsonMap>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct StepRows {
    pub rev: Revision,
    pub state_epoch: StateEpoch,
    pub steps: Vec<StepRowView>,
    /// More rows match after the last one returned.
    pub more: bool,
}
/// Which references a read returns: those a consumer makes, or those naming a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceSelection {
    /// The references these steps make (bindings and gates).
    Consumers(Vec<StepId>),
    /// The references these plan outputs make.
    Outputs(Vec<String>),
    /// Every reference naming one of these sources, from steps and plan outputs.
    Sources { kind: SourceKind, ids: Vec<String> },
}
/// A `plan_refs` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReferenceRow {
    pub consumer_kind: ConsumerKind,
    pub consumer_id: String,
    pub slot: String,
    pub ordinal: u32,
    pub kind: RefKind,
    pub source_kind: SourceKind,
    pub source_id: String,
    pub source_port: String,
    pub source_path: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceRows(pub Vec<ReferenceRow>);
/// A `plan_edges` row: `target` depends on `source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeRow {
    pub source: StepId,
    pub target: StepId,
    pub kind: EdgeKind,
    /// The unit whose `unit:` gate this edge expands, else `None` (`''` in SQL).
    pub via_unit: Option<UnitName>,
}
/// A selection's steps and every edge touching them. `boundary` are the steps outside the
/// selection that an edge reaches, in position order (drawn as boundary nodes).
#[derive(Debug, Clone, PartialEq)]
pub struct GraphRows {
    pub steps: Vec<StepRowView>,
    pub edges: Vec<EdgeRow>,
    pub boundary: Vec<StepRowView>,
}

// ---- prepared edits: the model → store handoff -----------------------------------------

/// A step's rebuildable index rows, derived from its declaration alone (given which bare
/// gate names are steps): `steps.unit`, `step_tags` and its `plan_refs`.
#[derive(Debug, Clone, PartialEq)]
pub struct StepIndexRows {
    pub step: StepId,
    pub unit: UnitName,
    /// Declaration order, deduplicated.
    pub tags: Vec<String>,
    /// `(slot, ordinal)` order.
    pub references: Vec<ReferenceRow>,
}
/// Everything an edit writes to the authored rows and their indexes. The store applies it
/// as given and derives nothing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RowDelta {
    /// The authored row changes, in the logged order (`PlanChange`).
    pub changes: Vec<PlanChange>,
    /// The index rows of every step a `step.put` writes, replacing that step's.
    pub step_index: Vec<StepIndexRows>,
    /// The `plan_refs` rows of every plan output an `output.put` writes, replacing that
    /// output's (an `output.delete` deletes them).
    pub output_refs: Vec<(String, Vec<ReferenceRow>)>,
    /// For each step whose incoming edges change (its own declaration changed, or a unit
    /// it gates on changed its exit steps), its complete set of incoming `plan_edges`.
    pub edges: Vec<(StepId, Vec<EdgeRow>)>,
}
/// One step's status as the edit's reconciliation leaves it (only steps that change).
#[derive(Debug, Clone, PartialEq)]
pub struct StatusTransition {
    pub step: StepId,
    pub from: StepStatus,
    pub to: StepStatus,
    pub skipped: Vec<crate::types::SkipReason>,
    pub error: Option<String>,
}
/// What the edit does to runtime state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StateDelta {
    /// Steps the edit removes: their outcome is archived, then their row is deleted.
    pub removed: Vec<StepId>,
    /// Steps the edit adds: fresh `pending` rows, `generation` the new revision.
    pub added: Vec<StepId>,
    pub transitions: Vec<StatusTransition>,
}
/// An edit worked out outside the writer from a certified base. The writer commits it only
/// while `tokens` hold (and `rev`, when the caller gave one, is current).
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedPlanEdit {
    pub tokens: ValidationTokens,
    /// The caller's explicit revision: a mismatch is `conflict`, never a re-preparation.
    pub rev: Option<Revision>,
    pub dry_run: bool,
    pub author: String,
    pub reason: String,
    pub rows: RowDelta,
    pub state: StateDelta,
    pub preview: EditPreview,
    /// The reply's `steps` (see each tool).
    pub steps: Option<Vec<StepId>>,
    pub board_warnings: Vec<String>,
    /// `step_set_input`'s report.
    pub inputs: Option<crate::edit::InputChanges>,
    /// `plan_prune`'s removal set; the store's age evidence is checked beside it.
    pub prune: Option<crate::units::PruneSet>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConsumerKind {
    Step,
    Output,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Binding,
    Gate,
    Output,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Step,
    Input,
    Unit,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Data,
    Gate,
}

// ---- logged changes --------------------------------------------------------------------

/// One resolved row change, as `plan_edits.changes`, a `plan.edit` record and an edit
/// preview carry it. A revision's changes are a set (at most one per collection and key),
/// listed header first, then deletes (inputs, outputs, steps), then puts (inputs, outputs,
/// steps), each group in key order for deletes and position order for puts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum PlanChange {
    #[serde(rename = "header.put")]
    HeaderPut { root_order: Vec<RootSection> },
    #[serde(rename = "input.put")]
    InputPut {
        name: String,
        position: u64,
        declaration: JsonValue,
    },
    #[serde(rename = "input.delete")]
    InputDelete { name: String },
    #[serde(rename = "output.put")]
    OutputPut {
        name: String,
        position: u64,
        binding: JsonMap,
    },
    #[serde(rename = "output.delete")]
    OutputDelete { name: String },
    #[serde(rename = "step.put")]
    StepPut {
        step: StepId,
        position: u64,
        declaration: JsonMap,
    },
    #[serde(rename = "step.delete")]
    StepDelete { step: StepId },
}

/// The `plan.edit` record's fields (record payload version 2): the change set replaces the
/// RFC 6902 `ops`. The `plan_edits` row stores the same `changes`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanEditEvent {
    pub rev: Revision,
    pub author: String,
    pub reason: String,
    pub changes: Vec<PlanChange>,
}

/// The records `plan_history` returns: every plan edit (from `plan_edits`, never trimmed)
/// and the log's retained `plan.input`, `step.output` and `step.retry` records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum HistoryEvent {
    #[serde(rename = "plan.edit")]
    PlanEdit(PlanEditEvent),
    #[serde(rename = "plan.input")]
    PlanInput {
        rev: Revision,
        author: String,
        reason: String,
        name: String,
        value: JsonValue,
    },
    #[serde(rename = "step.output")]
    StepOutput {
        rev: Revision,
        author: String,
        reason: String,
        step: StepId,
        outputs: JsonMap,
        force: bool,
    },
    #[serde(rename = "step.retry")]
    StepRetry {
        rev: Revision,
        author: String,
        reason: String,
        step: StepId,
        work: WorkGeneration,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HistoryRecord {
    pub seq: RecordSeq,
    pub at: String,
    pub project: Option<ProjectId>,
    #[serde(flatten)]
    pub event: HistoryEvent,
}

// ---- edit operations -------------------------------------------------------------------

/// What `order.set` reorders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrderCollection {
    Steps,
    Inputs,
    Outputs,
}

/// A step's fields to change: each given field replaces that field, null removes it, an
/// absent one is left as it is. `in` replaces the whole binding map. At least one field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepChanges {
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub run: Option<Option<JsonValue>>,
    #[serde(
        rename = "in",
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub bindings: Option<Option<JsonValue>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub scatter: Option<Option<JsonValue>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub doc: Option<Option<JsonValue>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub outputs: Option<Option<JsonValue>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub paused: Option<Option<JsonValue>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub after: Option<Option<JsonValue>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub tags: Option<Option<JsonValue>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub needs: Option<Option<JsonValue>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub priority: Option<Option<JsonValue>>,
}
impl StepChanges {
    /// The step keys in declaration-key spelling, in this order.
    pub const KEYS: [&'static str; 10] = [
        "run", "in", "scatter", "doc", "outputs", "paused", "after", "tags", "needs", "priority",
    ];
    /// Each given field by its declaration key: `Some(value)` replaces, `None` removes.
    pub fn fields(&self) -> Vec<(&'static str, Option<&JsonValue>)> {
        [
            &self.run,
            &self.bindings,
            &self.scatter,
            &self.doc,
            &self.outputs,
            &self.paused,
            &self.after,
            &self.tags,
            &self.needs,
            &self.priority,
        ]
        .into_iter()
        .zip(Self::KEYS)
        .filter_map(|(field, key)| field.as_ref().map(|value| (key, value.as_ref())))
        .collect()
    }
    pub fn is_empty(&self) -> bool {
        self.fields().is_empty()
    }
}

/// One operation of `plan_edit`. Operations apply in order to one candidate (each sees what
/// those before it did); the candidate is validated once, whole, and commits all or nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum PlanOp {
    /// Declare a plan input or replace its declaration (an existing one keeps its position).
    #[serde(rename = "input.put")]
    InputPut {
        name: String,
        declaration: JsonValue,
    },
    #[serde(rename = "input.remove")]
    InputRemove { name: String },
    /// Declare a plan output `{"source": source}` or replace it.
    #[serde(rename = "output.put")]
    OutputPut { name: String, source: String },
    #[serde(rename = "output.remove")]
    OutputRemove { name: String },
    #[serde(rename = "step.add")]
    StepAdd { step: StepId, spec: JsonMap },
    #[serde(rename = "step.update")]
    StepUpdate {
        step: StepId,
        changes: Box<StepChanges>,
    },
    #[serde(rename = "step.remove")]
    StepRemove { steps: Vec<StepId> },
    /// `step` is a step id or `unit:<name>` (that unit's entry steps).
    #[serde(rename = "edge.add")]
    EdgeAdd { step: String, after: Vec<String> },
    #[serde(rename = "edge.remove")]
    EdgeRemove { step: String, after: Vec<String> },
    #[serde(rename = "unit.add")]
    UnitAdd {
        recipe: String,
        unit: UnitName,
        #[serde(default)]
        params: JsonMap,
        /// Recipe step suffix (or `*`, the unit's entry steps) to gate entries.
        #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
        #[schemars(with = "std::collections::BTreeMap<String, Vec<String>>")]
        after: IndexMap<String, Vec<String>>,
        /// Recipe step suffix to input name to a literal value.
        #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
        #[schemars(with = "std::collections::BTreeMap<String, JsonMap>")]
        inputs: IndexMap<String, JsonMap>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tags: Vec<String>,
    },
    /// Change the unit's member steps, each by its exact id.
    #[serde(rename = "unit.update")]
    UnitUpdate {
        unit: UnitName,
        #[schemars(with = "std::collections::BTreeMap<String, StepChanges>")]
        changes: IndexMap<StepId, StepChanges>,
    },
    #[serde(rename = "unit.remove")]
    UnitRemove { unit: UnitName },
    /// Every current member of the collection exactly once, in the new order. Needs `rev`.
    #[serde(rename = "order.set")]
    OrderSet {
        collection: OrderCollection,
        ids: Vec<String>,
    },
}

/// What an edit's preview describes: the edited steps and what the edit changes for others
/// (`impact`, the default), or every ready step of the plan (`all`, only with `dry_run`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewScope {
    #[default]
    Impact,
    All,
}
impl PreviewScope {
    fn is_impact(&self) -> bool {
        *self == Self::Impact
    }
}

// ---- requests --------------------------------------------------------------------------
//
// Each request is the tool's flat public arguments; on the wire `project` is a
// `ProjectSelector` (`{"kind": "name", "value": "lash"}`) where the public form is the string.

/// `plan_read`: a page of the plan's steps, in `(position, id)` order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanRead {
    pub project: ProjectSelector,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<Vec<UnitName>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<StepId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Vec<StepStatus>>,
    /// Only the units this recipe matches (SPEC §6.8's dynamic matching).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(default = "yes")]
    pub compact: bool,
    #[serde(default = "default_limit")]
    pub limit: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}
impl PlanRead {
    pub fn filter(&self) -> PlanReadFilter {
        PlanReadFilter {
            units: self.units.clone(),
            steps: self.steps.clone(),
            status: self.status.clone(),
            recipe: self.recipe.clone(),
        }
    }
}

/// `step_get`: one step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepGet {
    pub project: ProjectSelector,
    pub step: StepId,
    #[serde(default, skip_serializing_if = "is_false")]
    pub compact: bool,
}

/// `unit_get`: one unit and its steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitGet {
    pub project: ProjectSelector,
    pub unit: UnitName,
    #[serde(default, skip_serializing_if = "is_false")]
    pub compact: bool,
}

/// `plan_edit`: several operations as one edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanEditRequest {
    pub project: ProjectSelector,
    pub ops: Vec<PlanOp>,
    /// The revision the edit was worked out from; another current one is `conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<Revision>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "PreviewScope::is_impact")]
    pub preview_scope: PreviewScope,
    /// `false` adds new steps paused (`"paused": true`) unless they set `paused` themselves.
    #[serde(default = "yes")]
    pub start: bool,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// `unit_update`: change a unit's member steps by their exact ids.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitUpdate {
    pub project: ProjectSelector,
    pub unit: UnitName,
    #[schemars(with = "std::collections::BTreeMap<String, StepChanges>")]
    pub changes: IndexMap<StepId, StepChanges>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<Revision>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "PreviewScope::is_impact")]
    pub preview_scope: PreviewScope,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// `unit_remove`: remove every step of a unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitRemove {
    pub project: ProjectSelector,
    pub unit: UnitName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<Revision>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "PreviewScope::is_impact")]
    pub preview_scope: PreviewScope,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// `plan_view` with `plan_read`'s filters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanViewQuery {
    pub project: ProjectSelector,
    #[serde(default = "mermaid")]
    pub format: PlanViewFormat,
    #[serde(default, skip_serializing_if = "is_false")]
    pub all: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<Vec<UnitName>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<StepId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Vec<StepStatus>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
}
fn mermaid() -> PlanViewFormat {
    PlanViewFormat::Mermaid
}

/// `plan_history`: a page of the plan's history, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanHistoryQuery {
    pub project: ProjectSelector,
    /// Only records whose `rev` is greater.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_rev: Option<Revision>,
    /// Only records whose `seq` is greater (the previous page's `next_after_seq`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<RecordSeq>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

// ---- replies ---------------------------------------------------------------------------

/// `true`, `false` or the pause's reason, as a step's `paused` reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PauseValue {
    Flag(bool),
    Reason(String),
}

/// A step in a few fields, read from indexed columns without its declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompactStep {
    pub id: StepId,
    pub unit: UnitName,
    /// The recipe its unit matches now, if any.
    pub recipe: Option<String>,
    pub position: u64,
    pub run: String,
    pub status: StepStatus,
    pub paused: PauseValue,
    pub priority: i64,
}
/// A step's reference to a plan input, another step's output, a step or a unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepReference {
    /// `binding` or `gate` (`output` is a plan output's, never a step's).
    pub kind: RefKind,
    /// `in.<input>` for a binding, `after` for a gate.
    pub slot: String,
    /// The index in a fan-in list or in `after`; 0 for a single source.
    pub ordinal: u32,
    pub source_kind: SourceKind,
    pub source_id: String,
    /// The output name of a step source; "" for an input, a unit or a step gate.
    pub source_port: String,
    /// The `.field` path after the source, without its leading dot; "" for none.
    pub source_path: String,
}
/// The compact fields, the declaration as written (`spec`) and its references.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FullStep {
    pub id: StepId,
    pub unit: UnitName,
    pub recipe: Option<String>,
    pub position: u64,
    pub run: String,
    pub status: StepStatus,
    pub paused: PauseValue,
    pub priority: i64,
    pub spec: JsonMap,
    pub references: Vec<StepReference>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StepView {
    Full(FullStep),
    Compact(CompactStep),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanReadResult {
    pub project: ProjectIdentity,
    pub rev: Revision,
    pub state_epoch: StateEpoch,
    pub recipe_generation: RecipeGeneration,
    pub steps: Vec<StepView>,
    /// Pass it back as `cursor` with the same project and filters for the next page; null on
    /// the last page.
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepGetResult {
    pub project: ProjectIdentity,
    pub rev: Revision,
    pub state_epoch: StateEpoch,
    pub step: StepView,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitView {
    pub id: UnitName,
    pub recipe: Option<String>,
    pub entry_steps: Vec<StepId>,
    pub exit_steps: Vec<StepId>,
    /// Every step succeeded or was skipped (SPEC §6.7).
    pub done: bool,
    /// No step running and every pending one external or not ready (SPEC §6.7).
    pub settled: bool,
    pub steps: Vec<StepView>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnitGetResult {
    pub project: ProjectIdentity,
    pub rev: Revision,
    pub state_epoch: StateEpoch,
    pub recipe_generation: RecipeGeneration,
    pub unit: UnitView,
}
/// `plan_get`: the whole plan, assembled from its rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanGetResult {
    pub project: ProjectIdentity,
    pub rev: Revision,
    pub plan: JsonMap,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanHistoryPage {
    pub project: ProjectIdentity,
    pub entries: Vec<HistoryRecord>,
    /// The last entry's seq when more entries match, for `after_seq`; null on the last page.
    pub next_after_seq: Option<RecordSeq>,
}

/// An edit's preview: the row changes it makes and what they do to the steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditPreview {
    pub scope: PreviewScope,
    pub changes: Vec<PlanChange>,
    /// Ready executable steps (never `core.external`), including those in `would_queue`:
    /// within `scope`.
    pub would_start: Vec<StepId>,
    pub would_queue: Vec<StepId>,
    pub would_skip: Vec<StepId>,
    pub would_stale: Vec<StepId>,
    pub errors: Vec<String>,
}
/// Every edit tool's reply (with `dry_run`, the preview alone).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditResult {
    pub project: ProjectIdentity,
    /// The new revision, or the current one when the edit changed nothing (its
    /// `preview.changes` is then empty and nothing was committed).
    pub rev: Revision,
    pub preview: EditPreview,
    /// The steps the edit was about (see the tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<StepId>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub board_warnings: Vec<String>,
}
/// `step_set_input`'s reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InputEditResult {
    #[serde(flatten)]
    pub edit: EditResult,
    pub changed: Vec<StepId>,
    pub running: Vec<StepId>,
    pub unsupported: Vec<UnsupportedInput>,
}
/// `plan_prune`'s reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PruneResult {
    #[serde(flatten)]
    pub edit: EditResult,
    pub units: Vec<UnitName>,
    pub kept: Vec<KeptUnit>,
}

// ---- cursors ---------------------------------------------------------------------------

/// `plan_read`'s filters, the part of a request a cursor is bound to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanReadFilter {
    pub units: Option<Vec<UnitName>>,
    pub steps: Option<Vec<StepId>>,
    pub status: Option<Vec<StepStatus>>,
    pub recipe: Option<String>,
}
impl PlanReadFilter {
    /// The first 16 hex digits of the SHA-256 of the project id and the filters as sets:
    /// `{"project":…,"units":…,"steps":…,"status":…,"recipe":…}` in that key order, each
    /// list sorted and deduplicated, an absent filter null, compact JSON.
    pub fn digest(&self, project: ProjectId) -> String {
        fn set<T: ToString>(values: &Option<Vec<T>>) -> serde_json::Value {
            match values {
                None => serde_json::Value::Null,
                Some(values) => {
                    let mut words: Vec<String> = values.iter().map(ToString::to_string).collect();
                    words.sort();
                    words.dedup();
                    serde_json::json!(words)
                }
            }
        }
        let canonical = format!(
            "{{\"project\":{},\"units\":{},\"steps\":{},\"status\":{},\"recipe\":{}}}",
            serde_json::json!(project.to_string()),
            set(&self.units),
            set(&self.steps),
            set(&self.status),
            serde_json::json!(self.recipe),
        );
        hex16(Sha256::digest(canonical.as_bytes()).as_slice())
    }
    /// A cursor binds the state epoch only when it filters on status, and the recipe
    /// generation only when it filters on a recipe.
    pub fn binds_state(&self) -> bool {
        self.status.is_some()
    }
    pub fn binds_recipes(&self) -> bool {
        self.recipe.is_some()
    }
}
fn hex16(bytes: &[u8]) -> String {
    bytes[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Where a `plan_read` page ended. Text form, opaque to callers:
/// `v1:<filter digest>:<rev>:<state epoch or ->:<recipe generation or ->:<position>:<step>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCursor {
    pub filter: String,
    pub rev: Revision,
    pub state_epoch: Option<StateEpoch>,
    pub recipe_generation: Option<RecipeGeneration>,
    pub position: u64,
    pub step: StepId,
}
impl PlanCursor {
    pub fn encode(&self) -> String {
        format!(
            "v1:{}:{}:{}:{}:{}:{}",
            self.filter,
            self.rev,
            self.state_epoch
                .map_or_else(|| "-".to_owned(), |e| e.0.to_string()),
            self.recipe_generation
                .as_ref()
                .map_or("-", |g| g.0.as_str()),
            self.position,
            self.step
        )
    }
    pub fn parse(text: &str) -> Result<Self, PlanRowsError> {
        let parts: Vec<&str> = text.split(':').collect();
        let ["v1", filter, rev, epoch, recipes, position, step] = parts.as_slice() else {
            return Err(PlanRowsError::CursorMalformed);
        };
        let hex = |s: &str| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex(filter) || !(*recipes == "-" || hex(recipes)) {
            return Err(PlanRowsError::CursorMalformed);
        }
        let number = |s: &str| s.parse::<u64>().map_err(|_| PlanRowsError::CursorMalformed);
        Ok(Self {
            filter: (*filter).to_owned(),
            rev: Revision(number(rev)?),
            state_epoch: match *epoch {
                "-" => None,
                epoch => Some(StateEpoch(number(epoch)?)),
            },
            recipe_generation: (*recipes != "-").then(|| RecipeGeneration((*recipes).to_owned())),
            position: number(position)?,
            step: StepId::new(*step).map_err(|_| PlanRowsError::CursorMalformed)?,
        })
    }
}

// ---- errors ----------------------------------------------------------------------------

/// The refusals the schema-3 plan tools add, each with its public kind and exact message.
/// Validation failures stay `invalid` with path errors (`ops[2].step: …`, `steps.x.in.y: …`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRowsError {
    /// `limit` 0.
    Limit,
    /// `plan_edit` (or `unit_update`'s `changes`) with nothing to do.
    NoOps,
    /// An empty list or object where at least one item is needed (a `step.update`'s
    /// `changes`, a `step.remove`'s `steps`, an `edge.add`'s `after`, …): `what` names the item.
    Empty {
        path: String,
        what: &'static str,
    },
    /// `preview_scope: "all"` without `dry_run: true`.
    PreviewAllNeedsDryRun,
    /// `order.set` in an edit without `rev`.
    OrderNeedsRev,
    /// A cursor this server did not issue.
    CursorMalformed,
    /// A cursor issued for another project or other filters.
    CursorMismatch,
    /// The plan (or, for a cursor bound to them, the statuses or recipes) changed since the
    /// cursor was issued.
    CursorExpired,
    NoStep {
        step: StepId,
    },
    NoUnit {
        unit: UnitName,
    },
    /// An explicit `rev` that is not the current one.
    StaleRev {
        current: Revision,
    },
}
impl From<PlanRowsError> for PublicError {
    fn from(error: PlanRowsError) -> Self {
        let bad = |message: String| PublicError::BadRequest { message };
        match error {
            PlanRowsError::Limit => bad(format!("limit must be 1 to {MAX_LIMIT}")),
            PlanRowsError::NoOps => bad("ops: name at least one operation".into()),
            PlanRowsError::Empty { path, what } => bad(format!("{path}: name at least one {what}")),
            PlanRowsError::PreviewAllNeedsDryRun => {
                bad("preview_scope \"all\" needs dry_run: true".into())
            }
            PlanRowsError::OrderNeedsRev => {
                bad("order.set needs rev: the revision whose order it lists".into())
            }
            PlanRowsError::CursorMalformed => bad("cursor is not one plan_read returned".into()),
            PlanRowsError::CursorMismatch => bad(
                "cursor belongs to another query: pass the same project and filters, or no cursor"
                    .into(),
            ),
            PlanRowsError::CursorExpired => PublicError::CursorExpired {
                message: "the plan changed since this cursor was issued; read again without cursor"
                    .into(),
            },
            PlanRowsError::NoStep { step } => PublicError::NotFound {
                message: format!("no step {step}"),
            },
            PlanRowsError::NoUnit { unit } => PublicError::NotFound {
                message: format!("no unit {unit}"),
            },
            PlanRowsError::StaleRev { current } => PublicError::Conflict {
                message: format!("plan is at rev {current}"),
                current_rev: Some(current),
            },
        }
    }
}
