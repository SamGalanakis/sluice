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
//! - **Claim and release.** `claim` and `release` move units between owners in one write
//!   transaction: a compare-and-set on `steps.owner`, the units' open questions re-addressed
//!   to whoever answers them now, and a note from `sluice` that records the handoff in the
//!   log (a `say` whose `data.unit_owner` holds `{by, reason, units: [{unit, from, to}],
//!   questions}`; no new record kind, so releases pinned before it read the log unchanged).
//!   Routing reads the owner when a message is posted, so later questions follow the unit.
use crate::{Result, StoreError, WriteTransaction, messages};
use rusqlite::{Connection, OptionalExtension, params};
use sluice_model::{
    error::PublicError,
    events::{Event, NotificationOutcome},
    identity::{LEGACY_ORCHESTRATOR, LIVE_READER_MINUTES, OWNER, SessionName, is_session, refusal},
    ids::{AttemptId, MessageId, ProjectId, ProjectSelector, RecordSeq, StepId},
    plan_rows::PlanEditCommit,
    rpc::JsonValue,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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

/// Mark `me` as reading `project` (`log_wait`): its heartbeat, its `next` position untouched.
pub fn beat(tx: &mut WriteTransaction<'_>, project: ProjectId, me: &SessionName) -> Result<()> {
    tx.sql().execute(
        "INSERT INTO readers(project_id,identity,stream,thread,cursor,heartbeat_at) VALUES (?1,?2,?3,'',0,?4)
         ON CONFLICT(project_id,identity,stream,thread) DO UPDATE SET heartbeat_at=excluded.heartbeat_at",
        params![project.to_string(), me.as_str(), READER_STREAM, now()?],
    )?;
    tx.changed(Some(project), "readers");
    Ok(())
}

/// Where `me`'s `next` left off in `project`: the end of the last batch it was given; `None`
/// before its first.
pub fn resume(sql: &Connection, project: ProjectId, me: &str) -> Result<Option<RecordSeq>> {
    Ok(sql
        .query_row(
            "SELECT cursor FROM readers WHERE project_id=?1 AND identity=?2 AND stream=?3 AND thread='' AND cursor>0",
            params![project.to_string(), me, READER_STREAM],
            |r| r.get(0),
        )
        .optional()?
        .map(RecordSeq))
}

/// `name`'s last heartbeat in `project` (RFC 3339), if it ever waited there.
pub fn heartbeat(sql: &Connection, project: ProjectId, name: &str) -> Result<Option<String>> {
    Ok(sql
        .query_row(
            "SELECT heartbeat_at FROM readers WHERE project_id=?1 AND identity=?2 AND stream=?3 AND thread=''",
            params![project.to_string(), name, READER_STREAM],
            |r| r.get(0),
        )
        .optional()?
        .flatten())
}

fn now() -> Result<String> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| StoreError::InvalidDatabase(e.to_string()))
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

