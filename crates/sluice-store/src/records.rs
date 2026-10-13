//! Typed feed reads and background retention. Durable domain rows are never trimmed here.

use crate::{Result, StoreError, WriteTransaction, schema::RECORD_PAYLOAD_VERSION};
use rusqlite::{Connection, params, params_from_iter, types::Value};
use sluice_model::{
    commands::{LogRead, RecordPage, StepStatus},
    error::PublicError,
    events::Record,
    ids::{ProjectId, RecordSeq},
};

pub const RETENTION_MAX: usize = 10_000;
pub const RETENTION_KEEP: usize = 9_000;
const KINDS: &[&str] = &[
    "plan.edit",
    "plan.input",
    "step.output",
    "step.retry",
    "step.cancel",
    "step.submit",
    "step.settle",
    "step.bypass",
    "step.status",
    "step.lease",
    "step.queued",
    "call",
    "message",
    "project.pause",
    "project.archive",
    "project.update",
    "project.board",
    "project.rename",
    "project.delete",
    "project.capacity",
    "project.notify",
    "run.adopt",
    "run.orphan",
    "run.completion_action",
    "run.completion_action.register",
    "unit.settled",
];
const GROUPS: &[&str] = &["plan", "step", "project", "run", "unit"];

/// Each filter narrows only the records it speaks of, and an empty one is no filter: `kinds`
/// every record, `threads` and `recipients` message records, `statuses` step.status records.
/// `threads` without `kinds` also keeps messages only.
#[derive(Debug, Clone)]
pub struct RecordFilter {
    pub since: Option<RecordSeq>,
    pub kinds: Vec<String>,
    pub threads: Vec<String>,
    /// step.status records whose `to` is one of these.
    pub statuses: Vec<StepStatus>,
    /// message records whose `to` is one of these.
    pub recipients: Vec<String>,
    /// Whose records (`next`, `log_wait`): none reads everyone's.
    pub scope: Option<ScopeFilter>,
    pub limit: u32,
}

