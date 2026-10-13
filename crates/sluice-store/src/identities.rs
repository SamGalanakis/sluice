//! Identities, owners and routing (SPEC §8.1): the one place that knows who is registered in a
//! project, who is reading, who answers a unit and where a message with no recipient goes.
//!
//! - **Registry.** A session's `readers` row in a project (stream `orchestrator`, thread `''`)
//!   is its registration: its first identity-bearing call there (a message, an edit, `next`)
//!   makes it. A name is the identity: a successor reusing it inherits everything.
//! - **Reading.** A session reads while its row's `heartbeat_at` (`next` beats every 30 s
//!   while it waits) is under `LIVE_READER_MINUTES` old.
//! - **Owner.** `steps.owner` is who answers a unit (`@session` or `owner`; NULL unowned),
//!   equal across the unit's rows: a new unit's rows take the edit's explicit owner, else the
//!   editor's, else the project's `fallback`; a step joining an existing unit takes its owner;
//!   row updates leave it alone. It is runtime metadata, never in a declaration.
//! - **Routing.** A run's message with no recipient (or the retired `orchestrator`) goes to its
//!   unit's owner, else the fallback, else `owner`.
//!
//! Claiming and releasing a unit (a compare-and-set on `steps.owner` in one write
//! transaction) and per-identity reading scopes build on `unit_owner` and `set_unit_owner`
//! here without touching routing.
use crate::{Result, StoreError, WriteTransaction};
use rusqlite::{Connection, OptionalExtension, params};
use sluice_model::{
    identity::{LIVE_READER_MINUTES, OWNER, SessionName, is_session, refusal},
    ids::{ProjectId, StepId},
    plan_rows::PlanEditCommit,
};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The `readers` stream sessions are registered and read under.
pub const READER_STREAM: &str = "orchestrator";

/// Register `me` in `project`: its readers row, made once; an existing one is left alone.
pub fn register(tx: &mut WriteTransaction<'_>, project: ProjectId, me: &SessionName) -> Result<()> {
    let made = tx.sql().execute(
        "INSERT INTO readers(project_id,identity,stream,thread,cursor) VALUES (?1,?2,?3,'',0)
         ON CONFLICT(project_id,identity,stream,thread) DO NOTHING",
        params![project.to_string(), me.as_str(), READER_STREAM],
    )?;
    if made > 0 {
        tx.changed(Some(project), "readers");
    }
    Ok(())
}

/// Whether `name` has a readers row in `project`.
pub fn registered(sql: &Connection, project: ProjectId, name: &str) -> Result<bool> {
    Ok(sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM readers WHERE project_id=?1 AND identity=?2 AND stream=?3 AND thread='')",
        params![project.to_string(), name, READER_STREAM],
        |r| r.get(0),
    )?)
}

/// Whether `name` is reading `project` now: a heartbeat under `LIVE_READER_MINUTES` old.
pub fn reading(sql: &Connection, project: ProjectId, name: &str) -> Result<bool> {
    Ok(sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM readers WHERE project_id=?1 AND identity=?2 AND stream=?3 AND thread=''
           AND heartbeat_at IS NOT NULL AND (julianday('now')-julianday(heartbeat_at))*1440 < ?4)",
        params![project.to_string(), name, READER_STREAM, LIVE_READER_MINUTES],
        |r| r.get(0),
    )?)
}

/// The receipt's note on a message to a session nobody is reading.
pub fn delivery_note(sql: &Connection, project: ProjectId, to: &str) -> Result<Option<String>> {
    Ok((is_session(to) && !reading(sql, project, to)?)
        .then(|| format!("queued; {to} has no live reader")))
}

/// Refuse `name` as a recipient unless it is registered in `project`.
pub fn check_registered(
    sql: &Connection,
    project: ProjectId,
    field: &str,
    name: &str,
) -> Result<()> {
    if registered(sql, project, name)? {
        return Ok(());
    }
    let project_name: String = sql
        .query_row(
            "SELECT name FROM projects WHERE project_id=?1",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| project.to_string());
    Err(refusal(
        field,
        &format!(
            "{name} is not a session of project {project_name}: a session registers there with its first message, edit or next; check the name"
        ),
    )
    .into())
}

/// An owner or fallback as given: `owner`, or a session registered in `project` (or `me`,
/// which the same call registers).
pub fn check_owner(
    sql: &Connection,
    project: ProjectId,
    field: &str,
    value: &str,
    me: Option<&SessionName>,
) -> Result<String> {
    let value = value.trim();
    if value == OWNER {
        return Ok(OWNER.into());
    }
    if !is_session(value) {
        return Err(refusal(
            field,
            &format!("{value:?} is not an owner: an @session (for example @lash-f4) or owner"),
        )
        .into());
    }
    if me.is_none_or(|me| me.as_str() != value) {
        check_registered(sql, project, field, value)?;
    }
    Ok(value.into())
}