/// The units a claim or release selects, in name order: `unit` and each of `units` by name or
/// pattern (`*` any run of characters, `?` one), and every unit with a step carrying one of
/// `tags`; with `owned_by` (`@session`, `owner` or `unowned`), those of them its owner holds
/// now, or with nothing else every unit it holds. Selecting nothing is `invalid`; a name,
/// pattern or tag that matches no unit, or an owner holding none of them, is `not_found`.
pub fn select_units(
    sql: &Connection,
    project: ProjectId,
    unit: Option<&str>,
    units: &[String],
    tags: &[String],
    owned_by: Option<&str>,
) -> Result<Vec<String>> {
    let mut patterns: Vec<&str> = unit
        .into_iter()
        .chain(units.iter().map(String::as_str))
        .map(str::trim)
        .collect();
    if patterns.is_empty() && tags.is_empty() {
        if owned_by.is_none() {
            return Err(refusal(
                "units",
                "select the units: unit, units (names or patterns), tags or owned_by",
            )
            .into());
        }
        patterns.push("*");
    }
    let all: Vec<String> = sql
        .prepare_cached("SELECT DISTINCT unit FROM steps WHERE project_id=?1 ORDER BY unit")?
        .query_map([project.to_string()], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut selected = BTreeSet::new();
    let mut missing = vec![];
    for pattern in patterns {
        let owned = [pattern.to_owned()];
        let found: Vec<&String> = all
            .iter()
            .filter(|unit| sluice_model::units::keep_match(&owned, unit).is_some())
            .collect();
        if found.is_empty() {
            missing.push(format!("no unit {pattern}"));
        }
        selected.extend(found.into_iter().cloned());
    }
    for tag in tags {
        let found: Vec<String> = sql
            .prepare_cached(
                "SELECT DISTINCT s.unit FROM steps s, json_each(s.declaration,'$.tags') t WHERE s.project_id=?1 AND t.value=?2",
            )?
            .query_map(params![project.to_string(), tag], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if found.is_empty() {
            missing.push(format!("no unit is tagged {tag}"));
        }
        selected.extend(found);
    }
    if !missing.is_empty() {
        return Err(PublicError::NotFound {
            message: missing.join("; "),
        }
        .into());
    }
    let Some(owned_by) = owned_by else {
        return Ok(selected.into_iter().collect());
    };
    let holder = expected(owned_by).map_err(|_| {
        refusal(
            "owned_by",
            &format!("{owned_by:?} is not an owner: an @session, owner or unowned"),
        )
    })?;
    let mut held = vec![];
    for unit in selected {
        if unit_owner(sql, project, &unit)?.flatten() == holder {
            held.push(unit);
        }
    }
    if held.is_empty() {
        return Err(PublicError::NotFound {
            message: format!("no unit selected is owned by {}", who(holder.as_deref())),
        }
        .into());
    }
    Ok(held)
}

/// One unit's change of owner (`None`: unowned).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OwnerChange {
    pub unit: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// What a claim or release did: the units that changed owner, those already as asked, the
/// open questions it re-addressed (as they now read) and the notes that record it.
#[derive(Debug, Clone, PartialEq)]
pub struct Handoff {
    pub units: Vec<OwnerChange>,
    pub unchanged: Vec<String>,
    pub questions: Vec<sluice_model::commands::Message>,
    pub notices: Vec<MessageId>,
}

/// The owner a claim expects: a session, the person, or none.
fn expected(value: &str) -> Result<Option<String>> {
    let value = value.trim();
    if value == "unowned" {
        return Ok(None);
    }
    if value == OWNER || is_session(value) {
        return Ok(Some(value.to_owned()));
    }
    Err(refusal(
        "expected_owner",
        &format!("{value:?} is not an owner: an @session, owner or unowned"),
    )
    .into())
}

fn who(owner: Option<&str>) -> &str {
    owner.unwrap_or("nobody (unowned)")
}

fn conflicts(lines: Vec<String>) -> StoreError {
    PublicError::Conflict {
        message: format!("nothing changed: {}", lines.join("; ")),
        current_rev: None,
    }
    .into()
}

/// `unit_claim`: `me` takes `units` (from `select_units`), all or none. A unit already `me`'s
/// is left as it is. With `expected` (an `@session`, `owner` or `unowned`) each unit's owner
/// must be that now; without it a unit is taken when it is unowned or its owner session is not
/// reading (no `next` or `log_wait` heartbeat in `LIVE_READER_MINUTES`). A unit the person
/// holds (`owner`), or a reading session's without `expected`, needs `force`. Anything else is
/// one `conflict` naming every unit's owner (and a reading owner's last heartbeat), and
/// nothing changes.
pub fn claim(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    me: &SessionName,
    units: &[String],
    expected_owner: Option<&str>,
    force: bool,
    reason: &str,
) -> Result<Handoff> {
    register(tx, project, me)?;
    let expected = expected_owner.map(expected).transpose()?;
    let mut refused = vec![];
    let mut taking = vec![];
    let mut unchanged = vec![];
    for unit in units {
        let current = unit_owner(tx.sql(), project, unit)?.flatten();
        if current.as_deref() == Some(me.as_str()) {
            unchanged.push(unit.clone());
            continue;
        }
        if let Some(expected) = &expected
            && expected != &current
        {
            refused.push(format!(
                "unit {unit} is owned by {}, not {}",
                who(current.as_deref()),
                who(expected.as_deref())
            ));
            continue;
        }
        match current.as_deref() {
            Some(OWNER) if !force => refused.push(format!(
                "unit {unit} is owned by the person (owner): pass force to take it"
            )),
            Some(session)
                if expected.is_none() && !force && reading(tx.sql(), project, session)? =>
            {
                let at = heartbeat(tx.sql(), project, session)?.unwrap_or_default();
                refused.push(format!(
                    "unit {unit} is owned by {session}, which is reading (last heartbeat {at}): pass expected_owner=\"{session}\" to take it over"
                ));
            }
            _ => taking.push(OwnerChange {
                unit: unit.clone(),
                from: current,
                to: Some(me.to_string()),
            }),
        }
    }
    if !refused.is_empty() {
        return Err(conflicts(refused));
    }
    hand_over(tx, project, me, taking, unchanged, reason, Handing::Claim)
}

/// `unit_release`: `me` hands `units` it owns to `to` (`@session` registered in the project,
/// or `owner`), or leaves them unowned, so the project's fallback (else the person) answers
/// them. A unit another owns (or nobody) needs `force`; a unit already `to`'s is left as it
/// is. All or none, as `claim`.
pub fn release(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    me: &SessionName,
    units: &[String],
    to: Option<&str>,
    force: bool,
    reason: &str,
) -> Result<Handoff> {
    register(tx, project, me)?;
    let to = to
        .map(str::trim)
        .filter(|to| !to.is_empty() && *to != "unowned")
        .map(|to| check_owner(tx.sql(), project, "to", to, Some(me)))
        .transpose()?;
    let mut refused = vec![];
    let mut giving = vec![];
    let mut unchanged = vec![];
    for unit in units {
        let current = unit_owner(tx.sql(), project, unit)?.flatten();
        if current == to {
            unchanged.push(unit.clone());
        } else if current.as_deref() != Some(me.as_str()) && !force {
            refused.push(format!(
                "unit {unit} is owned by {}, not you: pass force to release it",
                who(current.as_deref())
            ));
        } else {
            giving.push(OwnerChange {
                unit: unit.clone(),
                from: current,
                to: to.clone(),
            });
        }
    }
    if !refused.is_empty() {
        return Err(conflicts(refused));
    }
    hand_over(tx, project, me, giving, unchanged, reason, Handing::Release)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Handing {
    Claim,
    Release,
}

/// Write each change: the unit's owner, its open questions re-addressed from whoever answered
/// it to whoever answers it now (its owner, else the fallback, else the person), and one note
/// from `sluice` per party told: on a claim whoever answered the units before, on a release
/// whoever answers them now (`me` itself when that is `me`).
fn hand_over(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    me: &SessionName,
    changes: Vec<OwnerChange>,
    unchanged: Vec<String>,
    reason: &str,
    handing: Handing,
) -> Result<Handoff> {
    let fallback = fallback(tx.sql(), project)?;
    let answerer = |owner: &Option<String>| -> String {
        owner
            .clone()
            .or_else(|| fallback.clone())
            .unwrap_or_else(|| OWNER.into())
    };
    let mut moved = vec![];
    let mut told: BTreeMap<String, (Vec<&OwnerChange>, Vec<MessageId>)> = BTreeMap::new();
    for change in &changes {
        set_unit_owner(tx, project, &change.unit, change.to.as_deref())?;
        let (before, after) = (answerer(&change.from), answerer(&change.to));
        let ids = readdress(tx, project, &change.unit, &before, &after)?;
        let party = match handing {
            Handing::Claim => before,
            Handing::Release => after,
        };
        let entry = told.entry(party).or_default();
        entry.0.push(change);
        entry.1.extend(ids.iter().copied());
        moved.extend(ids);
    }
    let reason = reason.trim();
    let mut notices = vec![];
    for (party, (units, questions)) in told {
        let names = units
            .iter()
            .map(|c| c.unit.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let plural = if units.len() == 1 { "unit" } else { "units" };
        let mut body = match handing {
            Handing::Claim => {
                let from: BTreeSet<&str> = units.iter().map(|c| who(c.from.as_deref())).collect();
                format!(
                    "{me} claimed {plural} {names} from {}",
                    from.into_iter().collect::<Vec<_>>().join(", ")
                )
            }
            Handing::Release => match &units[0].to {
                Some(to) => format!("{me} handed {plural} {names} to {to}"),
                None => format!(
                    "{me} released {plural} {names}: unowned now, so {} answers",
                    fallback.as_deref().map_or_else(
                        || "the person (owner)".to_owned(),
                        |f| format!("the fallback {f}")
                    )
                ),
            },
        };
        if !reason.is_empty() {
            body.push_str(&format!(" ({reason})"));
        }
        body.push('.');
        if !questions.is_empty() {
            let ids = questions
                .iter()
                .map(|id| format!("#{}", id.0))
                .collect::<Vec<_>>()
                .join(", ");
            body.push_str(&format!(
                " Open questions re-addressed to {}: {ids}.",
                answerer(&units[0].to)
            ));
        }
        let data = serde_json::json!({"unit_owner": {
            "by": me.as_str(),
            "reason": reason,
            "units": units,
            "questions": questions.iter().map(|id| id.0).collect::<Vec<_>>(),
        }});
        let posted = messages::post(
            tx,
            messages::Post {
                project: ProjectSelector::Id(project),
                speaker: messages::Speaker::Sluice,
                body,
                verb: messages::Verb::Say {
                    to: party,
                    data: Some(JsonValue::try_from(data)?),
                },
            },
            &messages::NoPlanInputs,
        )?;
        notices.push(posted.message.id);
    }
    let questions = moved
        .into_iter()
        .map(|id| messages::message(tx.sql(), project, id))
        .collect::<Result<_>>()?;
    Ok(Handoff {
        units: changes,
        unchanged,
        questions,
        notices,
    })
}

/// Re-address `unit`'s open questions (asked by its steps) that went to `before` (or to the
/// retired `orchestrator`) to `after`; a question to the person reserves its notification, as
/// posting one does. The log keeps each question's record as it was posted.
fn readdress(
    tx: &mut WriteTransaction<'_>,
    project: ProjectId,
    unit: &str,
    before: &str,
    after: &str,
) -> Result<Vec<MessageId>> {
    let id = project.to_string();
    let ids: Vec<i64> = tx
        .sql()
        .prepare_cached(
            "SELECT m.id FROM messages m WHERE m.project_id=?1 AND m.needs_reply=1 AND m.resolved_by IS NULL
               AND m.closed_at IS NULL AND m.\"to\" IN (?2,?3) AND m.\"to\"!=?4
               AND m.\"from\" IN (SELECT s.step_id FROM steps s WHERE s.project_id=?1 AND s.unit=?5)
             ORDER BY m.id",
        )?
        .query_map(params![id, before, LEGACY_ORCHESTRATOR, after, unit], |r| {
            r.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    let at = now()?;
    for question in &ids {
        tx.sql().execute(
            "UPDATE messages SET \"to\"=?3 WHERE project_id=?1 AND id=?2",
            params![id, question, after],
        )?;
        if after == OWNER {
            let reserved = tx.sql().execute(
                "INSERT INTO notification_attempts(project_id,message_id,attempt_id,outcome,reserved_at) VALUES (?1,?2,?3,'reserved',?4)
                 ON CONFLICT(project_id,message_id) DO NOTHING",
                params![id, question, AttemptId::new().to_string(), at],
            )?;
            if reserved > 0 {
                tx.append_record(
                    Some(project),
                    Event::ProjectNotify {
                        message: MessageId(*question),
                        outcome: NotificationOutcome::Reserved,
                        error: None,
                    },
                )?;
            }
        }
    }
    if !ids.is_empty() {
        for view in ["messages", "questions", "status"] {
            tx.changed(Some(project), view);
        }
    }
    Ok(ids.into_iter().map(MessageId).collect())
}
