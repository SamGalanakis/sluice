//! Fresh Rust homes only. Writable opens are private to the writer actor.

use std::{fs::File, path::Path, time::Duration};

use fs4::FileExt;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use sluice_model::{error::PublicError, ids::HomeId};

pub const DATABASE_FILE: &str = "sluice.db";
pub const FORMAT_MAJOR: i64 = 1;
/// Changes only for a change that older binaries cannot read: a run's pinned `sluice` reads
/// this database itself and refuses any other version, so a new version needs a drained,
/// fenced migration with no live consumers of the old one (`convert::convert_home`). Schema 2
/// stays reserved for the historical interim board layout; schema 3 introduced normalized
/// plans, and schema 4 requires bounded execution and understands resource exhaustion.
/// Additive changes keep this version (`ADDED_COLUMNS`, `ADDED_VIEWS`).
pub const SCHEMA_VERSION: i64 = 4;
/// One record payload version per home: every current record is written at version 2.
pub const RECORD_PAYLOAD_VERSION: i64 = 2;
pub(crate) const APPLICATION_ID: i64 = 0x534c5543;
/// The complete schema-4 DDL a fresh home is created from.
pub(crate) const SCHEMA: &str = include_str!("../migrations/0004.sql");
/// The tables a schema-4 home has (`sqlite_%` excluded).
pub(crate) const TABLES: i64 = 27;
/// Columns added after schema 4 homes exist. Earlier additions are folded into its DDL.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "projects",
        "check_age_seconds",
        "INTEGER NOT NULL DEFAULT 7200",
    ),
    ("runs", "check_notified", "INTEGER NOT NULL DEFAULT 0"),
    ("runs", "failure_class", "TEXT"),
    ("runs", "agent_engine", "TEXT"),
    ("runs", "agent_model", "TEXT"),
    ("projects", "history_seen", "INTEGER NOT NULL DEFAULT 0"),
    (
        "projects",
        "history_seen_previous",
        "INTEGER NOT NULL DEFAULT 0",
    ),
    ("readers", "read_undo", "TEXT"),
    ("projects", "settings_undo", "TEXT NOT NULL DEFAULT '{}'"),
    ("runs", "restart_after", "INTEGER"),
    // JSON remains authoritative, including writes from pinned older releases. Generated
    // columns and covering indexes project its small facts without reading result bodies.
    (
        "runs",
        "result_status",
        "TEXT GENERATED ALWAYS AS (json_extract(result,'$.status')) VIRTUAL",
    ),
    (
        "runs",
        "result_error",
        "TEXT GENERATED ALWAYS AS (json_extract(result,'$.error')) VIRTUAL",
    ),
    ("steps", "unit_created_rev", "INTEGER"),
    ("steps", "restart_count", "INTEGER NOT NULL DEFAULT 0"),
    ("steps", "restart_history", "TEXT NOT NULL DEFAULT '[]'"),
    // Retired progress history, retained for schema compatibility.
    (
        "steps",
        "progress_runs",
        "TEXT CHECK (progress_runs IS NULL OR json_valid(progress_runs))",
    ),
];
const ADDED_INDEXES: &[(&str, &str)] = &[
    (
        "runs_step_timing",
        "CREATE INDEX runs_step_timing ON runs(project_id,step_id,generation,created_at,run_id,item_index,work_generation,started_at,finished_at,result_status,result_error)",
    ),
    (
        "runs_day",
        "CREATE INDEX runs_day ON runs(finished_at,project_id,step_id,created_at,run_id,unit,item_index,started_at,result_status,result_error)",
    ),
    (
        "runs_step_order",
        "CREATE INDEX runs_step_order ON runs(project_id,step_id,created_at,run_id)",
    ),
    (
        "runs_project_end",
        "CREATE INDEX runs_project_end ON runs(project_id,finished_at,started_at,created_at) WHERE step_id IS NOT NULL",
    ),
    (
        "runs_step_changed",
        "CREATE INDEX runs_step_changed ON runs(project_id,step_id,coalesce(finished_at,started_at,created_at))",
    ),
    (
        "messages_sender_time",
        "CREATE INDEX messages_sender_time ON messages(project_id,\"from\",at,id)",
    ),
    (
        "records_step_status",
        "CREATE INDEX records_step_status ON records(project_id,step_id,at) WHERE kind='step.status'",
    ),
    (
        "messages_owner_notes",
        "CREATE INDEX messages_owner_notes ON messages(\"to\",needs_reply,project_id,thread,id)",
    ),
    (
        "messages_resolved",
        "CREATE INDEX messages_resolved ON messages(project_id,resolved_by) WHERE needs_reply=1",
    ),
    (
        "messages_sender",
        "CREATE INDEX messages_sender ON messages(project_id,\"from\",id,at)",
    ),
];
// These triggers also maintain the projection for older pinned writers. They never change
// authored declarations or runtime state. A unit keeps its birth while any member survives.
const ADDED_TRIGGERS: &[(&str, &str)] = &[
    (
        "steps_birth_insert",
        "CREATE TRIGGER steps_birth_insert AFTER INSERT ON steps WHEN NEW.unit_created_rev IS NULL BEGIN UPDATE steps SET unit_created_rev=coalesce((SELECT min(unit_created_rev) FROM steps WHERE project_id=NEW.project_id AND coalesce(unit,step_id)=coalesce(NEW.unit,NEW.step_id) AND step_id<>NEW.step_id),(SELECT rev+1 FROM plans WHERE project_id=NEW.project_id),0) WHERE project_id=NEW.project_id AND step_id=NEW.step_id; END",
    ),
    (
        "steps_birth_move",
        "CREATE TRIGGER steps_birth_move AFTER UPDATE OF unit ON steps WHEN coalesce(OLD.unit,OLD.step_id)<>coalesce(NEW.unit,NEW.step_id) BEGIN UPDATE steps SET unit_created_rev=coalesce((SELECT min(unit_created_rev) FROM steps WHERE project_id=NEW.project_id AND coalesce(unit,step_id)=coalesce(NEW.unit,NEW.step_id) AND step_id<>NEW.step_id),(SELECT rev+1 FROM plans WHERE project_id=NEW.project_id),0) WHERE project_id=NEW.project_id AND step_id=NEW.step_id; END",
    ),
];
/// Views added after schema 4 homes exist, as `(name, definition)`.
const ADDED_VIEWS: &[(&str, &str)] = &[("parked", "CREATE VIEW parked AS
SELECT r.project_id,r.step_id,r.run_id,
json_extract(a.request,'$.park.condition') AS condition,
json_extract(a.request,'$.park.phase') AS phase,
coalesce(json_extract(a.request,'$.park.parked_at'),json_extract(a.request,'$.park.requested_at')) AS since,
max(0,(julianday('now')-julianday(coalesce(json_extract(a.request,'$.park.parked_at'),json_extract(a.request,'$.park.requested_at'))))*86400) AS waited_seconds,
coalesce(json_extract(a.request,'$.park.timeout_seconds'),3600) AS timeout_seconds
FROM runs r JOIN attempts a USING(attempt_id)
WHERE r.finished_at IS NULL AND a.phase<>'terminal' AND json_type(a.request,'$.park')='object'")];

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("this home already has a coordinator writer")]
    WriterLocked,
    #[error("Python-format sluice database refused; import into a fresh Rust home")]
    PythonFormat,
    #[error("unsupported home format major {found}; expected {FORMAT_MAJOR}")]
    UnsupportedFormat { found: i64 },
    #[error("unsupported schema version {found}; expected {SCHEMA_VERSION}")]
    UnsupportedSchema { found: i64 },
    /// A home (or backup) at schema 1, 2 or 3: only `sluice home migrate` (or a restore's private
    /// copy) converts it; opening never does.
    #[error(
        "this home is at schema {found}; run \"sluice home migrate\" to bring it to schema {SCHEMA_VERSION}"
    )]
    MigrationRequired { found: i64 },
    /// A schema-changing restore of a backup that holds live work, refused before anything is
    /// written: the restored home's adoption would reach the original home's units.
    #[error(
        "this backup holds live work ({attempts} attempts, {runs} runs, {leases} leases, {calls} calls); a schema-changing restore needs a backup taken with nothing live"
    )]
    LiveWorkInBackup {
        attempts: u64,
        runs: u64,
        leases: u64,
        calls: u64,
    },
    /// The conversion's preconditions failed; nothing was written. Each blocker names the
    /// project, the check and the revision.
    #[error("conversion to schema {SCHEMA_VERSION} is blocked: {}", .0.join("; "))]
    ConversionBlocked(Vec<String>),
    #[error("unrecognized or incomplete sluice database: {0}")]
    InvalidDatabase(String),
    #[error("store writer is closed")]
    Closed,
    #[error("store request panicked; its transaction was rolled back")]
    RequestPanicked,
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Public(#[from] PublicError),
}

