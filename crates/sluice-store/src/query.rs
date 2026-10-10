//! Bounded, untrusted read-only SQL on a fresh connection per request.
//!
//! Public relations use immutable `project_id`, never a mutable project name.
//! `outcomes` is a view of removed results. `messages`, `steps`, `runs`, and
//! `calls` are public relational tables, so no extra view migration is needed.
//!
//! - `outcomes`: result_id, project_id, step_id, generation, work_generation,
//!   attempt_id, unit, declaration, inputs, inputs_hash, status, outputs, error,
//!   manual, run_ids, recorded_at, removed_at.
//! - `messages`: id, project_id, thread, from, to, title, body, needs_reply,
//!   reply_to, answer, ui, input, data, run_id, at, claimed_by, resolved_by, closed_at.
//! - `steps`: project_id, step_id, position, generation, work_generation,
//!   declaration, status, unit, paused, outputs, error, skipped, manual,
//!   inputs_hash, result_id, run_ids, instances, total, done, delivery_cursor,
//!   progress, progress_at, progress_run (a running step's latest `step_progress` values,
//!   never final, cleared when its next run starts).
//! - `runs`: run_id, project_id, attempt_id, step_id, generation, work_generation,
//!   item_index, prev_run, unit, unit_name, boot_id, guardian_pid, guardian_start,
//!   cgroup, socket_challenge, release_id, protocol_major, assigned_after,
//!   assigned_through, started_at, created_at, finished_at, completion_id,
//!   completion_ack, result, completion_action, action_outcome, stopped (who cancelled or
//!   retried the run and why: `{"cancel": {author, reason, at}, "retry": {...}}`).
//! - `calls`: call_id, project_id, run_id, fn, status, inputs, outputs, error,
//!   direct, author, created_at, finished_at.
//!
//! JSON-valued columns remain JSON text; use `json_extract` or `json_each` to
//! inspect them. For example: `SELECT step_id, status FROM steps WHERE project_id
//! = ?`; `SELECT id, "from", "to", body FROM messages WHERE thread = ? ORDER BY id`.
//! Other public views are `log`, `step_changes`, `edits`, and `questions`.

use std::{
    io::{self, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
    types::{Value, ValueRef},
};
use sluice_model::error::PublicError;

use crate::schema;

pub const DEFAULT_ROWS: usize = 200;
pub const MAX_ROWS: usize = 1_000;
pub const MAX_BYTES: usize = 1 << 20;
pub const MAX_VM_OPERATIONS: u64 = 250_000;
pub const MAX_DURATION: Duration = Duration::from_secs(2);
const MAX_SQL_BYTES: usize = 100_000;

/// Denied even if a future connection registers these functions. Fresh query
/// connections register no application functions or extensions. SQLite's
/// bounded read functions, including JSON and aggregates, are otherwise allowed.
pub const DENIED_FUNCTIONS: &[&str] = &[
    "load_extension",
    "readfile",
    "writefile",
    "edit",
    "eval",
    "shell",
    "system",
    "exec",
    "fts3_tokenizer",
    "sqlite_log",
    "sqlite_rename_table",
    "sqlite_rename_column",
    "sqlite_rename_test",
    "sqlite_drop_column",
    "sqlite_rename_quotefix",
    "sqlite_drop_constraint",
    "sqlite_fail",
    "sqlite_add_constraint",
    "sqlite_find_constraint",
    "sqlite_filestat",
];

/// Trusted callers may tighten these limits, but cannot exceed the public caps.
#[derive(Clone, Copy, Debug)]
pub struct QueryLimits {
    pub duration: Duration,
    pub vm_operations: u64,
    pub bytes: usize,
}
impl Default for QueryLimits {
    fn default() -> Self {
        Self {
            duration: MAX_DURATION,
            vm_operations: MAX_VM_OPERATIONS,
            bytes: MAX_BYTES,
        }
    }
}

/// SQL storage classes that have an unambiguous JSON representation. BLOBs,
/// invalid UTF-8 and non-finite reals return explicit errors, never lossy values.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryCell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
}
impl QueryCell {
    fn json(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Integer(v) => (*v).into(),
            Self::Real(v) => (*v).into(),
            Self::Text(v) => v.clone().into(),
        }
    }
}