/// A wait's scope (SPEC §8.1, "What a session sees"), as SQL over each record:
///
/// - a unit's records (`unit.settled`, and every record naming a step: `step.*`, `run.*` of a
///   step) when the unit is the reader's (`mine`: its owner is the reader, or it is unowned and
///   the project's fallback is the reader or there is none) and is selected (`units` patterns,
///   `tags`); a removed step's unit is unknown, so its records read as an unowned unit's;
/// - a message when it is to or from the reader, to the retired `orchestrator` or to nobody
///   (stored before identities), or to or from a step of a unit as above;
/// - a call's records when the reader made the call;
/// - every other record (the project's own: pause, archive, settings, plan edits) always.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeFilter {
    /// The reading session, `@name`.
    pub me: Option<String>,
    /// Only the reader's own units (needs `me`).
    pub mine: bool,
    /// Unit-name patterns (`*` any run, `?` one character); empty selects every unit.
    pub units: Vec<String>,
    /// A unit carrying one of these tags; empty selects every unit.
    pub tags: Vec<String>,
}
impl ScopeFilter {
    /// Whether it narrows anything: an `all` scope with no selection reads everything.
    pub fn narrows(&self) -> bool {
        self.mine || !self.units.is_empty() || !self.tags.is_empty()
    }
    /// The SQL condition on `records` and its arguments, in order.
    fn condition(&self, args: &mut Vec<Value>) -> String {
        let me = || Value::Text(self.me.clone().unwrap_or_default());
        // Whether unit `u` (an SQL expression) is in scope; each call pushes its own arguments.
        let unit_ok = |u: &str, args: &mut Vec<Value>| {
            let mut parts = vec![];
            if self.mine {
                parts.push(format!(
                    "coalesce((SELECT s.owner FROM steps s WHERE s.project_id=records.project_id AND s.unit=({u}) ORDER BY s.owner IS NULL LIMIT 1),\
                     (SELECT p.fallback FROM projects p WHERE p.project_id=records.project_id),?)=?"
                ));
                args.push(me());
                args.push(me());
            }
            if !self.units.is_empty() {
                parts.push(format!(
                    "({})",
                    vec![format!("({u}) GLOB ?"); self.units.len()].join(" OR ")
                ));
                args.extend(self.units.iter().map(|p| Value::Text(glob(p))));
            }
            if !self.tags.is_empty() {
                parts.push(format!(
                    "EXISTS(SELECT 1 FROM steps s, json_each(s.declaration,'$.tags') t WHERE s.project_id=records.project_id AND s.unit=({u}) AND t.value IN ({}))",
                    vec!["?"; self.tags.len()].join(",")
                ));
                args.extend(self.tags.iter().cloned().map(Value::Text));
            }
            if parts.is_empty() {
                "1".to_owned()
            } else {
                format!("({})", parts.join(" AND "))
            }
        };
        let step_unit = |step: &str| {
            format!(
                "(SELECT s.unit FROM steps s WHERE s.project_id=records.project_id AND s.step_id={step})"
            )
        };
        let from = "json_extract(records.payload,'$.from')";
        let to = "json_extract(records.payload,'$.to')";
        let mut message = vec![];
        if self.me.is_some() {
            message.push(format!("{from}=? OR {to}=?"));
            args.push(me());
            args.push(me());
        }
        message.push(format!("{to} IS NULL OR {to}='orchestrator'"));
        let (from_unit, to_unit) = (step_unit(from), step_unit(to));
        message.push(format!(
            "({from_unit} IS NOT NULL AND {})",
            unit_ok(&from_unit, args)
        ));
        message.push(format!(
            "({to_unit} IS NOT NULL AND {})",
            unit_ok(&to_unit, args)
        ));
        let settled = unit_ok("json_extract(records.payload,'$.unit')", args);
        let stepped = unit_ok(&step_unit("records.step_id"), args);
        let call = if self.me.is_some() {
            args.push(me());
            "(SELECT c.author FROM calls c WHERE c.call_id=records.call_id OR c.run_id=records.call_id LIMIT 1)=?"
        } else {
            "0"
        };
        format!(
            " AND (CASE WHEN records.kind='message' THEN ({}) WHEN records.kind='unit.settled' THEN {settled} WHEN records.step_id IS NOT NULL THEN {stepped} WHEN records.call_id IS NOT NULL THEN {call} ELSE 1 END)",
            message.join(" OR ")
        )
    }
}
/// A unit pattern as SQLite's GLOB reads it: `*` and `?` as given, any `[` taken literally.
fn glob(pattern: &str) -> String {
    pattern.replace('[', "[[]")
}
impl Default for RecordFilter {
    fn default() -> Self {
        Self {
            since: None,
            kinds: vec![],
            threads: vec![],
            statuses: vec![],
            recipients: vec![],
            scope: None,
            limit: 200,
        }
    }
}
impl From<&LogRead> for RecordFilter {
    fn from(read: &LogRead) -> Self {
        Self {
            since: read.since_seq,
            kinds: read.kinds.clone().unwrap_or_default(),
            threads: read.threads.clone().unwrap_or_default(),
            statuses: read.statuses.clone().unwrap_or_default(),
            recipients: read.recipients.clone().unwrap_or_default(),
            scope: None,
            limit: read.limit,
        }
    }
}

/// Kept separate from the model's message-only PublicError so bounds remain typed.
#[derive(Debug, Clone, PartialEq)]
pub enum RecordRead {
    Page(RecordPage),
    CursorExpired {
        earliest: RecordSeq,
        latest: RecordSeq,
    },
}
impl RecordRead {
    pub fn into_page(self) -> Result<RecordPage> {
        match self {
            Self::Page(page) => Ok(page),
            Self::CursorExpired { earliest, latest } => Err(PublicError::CursorExpired {
                message: format!(
                    "cursor expired; earliest={}, latest={}; resnapshot status/messages",
                    earliest.0, latest.0
                ),
            }
            .into()),
        }
    }
}

