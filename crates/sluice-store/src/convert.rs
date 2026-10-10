//! The schema-3 converter (`docs/design/plan-rows.md` §10.2–§10.6): a home at schema 1 or 2
//! becomes a schema-3 home in one transaction, or is left untouched.
//!
//! The legacy plan history lives only here: each project's RFC 6902 edits are replayed from
//! `{"steps":{}}` with the old patch semantics (one operation at a time, restoring the key
//! order of every map above each touched path) and without validation, so no old revision is
//! reinterpreted with today's fn registry. At every revision the replayed document becomes
//! rows (positions by §10.6), the rows must export that document back byte for byte, and the
//! revision's logged changes are the difference from the previous rows. Nothing but the
//! database is read: no fn manifest, recipe file or `.env`.
//!
//! Preconditions are the same wherever it runs (`sluice home migrate`, a restore's private
//! copy) and have no switch: format 1, schema 1 or 2, sluice's identity, 23 tables, integrity
//! and foreign keys clean, **no live work** (no attempt not terminal, run not finished, lease
//! waiting or held, or call running), and every project's history complete and replayable.
//! A blocker is reported, never deleted or rewritten to pass.

use crate::{
    Result, StoreError,
    cost::{self, Counter},
    plans::{insert_edge, insert_reference},
    schema::{APPLICATION_ID, FORMAT_MAJOR, SCHEMA, SCHEMA_VERSION},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use sluice_model::{
    ids::{ProjectId, Revision},
    plan_index::{output_references, plan_edges, step_index},
    plan_rows::{PlanChange, PlanRows},
    rpc::{JsonMap, JsonValue},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    time::Duration,
};

/// The schema-1 columns added after homes existed (schema 1's `ADDED_COLUMNS`, less the
/// retired `projects.board_slots`): an older home or backup lacking some gains them before it
/// converts, so every home converts to the same shape.
const LEGACY_COLUMNS: &[(&str, &str, &str)] = &[
    ("projects", "board", "TEXT"),
    (
        "projects",
        "board_rev",
        "INTEGER NOT NULL DEFAULT 0 CHECK (board_rev >= 0)",
    ),
    ("projects", "board_doc", "TEXT"),
    (
        "projects",
        "board_doc_rev",
        "INTEGER NOT NULL DEFAULT 0 CHECK (board_doc_rev >= 0)",
    ),
    ("projects", "board_doc_at", "TEXT"),
    ("projects", "board_doc_author", "TEXT"),
    (
        "steps",
        "progress",
        "TEXT CHECK (progress IS NULL OR json_type(progress) = 'object')",
    ),
    ("steps", "progress_at", "TEXT"),
    ("steps", "progress_run", "TEXT"),
    (
        "projects",
        "prune_done_after",
        "INTEGER CHECK (prune_done_after IS NULL OR prune_done_after > 0)",
    ),
    (
        "projects",
        "prune_keep",
        "TEXT CHECK (prune_keep IS NULL OR json_type(prune_keep) = 'array')",
    ),
    ("messages", "read_at", "TEXT"),
    (
        "runs",
        "stopped",
        "TEXT CHECK (stopped IS NULL OR json_type(stopped) = 'object')",
    ),
];
/// The tables schema 3 replaces whole; every other table keeps its rows in place.
const REBUILT: [&str; 3] = ["plans", "plan_edits", "steps"];
/// The tables schema 3 adds.
const ADDED: [&str; 4] = ["plan_outputs", "plan_refs", "plan_edges", "step_tags"];

/// Live work in a home: what the drain counts as blockers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LiveWork {
    /// Attempts not terminal.
    pub attempts: u64,
    /// Runs not finished.
    pub runs: u64,
    /// Leases waiting or held.
    pub leases: u64,
    /// Calls running (pending calls have no process and are not live).
    pub calls: u64,
}
impl LiveWork {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
/// Count a home's live work (any schema: these tables are schema 1's too).
pub fn live_work(sql: &Connection) -> Result<LiveWork> {
    let count = |query: &str| -> Result<u64> {
        Ok(sql.query_row(query, [], |r| r.get::<_, i64>(0))?.max(0) as u64)
    };
    Ok(LiveWork {
        attempts: count("SELECT count(*) FROM attempts WHERE phase<>'terminal'")?,
        runs: count("SELECT count(*) FROM runs WHERE finished_at IS NULL")?,
        leases: count("SELECT count(*) FROM leases WHERE state IN ('waiting','held')")?,
        calls: count("SELECT count(*) FROM calls WHERE status='running'")?,
    })
}

/// What a conversion did (`sluice home migrate --json` prints it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionReport {
    pub from_schema: i64,
    pub projects: Vec<ProjectConversion>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectConversion {
    pub project_id: ProjectId,
    pub name: String,
    /// Plan revisions replayed and converted (`1 … rev`).
    pub revisions: u64,
    pub steps: u64,
    pub inputs: u64,
    pub outputs: u64,
    /// The project's records rewritten to payload version 2.
    pub records_rewritten: u64,
    /// Attempts whose completion snapshot was removed.
    pub attempt_snapshots_removed: u64,
}
impl ConversionReport {
    /// `{from_schema, projects: [{project_id, name, revisions, steps, inputs, outputs,
    /// records_rewritten, attempt_snapshots_removed}], warnings}`.
    pub fn to_json(&self) -> Value {
        json!({
            "from_schema": self.from_schema,
            "projects": self.projects.iter().map(|p| json!({
                "project_id": p.project_id.to_string(),
                "name": p.name,
                "revisions": p.revisions,
                "steps": p.steps,
                "inputs": p.inputs,
                "outputs": p.outputs,
                "records_rewritten": p.records_rewritten,
                "attempt_snapshots_removed": p.attempt_snapshots_removed,
            })).collect::<Vec<_>>(),
            "warnings": self.warnings,
        })
    }
}

/// One project's converted plan: its final rows and every revision's changes.
struct Converted {
    project: ProjectId,
    name: String,
    rows: PlanRows,
    revisions: Vec<RevisionRow>,
}
struct RevisionRow {
    rev: i64,
    seq: i64,
    at: String,
    author: String,
    reason: String,
    changes: Vec<PlanChange>,
}

/// Convert the database at `database` (schema 1 or 2) to schema 3 in one transaction, then
/// check its integrity and foreign keys. Any failed precondition (`ConversionBlocked`, each
/// blocker naming the project, the check and the revision) or error leaves the file as it was.
pub fn convert_home(database: &Path) -> Result<ConversionReport> {
    let mut connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    // SQLite's procedure for changing a table's shape: foreign keys off for the transaction
    // (a parent is replaced while its children keep their rows), checked whole before commit.
    connection.pragma_update(None, "foreign_keys", false)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let from_schema = preconditions(&tx)?;
    add_legacy_columns(&tx)?;
    let (converted, plan_records, blocked, mut blockers) = replay_projects(&tx)?;
    let edits: HashMap<(String, i64), &RevisionRow> = converted
        .iter()
        .flat_map(|plan| {
            plan.revisions
                .iter()
                .map(move |row| ((plan.project.to_string(), row.rev), row))
        })
        .collect();
    let names: HashMap<String, &str> = converted
        .iter()
        .map(|plan| (plan.project.to_string(), plan.name.as_str()))
        .collect();
    let mut warnings = vec![];
    let mut rewrites = vec![];
    // A blocked project's records are not checked: its own blocker already names it.
    for record in plan_records
        .iter()
        .filter(|r| !blocked.contains(&r.project))
    {
        let name = names.get(&record.project).copied().unwrap_or("?");
        match edits.get(&(record.project.clone(), record.rev)) {
            None => blockers.push(format!(
                "project {name} ({}): plan.edit record {} at rev {}: no plan_edits row for its revision",
                record.project, record.seq, record.rev
            )),
            Some(row) => {
                if row.author != record.author || row.reason != record.reason {
                    warnings.push(format!(
                        "project {name} ({}): plan.edit record {} at rev {}: its author or reason differs from the history row; the record keeps its own",
                        record.project, record.seq, record.rev
                    ));
                }
                rewrites.push((record, &row.changes));
            }
        }
    }
    if !blockers.is_empty() {
        return Err(StoreError::ConversionBlocked(blockers));
    }
    let fresh = fresh_schema()?;
    // Views and triggers name tables by text: they go first and come back from schema 3.
    for view in names_of(&tx, "view")? {
        tx.execute_batch(&format!("DROP VIEW \"{view}\""))?;
    }
    for trigger in names_of(&tx, "trigger")? {
        tx.execute_batch(&format!("DROP TRIGGER \"{trigger}\""))?;
    }
    // Renaming in legacy mode leaves the children's foreign keys naming the original table,
    // so they bind to its replacement.
    tx.pragma_update(None, "legacy_alter_table", true)?;
    for table in REBUILT {
        tx.execute_batch(&format!("ALTER TABLE {table} RENAME TO {table}_v1"))?;
    }
    for ((kind, table), sql) in &fresh.objects {
        if kind == "table" && !REBUILT.contains(&table.as_str()) && !ADDED.contains(&table.as_str())
        {
            rebuild_if_drifted(&tx, &fresh.connection, table, sql)?;
        }
    }
    tx.pragma_update(None, "legacy_alter_table", false)?;
    for table in REBUILT.iter().chain(&ADDED) {
        tx.execute_batch(fresh.sql("table", table)?)?;
    }
    let mut report = ConversionReport {
        from_schema,
        projects: vec![],
        warnings,
    };
    tx.execute_batch(
        "CREATE TEMP TABLE conv_steps(project_id TEXT NOT NULL, step_id TEXT NOT NULL,
           position INTEGER NOT NULL, unit TEXT NOT NULL, declaration TEXT NOT NULL,
           PRIMARY KEY (project_id, step_id))",
    )?;
    for plan in &converted {
        write_plan(&tx, plan)?;
        report.projects.push(ProjectConversion {
            project_id: plan.project,
            name: plan.name.clone(),
            revisions: plan.revisions.len() as u64,
            steps: plan.rows.steps.len() as u64,
            inputs: plan.rows.inputs.len() as u64,
            outputs: plan.rows.outputs.len() as u64,
            records_rewritten: 0,
            attempt_snapshots_removed: 0,
        });
    }
    let old_steps: i64 = tx.query_row("SELECT count(*) FROM steps_v1", [], |r| r.get(0))?;
    let copied = tx.execute(
        "INSERT INTO steps(project_id,step_id,position,generation,work_generation,declaration,
           status,unit,paused,run,priority,needs,outputs,error,skipped,manual,inputs_hash,
           result_id,run_ids,instances,total,done,delivery_cursor,progress,progress_at,progress_run)
         SELECT o.project_id,o.step_id,c.position,o.generation,o.work_generation,c.declaration,
           o.status,c.unit,
           CASE json_type(c.declaration,'$.paused') WHEN 'true' THEN 'true'
             WHEN 'text' THEN json_quote(json_extract(c.declaration,'$.paused')) ELSE 'false' END,
           json_extract(c.declaration,'$.run'),coalesce(json_extract(c.declaration,'$.priority'),0),
           json_extract(c.declaration,'$.needs'),
           o.outputs,o.error,o.skipped,o.manual,o.inputs_hash,o.result_id,o.run_ids,o.instances,
           o.total,o.done,o.delivery_cursor,o.progress,o.progress_at,o.progress_run
         FROM steps_v1 o JOIN temp.conv_steps c ON c.project_id=o.project_id AND c.step_id=o.step_id",
        [],
    )?;
    if copied as i64 != old_steps {
        return Err(StoreError::ConversionBlocked(vec![format!(
            "steps: {} step rows belong to no converted plan",
            old_steps - copied as i64
        )]));
    }
    for plan in &converted {
        write_indexes(&tx, plan)?;
    }
    tx.execute_batch(
        "DROP TABLE temp.conv_steps; DROP TABLE plans_v1; DROP TABLE plan_edits_v1; DROP TABLE steps_v1;",
    )?;
    // Records: every payload at version 2, each plan.edit's ops replaced by its changes.
    for (record, changes) in rewrites {
        let mut payload: serde_json::Map<String, Value> = serde_json::from_str(&record.payload)?;
        payload.remove("ops");
        payload.insert("changes".into(), serde_json::to_value(changes)?);
        tx.execute(
            "UPDATE records SET payload=?2 WHERE seq=?1",
            params![record.seq, Value::Object(payload).to_string()],
        )?;
    }
    let mut rewritten: HashMap<String, u64> = HashMap::new();
    {
        let mut statement = tx.prepare(
            "SELECT project_id,count(*) FROM records WHERE payload_version<>?1 GROUP BY project_id",
        )?;
        let mut rows = statement.query([crate::schema::RECORD_PAYLOAD_VERSION])?;
        while let Some(row) = rows.next()? {
            if let Some(project) = row.get::<_, Option<String>>(0)? {
                rewritten.insert(project, row.get::<_, i64>(1)?.max(0) as u64);
            }
        }
    }
    tx.execute(
        "UPDATE records SET payload_version=?1 WHERE payload_version<>?1",
        [crate::schema::RECORD_PAYLOAD_VERSION],
    )?;
    let removed = remove_snapshots(&tx, &mut report.warnings)?;
    for project in &mut report.projects {
        let id = project.project_id.to_string();
        project.records_rewritten = rewritten.get(&id).copied().unwrap_or(0);
        project.attempt_snapshots_removed = removed.get(&id).copied().unwrap_or(0);
    }
    for kind in ["index", "trigger", "view"] {
        for (name, sql) in &fresh.objects {
            if name.0 == kind && !object_exists(&tx, kind, &name.1)? {
                tx.execute_batch(sql)?;
            }
        }
    }
    tx.execute(
        "UPDATE home_meta SET schema_version=?1 WHERE singleton=1",
        [SCHEMA_VERSION],
    )?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    let differences = schema_differences(&fresh.connection, &tx)?;
    if !differences.is_empty() {
        return Err(StoreError::ConversionBlocked(
            differences
                .into_iter()
                .map(|d| format!("schema: {d}"))
                .collect(),
        ));
    }
    clean(&tx)?;
    tx.commit()?;
    clean(&connection)?;
    Ok(report)
}