/// Column names in SELECT order and owned typed cells. Duplicate column names
/// are retained. `encoded()` includes the whole `{columns,rows,truncated}`
/// envelope and is guaranteed to fit the selected byte budget.
#[derive(Clone)]
pub struct QueryResult {
    columns: Vec<String>,
    rows: Vec<Vec<QueryCell>>,
    truncated: bool,
    encoded: Vec<u8>,
}
impl std::fmt::Debug for QueryResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryResult")
            .field("columns", &self.columns)
            .field("row_count", &self.rows.len())
            .field("truncated", &self.truncated)
            .field("encoded_bytes", &self.encoded.len())
            .finish()
    }
}
impl QueryResult {
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
    pub fn rows(&self) -> &[Vec<QueryCell>] {
        &self.rows
    }
    pub fn truncated(&self) -> bool {
        self.truncated
    }
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }
}

type Result<T> = std::result::Result<T, PublicError>;
fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
fn invalid(message: impl Into<String>) -> PublicError {
    let message = message.into();
    PublicError::Invalid {
        errors: vec![format!("sql: {message}")],
        message,
    }
}

/// Run one SELECT or WITH; positional parameters bind SQLite values without
/// interpolation. Call on a blocking thread. Row lookahead distinguishes an
/// exact row limit from truncation. Byte truncation stops before a crossing row
/// is retained, without evaluating subsequent rows. The progress hook runs on
/// every VM instruction, including preparation where SQLite supports it.
/// Deadline enforcement is cooperative: one bounded scalar cannot be preempted.
pub fn query(
    home: &Path,
    sql: &str,
    params: Option<&[Value]>,
    limit: Option<usize>,
) -> Result<QueryResult> {
    query_with_limits(home, sql, params, limit, QueryLimits::default())
}

