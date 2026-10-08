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
    pub limit: u32,
}
impl Default for RecordFilter {
    fn default() -> Self {
        Self {
            since: None,
            kinds: vec![],
            threads: vec![],
            statuses: vec![],
            recipients: vec![],
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