impl StoreError {
    /// No automatic retries. The caller must guarantee identical keyed requests
    /// are idempotent before passing true here.
    pub fn into_public(self, idempotent: bool) -> PublicError {
        match self {
            Self::Public(PublicError::Busy { message, .. }) => PublicError::Busy {
                message,
                retryable: idempotent,
            },
            Self::Public(error) => error,
            Self::LiveWorkInBackup {
                attempts,
                runs,
                leases,
                calls,
            } => sluice_model::plan_rows::PlanRowsError::LiveWorkInBackup {
                attempts,
                runs,
                leases,
                calls,
            }
            .into(),
            Self::ConversionBlocked(blockers) => PublicError::Invalid {
                message: format!("conversion to schema {SCHEMA_VERSION} is blocked"),
                errors: blockers,
            },
            Self::WriterLocked => PublicError::Busy {
                message: self.to_string(),
                retryable: false,
            },
            Self::Sql(rusqlite::Error::SqliteFailure(code, detail)) => {
                let message = detail.unwrap_or_else(|| code.to_string());
                match code.code {
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                        PublicError::Busy {
                            message,
                            retryable: idempotent,
                        }
                    }
                    rusqlite::ErrorCode::ConstraintViolation
                        if matches!(
                            code.extended_code,
                            rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                                | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                        ) =>
                    {
                        PublicError::Conflict {
                            message,
                            current_rev: None,
                        }
                    }
                    rusqlite::ErrorCode::ConstraintViolation => PublicError::Invalid {
                        errors: vec![message.clone()],
                        message,
                    },
                    _ => PublicError::Storage { message },
                }
            }
            error => PublicError::Storage {
                message: error.to_string(),
            },
        }
    }
}

