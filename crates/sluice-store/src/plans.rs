//! Plan and result commands composed inside one writer transaction.

use crate::{Result, StoreError, WriteTransaction};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sluice_model::{
    commands::{EditPreview, EditResult, ProjectIdentity, RetryResult, StepSelection, StepStatus},
    edit::PreparedEdit,
    error::PublicError,
    events::Event,
    gates::{self, GateDecision, StateSnapshot, StepState},
    hash::EffectiveInput,
    ids::{
        AttemptId, ProjectId, ResultId, Revision, RunId, StepGeneration, StepId, UnitName,
        WorkGeneration,
    },
    plan::{Pause, Plan, Step, inputs_hash},
    rpc::JsonMap,
    types::{Type, check_value_at},
    units::retry_walk,
};

/// A compiled snapshot. Commands recheck its revision and document in the writer.
#[derive(Debug, Clone)]
pub struct PlanContext {
    pub project: ProjectId,
    pub revision: Revision,
    pub plan: Plan,
}

/// Store evidence for an age-filtered model prune. Prepare the model edit using
/// units(), then pass both the edit and this certificate to apply_prune.
#[derive(Debug, Clone)]
pub struct PruneEligibility {
    project: ProjectId,
    revision: Revision,
    cutoff: time::OffsetDateTime,
    units: Vec<UnitName>,
    results: std::collections::BTreeMap<StepId, ResultId>,
}
impl PruneEligibility {
    pub fn units(&self) -> &[UnitName] {
        &self.units
    }
}

/// Read inside the same snapshot as the model's edit preparation. Each eligible
/// unit is done and every member's current result was recorded by the cutoff.
pub fn prune_eligible(
    c: &Connection,
    context: &PlanContext,
    cutoff: time::OffsetDateTime,
) -> Result<PruneEligibility> {
    let (rev, doc): (i64, String) = c.query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [context.project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if rev != sql_counter(context.revision.0)?
        || serde_json::from_str::<JsonMap>(&doc)? != *context.plan.document()
    {
        return Err(conflict("plan changed before prune preparation"));
    }
    let state = read_state(c, context.project)?;
    let mut evidence = PruneEligibility {
        project: context.project,
        revision: context.revision,
        cutoff,
        units: vec![],
        results: Default::default(),
    };
    for (name, unit) in context.plan.units() {
        if !unit.done(&state) {
            continue;
        }
        let mut results = vec![];
        for step in &unit.steps {
            let Some((id, recorded_at)) = current_prune_result(c, context.project, step)? else {
                break;
            };
            if recorded_at > cutoff {
                break;
            }
            results.push((step.clone(), id));
        }
        if results.len() == unit.steps.len() {
            evidence.units.push(name.clone());
            evidence.results.extend(results);
        }
    }
    Ok(evidence)
}

fn current_prune_result(
    c: &Connection,
    project: ProjectId,
    step: &StepId,
) -> Result<Option<(ResultId, time::OffsetDateTime)>> {
    let row: Option<(String, String)> = c.query_row(
        "SELECT r.result_id,r.recorded_at FROM steps s JOIN step_results r ON r.result_id=s.result_id
         WHERE s.project_id=?1 AND s.step_id=?2",
        params![project.to_string(), step.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    row.map(|(id, at)| {
        let id = id
            .parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?;
        let at = time::OffsetDateTime::parse(&at, &time::format_description::well_known::Rfc3339)
            .map_err(|e| StoreError::InvalidDatabase(e.to_string()))?;
        Ok((id, at))
    })
    .transpose()
}

/// Apply an age-filtered prune with its original cutoff and frozen result IDs.
/// A new result or a retried member conflicts even when plan revision is unchanged.
pub fn apply_prune(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    edit: PreparedEdit,
    evidence: &PruneEligibility,
) -> Result<EditResult> {
    let prune = edit
        .prune
        .as_ref()
        .ok_or_else(|| invalid("edit is not a prune"))?;
    if evidence.project != project
        || evidence.revision != edit.expected
        || prune
            .units
            .iter()
            .any(|unit| !evidence.units.contains(unit))
    {
        return Err(conflict(
            "prune eligibility does not match the prepared edit",
        ));
    }
    for step in &prune.steps {
        let current = current_prune_result(tx.sql(), project, step)?;
        if current
            .is_none_or(|(id, at)| evidence.results.get(step) != Some(&id) || at > evidence.cutoff)
        {
            return Err(conflict(format!(
                "steps.{step}: prune result changed; prepare prune again"
            )));
        }
    }
    apply_edit(tx, project, edit)
}

/// The message owner implements this synchronous adapter with its post command.
/// Validation checks all semantic preconditions without writes. After it succeeds,
/// post_retry may report storage failures only; the writer then rolls back.
pub trait RetryMessages {
    fn validate_retry(
        &self,
        tx: &WriteTransaction<'_>,
        project: ProjectId,
        steps: &[StepId],
        body: &str,
        author: &str,
    ) -> Result<()>;
    fn post_retry(
        &mut self,
        tx: &mut WriteTransaction<'_>,
        project: ProjectId,
        step: &StepId,
        body: &str,
        author: &str,
    ) -> Result<()>;
}

pub(crate) fn invalid(message: impl Into<String>) -> StoreError {
    let message = message.into();
    PublicError::Invalid {
        errors: vec![message.clone()],
        message,
    }
    .into()
}
pub(crate) fn conflict(message: impl Into<String>) -> StoreError {
    PublicError::Conflict {
        message: message.into(),
        current_rev: None,
    }
    .into()
}
pub(crate) fn sql_counter(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| invalid("counter exceeds SQLite integer range"))
}
pub(crate) fn now() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| StoreError::InvalidDatabase(e.to_string()))
}
pub(crate) fn identity(c: &Connection, project: ProjectId) -> Result<ProjectIdentity> {
    let name: Option<String> = c
        .query_row(
            "SELECT name FROM projects WHERE project_id=?1 AND deleted_at IS NULL",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    let name = name.ok_or_else(|| PublicError::NotFound {
        message: "project does not exist".into(),
    })?;
    Ok(ProjectIdentity {
        project_id: project,
        name: name
            .parse()
            .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
    })
}
pub(crate) fn check_context(tx: &WriteTransaction<'_>, context: &PlanContext) -> Result<()> {
    identity(tx.sql(), context.project)?;
    let (rev, doc): (i64, String) = tx.sql().query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [context.project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if Revision(rev as u64) != context.revision {
        return Err(PublicError::Conflict {
            message: format!("plan is at rev {rev}"),
            current_rev: Some(Revision(rev as u64)),
        }
        .into());
    }
    if serde_json::from_str::<JsonMap>(&doc)? != *context.plan.document() {
        return Err(conflict("compiled plan does not match stored revision"));
    }
    Ok(())
}

/// Initialize rev 1, including ordered input and step projections. Called by
/// project creation in its own transaction, never opens another connection.
pub fn initialize_plan(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    plan: &Plan,
) -> Result<()> {
    tx.sql().execute(
        "INSERT INTO plans(project_id,rev,doc) VALUES (?1,1,?2)",
        params![project.to_string(), serde_json::to_string(plan.document())?],
    )?;
    sync_projection(tx, project, plan, Revision(1))?;
    tx.changed(Some(project), "plan");
    tx.changed(Some(project), "status");
    Ok(())
}

pub fn read_state(c: &Connection, project: ProjectId) -> Result<StateSnapshot> {
    let mut state = StateSnapshot::default();
    let paused: bool = c.query_row(
        "SELECT paused FROM projects WHERE project_id=?1",
        [project.to_string()],
        |r| r.get(0),
    )?;
    state.paused = if paused { Pause::Yes } else { Pause::No };
    let mut query = c.prepare(
        "SELECT name,value FROM inputs WHERE project_id=?1 AND value IS NOT NULL ORDER BY position",
    )?;
    for row in query.query_map([project.to_string()], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })? {
        let (name, value) = row?;
        state.inputs.0.insert(name, serde_json::from_str(&value)?);
    }
    let mut query = c.prepare("SELECT step_id,status,outputs,inputs_hash,skipped,error FROM steps WHERE project_id=?1 ORDER BY position")?;
    for row in query.query_map([project.to_string()], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })? {
        let (id, status, outputs, hash, skipped, error) = row?;
        state.steps.insert(
            id.parse()
                .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
            step_state(status, outputs, hash, skipped, error)?,
        );
    }
    Ok(state)
}
/// A step row's state columns as `read_state` reads them.
fn step_state(
    status: String,
    outputs: Option<String>,
    hash: Option<String>,
    skipped: Option<String>,
    error: Option<String>,
) -> Result<StepState> {
    Ok(StepState {
        status: serde_json::from_value(json!(status))?,
        outputs: outputs
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default(),
        inputs_hash: hash.map(|s| s.parse()).transpose()?,
        skipped: skipped
            .map(|s| serde_json::from_str(&s))
            .transpose()?
            .unwrap_or_default(),
        error: error
            .map(|s| serde_json::from_str::<PublicError>(&s).map(|e| e.to_string()))
            .transpose()?,
        queued: vec![],
    })
}

