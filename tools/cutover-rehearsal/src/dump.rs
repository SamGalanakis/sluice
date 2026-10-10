//! A database as SQL text: its tables, their rows, then its indexes, triggers and views, and
//! its `application_id` and `user_version`. Loading the text into an empty file gives the same
//! database, so legacy homes can be kept as reviewable fixtures and rebuilt by a release that
//! can no longer create them.

use rusqlite::Connection;

fn literal(value: rusqlite::types::ValueRef<'_>) -> String {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => "NULL".into(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => format!("{f:?}"),
        ValueRef::Text(t) => format!("'{}'", String::from_utf8_lossy(t).replace('\'', "''")),
        ValueRef::Blob(b) => format!(
            "X'{}'",
            b.iter().map(|b| format!("{b:02X}")).collect::<String>()
        ),
    }
}

/// The whole database as SQL text, rows in rowid order.
pub fn dump(sql: &Connection) -> rusqlite::Result<String> {
    let mut out = String::from("PRAGMA foreign_keys=OFF;\nBEGIN;\n");
    let mut q = sql.prepare(
        "SELECT type, name, sql FROM sqlite_schema
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY rowid",
    )?;
    let entries: Vec<(String, String, String)> = q
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (_, _, text) in entries.iter().filter(|(kind, _, _)| kind == "table") {
        out.push_str(text);
        out.push_str(";\n");
    }
    for (_, name, _) in entries.iter().filter(|(kind, _, _)| kind == "table") {
        let columns: Vec<String> = sql
            .prepare("SELECT name FROM pragma_table_xinfo(?1) WHERE hidden=0 ORDER BY cid")?
            .query_map([name], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let list = columns
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(",");
        let mut q = sql.prepare(&format!("SELECT {list} FROM \"{name}\" ORDER BY rowid"))?;
        let rows = q
            .query_map([], |r| {
                (0..columns.len())
                    .map(|i| r.get_ref(i).map(literal))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for row in rows {
            out.push_str(&format!(
                "INSERT INTO \"{name}\"({list}) VALUES({});\n",
                row.join(",")
            ));
        }
    }
    let sequences: bool = sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='sqlite_sequence')",
        [],
        |r| r.get(0),
    )?;
    if sequences {
        out.push_str("DELETE FROM sqlite_sequence;\n");
        let mut q = sql.prepare("SELECT name, seq FROM sqlite_sequence ORDER BY name")?;
        for row in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
            let (name, seq) = row?;
            out.push_str(&format!(
                "INSERT INTO sqlite_sequence(name,seq) VALUES('{name}',{seq});\n"
            ));
        }
    }
    for (_, _, text) in entries.iter().filter(|(kind, _, _)| kind != "table") {
        out.push_str(text);
        out.push_str(";\n");
    }
    out.push_str("COMMIT;\n");
    for pragma in ["application_id", "user_version"] {
        let value: i64 = sql.pragma_query_value(None, pragma, |r| r.get(0))?;
        out.push_str(&format!("PRAGMA {pragma}={value};\n"));
    }
    Ok(out)
}

/// Load SQL text (a [`dump`]) into a new database file.
pub fn load(path: &std::path::Path, text: &str) -> rusqlite::Result<Connection> {
    let sql = Connection::open(path)?;
    sql.execute_batch(text)?;
    Ok(sql)
}

/// Copy a home's database through SQLite's backup API, the source opened read-only, as
/// `scripts/compat-check` copies the live one.
pub fn backup_copy(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> rusqlite::Result<()> {
    let from = Connection::open_with_flags(
        source,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut to = Connection::open(destination)?;
    let backup = rusqlite::backup::Backup::new(&from, &mut to)?;
    backup.run_to_completion(256, std::time::Duration::ZERO, None)?;
    Ok(())
}