pub fn bounds(sql: &Connection, project: Option<ProjectId>) -> Result<(RecordSeq, RecordSeq)> {
    Ok(sql.query_row(
        "SELECT coalesce(min(seq),0),coalesce(max(seq),0) FROM records WHERE project_id IS ?1",
        [project.map(|id| id.to_string())],
        |r| Ok((RecordSeq(r.get(0)?), RecordSeq(r.get(1)?))),
    )?)
}

fn floor_path(project: Option<ProjectId>) -> String {
    format!(
        "$.record_floors.\"{}\"",
        project.map_or_else(|| "home".into(), |id| id.to_string())
    )
}

/// Call within a ReadPool snapshot. The cursor and records come from that same snapshot.
pub fn read_records(
    sql: &Connection,
    project: Option<ProjectId>,
    filter: &RecordFilter,
) -> Result<RecordRead> {
    if filter.limit == 0 || filter.since.is_some_and(|seq| seq.0 < 0) {
        return Err(PublicError::BadRequest {
            message: "limit must be positive and cursor nonnegative".into(),
        }
        .into());
    }
    for kind in &filter.kinds {
        if !KINDS.contains(&kind.as_str()) && !GROUPS.contains(&kind.as_str()) {
            return Err(PublicError::BadRequest {
                message: format!("unknown record kind {kind}"),
            }
            .into());
        }
    }
    let (earliest, latest) = bounds(sql, project)?;
    let floor: i64 = sql.query_row(
        "SELECT coalesce(json_extract(maintenance_settings,?1),0) FROM home_meta WHERE singleton=1",
        [floor_path(project)],
        |r| r.get(0),
    )?;
    if filter.since.is_some_and(|seq| seq.0 < floor) {
        return Ok(RecordRead::CursorExpired { earliest, latest });
    }
    // Seqs are the home's, so a cursor may pass this log's end, but never the
    // last seq the home has issued: one beyond it comes from another home or
    // from before an import renumbered the log, and would wait forever.
    let issued: i64 = sql.query_row(
        "SELECT coalesce((SELECT seq FROM sqlite_sequence WHERE name='records'),0)",
        [],
        |r| r.get(0),
    )?;
    if filter.since.is_some_and(|seq| seq.0 > issued) {
        return Ok(RecordRead::CursorExpired { earliest, latest });
    }
    let mut query =
        "SELECT seq,at,payload_version,payload FROM records WHERE project_id IS ?".to_owned();
    let mut args = vec![project.map_or(Value::Null, |id| Value::Text(id.to_string()))];
    let mut kinds = filter.kinds.clone();
    if kinds.is_empty() && !filter.threads.is_empty() {
        kinds.push("message".into());
    }
    if !kinds.is_empty() {
        query.push_str(" AND (");
        for (i, kind) in kinds.iter().enumerate() {
            if i > 0 {
                query.push_str(" OR ");
            }
            if GROUPS.contains(&kind.as_str()) {
                query.push_str("kind GLOB ?");
                args.push(Value::Text(format!("{kind}.*")));
            } else {
                query.push_str("kind=?");
                args.push(Value::Text(kind.clone()));
            }
        }
        query.push(')');
    }
    if !filter.threads.is_empty() {
        query.push_str(" AND (kind!='message' OR thread IN (");
        query.push_str(&vec!["?"; filter.threads.len()].join(","));
        query.push_str("))");
        args.extend(filter.threads.iter().cloned().map(Value::Text));
    }
    for (kind, values) in [
        (
            "step.status",
            filter
                .statuses
                .iter()
                .map(|status| status.as_str().to_owned())
                .collect::<Vec<_>>(),
        ),
        ("message", filter.recipients.clone()),
    ] {
        if !values.is_empty() {
            query.push_str(" AND (kind!=? OR json_extract(payload,'$.to') IN (");
            query.push_str(&vec!["?"; values.len()].join(","));
            query.push_str("))");
            args.push(Value::Text(kind.into()));
            args.extend(values.into_iter().map(Value::Text));
        }
    }
    if let Some(scope) = filter.scope.as_ref().filter(|scope| scope.narrows()) {
        query.push_str(&scope.condition(&mut args));
    }
    if let Some(seq) = filter.since {
        query.push_str(" AND seq>? ORDER BY seq LIMIT ?");
        args.push(Value::Integer(seq.0));
    } else {
        query.push_str(" ORDER BY seq DESC LIMIT ?");
    }
    args.push(Value::Integer(i64::from(filter.limit) + 1));
    let mut stmt = sql.prepare(&query)?;
    let mut rows = stmt.query(params_from_iter(args))?;
    let mut records = vec![];
    while let Some(row) = rows.next()? {
        let version: i64 = row.get(2)?;
        if version != RECORD_PAYLOAD_VERSION {
            return Err(StoreError::InvalidDatabase(format!(
                "unsupported record payload version {version}"
            )));
        }
        let payload: String = row.get(3)?;
        records.push(Record {
            seq: RecordSeq(row.get(0)?),
            at: row.get(1)?,
            project,
            event: serde_json::from_str(&payload)?,
        });
    }
    let more = records.len() > filter.limit as usize;
    records.truncate(filter.limit as usize);
    let last_seq = if filter.since.is_some() && more {
        records.last().map_or(latest, |r| r.seq)
    } else {
        RecordSeq(latest.0.max(filter.since.map_or(0, |s| s.0)))
    };
    if filter.since.is_none() {
        records.reverse();
    }
    Ok(RecordRead::Page(RecordPage { records, last_seq }))
}