/// The project's fallback: who answers its unowned units.
pub fn fallback(sql: &Connection, project: ProjectId) -> Result<Option<String>> {
    Ok(sql
        .query_row(
            "SELECT fallback FROM projects WHERE project_id=?1",
            [project.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .flatten())
}

/// A unit's owner: `None` when the unit has no step, `Some(None)` when it is unowned.
pub fn unit_owner(
    sql: &Connection,
    project: ProjectId,
    unit: &str,
) -> Result<Option<Option<String>>> {
    Ok(sql
        .query_row(
            "SELECT owner FROM steps WHERE project_id=?1 AND unit=?2 ORDER BY owner IS NULL LIMIT 1",
            params![project.to_string(), unit],
            |r| r.get(0),
        )
        .optional()?)
}

/// The owner of `step`'s unit: `None` when the step is not in the plan or its unit is unowned.
pub fn step_owner(sql: &Connection, project: ProjectId, step: &str) -> Result<Option<String>> {
    let unit: Option<String> = sql
        .query_row(
            "SELECT unit FROM steps WHERE project_id=?1 AND step_id=?2",
            params![project.to_string(), step],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match unit {
        Some(unit) => unit_owner(sql, project, &unit)?.flatten(),
        None => None,
    })
}

/// Set every row of `unit` to `owner` (a claim or a release builds on this).
pub fn set_unit_owner(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    unit: &str,
    owner: Option<&str>,
) -> Result<usize> {
    let changed = tx.sql().execute(
        "UPDATE steps SET owner=?3 WHERE project_id=?1 AND unit=?2 AND owner IS NOT ?3",
        params![project.to_string(), unit, owner],
    )?;
    if changed > 0 {
        tx.changed(Some(project), "status");
    }
    Ok(changed)
}

/// Who a run's message goes to when it names nobody (or the retired `orchestrator`): its
/// step's unit's owner (a call's run: its caller, when that is a session), else the project's
/// fallback, else the person.
pub fn route_run(
    sql: &Connection,
    project: ProjectId,
    step: Option<&str>,
    call_author: Option<&str>,
) -> Result<String> {
    let owner = match step {
        Some(step) => step_owner(sql, project, step)?,
        None => call_author.filter(|a| is_session(a)).map(str::to_owned),
    };
    Ok(match owner {
        Some(owner) => owner,
        None => fallback(sql, project)?.unwrap_or_else(|| OWNER.into()),
    })
}

/// Who a message from the dashboard goes to when it names nobody (a board Button): the
/// project's fallback session; with none it is refused.
pub fn route_owner(sql: &Connection, project: ProjectId) -> Result<String> {
    match fallback(sql, project)? {
        Some(fallback) if fallback != OWNER => Ok(fallback),
        _ => Err(refusal(
            "to",
            "this project has no fallback session to send it to: set one with project_update(fallback=\"@name\")",
        )
        .into()),
    }
}

/// Who edits: the session whose edit it is, a run (its step), or nobody named.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Editor {
    Session(SessionName),
    Run(StepId),
    #[default]
    Anonymous,
}

/// An edit's owners as asked: who edits, and the owner the call names for a unit
/// (`unit_add`, `plan_edit`'s `unit.add`) or for the unit a step joins (`step_add`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OwnerIntent {
    pub editor: Editor,
    /// `(unit, owner)` as given.
    pub units: Vec<(String, String)>,
    /// `(step, owner)` as given: the owner of the unit the step is in.
    pub steps: Vec<(StepId, String)>,
}

