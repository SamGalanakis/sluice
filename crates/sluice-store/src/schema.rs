//! Fresh Rust homes only. Writable opens are private to the writer actor.

use std::{fs::File, path::Path, time::Duration};

use fs4::FileExt;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use sluice_model::{error::PublicError, ids::HomeId};

pub const DATABASE_FILE: &str = "sluice.db";
pub const FORMAT_MAJOR: i64 = 1;
/// Changes only for a change that older binaries cannot read. A run's pinned `sluice` reads
/// this database itself and refuses any other version, so a bump would break `step_submit` for
/// every run started before the deploy. Additive columns keep this version: a fresh home gets
/// them from the schema, and the writer adds them to an existing home (`ADDED_COLUMNS`).
pub const SCHEMA_VERSION: i64 = 1;
pub const RECORD_PAYLOAD_VERSION: i64 = 1;
const APPLICATION_ID: i64 = 0x534c5543;
const SCHEMA: &str = include_str!("../migrations/0001.sql");
/// Columns added after homes existed, as `(table, column, definition)`. The writer adds the
/// missing ones when it opens a home, before anything else touches it.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    ("projects", "board", "TEXT"),
    (
        "projects",
        "board_rev",
        "INTEGER NOT NULL DEFAULT 0 CHECK (board_rev >= 0)",
    ),
    ("projects", "board_slots", "TEXT"),
    // A running step's latest values (`step_progress`), kept until its next run starts.
    (
        "steps",
        "progress",
        "TEXT CHECK (progress IS NULL OR json_type(progress) = 'object')",
    ),
    ("steps", "progress_at", "TEXT"),
    ("steps", "progress_run", "TEXT"),
    // Automatic retiring of done units: the age in seconds (null is off) and the keep patterns.
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
];
/// Views added after homes existed, as `(name, definition)`: the writer creates the missing
/// ones with the columns. A view is no table, so a release that counts the home's tables
/// (every pinned one does) reads a home that has it.
const ADDED_VIEWS: &[(&str, &str)] = &[(
    "board_slots",
    "CREATE VIEW board_slots AS SELECT p.project_id AS project_id, s.key AS key,
  json_extract(s.value, '$.markdown') AS markdown, json_extract(s.value, '$.at') AS updated_at,
  json_extract(s.value, '$.author') AS author
  FROM projects p, json_each(p.board_slots) s WHERE p.deleted_at IS NULL",
)];
/// The board columns briefly shipped as schema 2. A home or backup marked 2 is schema 1 with
/// those columns, and its writer marks it 1 again. Remove once no home or backup is marked 2.
const BOARD_INTERIM_SCHEMA: i64 = 2;

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
        verify_schema(&connection, true)?;
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
    } else {
        conform(&mut connection)?;
    }
    Ok(connection)
}

/// Bring a restored copy forward (the copy is private to its restore).
pub(crate) fn upgrade_copy(database: &Path) -> Result<()> {
    let mut connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.pragma_update(None, "foreign_keys", true)?;
    verify_schema(&connection, true)?;
    conform(&mut connection)
}

/// Add a verified home's missing `ADDED_COLUMNS` and mark it `SCHEMA_VERSION`, in one
/// immediate transaction; a home already in shape is left alone.
fn conform(connection: &mut Connection) -> Result<()> {
    let marked: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if marked == SCHEMA_VERSION
        && missing_columns(connection)?.is_empty()
        && missing_views(connection)?.is_empty()
    {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (table, column, definition) in missing_columns(&transaction)? {
        transaction.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    for definition in missing_views(&transaction)? {
        transaction.execute_batch(definition)?;
    }
    transaction.execute(
        "UPDATE home_meta SET schema_version=?1 WHERE singleton=1",
        [SCHEMA_VERSION],
    )?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
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

fn verify(connection: &Connection) -> Result<()> {
    verify_schema(connection, false)?;
    if let Some((table, column, _)) = missing_columns(connection)?.first() {
        return Err(StoreError::InvalidDatabase(format!(
            "{table}.{column} is missing until the home's writer opens it"
        )));
    }
    Ok(())
}

/// `writer`: a home marked with the interim board schema passes, for the writer to conform.
fn verify_schema(connection: &Connection, writer: bool) -> Result<()> {
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
    if schema != SCHEMA_VERSION && !(writer && schema == BOARD_INTERIM_SCHEMA) {
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
    if tables != 23 {
        return Err(StoreError::InvalidDatabase(format!(
            "expected 23 tables, found {tables}"
        )));
    }
    Ok(())
}