/// The stored rows a plan edit is worked out from: the plan's revision (which fixes its
/// document), the project's pause, its plan inputs, its step rows (projection and state)
/// and its resource declarations, read as stored. An edit prepared and worked out from a
/// read snapshot whose witness still holds in the writer is the edit the writer would
/// have worked out itself, so the writer only checks this and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Witness(Vec<u8>);
const WITNESS: [&str; 5] = [
    "SELECT rev FROM plans WHERE project_id=?1",
    "SELECT paused FROM projects WHERE project_id=?1",
    "SELECT name,position,value,declaration FROM inputs WHERE project_id=?1 ORDER BY position",
    "SELECT step_id,position,declaration,unit,paused,status,outputs,inputs_hash,skipped,error FROM steps WHERE project_id=?1 ORDER BY position",
    "SELECT name,declaration FROM resources WHERE project_id=?1 ORDER BY name",
];
impl Witness {
    /// The witness, and the state `read_state` reads, from one pass over the rows.
    pub fn read_with_state(c: &Connection, project: ProjectId) -> Result<(Self, StateSnapshot)> {
        let mut bytes = vec![];
        let mut state = StateSnapshot::default();
        let mut paused = None;
        witness_scan(
            c,
            project,
            &mut |part| {
                bytes.extend_from_slice(part);
                true
            },
            &mut |query, row| {
                match query {
                    1 => paused = Some(row.get::<_, bool>(0)?),
                    2 => {
                        if let Some(value) = row.get::<_, Option<String>>(2)? {
                            state
                                .inputs
                                .0
                                .insert(row.get(0)?, serde_json::from_str(&value)?);
                        }
                    }
                    3 => {
                        let id: String = row.get(0)?;
                        state.steps.insert(
                            id.parse()
                                .map_err(|e| StoreError::InvalidDatabase(format!("{e}")))?,
                            step_state(
                                row.get(5)?,
                                row.get(6)?,
                                row.get(7)?,
                                row.get(8)?,
                                row.get(9)?,
                            )?,
                        );
                    }
                    _ => {}
                }
                Ok(())
            },
        )?;
        // As read_state, which refuses a project it cannot find.
        let paused = paused.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        state.paused = if paused { Pause::Yes } else { Pause::No };
        Ok((Self(bytes), state))
    }
    /// Whether the rows are still exactly as read: compared as they are scanned, with no
    /// copy, stopping at the first difference.
    pub fn holds(&self, c: &Connection, project: ProjectId) -> Result<bool> {
        let mut at = 0;
        let same = witness_scan(
            c,
            project,
            &mut |part| {
                let end = at + part.len();
                let same = self.0.get(at..end) == Some(part);
                at = end;
                same
            },
            &mut |_, _| Ok(()),
        )?;
        Ok(same && at == self.0.len())
    }
    /// `holds` in a writer transaction, for a witness read in a snapshot that began after
    /// `mark`: when no row committed since the mark is the project's, it holds without the
    /// project's rows being read again; else (or when the log no longer reaches back to
    /// the mark) they are compared.
    pub fn holds_since(
        &self,
        tx: &WriteTransaction<'_>,
        project: ProjectId,
        mark: crate::RowMark,
    ) -> Result<bool> {
        let Some(rows) = tx.rows_changed_since(mark) else {
            return self.holds(tx.sql(), project);
        };
        let project_id = project.to_string();
        let mut seen = std::collections::HashSet::new();
        for (table, rowid) in rows {
            // A rowid changed twice may be a row deleted and its rowid taken by another.
            if !seen.insert((table, rowid)) {
                return self.holds(tx.sql(), project);
            }
            let owner: Option<Option<String>> = tx
                .sql()
                .prepare_cached(&format!("SELECT project_id FROM {table} WHERE rowid=?1"))?
                .query_row([rowid], |r| r.get(0))
                .optional()?;
            // A row since deleted may have been the project's.
            if owner.is_none_or(|owner| owner.as_deref() == Some(project_id.as_str())) {
                return self.holds(tx.sql(), project);
            }
        }
        Ok(true)
    }
}
/// Feed every witnessed cell to `sink` as a type tag, a length and its bytes, each row
/// and each query closed by a marker; false as soon as `sink` refuses a part.
/// `read` is handed each row first, with its query's index in WITNESS.
fn witness_scan(
    c: &Connection,
    project: ProjectId,
    sink: &mut dyn FnMut(&[u8]) -> bool,
    read: &mut dyn FnMut(usize, &rusqlite::Row<'_>) -> Result<()>,
) -> Result<bool> {
    use rusqlite::types::ValueRef;
    let project = project.to_string();
    for (query_index, sql) in WITNESS.iter().enumerate() {
        let mut query = c.prepare_cached(sql)?;
        let columns = query.column_count();
        let mut rows = query.query([&project])?;
        while let Some(row) = rows.next()? {
            read(query_index, row)?;
            if !sink(b"r") {
                return Ok(false);
            }
            for index in 0..columns {
                let number;
                let (tag, bytes): (u8, &[u8]) = match row.get_ref(index)? {
                    ValueRef::Null => (0, &[]),
                    ValueRef::Integer(n) => {
                        number = n.to_le_bytes();
                        (1, &number)
                    }
                    ValueRef::Real(f) => {
                        number = f.to_bits().to_le_bytes();
                        (2, &number)
                    }
                    ValueRef::Text(text) => (3, text),
                    ValueRef::Blob(blob) => (4, blob),
                };
                let mut head = [tag; 9];
                head[1..].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
                if !sink(&head) || !sink(bytes) {
                    return Ok(false);
                }
            }
        }
        if !sink(b"e") {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn wire_step(plan: &Plan, id: &StepId) -> Result<Value> {
    plan_steps(plan.document())
        .and_then(|steps| steps.get(id.as_str()))
        .cloned()
        .ok_or_else(|| invalid("compiled step is missing its document"))
}
/// A plan document's `steps` object, read in place.
fn plan_steps(document: &JsonMap) -> Option<&serde_json::Map<String, Value>> {
    document
        .0
        .get("steps")
        .and_then(|steps| steps.as_value().as_object())
}

/// The step and input rows a plan needs. Only the step rows that differ from those
/// stored (position, declaration, unit, paused) are written, so an edit touching a few
/// steps writes a few rows whatever the plan's size.
struct Projection {
    steps: i64,
    generation: i64,
    /// Rows that change position: they go above the whole current range first, so no
    /// two rows ever share a position (UNIQUE) while the others take theirs.
    moving: Vec<String>,
    /// (id, position, declaration, unit, paused) to insert or update.
    rows: Vec<(String, i64, String, String, String)>,
    /// (name, declaration) of every plan input, in plan order.
    inputs: Vec<(String, String)>,
}
fn projection(
    c: &Connection,
    project: ProjectId,
    plan: &Plan,
    revision: Revision,
) -> Result<Projection> {
    let documents =
        plan_steps(plan.document()).ok_or_else(|| invalid("compiled plan has no steps"))?;
    let mut stored: std::collections::HashMap<
        String,
        (i64, String, Option<String>, Option<String>),
    > = std::collections::HashMap::with_capacity(plan.steps().len());
    {
        let mut query = c.prepare_cached(
            "SELECT step_id,position,declaration,unit,paused FROM steps WHERE project_id=?1",
        )?;
        for row in query.query_map([project.to_string()], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        })? {
            let (id, row) = row?;
            stored.insert(id, row);
        }
    }
    let mut moving = vec![];
    let mut rows = vec![];
    for (position, (id, step)) in plan.steps().iter().enumerate() {
        let position = position as i64;
        let declaration = documents
            .get(id.as_str())
            .ok_or_else(|| invalid("compiled step is missing its document"))?
            .to_string();
        let unit = step.unit_name().to_string();
        let paused = match &step.paused {
            Pause::No => "false".into(),
            Pause::Yes => "true".into(),
            Pause::Reason(reason) => serde_json::to_string(reason)?,
        };
        match stored.get(id.as_str()) {
            Some(row)
                if row.0 == position
                    && row.1 == declaration
                    && row.2.as_deref() == Some(unit.as_str())
                    && row.3.as_deref() == Some(paused.as_str()) =>
            {
                continue;
            }
            Some(row) if row.0 != position => moving.push(id.to_string()),
            _ => {}
        }
        rows.push((id.to_string(), position, declaration, unit, paused));
    }
    Ok(Projection {
        steps: plan.steps().len() as i64,
        generation: sql_counter(revision.0)?,
        moving,
        rows,
        inputs: plan
            .inputs()
            .iter()
            .map(|(name, declaration)| {
                (
                    name.clone(),
                    json!({"type":declaration.ty,"doc":declaration.doc}).to_string(),
                )
            })
            .collect(),
    })
}
fn write_projection(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    projection: &Projection,
) -> Result<()> {
    let project = project.to_string();
    if !projection.moving.is_empty() {
        let offset: i64 = tx.sql().query_row(
            "SELECT coalesce(max(position),0)+?2+1 FROM steps WHERE project_id=?1",
            params![project, projection.steps],
            |r| r.get(0),
        )?;
        let mut shift = tx.sql().prepare_cached(
            "UPDATE steps SET position=position+?3 WHERE project_id=?1 AND step_id=?2",
        )?;
        for id in &projection.moving {
            shift.execute(params![project, id, offset])?;
        }
    }
    {
        let mut upsert = tx.sql().prepare_cached("INSERT INTO steps(project_id,step_id,position,generation,declaration,unit,paused) VALUES (?1,?2,?3,?4,?5,?6,?7)
            ON CONFLICT(project_id,step_id) DO UPDATE SET position=excluded.position,declaration=excluded.declaration,unit=excluded.unit,paused=excluded.paused")?;
        for (id, position, declaration, unit, paused) in &projection.rows {
            upsert.execute(params![
                project,
                id,
                position,
                projection.generation,
                declaration,
                unit,
                paused
            ])?;
        }
    }
    let offset: i64 = tx.sql().query_row(
        "SELECT coalesce(max(position),0)+?2+1 FROM inputs WHERE project_id=?1",
        params![project, projection.inputs.len() as i64],
        |r| r.get(0),
    )?;
    tx.sql().execute(
        "UPDATE inputs SET position=position+?2 WHERE project_id=?1",
        params![project, offset],
    )?;
    for (position, (name, declaration)) in projection.inputs.iter().enumerate() {
        tx.sql().execute("INSERT INTO inputs(project_id,name,position,declaration,generation) VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(project_id,name) DO UPDATE SET position=excluded.position,declaration=excluded.declaration",
            params![project,name,position as i64,declaration,projection.generation])?;
    }
    let names: Vec<String> = {
        let mut query = tx
            .sql()
            .prepare("SELECT name FROM inputs WHERE project_id=?1")?;
        query
            .query_map([&project], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for name in names {
        if !projection.inputs.iter().any(|(kept, _)| *kept == name) {
            tx.sql().execute(
                "DELETE FROM inputs WHERE project_id=?1 AND name=?2",
                params![project, name],
            )?;
        }
    }
    Ok(())
}
fn sync_projection(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    plan: &Plan,
    revision: Revision,
) -> Result<()> {
    let projection = projection(tx.sql(), project, plan, revision)?;
    write_projection(tx, project, &projection)
}

/// The state rows read back once an edit's projection is written: removed steps gone,
/// new ones fresh and pending, plan inputs without a value left out, in plan order.
fn projected_state(plan: &Plan, state: &StateSnapshot) -> StateSnapshot {
    StateSnapshot {
        paused: state.paused.clone(),
        inputs: JsonMap(
            plan.inputs()
                .keys()
                .filter_map(|name| Some((name.clone(), state.inputs.0.get(name)?.clone())))
                .collect(),
        ),
        steps: plan
            .steps()
            .keys()
            .map(|id| (id.clone(), state.steps.get(id).cloned().unwrap_or_default()))
            .collect(),
    }
}

/// The plan at its revision and the state it is read with, from one snapshot.
pub struct Current<'a> {
    pub revision: Revision,
    pub document: &'a JsonMap,
    pub state: &'a StateSnapshot,
    /// `state` is the state the edit was prepared with, so the state it reconciled to
    /// (`PreparedEdit::reconciled`) is reused rather than worked out again.
    pub prepared_with: bool,
}
/// What a prepared edit writes, worked out by `edit_effect` and written by `commit_effect`.
pub struct EditEffect {
    project: ProjectId,
    revision: Revision,
    preview: EditPreview,
    steps: Option<Vec<StepId>>,
    /// None when nothing is written: an edit that changes nothing, or a dry run.
    commit: Option<Box<EditCommit>>,
}
struct EditCommit {
    revision: Revision,
    removed: Vec<StepId>,
    projection: Projection,
    document: String,
    author: String,
    reason: String,
    ops: Vec<sluice_model::commands::PatchOperation>,
    statuses: Vec<StatusChange>,
    plan: Option<Plan>,
}
impl EditEffect {
    /// The plan this edit commits, if it commits one.
    pub fn take_plan(&mut self) -> Option<(Revision, Plan)> {
        let commit = self.commit.as_mut()?;
        Some((commit.revision, commit.plan.take()?))
    }
}

/// Work out a prepared edit against the plan and state it was prepared from, reading
/// only. Refusals (a stale revision, a running step changed, a prune no longer done,
/// input values that no longer fit) are decided here.
pub fn edit_effect(
    c: &Connection,
    project: ProjectId,
    edit: PreparedEdit,
    current: Current<'_>,
) -> Result<EditEffect> {
    let rev = sql_counter(current.revision.0)?;
    if rev != sql_counter(edit.expected.0)? {
        return Err(PublicError::Conflict {
            message: format!("plan is at rev {rev}"),
            current_rev: Some(current.revision),
        }
        .into());
    }
    // An edit that changes nothing commits nothing: no rev, record or history row.
    if sluice_model::hash::data_equal_maps(current.document, edit.plan.document())? {
        return Ok(EditEffect {
            project,
            revision: current.revision,
            preview: EditPreview {
                ops: vec![],
                ..edit.preview
            },
            steps: edit.steps,
            commit: None,
        });
    }
    let state = current.state;
    if let Some(prune) = &edit.prune {
        // Retry and manual output writes can change eligibility without a plan edit.
        // The revision check alone therefore cannot certify a prepared prune.
        let members: Vec<&str> = prune.steps.iter().map(|id| id.as_str()).collect();
        let mut query = c.prepare("SELECT step_id,unit,status FROM steps WHERE project_id=?1")?;
        for row in query.query_map([project.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (id, unit, status) = row?;
            let unit = unit.as_deref().unwrap_or(&id);
            if (members.contains(&id.as_str())
                || prune.units.iter().any(|name| name.as_str() == unit))
                && !matches!(status.as_str(), "succeeded" | "skipped")
            {
                return Err(conflict(format!(
                    "unit {unit} is no longer done; prepare prune again"
                )));
            }
        }
        if prune.steps.iter().any(|id| !state.steps.contains_key(id)) {
            return Err(conflict("prune member disappeared; prepare prune again"));
        }
    }
    let (old_steps, new_steps) = (
        plan_steps(current.document),
        plan_steps(edit.plan.document()),
    );
    for (id, entry) in &state.steps {
        if entry.status != StepStatus::Running {
            continue;
        }
        let old = old_steps.and_then(|steps| steps.get(id.as_str()));
        let new = new_steps.and_then(|steps| steps.get(id.as_str()));
        let strip = |value: &Value| {
            let mut value = value.clone();
            if let Some(map) = value.as_object_mut() {
                map.remove("paused");
                map.remove("tags");
            }
            value
        };
        if new.is_none() || old.map(strip) != new.map(strip) {
            return Err(invalid(format!(
                "steps.{id}: cannot change a running step, except paused and tags"
            )));
        }
    }
    let mut candidate_inputs = state.inputs.clone();
    candidate_inputs
        .0
        .retain(|name, _| edit.plan.inputs().contains_key(name));
    edit.plan
        .validate_input_values(&candidate_inputs)
        .map_err(|errors| {
            invalid(
                errors
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        })?;
    if edit.dry_run {
        return Ok(EditEffect {
            project,
            revision: current.revision,
            preview: edit.preview,
            steps: edit.steps,
            commit: None,
        });
    }
    let revision = Revision(
        rev.checked_add(1)
            .ok_or_else(|| invalid("revision exhausted"))? as u64,
    );
    let removed = state
        .steps
        .keys()
        .filter(|id| !edit.plan.steps().contains_key(*id))
        .cloned()
        .collect();
    let projection = projection(c, project, &edit.plan, revision)?;
    let projected = projected_state(&edit.plan, state);
    let statuses = if current.prepared_with {
        let reused = reconciled_changes(&edit.plan, &projected, &edit.reconciled);
        // Every debug build (the test suite) checks the reuse against the full reconcile.
        debug_assert!(
            reused == status_changes(&edit.plan, &projected),
            "reconciled state reused for a different state"
        );
        reused
    } else {
        status_changes(&edit.plan, &projected)
    };
    Ok(EditEffect {
        project,
        revision,
        preview: edit.preview,
        steps: edit.steps,
        commit: Some(Box::new(EditCommit {
            revision,
            removed,
            projection,
            document: serde_json::to_string(edit.plan.document())?,
            author: edit.author.unwrap_or_default(),
            reason: edit.reason,
            ops: edit.ops,
            statuses,
            plan: Some(edit.plan),
        })),
    })
}

/// Write an edit's effect: remove and archive its removed steps, write the rows that
/// change, the plan, its record and history row, and the statuses it settles.
pub fn commit_effect(tx: &mut WriteTransaction<'_>, effect: EditEffect) -> Result<EditResult> {
    let project = effect.project;
    let project_identity = identity(tx.sql(), project)?;
    let Some(commit) = effect.commit else {
        return Ok(EditResult {
            project: project_identity,
            rev: effect.revision,
            preview: effect.preview,
            steps: effect.steps,
            board_warnings: vec![],
        });
    };
    for id in &commit.removed {
        archive(tx, project, id)?;
        tx.sql().execute(
            "DELETE FROM steps WHERE project_id=?1 AND step_id=?2",
            params![project.to_string(), id.as_str()],
        )?;
    }
    write_projection(tx, project, &commit.projection)?;
    tx.sql().execute(
        "UPDATE plans SET rev=?2,doc=?3 WHERE project_id=?1",
        params![
            project.to_string(),
            sql_counter(commit.revision.0)?,
            commit.document
        ],
    )?;
    let record = tx.append_record(
        Some(project),
        Event::PlanEdit {
            rev: commit.revision,
            author: commit.author.clone(),
            reason: commit.reason.clone(),
            ops: commit.ops.clone(),
        },
    )?;
    tx.sql().execute("INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,ops) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![project.to_string(),sql_counter(commit.revision.0)?,record.seq.0,record.at,commit.author,commit.reason,serde_json::to_string(&commit.ops)?])?;
    tx.changed(Some(project), "plan");
    tx.changed(Some(project), "edits");
    write_status_changes(tx, project, &commit.statuses)?;
    Ok(EditResult {
        project: project_identity,
        rev: commit.revision,
        preview: effect.preview,
        steps: effect.steps,
        board_warnings: vec![],
    })
}

pub fn apply_edit(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    edit: PreparedEdit,
) -> Result<EditResult> {
    identity(tx.sql(), project)?;
    let (rev, doc): (i64, String) = tx.sql().query_row(
        "SELECT rev,doc FROM plans WHERE project_id=?1",
        [project.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let document: JsonMap = serde_json::from_str(&doc)?;
    let state = read_state(tx.sql(), project)?;
    let effect = edit_effect(
        tx.sql(),
        project,
        edit,
        Current {
            revision: Revision(rev as u64),
            document: &document,
            state: &state,
            prepared_with: false,
        },
    )?;
    commit_effect(tx, effect)
}

/// Snapshot the current terminal projection. Previously recorded declaration and
/// unit remain execution provenance even when a completed step is later retagged.
pub(crate) fn snapshot_result(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: &StepId,
    attempt: Option<AttemptId>,
    inputs: Option<&JsonMap>,
) -> Result<ResultId> {
    let result = ResultId::new();
    let at = now()?;
    tx.sql().execute("INSERT INTO step_results(result_id,project_id,step_id,generation,work_generation,attempt_id,unit,declaration,inputs,inputs_hash,status,outputs,error,manual,run_ids,recorded_at)
        SELECT ?3,s.project_id,s.step_id,s.generation,s.work_generation,coalesce(?4,r.attempt_id),
            CASE WHEN ?4 IS NOT NULL THEN a.unit WHEN s.manual=1 AND r.result_id IS NULL THEN s.unit ELSE coalesce(r.unit,s.unit) END,
            CASE WHEN ?4 IS NOT NULL THEN json_extract(a.request,'$.declaration') WHEN s.manual=1 AND r.result_id IS NULL THEN s.declaration ELSE coalesce(r.declaration,s.declaration) END,
            coalesce(?5,r.inputs),s.inputs_hash,s.status,s.outputs,coalesce(s.error,CASE WHEN s.skipped IS NOT NULL THEN json_object('skipped',json(s.skipped)) END),s.manual,s.run_ids,?6
        FROM steps s LEFT JOIN step_results r ON s.result_id=r.result_id LEFT JOIN attempts a ON a.attempt_id=?4 AND a.project_id=s.project_id WHERE s.project_id=?1 AND s.step_id=?2",
        params![project.to_string(),id.as_str(),result.to_string(),attempt.map(|a|a.to_string()),inputs.map(serde_json::to_string).transpose()?,at])?;
    tx.sql().execute(
        "UPDATE steps SET result_id=?3 WHERE project_id=?1 AND step_id=?2",
        params![project.to_string(), id.as_str(), result.to_string()],
    )?;
    tx.changed(Some(project), "status");
    Ok(result)
}
fn archive(tx: &mut WriteTransaction<'_>, project: ProjectId, id: &StepId) -> Result<()> {
    let (status, result): (String, Option<String>) = tx.sql().query_row(
        "SELECT status,result_id FROM steps WHERE project_id=?1 AND step_id=?2",
        params![project.to_string(), id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let result = if matches!(
        status.as_str(),
        "succeeded" | "failed" | "stale" | "skipped"
    ) {
        Some(snapshot_result(tx, project, id, None, None)?.to_string())
    } else {
        result
    };
    if let Some(result) = result {
        tx.sql().execute(
            "UPDATE step_results SET removed_at=?2 WHERE result_id=?1",
            params![result, now()?],
        )?;
        tx.changed(Some(project), "outcomes");
    }
    Ok(())
}
pub(crate) fn status_record(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    id: &StepId,
    from: StepStatus,
    to: StepStatus,
    error: Option<PublicError>,
) -> Result<()> {
    let (runs, declaration): (String, String) = tx.sql().query_row(
        "SELECT run_ids,declaration FROM steps WHERE project_id=?1 AND step_id=?2",
        params![project.to_string(), id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let declaration: Value = serde_json::from_str(&declaration)?;
    let needs = if to == StepStatus::Running && from != StepStatus::Running {
        serde_json::from_value(
            declaration
                .get("needs")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )?
    } else {
        JsonMap::default()
    };
    tx.append_record(
        Some(project),
        Event::StepStatus {
            step: id.clone(),
            from: Some(from),
            to,
            error,
            run_ids: serde_json::from_str(&runs)?,
            needs,
        },
    )?;
    tx.changed(Some(project), "status");
    Ok(())
}

/// Apply the pure model's gate and data-hash transitions. Never reserves work.
pub fn reconcile(tx: &mut WriteTransaction<'_>, context: &PlanContext) -> Result<()> {
    check_context(tx, context)?;
    let before = read_state(tx.sql(), context.project)?;
    let changes = status_changes(&context.plan, &before);
    write_status_changes(tx, context.project, &changes)
}
/// A step whose status, skip reasons or error the model's reconcile changes.
#[derive(Debug, PartialEq)]
struct StatusChange {
    id: StepId,
    previous: StepStatus,
    entry: StepState,
}
fn status_changes(plan: &Plan, before: &StateSnapshot) -> Vec<StatusChange> {
    let after = gates::reconcile(plan, before);
    after
        .steps
        .into_iter()
        .filter_map(|(id, entry)| {
            let previous = before.status(&id);
            let old = before.steps.get(&id);
            if previous == entry.status
                && old.is_some_and(|old| old.skipped == entry.skipped && old.error == entry.error)
            {
                return None;
            }
            Some(StatusChange {
                id,
                previous,
                entry,
            })
        })
        .collect()
}
/// `status_changes` from the state an edit's preparation reconciled to: the same
/// changes, since it reconciled the same plan from the same steps, less the new ones it
/// left pending and untouched, which `projected` holds as fresh rows.
fn reconciled_changes(
    plan: &Plan,
    projected: &StateSnapshot,
    reconciled: &StateSnapshot,
) -> Vec<StatusChange> {
    plan.steps()
        .keys()
        .filter_map(|id| {
            let entry = reconciled.steps.get(id)?;
            let previous = projected.status(id);
            let old = projected.steps.get(id);
            if previous == entry.status
                && old.is_some_and(|old| old.skipped == entry.skipped && old.error == entry.error)
            {
                return None;
            }
            Some(StatusChange {
                id: id.clone(),
                previous,
                entry: entry.clone(),
            })
        })
        .collect()
}
fn write_status_changes(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    changes: &[StatusChange],
) -> Result<()> {
    for StatusChange {
        id,
        previous,
        entry,
    } in changes
    {
        let error = entry
            .error
            .clone()
            .map(|message| PublicError::FnFailure { message });
        tx.sql().execute(
            "UPDATE steps SET status=?3,skipped=?4,error=?5 WHERE project_id=?1 AND step_id=?2",
            params![
                project.to_string(),
                id.as_str(),
                entry.status.as_str(),
                if entry.skipped.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&entry.skipped)?)
                },
                error.as_ref().map(serde_json::to_string).transpose()?
            ],
        )?;
        if !matches!(entry.status, StepStatus::Pending | StepStatus::Running) {
            snapshot_result(tx, project, id, None, None)?;
        }
        status_record(
            tx,
            project,
            id,
            previous.clone(),
            entry.status.clone(),
            error,
        )?;
    }
    Ok(())
}

pub fn set_input(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    name: &str,
    value: sluice_model::rpc::JsonValue,
    author: String,
    reason: String,
) -> Result<()> {
    check_context(tx, context)?;
    let declaration = context
        .plan
        .inputs()
        .get(name)
        .ok_or_else(|| invalid("no such plan input"))?;
    check_value_at(&declaration.ty, value.as_value(), &format!("inputs.{name}")).map_err(|e| {
        invalid(
            e.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    tx.sql().execute(
        "UPDATE inputs SET value=?3 WHERE project_id=?1 AND name=?2",
        params![
            context.project.to_string(),
            name,
            serde_json::to_string(&value)?
        ],
    )?;
    tx.append_record(
        Some(context.project),
        Event::PlanInput {
            rev: context.revision,
            author,
            reason,
            name: name.into(),
            value,
        },
    )?;
    tx.changed(Some(context.project), "plan");
    reconcile(tx, context)
}

pub(crate) fn validate_outputs(
    step: &Step,
    outputs: &JsonMap,
    submission: bool,
    item: bool,
) -> Result<()> {
    let declarations: Vec<_> = if submission {
        step.declared_outputs
            .iter()
            .map(|(name, decl)| (name.clone(), decl.ty.clone()))
            .collect()
    } else {
        step.signature
            .outputs
            .iter()
            .map(|(name, ty)| (name.clone(), ty.clone()))
            .chain(
                step.declared_outputs
                    .iter()
                    .map(|(name, decl)| (name.clone(), decl.ty.clone())),
            )
            .collect()
    };
    let mut errors = vec![];
    for (name, ty) in &declarations {
        let ty = if !item && step.scatter.is_some() {
            Type::List(Box::new(ty.clone()))
        } else {
            ty.clone()
        };
        match outputs.0.get(name) {
            Some(value) => {
                if let Err(e) = check_value_at(&ty, value.as_value(), &format!("outputs.{name}")) {
                    errors.extend(e.into_iter().map(|e| e.to_string()));
                }
            }
            None if matches!(ty, Type::Optional(_))
                || matches!(ty,Type::List(ref inner) if matches!(**inner,Type::Optional(_))) => {}
            None => errors.push(format!("outputs.{name}: required")),
        }
    }
    for name in outputs.0.keys() {
        if !declarations.iter().any(|(decl, _)| decl == name) {
            errors.push(format!("outputs.{name}: undeclared output"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PublicError::Invalid {
            message: "outputs do not match declarations".into(),
            errors,
        }
        .into())
    }
}

/// Record failure of ready work that never created a process or acquired holds.
pub fn fail_unlaunched(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    id: &StepId,
    expected_hash: sluice_model::hash::InputsHash,
    error: PublicError,
) -> Result<()> {
    check_context(tx, context)?;
    let state = read_state(tx.sql(), context.project)?;
    let step = context
        .plan
        .steps()
        .get(id)
        .ok_or_else(|| invalid("no such step"))?;
    if state.status(id) != StepStatus::Pending
        || !matches!(
            gates::evaluate_step(&context.plan, &state, step),
            GateDecision::Ready
        )
        || inputs_hash(&context.plan, &state, step) != Some(expected_hash)
    {
        return Err(conflict("unlaunched step changed during evaluation"));
    }
    tx.sql().execute("UPDATE steps SET status='failed',error=?3,skipped=NULL,manual=0,result_id=NULL WHERE project_id=?1 AND step_id=?2", params![context.project.to_string(), id.as_str(), serde_json::to_string(&error)?])?;
    snapshot_result(tx, context.project, id, None, None)?;
    status_record(
        tx,
        context.project,
        id,
        StepStatus::Pending,
        StepStatus::Failed,
        Some(error),
    )?;
    reconcile(tx, context)
}

/// Settle scheduler-owned inline work without a process or a manual-output marker.
/// Rechecks readiness and effective inputs in the writer transaction.
pub fn settle_inline(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    expected_hash: sluice_model::hash::InputsHash,
    request: sluice_model::commands::StepSetOutput,
) -> Result<ResultId> {
    check_context(tx, context)?;
    let step = context
        .plan
        .steps()
        .get(&request.step)
        .ok_or_else(|| invalid("no such step"))?;
    let state = read_state(tx.sql(), context.project)?;
    if state.status(&request.step) != StepStatus::Pending
        || !matches!(
            gates::evaluate_step(&context.plan, &state, step),
            GateDecision::Ready
        )
    {
        return Err(conflict("inline step is no longer ready"));
    }
    validate_outputs(step, &request.outputs, false, false)?;
    let hash = inputs_hash(&context.plan, &state, step)
        .ok_or_else(|| invalid("inline inputs unavailable"))?;
    if hash != expected_hash {
        return Err(conflict("inline inputs changed during evaluation"));
    }
    let inputs = effective_map(&context.plan, &state, step)?;
    tx.sql().execute("UPDATE steps SET status='succeeded',outputs=?3,inputs_hash=?4,error=NULL,skipped=NULL,manual=0,run_ids='[]',instances='{}',total=NULL,done=0,result_id=NULL WHERE project_id=?1 AND step_id=?2",
        params![context.project.to_string(), request.step.as_str(), serde_json::to_string(&request.outputs)?, hash.to_string()])?;
    let result = snapshot_result(tx, context.project, &request.step, None, inputs.as_ref())?;
    status_record(
        tx,
        context.project,
        &request.step,
        StepStatus::Pending,
        StepStatus::Succeeded,
        None,
    )?;
    reconcile(tx, context)?;
    Ok(result)
}

pub fn step_set_output(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: sluice_model::commands::StepSetOutput,
) -> Result<ResultId> {
    check_context(tx, context)?;
    check_selector(tx.sql(), context.project, &request.project)?;
    let step = context
        .plan
        .steps()
        .get(&request.step)
        .ok_or_else(|| invalid("no such step"))?;
    let state = read_state(tx.sql(), context.project)?;
    if state.status(&request.step) == StepStatus::Running {
        return Err(invalid("cannot set outputs while running"));
    }
    validate_outputs(step, &request.outputs, false, false)?;
    if !request.force {
        // Pauses hold back launches only; a paused step can still be given its result.
        let errors = match gates::evaluate_inputs(&context.plan, &state, step) {
            GateDecision::Ready => vec![],
            GateDecision::Wait(reasons) => reasons,
            GateDecision::Skip(reasons) => reasons.into_iter().map(|r| r.to_string()).collect(),
            GateDecision::Invalid(errors) => errors.into_iter().map(|e| e.to_string()).collect(),
        };
        if !errors.is_empty() {
            return Err(PublicError::Invalid {
                message: "step gates or inputs are not ready".into(),
                errors,
            }
            .into());
        }
    }
    let hash = inputs_hash(&context.plan, &state, step);
    tx.sql().execute("UPDATE steps SET status='succeeded',outputs=?3,inputs_hash=?4,error=NULL,skipped=NULL,manual=1,run_ids='[]',instances='{}',total=NULL,done=0,result_id=NULL WHERE project_id=?1 AND step_id=?2",
        params![context.project.to_string(),request.step.as_str(),serde_json::to_string(&request.outputs)?,hash.map(|h|h.to_string())])?;
    let inputs = effective_map(&context.plan, &state, step)?;
    let result = snapshot_result(tx, context.project, &request.step, None, inputs.as_ref())?;
    tx.append_record(
        Some(context.project),
        Event::StepOutput {
            rev: context.revision,
            author: request.author.unwrap_or_default(),
            reason: request.reason,
            step: request.step.clone(),
            outputs: request.outputs,
            force: request.force,
        },
    )?;
    status_record(
        tx,
        context.project,
        &request.step,
        state.status(&request.step),
        StepStatus::Succeeded,
        None,
    )?;
    reconcile(tx, context)?;
    Ok(result)
}
pub(crate) fn effective_map(
    plan: &Plan,
    state: &StateSnapshot,
    step: &Step,
) -> Result<Option<JsonMap>> {
    sluice_model::plan::effective_inputs(plan, state, step)
        .map(|bindings| {
            let mut map = JsonMap::default();
            for (name, value) in bindings {
                map.0.insert(
                    name,
                    match value {
                        EffectiveInput::Value(v) => v,
                        EffectiveInput::File(path) => json!({"file":path}).try_into()?,
                    },
                );
            }
            Ok(map)
        })
        .transpose()
}
pub(crate) fn check_selector(
    c: &Connection,
    project: ProjectId,
    selector: &sluice_model::ids::ProjectSelector,
) -> Result<()> {
    let current = identity(c, project)?;
    let matches = match selector {
        sluice_model::ids::ProjectSelector::Id(id) => *id == project,
        sluice_model::ids::ProjectSelector::Name(name) => *name == current.name,
    };
    if matches {
        Ok(())
    } else {
        Err(invalid("project selector does not match command context"))
    }
}
fn select(context: &PlanContext, selection: &StepSelection) -> Result<Vec<StepId>> {
    if let Some(ids) = &selection.steps {
        for id in ids {
            if !context.plan.steps().contains_key(id) {
                return Err(invalid(format!("no such step {id}")));
            }
        }
    }
    if selection.steps.as_ref().is_none_or(|ids| ids.is_empty())
        && selection.tags.as_ref().is_none_or(|tags| tags.is_empty())
    {
        return Err(invalid("select steps by ids or tags"));
    }
    Ok(context
        .plan
        .steps()
        .iter()
        .filter(|(id, step)| {
            selection.steps.as_ref().is_some_and(|ids| ids.contains(id))
                || selection
                    .tags
                    .as_ref()
                    .is_some_and(|tags| tags.iter().any(|tag| step.tags.contains(tag)))
        })
        .map(|(id, _)| id.clone())
        .collect())
}

pub fn step_retry(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: sluice_model::commands::StepRetry,
    messages: &mut impl RetryMessages,
) -> Result<RetryResult> {
    check_context(tx, context)?;
    check_selector(tx.sql(), context.project, &request.project)?;
    retry_selected(
        tx,
        context,
        &select(context, &request.selection)?,
        request.message.as_deref(),
        request.author.as_deref().unwrap_or_default(),
        request.reason.as_deref().unwrap_or_default(),
        messages,
    )
}
pub(crate) fn retry_selected(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    selected: &[StepId],
    message: Option<&str>,
    author: &str,
    reason: &str,
    messages: &mut impl RetryMessages,
) -> Result<RetryResult> {
    let walk = prepare_retry(tx, context, selected, message, author, messages)?;
    apply_retry(tx, context, walk, message, author, reason, messages)
}
pub(crate) fn prepare_retry(
    tx: &WriteTransaction<'_>,
    context: &PlanContext,
    selected: &[StepId],
    message: Option<&str>,
    author: &str,
    messages: &impl RetryMessages,
) -> Result<sluice_model::units::RetryWalk> {
    let state = read_state(tx.sql(), context.project)?;
    let walk = retry_walk(&context.plan, &state, selected).map_err(|e| {
        invalid(
            e.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    })?;
    if let Some(body) = message {
        validate_message(body)?;
        messages.validate_retry(tx, context.project, &walk.steps, body, author)?;
    }
    Ok(walk)
}
pub(crate) fn apply_retry(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    walk: sluice_model::units::RetryWalk,
    message: Option<&str>,
    author: &str,
    reason: &str,
    messages: &mut impl RetryMessages,
) -> Result<RetryResult> {
    let state = read_state(tx.sql(), context.project)?;
    for id in walk.resets() {
        let previous = state.status(id);
        let mut retained = json!({});
        if previous == StepStatus::Failed && context.plan.steps()[id].scatter.is_some() {
            let instances: String = tx.sql().query_row(
                "SELECT instances FROM steps WHERE project_id=?1 AND step_id=?2",
                params![context.project.to_string(), id.as_str()],
                |r| r.get(0),
            )?;
            retained = serde_json::from_str(&instances)?;
            retained
                .as_object_mut()
                .ok_or_else(|| invalid("scatter instances are not an object"))?
                .retain(|_, item| item["status"] == "succeeded");
        }
        let done = retained.as_object().expect("retained object").len() as i64;
        tx.sql().execute("UPDATE steps SET status='pending',work_generation=work_generation+1,error=NULL,skipped=NULL,done=?4,instances=?3 WHERE project_id=?1 AND step_id=?2",
            params![context.project.to_string(),id.as_str(),retained.to_string(),done])?;
        let work = tx.sql().query_row(
            "SELECT work_generation FROM steps WHERE project_id=?1 AND step_id=?2",
            params![context.project.to_string(), id.as_str()],
            |r| r.get::<_, i64>(0),
        )?;
        let record = tx.append_record(
            Some(context.project),
            Event::StepRetry {
                rev: context.revision,
                author: author.into(),
                reason: reason.into(),
                step: id.clone(),
                work: WorkGeneration(work as u64),
            },
        )?;
        mark_stopped(tx, context.project, id, "retry", author, reason, &record.at)?;
        status_record(tx, context.project, id, previous, StepStatus::Pending, None)?;
    }
    if let Some(body) = message {
        for id in &walk.steps {
            messages.post_retry(tx, context.project, id, body, author)?;
        }
    }
    Ok(RetryResult {
        project: identity(tx.sql(), context.project)?,
        steps: walk.steps,
        rearmed: walk.rearmed,
        stopped_at: walk.stopped_at,
    })
}
/// Keep who cancelled or retried `step`, and why, on its latest run of each item (`runs.stopped`
/// under `kind`, "cancel" or "retry"), so a trimmed log does not lose it. A step with no run yet
/// keeps it only in the log.
pub(crate) fn mark_stopped(
    tx: &WriteTransaction<'_>,
    project: ProjectId,
    step: &StepId,
    kind: &str,
    author: &str,
    reason: &str,
    at: &str,
) -> Result<()> {
    let entry = json!({"author": author, "reason": reason, "at": at}).to_string();
    tx.sql().execute(
        "UPDATE runs SET stopped=json_set(coalesce(stopped,'{}'),'$.'||?3,json(?4))
         WHERE project_id=?1 AND step_id=?2 AND NOT EXISTS (SELECT 1 FROM runs n
           WHERE n.project_id=runs.project_id AND n.step_id=runs.step_id AND n.item_index=runs.item_index
           AND (n.created_at>runs.created_at OR (n.created_at=runs.created_at AND n.run_id>runs.run_id)))",
        params![project.to_string(), step.as_str(), kind, entry],
    )?;
    Ok(())
}
pub(crate) fn validate_message(body: &str) -> Result<()> {
    if body.trim().is_empty() || body.len() > 65536 {
        return Err(invalid("retry message must contain 1 to 65536 bytes"));
    }
    Ok(())
}

/// Cancellation records intent for admitted work. Pending outside work can fail
/// immediately because there is no process or hold to clean up.
pub fn step_cancel(
    tx: &mut WriteTransaction<'_>,
    context: &PlanContext,
    request: sluice_model::commands::StepCancel,
) -> Result<Vec<StepId>> {
    check_context(tx, context)?;
    check_selector(tx.sql(), context.project, &request.project)?;
    let selected = select(context, &request.selection)?;
    let state = read_state(tx.sql(), context.project)?;
    for id in &selected {
        match state.status(id) {
            StepStatus::Running => {}
            StepStatus::Pending if context.plan.steps()[id].is_external() => {}
            _ => {
                return Err(invalid(format!(
                    "step {id} must be running or pending external work"
                )));
            }
        }
    }
    for id in &selected {
        let author = request.author.clone().unwrap_or_default();
        let record = tx.append_record(
            Some(context.project),
            Event::StepCancel {
                step: id.clone(),
                author: author.clone(),
                reason: request.reason.clone(),
            },
        )?;
        mark_stopped(
            tx,
            context.project,
            id,
            "cancel",
            &author,
            &request.reason,
            &record.at,
        )?;
        let status = state.status(id);
        if status == StepStatus::Running {
            tx.sql().execute("UPDATE attempts SET cancel_requested=1 WHERE project_id=?1 AND step_id=?2 AND phase<>'terminal'",params![context.project.to_string(),id.as_str()])?;
        } else {
            let error = PublicError::Cancelled {
                message: request.reason.clone(),
            };
            tx.sql().execute("UPDATE steps SET status='failed',error=?3,skipped=NULL,manual=0,result_id=NULL,outputs=NULL WHERE project_id=?1 AND step_id=?2",params![context.project.to_string(),id.as_str(),serde_json::to_string(&error)?])?;
            snapshot_result(tx, context.project, id, None, None)?;
            status_record(
                tx,
                context.project,
                id,
                status,
                StepStatus::Failed,
                Some(error),
            )?;
        }
    }
    tx.changed(Some(context.project), "status");
    Ok(selected)
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResultError {
    Failure(PublicError),
    Skipped(Vec<sluice_model::types::SkipReason>),
}

/// Owned immutable result content; only removed_at is later archival metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct StepResult {
    pub id: ResultId,
    pub project: ProjectId,
    pub step: StepId,
    pub generation: StepGeneration,
    pub work: WorkGeneration,
    pub attempt: Option<AttemptId>,
    pub unit: Option<UnitName>,
    pub declaration: JsonMap,
    pub inputs: Option<JsonMap>,
    pub inputs_hash: Option<sluice_model::hash::InputsHash>,
    pub status: StepStatus,
    pub outputs: Option<JsonMap>,
    pub error: Option<ResultError>,
    pub manual: bool,
    pub run_ids: Vec<RunId>,
    pub recorded_at: String,
    pub removed_at: Option<String>,
}
pub fn read_result(c: &Connection, id: ResultId) -> Result<Option<StepResult>> {
    let row: Option<String> = c.query_row("SELECT json_object('id',result_id,'project',project_id,'step',step_id,'generation',generation,'work',work_generation,'attempt',attempt_id,'unit',unit,'declaration',json(declaration),'inputs',json(inputs),'inputs_hash',inputs_hash,'status',status,'outputs',json(outputs),'error',json(error),'manual',json(CASE WHEN manual THEN 'true' ELSE 'false' END),'run_ids',json(run_ids),'recorded_at',recorded_at,'removed_at',removed_at) FROM step_results WHERE result_id=?1",[id.to_string()],|r|r.get(0)).optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let value: Value = serde_json::from_str(&row)?;
    let error = if value["error"].is_null() {
        None
    } else if let Some(skipped) = value["error"].get("skipped") {
        Some(ResultError::Skipped(serde_json::from_value(
            skipped.clone(),
        )?))
    } else {
        Some(ResultError::Failure(serde_json::from_value(
            value["error"].clone(),
        )?))
    };
    Ok(Some(StepResult {
        id: serde_json::from_value(value["id"].clone())?,
        project: serde_json::from_value(value["project"].clone())?,
        step: serde_json::from_value(value["step"].clone())?,
        generation: serde_json::from_value(value["generation"].clone())?,
        work: serde_json::from_value(value["work"].clone())?,
        attempt: serde_json::from_value(value["attempt"].clone())?,
        unit: serde_json::from_value(value["unit"].clone())?,
        declaration: serde_json::from_value(value["declaration"].clone())?,
        inputs: serde_json::from_value(value["inputs"].clone())?,
        inputs_hash: serde_json::from_value(value["inputs_hash"].clone())?,
        status: serde_json::from_value(value["status"].clone())?,
        outputs: serde_json::from_value(value["outputs"].clone())?,
        error,
        manual: serde_json::from_value(value["manual"].clone())?,
        run_ids: serde_json::from_value(value["run_ids"].clone())?,
        recorded_at: serde_json::from_value(value["recorded_at"].clone())?,
        removed_at: serde_json::from_value(value["removed_at"].clone())?,
    }))
}

/// Complete authored edits plus retained manual/input/retry records in sequence order.
/// Feed retention must not shorten the plan's edit history.
pub fn history(
    sql: &Connection,
    project: ProjectId,
    since_rev: Option<Revision>,
) -> Result<Vec<sluice_model::events::Record>> {
    let mut stmt = sql.prepare(
        "SELECT seq,at,payload,payload_version FROM (
         SELECT seq,at,payload,payload_version FROM records WHERE project_id=?1
           AND kind IN ('plan.input','step.output','step.retry')
         UNION ALL
         SELECT seq,at,json_object('kind','plan.edit','rev',rev,'author',author,
           'reason',reason,'ops',json(ops)),1 FROM plan_edits WHERE project_id=?1
         ) WHERE (?2 IS NULL OR json_extract(payload,'$.rev')>?2) ORDER BY seq",
    )?;
    let rows = stmt.query_map(
        params![
            project.to_string(),
            since_rev.map(|r| sql_counter(r.0)).transpose()?
        ],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        },
    )?;
    rows.map(|row| {
        let (seq, at, payload, version) = row?;
        if version != crate::schema::RECORD_PAYLOAD_VERSION {
            return Err(StoreError::InvalidDatabase(format!(
                "unsupported record payload version {version}"
            )));
        }
        Ok(sluice_model::events::Record {
            seq: sluice_model::ids::RecordSeq(seq),
            at,
            project: Some(project),
            event: serde_json::from_str(&payload)?,
        })
    })
    .collect()
}

/// Compute and freeze an age cutoff for coordinator prune preparation.
pub fn prune_eligible_age(
    sql: &Connection,
    context: &PlanContext,
    older_than_seconds: u64,
) -> Result<PruneEligibility> {
    let seconds = i64::try_from(older_than_seconds).map_err(|_| invalid("prune age too large"))?;
    let cutoff = time::OffsetDateTime::now_utc()
        .checked_sub(time::Duration::seconds(seconds))
        .ok_or_else(|| invalid("prune age too large"))?;
    prune_eligible(sql, context, cutoff)
}