impl From<sluice_model::plan_rows::PlanRowsError> for StoreError {
    fn from(error: sluice_model::plan_rows::PlanRowsError) -> Self {
        Self::Public(error.into())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub(crate) fn lock_home(home: &Path) -> Result<File> {
    std::fs::create_dir_all(home)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("coordinator.lock"))?;
    match FileExt::try_lock(&lock) {
        Ok(()) => Ok(lock),
        Err(fs4::TryLockError::WouldBlock) => Err(StoreError::WriterLocked),
        Err(fs4::TryLockError::Error(error)) => Err(error.into()),
    }
}

pub(crate) fn open_writer(home: &Path, timeout: Duration) -> Result<Connection> {
    let mut connection = Connection::open_with_flags(
        home.join(DATABASE_FILE),
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(timeout)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    // Inspect before changing persistent pragmas, including WAL, on a refused home.
    let tables: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    let user_version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if tables != 0 {
        verify_schema(&connection)?;
    } else if user_version != 0 {
        return Err(StoreError::UnsupportedSchema {
            found: user_version,
        });
    }
    let journal: String = connection.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    if !journal.eq_ignore_ascii_case("wal") {
        return Err(StoreError::InvalidDatabase("WAL mode unavailable".into()));
    }
    connection.pragma_update(None, "synchronous", "FULL")?;
    if tables == 0 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(SCHEMA)?;
        transaction.execute(
            "INSERT INTO home_meta(singleton, home_id, format_major, schema_version) VALUES (1,?1,?2,?3)",
            (HomeId::new().to_string(), FORMAT_MAJOR, SCHEMA_VERSION),
        )?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
    }
    conform(&mut connection)?;
    crate::messages::upgrade_dismissals(&mut connection)?;
    Ok(connection)
}

/// Add a verified home's missing `ADDED_COLUMNS` and `ADDED_VIEWS`, in one immediate
/// transaction; a home already in shape is left alone.
pub(crate) fn conform(connection: &mut Connection) -> Result<()> {
    if missing_columns(connection)?.is_empty()
        && missing_views(connection)?.is_empty()
        && missing_indexes(connection)?.is_empty()
        && missing_objects(connection, "trigger", ADDED_TRIGGERS)?.is_empty()
    {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let backfill_births = missing_columns(&transaction)?
        .iter()
        .any(|(_, column, _)| *column == "unit_created_rev");
    for (table, column, definition) in missing_columns(&transaction)? {
        transaction.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    for definition in missing_views(&transaction)? {
        transaction.execute_batch(definition)?;
    }
    if backfill_births {
        backfill_unit_births(&transaction)?;
    }
    for definition in missing_objects(&transaction, "trigger", ADDED_TRIGGERS)? {
        transaction.execute_batch(definition)?;
    }
    for definition in missing_indexes(&transaction)? {
        transaction.execute_batch(definition)?;
    }
    transaction.commit()?;
    verify(connection)
}

pub(crate) fn open_reader(home: &Path, timeout: Duration) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        home.join(DATABASE_FILE),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(timeout)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "query_only", true)?;
    verify(&connection)?;
    Ok(connection)
}

fn missing_columns(
    connection: &Connection,
) -> Result<Vec<(&'static str, &'static str, &'static str)>> {
    let mut missing = Vec::new();
    for &(table, column, definition) in ADDED_COLUMNS {
        let present: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_xinfo(?1) WHERE name=?2)",
            [table, column],
            |row| row.get(0),
        )?;
        if !present {
            missing.push((table, column, definition));
        }
    }
    Ok(missing)
}