pub fn query_with_limits(
    home: &Path,
    sql: &str,
    params: Option<&[Value]>,
    limit: Option<usize>,
    limits: QueryLimits,
) -> Result<QueryResult> {
    let rows_limit = limit.unwrap_or(DEFAULT_ROWS);
    if !(1..=MAX_ROWS).contains(&rows_limit) {
        return Err(bad("limit: expected an integer in 1..1000"));
    }
    if sql.len() > MAX_SQL_BYTES {
        return Err(bad("sql: exceeds 100000 bytes"));
    }
    if limits.duration > MAX_DURATION
        || limits.vm_operations == 0
        || limits.vm_operations > MAX_VM_OPERATIONS
        || limits.bytes == 0
        || limits.bytes > MAX_BYTES
    {
        return Err(bad(
            "query limits must not exceed the public caps and budgets must be positive",
        ));
    }
    one_statement(sql)?;
    let deadline = Instant::now() + limits.duration;
    // No SQLite busy sleep may extend the request deadline.
    let connection = schema::open_reader(home, Duration::ZERO).map_err(|e| match e {
        schema::StoreError::Sql(e) => sql_error(e, 0),
        other => invalid(other.to_string()),
    })?;
    for (kind, cap) in [
        (Limit::SQLITE_LIMIT_LENGTH, MAX_BYTES as i32),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES as i32),
        (Limit::SQLITE_LIMIT_COLUMN, 200),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 200),
        (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 50),
        (Limit::SQLITE_LIMIT_VDBE_OP, MAX_VM_OPERATIONS as i32),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
    ] {
        connection
            .set_limit(kind, cap)
            .map_err(|e| sql_error(e, 0))?;
    }
    let denied = Arc::new(AtomicBool::new(false));
    let hook_denied = denied.clone();
    connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            let decision = match context.action {
                AuthAction::Select | AuthAction::Read { .. } | AuthAction::Recursive => {
                    Authorization::Allow
                }
                AuthAction::Function { function_name }
                    if !DENIED_FUNCTIONS
                        .iter()
                        .any(|name| function_name.eq_ignore_ascii_case(name)) =>
                {
                    Authorization::Allow
                }
                // Deny unknown actions too, including all PRAGMAs and transaction control.
                _ => Authorization::Deny,
            };
            if decision == Authorization::Deny {
                hook_denied.store(true, Ordering::Relaxed);
            }
            decision
        }))
        .map_err(|e| sql_error(e, 0))?;
    let interrupted = Arc::new(AtomicU8::new(0));
    let hook_reason = interrupted.clone();
    let mut operations = 0_u64;
    connection
        .progress_handler(
            1,
            Some(move || {
                operations += 1;
                let reason = if Instant::now() >= deadline {
                    1
                } else if operations >= limits.vm_operations {
                    2
                } else {
                    0
                };
                if reason != 0 {
                    hook_reason.store(reason, Ordering::Relaxed);
                }
                reason != 0
            }),
        )
        .map_err(|e| sql_error(e, 0))?;
    let map_sql = |e| {
        if denied.load(Ordering::Relaxed) {
            bad(format!("query is read-only: {e}"))
        } else {
            sql_error(e, interrupted.load(Ordering::Relaxed))
        }
    };
    check_deadline(deadline)?;
    let mut statement = connection.prepare(sql).map_err(map_sql)?;
    if !statement.readonly() || statement.column_count() == 0 {
        return Err(bad(
            "query is read-only: expected one SELECT or WITH statement",
        ));
    }
    let columns: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut encoded = b"{\"columns\":".to_vec();
    encoded.extend(
        encode(&serde_json::json!(columns), limits.bytes)
            .map_err(|_| bad("query columns exceed the encoded output byte budget"))?,
    );
    encoded.extend_from_slice(b",\"rows\":[");
    // The false suffix is one byte longer than true, reserve that larger size.
    const SUFFIX: &[u8] = b"],\"truncated\":false}";
    let base_size = encoded.len() + SUFFIX.len();
    if base_size > limits.bytes {
        return Err(bad("query columns exceed the encoded output byte budget"));
    }
    let row_capacity = limits.bytes - base_size;
    let mut cursor = statement
        .query(rusqlite::params_from_iter(params.unwrap_or(&[])))
        .map_err(map_sql)?;
    let mut rows = Vec::new();
    let mut truncated = false;
    loop {
        check_deadline(deadline)?;
        let Some(row) = cursor.next().map_err(map_sql)? else {
            break;
        };
        if rows.len() == rows_limit {
            truncated = true;
            break;
        }
        let mut cells = Vec::with_capacity(columns.len());
        let mut row_bytes = vec![b'['];
        for (index, name) in columns.iter().enumerate() {
            let cell = match row.get_ref(index).map_err(map_sql)? {
                ValueRef::Null => QueryCell::Null,
                ValueRef::Integer(v) => QueryCell::Integer(v),
                ValueRef::Real(v) if v.is_finite() => QueryCell::Real(v),
                ValueRef::Real(_) => {
                    return Err(invalid(format!("column {name:?} holds a non-finite real")));
                }
                ValueRef::Text(v) => QueryCell::Text(
                    std::str::from_utf8(v)
                        .map_err(|_| invalid(format!("column {name:?} holds invalid UTF-8")))?
                        .to_owned(),
                ),
                ValueRef::Blob(_) => {
                    return Err(bad(format!(
                        "column {name:?} holds binary data: select hex(\"{name}\") or length(\"{name}\") instead"
                    )));
                }
            };
            let bytes = encode(&cell.json(), row_capacity.saturating_sub(2)).map_err(|_| {
                bad(format!(
                    "column {name:?}: cell too large for the encoded output byte budget"
                ))
            })?;
            if index != 0 {
                row_bytes.push(b',');
            }
            if row_bytes.len() + bytes.len() + 1 > row_capacity {
                return Err(bad("one row exceeds the encoded output byte budget"));
            }
            row_bytes.extend(bytes);
            cells.push(cell);
        }
        row_bytes.push(b']');
        check_deadline(deadline)?;
        if row_bytes.len() > row_capacity {
            return Err(bad("one row exceeds the encoded output byte budget"));
        }
        let comma = usize::from(!rows.is_empty());
        if encoded.len() + comma + row_bytes.len() + SUFFIX.len() > limits.bytes {
            truncated = true;
            break;
        }
        if comma != 0 {
            encoded.push(b',');
        }
        encoded.extend(row_bytes);
        rows.push(cells);
    }
    encoded.extend_from_slice(if truncated {
        b"],\"truncated\":true}"
    } else {
        SUFFIX
    });
    check_deadline(deadline)?;
    Ok(QueryResult {
        columns,
        rows,
        truncated,
        encoded,
    })
}

fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(bad("interrupted: query deadline exceeded"))
    } else {
        Ok(())
    }
}
fn sql_error(error: rusqlite::Error, reason: u8) -> PublicError {
    if reason != 0 {
        return bad(if reason == 1 {
            "interrupted: query deadline exceeded"
        } else {
            "interrupted: query VM-operation budget exceeded"
        });
    }
    match &error {
        rusqlite::Error::SqliteFailure(code, _) => match code.code {
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked => {
                PublicError::Busy {
                    message: error.to_string(),
                    retryable: true,
                }
            }
            rusqlite::ErrorCode::TooBig => {
                bad("cell too large: SQLite value exceeds the 1 MiB limit")
            }
            rusqlite::ErrorCode::AuthorizationForStatementDenied
            | rusqlite::ErrorCode::ReadOnly => bad(format!("query is read-only: {error}")),
            _ => invalid(error.to_string()),
        },
        rusqlite::Error::InvalidParameterCount(..)
        | rusqlite::Error::MultipleStatement
        | rusqlite::Error::NulError(_) => bad(error.to_string()),
        _ => invalid(error.to_string()),
    }
}

// serde_json streams into this writer; an escaped cell cannot allocate past its
// remaining budget before we discover that it is too large.
struct BoundedBuffer {
    bytes: Vec<u8>,
    cap: usize,
}
impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.cap.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("encoded output byte budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encode(
    value: &serde_json::Value,
    cap: usize,
) -> std::result::Result<Vec<u8>, serde_json::Error> {
    let mut buffer = BoundedBuffer {
        bytes: Vec::new(),
        cap,
    };
    serde_json::to_writer(&mut buffer, value)?;
    Ok(buffer.bytes)
}

// Avoid rusqlite's recursive tail preparation on a long list of statements.
// This only locates a statement separator; SQLite remains the SQL parser.
fn one_statement(sql: &str) -> Result<()> {
    let bytes = sql.as_bytes();
    if bytes.contains(&0) {
        return Err(bad("sql: NUL bytes are not allowed"));
    }
    let mut i = 0;
    let mut ended = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < bytes.len() && !bytes[i..].starts_with(b"*/") {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if ended {
            return Err(bad("sql: expected one SELECT or WITH statement"));
        }
        if b == b';' {
            ended = true;
            i += 1;
            continue;
        }
        if matches!(b, b'\'' | b'"' | b'`' | b'[') {
            let closing = if b == b'[' { b']' } else { b };
            i += 1;
            while i < bytes.len() {
                if bytes[i] == closing {
                    i += 1;
                    if closing != b']' && i < bytes.len() && bytes[i] == closing {
                        i += 1;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    Ok(())
}