/// A table whose columns differ from schema 3's (`projects` keeps a retired column, or a
/// home gained its added columns in another order) is replaced by schema 3's definition
/// holding the same rows: the columns both have are copied, and an AUTOINCREMENT counter
/// keeps its value.
fn rebuild_if_drifted(
    sql: &Connection,
    fresh: &Connection,
    table: &str,
    definition: &str,
) -> Result<()> {
    let columns = |connection: &Connection| -> Result<Vec<String>> {
        Ok(connection
            .prepare(
                "SELECT name||' '||type||' '||\"notnull\"||' '||quote(dflt_value)||' '||pk||' '||hidden
                 FROM pragma_table_xinfo(?1) ORDER BY cid",
            )?
            .query_map([table], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    };
    if columns(sql)? == columns(fresh)? {
        return Ok(());
    }
    let names = |connection: &Connection| -> Result<Vec<String>> {
        Ok(connection
            .prepare("SELECT name FROM pragma_table_xinfo(?1) ORDER BY cid")?
            .query_map([table], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    };
    let old = names(sql)?;
    let common: Vec<String> = names(fresh)?
        .into_iter()
        .filter(|name| old.contains(name))
        .map(|name| format!("\"{name}\""))
        .collect();
    let counter: Option<i64> = if object_exists(sql, "table", "sqlite_sequence")? {
        sql.query_row(
            "SELECT seq FROM sqlite_sequence WHERE name=?1",
            [table],
            |r| r.get(0),
        )
        .optional()?
    } else {
        None
    };
    sql.execute_batch(&format!("ALTER TABLE {table} RENAME TO {table}_v1"))?;
    sql.execute_batch(definition)?;
    let common = common.join(",");
    sql.execute_batch(&format!(
        "INSERT INTO {table}({common}) SELECT {common} FROM {table}_v1; DROP TABLE {table}_v1;"
    ))?;
    if let Some(counter) = counter {
        sql.execute(
            "DELETE FROM sqlite_sequence WHERE name IN (?1, ?1||'_v1')",
            [table],
        )?;
        sql.execute(
            "INSERT INTO sqlite_sequence(name,seq) VALUES (?1,?2)",
            params![table, counter],
        )?;
    }
    Ok(())
}

/// §10.3's home-wide preconditions, read inside the conversion's transaction.
fn preconditions(sql: &Connection) -> Result<i64> {
    let has_meta: bool = sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='home_meta')",
        [],
        |r| r.get(0),
    )?;
    if !has_meta {
        return Err(StoreError::InvalidDatabase("home_meta is missing".into()));
    }
    let (major, schema): (i64, i64) = sql.query_row(
        "SELECT format_major,schema_version FROM home_meta WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if major != FORMAT_MAJOR {
        return Err(StoreError::UnsupportedFormat { found: major });
    }
    if schema == SCHEMA_VERSION {
        return Err(StoreError::ConversionBlocked(vec![
            "home: already at schema 3".into(),
        ]));
    }
    if !matches!(schema, 1 | 2) {
        return Err(StoreError::UnsupportedSchema { found: schema });
    }
    let user_version: i64 = sql.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if user_version != schema {
        return Err(StoreError::UnsupportedSchema {
            found: user_version,
        });
    }
    let app: i64 = sql.pragma_query_value(None, "application_id", |r| r.get(0))?;
    if app != APPLICATION_ID {
        return Err(StoreError::InvalidDatabase(
            "application_id mismatch".into(),
        ));
    }
    let tables: i64 = sql.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if tables != 23 {
        return Err(StoreError::InvalidDatabase(format!(
            "expected 23 tables, found {tables}"
        )));
    }
    clean(sql)?;
    let live = live_work(sql)?;
    if !live.is_empty() {
        return Err(StoreError::ConversionBlocked(vec![format!(
            "home: live work ({} attempts, {} runs, {} leases, {} calls); drain the home and end its runs first",
            live.attempts, live.runs, live.leases, live.calls
        )]));
    }
    Ok(schema)
}

/// `integrity_check` says ok and `foreign_key_check` finds nothing.
fn clean(sql: &Connection) -> Result<()> {
    let verdicts: Vec<String> = sql
        .prepare("PRAGMA integrity_check")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if verdicts != ["ok"] {
        return Err(StoreError::InvalidDatabase(format!(
            "integrity_check: {}",
            verdicts.join("; ")
        )));
    }
    if sql
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(StoreError::InvalidDatabase(
            "foreign_key_check failed".into(),
        ));
    }
    Ok(())
}

fn column_exists(sql: &Connection, table: &str, column: &str) -> Result<bool> {
    Ok(sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
        [table, column],
        |r| r.get(0),
    )?)
}
fn add_legacy_columns(sql: &Connection) -> Result<()> {
    for (table, column, definition) in LEGACY_COLUMNS {
        if !column_exists(sql, table, column)? {
            sql.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))?;
        }
    }
    Ok(())
}
fn names_of(sql: &Connection, kind: &str) -> Result<Vec<String>> {
    Ok(sql
        .prepare("SELECT name FROM sqlite_schema WHERE type=?1 ORDER BY name")?
        .query_map([kind], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
fn object_exists(sql: &Connection, kind: &str, name: &str) -> Result<bool> {
    Ok(sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type=?1 AND name=?2)",
        [kind, name],
        |r| r.get(0),
    )?)
}

/// A fresh schema-3 database in memory: the objects a converted home must end up with.
struct Fresh {
    connection: Connection,
    /// `((type, name), sql)` of every object with SQL, in creation order.
    objects: Vec<((String, String), String)>,
}
impl Fresh {
    fn sql(&self, kind: &str, name: &str) -> Result<&str> {
        self.objects
            .iter()
            .find(|((k, n), _)| k == kind && n == name)
            .map(|(_, sql)| sql.as_str())
            .ok_or_else(|| StoreError::InvalidDatabase(format!("schema 3 has no {kind} {name}")))
    }
}
fn fresh_schema() -> Result<Fresh> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(SCHEMA)?;
    let objects = connection
        .prepare("SELECT type,name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY rowid")?
        .query_map([], |r| Ok(((r.get(0)?, r.get(1)?), r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Fresh {
        connection,
        objects,
    })
}

/// Everything §2.1's schema equivalence compares, as lines: each table's columns
/// (`pragma_table_xinfo`: name, type, not-null, default, primary key, hidden) and indexes
/// (`pragma_index_list` and `pragma_index_xinfo`), and each trigger's and view's SQL.
pub fn schema_fingerprint(sql: &Connection) -> Result<Vec<String>> {
    let mut lines = vec![];
    for table in sql
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        let mut columns = sql.prepare(
            "SELECT cid,name,type,\"notnull\",quote(dflt_value),pk,hidden FROM pragma_table_xinfo(?1) ORDER BY cid",
        )?;
        let mut rows = columns.query([&table])?;
        while let Some(row) = rows.next()? {
            lines.push(format!(
                "table {table} column {} {} {} notnull={} default={} pk={} hidden={}",
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?
            ));
        }
        let indexes: Vec<(String, i64, String, i64)> = sql
            .prepare("SELECT name,\"unique\",origin,partial FROM pragma_index_list(?1) ORDER BY name")?
            .query_map([&table], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (index, unique, origin, partial) in indexes {
            let mut keys = vec![];
            let mut columns = sql.prepare(
                "SELECT seqno,cid,quote(name),desc,coll,key FROM pragma_index_xinfo(?1) ORDER BY seqno",
            )?;
            let mut rows = columns.query([&index])?;
            while let Some(row) = rows.next()? {
                keys.push(format!(
                    "{}:{}:{}:{}:{}:{}",
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    row.get::<_, i64>(5)?
                ));
            }
            let definition: Option<String> = sql
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type='index' AND name=?1",
                    [&index],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            lines.push(format!(
                "table {table} index {index} unique={unique} origin={origin} partial={partial} [{}] {}",
                keys.join(" "),
                definition.unwrap_or_default()
            ));
        }
    }
    for kind in ["trigger", "view"] {
        let mut statement =
            sql.prepare("SELECT name,tbl_name,sql FROM sqlite_schema WHERE type=?1 ORDER BY name")?;
        let mut rows = statement.query([kind])?;
        while let Some(row) = rows.next()? {
            lines.push(format!(
                "{kind} {} on {}: {}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?.unwrap_or_default()
            ));
        }
    }
    Ok(lines)
}
/// The lines one schema has that the other lacks (`-` fresh only, `+` converted only).
fn schema_differences(fresh: &Connection, converted: &Connection) -> Result<Vec<String>> {
    let fresh = schema_fingerprint(fresh)?;
    let converted = schema_fingerprint(converted)?;
    let in_fresh: HashSet<&String> = fresh.iter().collect();
    let in_converted: HashSet<&String> = converted.iter().collect();
    Ok(fresh
        .iter()
        .filter(|line| !in_converted.contains(line))
        .map(|line| format!("- {line}"))
        .chain(
            converted
                .iter()
                .filter(|line| !in_fresh.contains(line))
                .map(|line| format!("+ {line}")),
        )
        .collect())
}

/// A retained `plan.edit` record.
struct PlanRecord {
    seq: i64,
    project: String,
    rev: i64,
    author: String,
    reason: String,
    payload: String,
}

/// Replay every project's history (§10.4 steps 1 to 5): the converted plans, the retained
/// `plan.edit` records, the blocked projects' ids and their blockers.
#[allow(clippy::type_complexity)]
fn replay_projects(
    sql: &Connection,
) -> Result<(
    Vec<Converted>,
    Vec<PlanRecord>,
    HashSet<String>,
    Vec<String>,
)> {
    let plans: Vec<(String, i64, String, String)> = sql
        .prepare(
            "SELECT p.project_id,p.rev,p.doc,coalesce(r.name,'') FROM plans p
             LEFT JOIN projects r ON r.project_id=p.project_id ORDER BY r.name,p.project_id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut converted = vec![];
    let mut blocked = HashSet::new();
    let mut blockers = vec![];
    for (project, rev, document, name) in plans {
        let label = format!("project {name} ({project})");
        match replay_project(sql, &project, rev, &document) {
            Ok((rows, revisions)) => converted.push(Converted {
                project: project
                    .parse()
                    .map_err(|_| StoreError::InvalidDatabase("invalid ProjectId".into()))?,
                name,
                rows,
                revisions,
            }),
            Err(Blocker(blocker)) => {
                blockers.push(format!("{label}: {blocker}"));
                blocked.insert(project);
            }
        }
    }
    let records = sql
        .prepare(
            "SELECT seq,coalesce(project_id,''),payload FROM records WHERE kind='plan.edit' ORDER BY seq",
        )?
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .map(|(seq, project, payload)| {
            let value: Value = serde_json::from_str(&payload)?;
            Ok(PlanRecord {
                seq,
                project,
                rev: value["rev"].as_i64().unwrap_or(0),
                author: value["author"].as_str().unwrap_or_default().to_owned(),
                reason: value["reason"].as_str().unwrap_or_default().to_owned(),
                payload,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((converted, records, blocked, blockers))
}

/// A precondition a project fails: the check and its revision.
struct Blocker(String);

/// One project's replay: every revision's rows checked against its document, the final
/// document against `plans.doc`, and the step and input rows against it.
fn replay_project(
    sql: &Connection,
    project: &str,
    rev: i64,
    stored: &str,
) -> std::result::Result<(PlanRows, Vec<RevisionRow>), Blocker> {
    let fail = |message: String| Blocker(message);
    let storage = |error: rusqlite::Error| Blocker(format!("history unreadable: {error}"));
    let edits: Vec<(i64, i64, String, String, String, String)> = sql
        .prepare(
            "SELECT rev,seq,at,author,reason,ops FROM plan_edits WHERE project_id=?1 ORDER BY rev",
        )
        .map_err(storage)?
        .query_map([project], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .map_err(storage)?
        .collect::<rusqlite::Result<_>>()
        .map_err(storage)?;
    // 1: history is complete and starts at the initializer's origin.
    for (index, (found, ..)) in edits.iter().enumerate() {
        let expected = index as i64 + 1;
        if *found != expected {
            return Err(fail(format!(
                "history is incomplete at rev {expected}: unknown origin (revisions must be 1 to {rev})"
            )));
        }
    }
    if edits.len() as i64 != rev {
        return Err(fail(format!(
            "history is incomplete at rev {}: unknown origin (revisions must be 1 to {rev})",
            edits.len() + 1
        )));
    }
    let mut document = json!({"steps": {}});
    let mut previous: Option<PlanRows> = None;
    let mut revisions = vec![];
    for (rev, seq, at, author, reason, ops) in edits {
        let ops: Value = serde_json::from_str(&ops)
            .map_err(|e| fail(format!("rev {rev}: its ops are not JSON: {e}")))?;
        let ops = ops
            .as_array()
            .ok_or_else(|| fail(format!("rev {rev}: its ops are not a list")))?;
        if rev == 1 && !ops.is_empty() {
            return Err(fail(
                "rev 1: unknown origin (the project was not created empty)".into(),
            ));
        }
        // 2: the RFC 6902 operations with the legacy semantics, no validation.
        legacy_patch(&mut document, ops).map_err(|e| fail(format!("rev {rev}: {e}")))?;
        let replayed = strict_map(&document).map_err(|e| {
            fail(format!(
                "rev {rev}: the replayed plan is not strict JSON: {e}"
            ))
        })?;
        // 3: rows of this revision, positioned against the previous one's, export it back.
        let mut rows = PlanRows::from_document(&replayed, previous.as_ref())
            .map_err(|e| fail(format!("rev {rev}: the replayed plan has no rows: {e}")))?;
        rows.header.rev = Revision(rev as u64);
        if compact(&rows.to_document()) != compact(&replayed) {
            return Err(fail(format!(
                "rev {rev}: the rows do not export the replayed plan"
            )));
        }
        let changes = rows.changes_from(previous.as_ref());
        if let Some(previous) = &previous {
            cost::add(Counter::PositionsRenumbered, moved(previous, &rows));
        }
        revisions.push(RevisionRow {
            rev,
            seq,
            at,
            author,
            reason,
            changes,
        });
        previous = Some(rows);
    }
    let rows = previous.ok_or_else(|| fail("history is empty: unknown origin".into()))?;
    // 4: the final document is the stored plan.
    let stored: JsonMap = serde_json::from_str(stored).map_err(|e| {
        fail(format!(
            "rev {rev}: the stored plan is not strict JSON: {e}"
        ))
    })?;
    let exported = rows.to_document();
    if compact(&exported) != compact(&stored) {
        return Err(fail(format!(
            "rev {rev}: the replayed plan differs from the stored plan"
        )));
    }
    // 5: the step and input rows are the plan's.
    let steps: Vec<(String, String)> = sql
        .prepare("SELECT step_id,declaration FROM steps WHERE project_id=?1")
        .map_err(storage)?
        .query_map([project], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(storage)?
        .collect::<rusqlite::Result<_>>()
        .map_err(storage)?;
    let declared: HashMap<&str, &JsonMap> = rows
        .steps
        .iter()
        .map(|row| (row.step.as_str(), &row.declaration))
        .collect();
    if steps.len() != declared.len() {
        return Err(fail(format!(
            "rev {rev}: {} step rows for {} steps in the plan",
            steps.len(),
            declared.len()
        )));
    }
    for (step, declaration) in &steps {
        let Some(expected) = declared.get(step.as_str()) else {
            return Err(fail(format!(
                "rev {rev}: step row {step} is not in the plan"
            )));
        };
        let found: JsonMap = serde_json::from_str(declaration)
            .map_err(|e| fail(format!("rev {rev}: step row {step}: {e}")))?;
        if compact(&found) != compact(expected) {
            return Err(fail(format!(
                "rev {rev}: step row {step}'s declaration differs from the plan's"
            )));
        }
    }
    let inputs: HashSet<String> = sql
        .prepare("SELECT name FROM inputs WHERE project_id=?1")
        .map_err(storage)?
        .query_map([project], |r| r.get(0))
        .map_err(storage)?
        .collect::<rusqlite::Result<_>>()
        .map_err(storage)?;
    let names: HashSet<String> = rows.inputs.iter().map(|row| row.name.clone()).collect();
    if inputs != names {
        return Err(fail(format!(
            "rev {rev}: the input rows are not the plan's inputs"
        )));
    }
    Ok((rows, revisions))
}

/// Rows that stay but move between two revisions' rows.
fn moved(before: &PlanRows, after: &PlanRows) -> u64 {
    let positions = |rows: Vec<(String, u64)>| rows.into_iter().collect::<HashMap<_, _>>();
    let pairs = [
        (
            positions(
                before
                    .inputs
                    .iter()
                    .map(|r| (r.name.clone(), r.position))
                    .collect(),
            ),
            positions(
                after
                    .inputs
                    .iter()
                    .map(|r| (r.name.clone(), r.position))
                    .collect(),
            ),
        ),
        (
            positions(
                before
                    .outputs
                    .iter()
                    .map(|r| (r.name.clone(), r.position))
                    .collect(),
            ),
            positions(
                after
                    .outputs
                    .iter()
                    .map(|r| (r.name.clone(), r.position))
                    .collect(),
            ),
        ),
        (
            positions(
                before
                    .steps
                    .iter()
                    .map(|r| (r.step.to_string(), r.position))
                    .collect(),
            ),
            positions(
                after
                    .steps
                    .iter()
                    .map(|r| (r.step.to_string(), r.position))
                    .collect(),
            ),
        ),
    ];
    pairs
        .iter()
        .map(|(before, after)| {
            after
                .iter()
                .filter(|(key, position)| before.get(*key).is_some_and(|p| p != *position))
                .count() as u64
        })
        .sum()
}

fn compact<T: serde::Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}
fn strict_map(value: &Value) -> std::result::Result<JsonMap, String> {
    match value {
        Value::Object(map) => Ok(JsonMap(
            map.iter()
                .map(|(key, value)| {
                    JsonValue::try_from(value.clone())
                        .map(|value| (key.clone(), value))
                        .map_err(|e| e.to_string())
                })
                .collect::<std::result::Result<_, _>>()?,
        )),
        _ => Err("the plan is not an object".into()),
    }
}

/// `Plan::patch`'s semantics (schema 1), without validation: each operation applied alone,
/// then the key order of every map above its path (and above `from` for a move) restored,
/// keys the map did not have after them; a replacement subtree keeps its supplied order.
fn legacy_patch(document: &mut Value, ops: &[Value]) -> std::result::Result<(), String> {
    for (index, operation) in ops.iter().enumerate() {
        let path = operation["path"]
            .as_str()
            .ok_or_else(|| format!("ops[{index}]: no path"))?;
        let mut orders = ancestor_orders(document, path);
        if operation["op"] == "move" {
            let from = operation["from"]
                .as_str()
                .ok_or_else(|| format!("ops[{index}]: a move with no from"))?;
            orders.extend(ancestor_orders(document, from));
        }
        let patch: json_patch::Patch =
            serde_json::from_value(Value::Array(vec![operation.clone()]))
                .map_err(|e| format!("ops[{index}]: {e}"))?;
        json_patch::patch(document, &patch)
            .map_err(|e| format!("ops[{index}]: the patch does not apply: {e}"))?;
        for (parent, keys) in orders {
            restore_order(document, &parent, &keys);
        }
    }
    Ok(())
}
/// The key order of every map above `path`, outermost first.
fn ancestor_orders(value: &Value, path: &str) -> Vec<(String, Vec<String>)> {
    path.match_indices('/')
        .filter_map(|(offset, _)| {
            let parent = &path[..offset];
            let map = value.pointer(parent)?.as_object()?;
            Some((parent.to_owned(), map.keys().cloned().collect()))
        })
        .collect()
}
/// Put the map at `parent` back in `keys` order, keys it did not have after them.
fn restore_order(value: &mut Value, parent: &str, keys: &[String]) {
    if let Some(new) = value.pointer_mut(parent).and_then(Value::as_object_mut) {
        let known: HashSet<&str> = keys.iter().map(String::as_str).collect();
        let added: Vec<String> = new
            .keys()
            .filter(|key| !known.contains(key.as_str()))
            .cloned()
            .collect();
        let mut remaining = std::mem::take(new);
        for key in keys.iter().chain(&added) {
            if let Some(value) = remaining.swap_remove(key) {
                new.insert(key.clone(), value);
            }
        }
    }
}

/// §10.4 step 6's authored rows for one project: `plans`, its history rows, the input
/// declarations and positions (moved above the maximum first), the plan outputs, and the
/// steps' final positions, units and declarations for the copy.
fn write_plan(sql: &Connection, plan: &Converted) -> Result<()> {
    let id = plan.project.to_string();
    let rev = plan.revisions.len() as i64;
    sql.execute(
        "INSERT INTO plans(project_id,rev,root_order,state_epoch) VALUES (?1,?2,?3,0)",
        params![id, rev, compact(&plan.rows.header.root_order)],
    )?;
    {
        let mut insert = sql.prepare_cached(
            "INSERT INTO plan_edits(project_id,rev,seq,at,author,reason,changes) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        )?;
        for row in &plan.revisions {
            insert.execute(params![
                id,
                row.rev,
                row.seq,
                row.at,
                row.author,
                row.reason,
                compact(&row.changes)
            ])?;
        }
    }
    sql.execute(
        "UPDATE inputs SET position=position+(SELECT coalesce(max(position),0)+1 FROM inputs WHERE project_id=?1)+?2
         WHERE project_id=?1",
        params![id, plan.rows.inputs.len() as i64],
    )?;
    for input in &plan.rows.inputs {
        sql.execute(
            "UPDATE inputs SET position=?3,declaration=?4 WHERE project_id=?1 AND name=?2",
            params![
                id,
                input.name,
                input.position as i64,
                compact(&input.declaration)
            ],
        )?;
    }
    for output in &plan.rows.outputs {
        sql.execute(
            "INSERT INTO plan_outputs(project_id,name,position,binding) VALUES (?1,?2,?3,?4)",
            params![
                id,
                output.name,
                output.position as i64,
                compact(&output.binding)
            ],
        )?;
    }
    let ids: HashSet<&str> = plan
        .rows
        .steps
        .iter()
        .map(|row| row.step.as_str())
        .collect();
    let mut insert = sql.prepare_cached(
        "INSERT INTO temp.conv_steps(project_id,step_id,position,unit,declaration) VALUES (?1,?2,?3,?4,?5)",
    )?;
    for row in &plan.rows.steps {
        let index = step_index(&row.step, &row.declaration, &|name| ids.contains(name));
        insert.execute(params![
            id,
            row.step.as_str(),
            row.position as i64,
            index.unit.as_str(),
            compact(&row.declaration)
        ])?;
    }
    Ok(())
}

/// §10.4 step 6's index rows for one project: `step_tags`, `plan_refs` and `plan_edges`.
fn write_indexes(sql: &Connection, plan: &Converted) -> Result<()> {
    let id = plan.project.to_string();
    let ids: HashSet<&str> = plan
        .rows
        .steps
        .iter()
        .map(|row| row.step.as_str())
        .collect();
    let mut tag =
        sql.prepare_cached("INSERT INTO step_tags(project_id,step_id,tag) VALUES (?1,?2,?3)")?;
    for row in &plan.rows.steps {
        let index = step_index(&row.step, &row.declaration, &|name| ids.contains(name));
        for name in &index.tags {
            tag.execute(params![id, row.step.as_str(), name])?;
        }
        for reference in &index.references {
            insert_reference(sql, plan.project, reference)?;
        }
    }
    for output in &plan.rows.outputs {
        for reference in output_references(&output.name, &output.binding) {
            insert_reference(sql, plan.project, &reference)?;
        }
    }
    for edge in plan_edges(&plan.rows) {
        insert_edge(sql, plan.project, &edge)?;
    }
    Ok(())
}

/// An attempt with a completion snapshot: its id, project, step, declaration and the
/// snapshot's declaration of its step.
type Snapshot = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// §10.4 step 8: each attempt's completion snapshot is removed from `request.provenance.runtime`
/// and `provenance.runtime`; a snapshot whose step declaration differs from the attempt's own
/// is a warning (the attempt is terminal and its result recorded). Returns removals by project.
fn remove_snapshots(sql: &Connection, warnings: &mut Vec<String>) -> Result<BTreeMap<String, u64>> {
    let mut removed = BTreeMap::new();
    let snapshots: Vec<Snapshot> = sql
        .prepare(
            "SELECT attempt_id,project_id,step_id,json_extract(request,'$.declaration'),
               CASE WHEN step_id IS NOT NULL THEN
                 json_extract(request,'$.provenance.runtime.completion.document.steps.\"'||step_id||'\"') END
             FROM attempts
             WHERE json_type(request,'$.provenance.runtime.completion') IS NOT NULL
                OR json_type(provenance,'$.runtime.completion') IS NOT NULL",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut update = sql.prepare(
        "UPDATE attempts SET request=json_remove(request,'$.provenance.runtime.completion'),
           provenance=json_remove(provenance,'$.runtime.completion') WHERE attempt_id=?1",
    )?;
    for (attempt, project, step, declaration, snapshot) in snapshots {
        let parse = |text: &Option<String>| {
            text.as_deref()
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                .map(|value| compact(&value))
        };
        if step.is_some() && parse(&declaration) != parse(&snapshot) {
            warnings.push(format!(
                "attempt {attempt} (step {}): its completion snapshot's declaration differs from the attempt's",
                step.as_deref().unwrap_or_default()
            ));
        }
        update.execute([&attempt])?;
        *removed.entry(project.unwrap_or_default()).or_insert(0) += 1;
    }
    Ok(removed)
}