fn missing_views(connection: &Connection) -> Result<Vec<&'static str>> {
    let mut missing = Vec::new();
    for &(name, definition) in ADDED_VIEWS {
        let present: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='view' AND name=?1)",
            [name],
            |row| row.get(0),
        )?;
        if !present {
            missing.push(definition);
        }
    }
    Ok(missing)
}

fn missing_indexes(connection: &Connection) -> Result<Vec<&'static str>> {
    missing_objects(connection, "index", ADDED_INDEXES)
}
fn missing_objects(
    connection: &Connection,
    kind: &str,
    definitions: &[(&str, &'static str)],
) -> Result<Vec<&'static str>> {
    let mut missing = Vec::new();
    for &(name, definition) in definitions {
        let present: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type=?1 AND name=?2)",
            [kind, name],
            |row| row.get(0),
        )?;
        if !present {
            missing.push(definition);
        }
    }
    Ok(missing)
}
/// Replay once, at additive migration, rather than on the first dashboard request. The
/// log is authoritative; units retain their revision across changes to their membership.
fn backfill_unit_births(sql: &Connection) -> Result<()> {
    use sluice_model::plan_rows::PlanChange;
    use std::collections::{BTreeMap, BTreeSet};
    let mut projects = sql.prepare("SELECT project_id FROM projects")?;
    let ids = projects
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut edits =
        sql.prepare("SELECT rev,changes FROM plan_edits WHERE project_id=?1 ORDER BY rev")?;
    for id in ids {
        let mut steps = BTreeMap::<String, String>::new();
        let mut births = BTreeMap::<String, i64>::new();
        let mut rows = edits.query([&id])?;
        while let Some(row) = rows.next()? {
            let rev: i64 = row.get(0)?;
            for change in serde_json::from_str::<Vec<PlanChange>>(&row.get::<_, String>(1)?)? {
                match change {
                    PlanChange::StepPut {
                        step, declaration, ..
                    } => {
                        let unit = declaration
                            .0
                            .get("tags")
                            .and_then(|t| t.as_value().as_array())
                            .into_iter()
                            .flatten()
                            .filter_map(|t| t.as_str())
                            .find_map(|t| t.strip_prefix("unit:"))
                            .unwrap_or(step.as_str());
                        steps.insert(step.to_string(), unit.to_owned());
                    }
                    PlanChange::StepDelete { step } => {
                        steps.remove(step.as_str());
                    }
                    _ => {}
                }
            }
            let units: BTreeSet<_> = steps.values().collect();
            births.retain(|unit, _| units.contains(unit));
            for unit in units {
                births.entry(unit.clone()).or_insert(rev);
            }
        }
        for (unit, rev) in births {
            sql.execute("UPDATE steps SET unit_created_rev=?3 WHERE project_id=?1 AND coalesce(unit,step_id)=?2",rusqlite::params![id,unit,rev])?;
        }
    }
    sql.execute(
        "UPDATE steps SET unit_created_rev=0 WHERE unit_created_rev IS NULL",
        [],
    )?;
    Ok(())
}