/// Background maintenance, composed inside the writer transaction. No domain GC.
pub fn trim_records(tx: &mut WriteTransaction<'_>, project: Option<ProjectId>) -> Result<usize> {
    trim_to(tx, project, RETENTION_MAX, RETENTION_KEEP)
}

pub fn trim_to(
    tx: &mut WriteTransaction<'_>,
    project: Option<ProjectId>,
    maximum: usize,
    keep: usize,
) -> Result<usize> {
    if keep == 0 || keep > maximum {
        return Err(PublicError::BadRequest {
            message: "retention requires 0 < keep <= maximum".into(),
        }
        .into());
    }
    let scope = project.map(|id| id.to_string());
    let count: i64 = tx.sql().query_row(
        "SELECT count(*) FROM records WHERE project_id IS ?1",
        [&scope],
        |r| r.get(0),
    )?;
    if count
        <= i64::try_from(maximum).map_err(|_| PublicError::BadRequest {
            message: "retention too large".into(),
        })?
    {
        return Ok(0);
    }
    let oldest: i64 = tx.sql().query_row(
        "SELECT seq FROM records WHERE project_id IS ?1 ORDER BY seq DESC LIMIT 1 OFFSET ?2",
        params![scope, (keep - 1) as i64],
        |r| r.get(0),
    )?;
    let removed_through: i64 = tx.sql().query_row(
        "SELECT max(seq) FROM records WHERE project_id IS ?1 AND seq<?2",
        params![scope, oldest],
        |r| r.get(0),
    )?;
    let removed = tx.sql().execute(
        "DELETE FROM records WHERE project_id IS ?1 AND seq<?2",
        params![scope, oldest],
    )?;
    tx.sql().execute("UPDATE home_meta SET maintenance_settings=json_set(maintenance_settings,?1,max(coalesce(json_extract(maintenance_settings,?1),0),?2)),record_floor=max(record_floor,?2) WHERE singleton=1",
        params![floor_path(project), removed_through])?;
    tx.changed(project, "log");
    tx.changed(None, "log");
    Ok(removed)
}