/// The owners an edit's written steps take, worked out in its writer transaction before the
/// rows move: each step it adds or moves into another unit gets its unit's owner. A unit that
/// keeps rows the edit does not add or move in keeps their owner (an owner named for it must
/// be that one); a new unit takes the owner named for it, else (when it is made of moved steps
/// only) the first such step's, else the editor's (a session itself, a run its own unit's
/// owner), else the project's fallback, else none. Registers the editing session.
pub fn edit_owners(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    commit: &PlanEditCommit,
    intent: &OwnerIntent,
) -> Result<Vec<(StepId, Option<String>)>> {
    let me = match &intent.editor {
        Editor::Session(me) => {
            register(tx, project, me)?;
            Some(me)
        }
        _ => None,
    };
    let sql = tx.sql();
    let id = project.to_string();
    let mut named: BTreeMap<String, String> = BTreeMap::new();
    let unit_of: HashMap<&str, &str> = commit
        .rows
        .step_index
        .iter()
        .map(|row| (row.step.as_str(), row.unit.as_str()))
        .collect();
    let given = intent
        .units
        .iter()
        .map(|(unit, owner)| (unit.as_str(), owner))
        .chain(
            intent
                .steps
                .iter()
                .filter_map(|(step, owner)| unit_of.get(step.as_str()).map(|unit| (*unit, owner))),
        );
    for (unit, owner) in given {
        let owner = check_owner(sql, project, "owner", owner, me)?;
        if let Some(other) = named.insert(unit.to_owned(), owner.clone())
            && other != owner
        {
            return Err(refusal(
                "owner",
                &format!("unit {unit} is given two owners, {other} and {owner}"),
            )
            .into());
        }
    }
    let added: HashSet<&str> = commit.state.added.iter().map(StepId::as_str).collect();
    let removed: HashSet<&str> = commit.state.removed.iter().map(StepId::as_str).collect();
    // Each written step's current unit and owner, when it has a row.
    let mut current: HashMap<&str, (String, Option<String>)> = HashMap::new();
    {
        let mut row =
            sql.prepare_cached("SELECT unit,owner FROM steps WHERE project_id=?1 AND step_id=?2")?;
        for index in &commit.rows.step_index {
            if let Some(found) = row
                .query_row(params![id, index.step.as_str()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .optional()?
            {
                current.insert(index.step.as_str(), found);
            }
        }
    }
    let joining = |step: &str, unit: &str| {
        added.contains(step) || current.get(step).is_some_and(|(was, _)| was != unit)
    };
    let editor_owner = match &intent.editor {
        Editor::Session(me) => Some(me.to_string()),
        Editor::Run(step) => step_owner(sql, project, step.as_str())?,
        Editor::Anonymous => None,
    };
    let fallback = fallback(sql, project)?;
    let mut owners = Vec::new();
    let mut decided: HashMap<&str, Option<String>> = HashMap::new();
    for index in &commit.rows.step_index {
        let (step, unit) = (index.step.as_str(), index.unit.as_str());
        if !joining(step, unit) {
            continue;
        }
        if !decided.contains_key(unit) {
            // The rows that stay in the unit: not removed, not added, not moving out.
            let mut stays = sql.prepare_cached(
                "SELECT step_id,owner FROM steps WHERE project_id=?1 AND unit=?2",
            )?;
            let kept: Vec<(String, Option<String>)> = stays
                .query_map(params![id, unit], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|(member, _)| {
                    !removed.contains(member.as_str())
                        && unit_of.get(member.as_str()).is_none_or(|to| *to == unit)
                })
                .collect();
            let owner = if let Some((_, owner)) = kept.first() {
                if let Some(given) = named.get(unit)
                    && Some(given) != owner.as_ref()
                {
                    return Err(refusal(
                        "owner",
                        &format!(
                            "unit {unit} already exists, owned by {}: a step added to it takes that owner",
                            owner.as_deref().unwrap_or("nobody")
                        ),
                    )
                    .into());
                }
                owner.clone()
            } else {
                let moved = commit
                    .rows
                    .step_index
                    .iter()
                    .filter(|other| {
                        other.unit.as_str() == unit && !added.contains(other.step.as_str())
                    })
                    .find_map(|other| {
                        current
                            .get(other.step.as_str())
                            .and_then(|(_, o)| o.clone())
                    });
                named
                    .get(unit)
                    .cloned()
                    .or(moved)
                    .or_else(|| editor_owner.clone())
                    .or_else(|| fallback.clone())
            };
            decided.insert(unit, owner);
        }
        owners.push((index.step.clone(), decided[unit].clone()));
    }
    if let Some((unit, _)) = named
        .iter()
        .find(|(unit, _)| !decided.contains_key(unit.as_str()))
    {
        return Err(refusal(
            "owner",
            &format!("unit {unit} gets no new step from this edit, so its owner stays as it is"),
        )
        .into());
    }
    Ok(owners)
}

/// Write the owners `edit_owners` worked out, once the edit's rows are in.
pub fn write_owners(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    owners: &[(StepId, Option<String>)],
) -> Result<()> {
    let id = project.to_string();
    for (step, owner) in owners {
        let changed = tx.sql().execute(
            "UPDATE steps SET owner=?3 WHERE project_id=?1 AND step_id=?2",
            params![id, step.as_str(), owner],
        )?;
        if changed != 1 {
            return Err(StoreError::InvalidDatabase(format!(
                "step {step} was written by the edit but has no row"
            )));
        }
    }
    Ok(())
}

/// Set or clear the project's fallback (`project_update`), checked as an owner is.
pub fn set_fallback(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    fallback: Option<&str>,
) -> Result<()> {
    let value = fallback
        .map(|value| check_owner(tx.sql(), project, "fallback", value, None))
        .transpose()?;
    tx.sql().execute(
        "UPDATE projects SET fallback=?2 WHERE project_id=?1",
        params![project.to_string(), value],
    )?;
    Ok(())
}