fn verify(connection: &Connection) -> Result<()> {
    verify_schema(connection)?;
    if let Some((table, column, _)) = missing_columns(connection)?.first() {
        return Err(StoreError::InvalidDatabase(format!(
            "{table}.{column} is missing until the home's writer opens it"
        )));
    }
    Ok(())
}

/// The home's identity and version: format 1, schema 4 (a schema-1, -2 or -3 home is
/// `MigrationRequired`), its user_version equal, sluice's application_id and the 27 tables.
pub(crate) fn verify_schema(connection: &Connection) -> Result<()> {
    let has_meta: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='home_meta')",
        [],
        |row| row.get(0),
    )?;
    if !has_meta {
        let python: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('projects') WHERE name='name')
             AND NOT EXISTS(SELECT 1 FROM pragma_table_info('projects') WHERE name='project_id')",
            [],
            |row| row.get(0),
        )?;
        return Err(if python {
            StoreError::PythonFormat
        } else {
            StoreError::InvalidDatabase("home_meta is missing".into())
        });
    }
    let (home, major, schema): (String, i64, i64) = connection.query_row(
        "SELECT home_id, format_major, schema_version FROM home_meta WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if major != FORMAT_MAJOR {
        return Err(StoreError::UnsupportedFormat { found: major });
    }
    if matches!(schema, 1..=3) {
        return Err(StoreError::MigrationRequired { found: schema });
    }
    if schema != SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema { found: schema });
    }
    if home.parse::<HomeId>().is_err() {
        return Err(StoreError::InvalidDatabase("invalid HomeId".into()));
    }
    let user_version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if user_version != schema {
        return Err(StoreError::UnsupportedSchema {
            found: user_version,
        });
    }
    let app: i64 = connection.pragma_query_value(None, "application_id", |r| r.get(0))?;
    if app != APPLICATION_ID {
        return Err(StoreError::InvalidDatabase(
            "application_id mismatch".into(),
        ));
    }
    let tables: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if tables != TABLES {
        return Err(StoreError::InvalidDatabase(format!(
            "expected {TABLES} tables, found {tables}"
        )));
    }
    Ok(())
}
