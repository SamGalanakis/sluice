//! The only Python schema-v6 SQL reader. Its connection always points at a
//! private copy, including the source WAL, never at the source database.
use super::files;
use anyhow::{Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{Value, json};
use std::path::Path;

pub struct Snapshot {
    pub projects: Vec<OldProject>,
    pub dropped: Value,
}
pub struct OldProject {
    pub name: String,
    pub settings: Value,
    pub icon: Option<Vec<u8>>,
    pub plan: Value,
    pub state: Value,
    pub questions: Vec<Value>,
    pub submissions: Value,
}

pub fn snapshot(src: &Path, private: &Path) -> Result<Snapshot> {
    files::directory(private)?;
    for name in ["sluice.db", "sluice.db-wal", "sluice.db-shm"] {
        let path = src.join(name);
        if path.exists() {
            files::write(&private.join(name), &files::read(&path)?, 0o600)?;
        }
    }
    let db = Connection::open_with_flags(
        private.join("sluice.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(version == 6, "expected Python schema v6, found {version}");
    let integrity: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    ensure!(integrity == "ok", "source database integrity failed");
    let mut projects = Vec::new();
    let mut rows = db.prepare("SELECT name,description,paused,archived,resources,icon_text,icon_type,icon,created FROM projects ORDER BY name")?;
    let iter = rows.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, bool>(2)?,
            r.get::<_, bool>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<Vec<u8>>>(7)?,
            r.get::<_, String>(8)?,
        ))
    })?;
    for row in iter {
        let (name, description, paused, archived, resources, icon_text, icon_type, icon, created) =
            row?;
        let plan = document(&db, "plans", &name)?.unwrap_or_else(|| json!({"steps":{}}));
        let revision: i64 = db
            .query_row("SELECT rev FROM plans WHERE project=?1", [&name], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(1);
        let state = document(&db, "states", &name)?.unwrap_or_else(|| json!({"steps":{}}));
        let mut questions = Vec::new();
        let mut q = db.prepare("SELECT n,title,body,ui,input,sender,run,created FROM inbox WHERE project=?1 AND status='open' ORDER BY n")?;
        for row in q.query_map([&name], |r| Ok(json!({"n":r.get::<_,i64>(0)?,"title":r.get::<_,String>(1)?,"body":r.get::<_,Option<String>>(2)?,"ui":r.get::<_,Option<String>>(3)?,"input":r.get::<_,Option<String>>(4)?,"sender":r.get::<_,Option<String>>(5)?,"run":r.get::<_,Option<String>>(6)?,"at":r.get::<_,String>(7)?})))? { questions.push(row?); }
        let mut submissions = json!({});
        let mut q =
            db.prepare("SELECT run,outputs FROM submissions WHERE project=?1 ORDER BY run")?;
        for row in q.query_map([&name], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (run, raw) = row?;
            submissions[run] = parse(&raw)?;
        }
        projects.push(OldProject { name, settings:json!({"description":description,"paused":paused,"archived":archived,"resources":parse(&resources)?,"icon_text":icon_text,"icon_type":icon_type,"created":created,"plan_revision":revision}),icon,plan,state,questions,submissions });
    }
    let mut dropped = json!({});
    for table in ["records", "plan_edits", "outcomes", "leases", "readers"] {
        let n: i64 = db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        dropped[table] = json!(n);
    }
    let mut calls = Vec::new();
    let mut q = db.prepare("SELECT call,project,fn,status FROM calls WHERE status IN ('pending','running') ORDER BY call")?;
    for row in q.query_map([], |r| Ok(json!({"call":r.get::<_,String>(0)?,"project":r.get::<_,Option<String>>(1)?,"fn":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?})))? { calls.push(row?); }
    dropped["calls_for_reconciliation"] = json!(calls);
    dropped["answered_inbox"] = json!(db.query_row(
        "SELECT count(*) FROM inbox WHERE status<>'open'",
        [],
        |r| r.get::<_, i64>(0)
    )?);
    dropped["run_artifacts"] = json!(
        "All old run directories and process/control artifacts are dropped; only continuation checkpoints are retained."
    );
    Ok(Snapshot { projects, dropped })
}

fn parse(raw: &str) -> Result<Value> {
    Ok(
        sluice_model::rpc::decode_json::<sluice_model::rpc::JsonValue>(raw.as_bytes())?
            .into_value(),
    )
}
fn document(db: &Connection, table: &str, name: &str) -> Result<Option<Value>> {
    db.query_row(
        &format!("SELECT doc FROM {table} WHERE project=?1"),
        [name],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|s| parse(&s))
    .transpose()
}
