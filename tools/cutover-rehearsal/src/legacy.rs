//! The legacy history oracle: every schema-1 or schema-2 plan's documents, revision by
//! revision, replayed from `plan_edits.ops` with the old release's patch semantics
//! (`Plan::patch`: RFC 6902 one operation at a time, restoring the key order of every map
//! above each touched path) and without validation, from the origin `{"steps":{}}`.
//!
//! The schema-3 converter must rebuild exactly these documents at every revision
//! (`docs/design/plan-rows.md` §10.4, §10.5). The oracle runs against the old release only,
//! so it is independent of the converter's own replay; the fixture generator checks it
//! against the documents the old store actually stored after each edit.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sluice_model::commands::PatchOperation;

/// One project's history as the oracle replays it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectHistory {
    pub project_id: String,
    pub name: String,
    /// Each revision's document, rev 1 first, as far as the replay got.
    pub revisions: Vec<Revision>,
    /// What stops the conversion of this project (§10.4), each naming the check and the rev.
    pub blockers: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Revision {
    pub rev: u64,
    pub document: Map<String, Value>,
}

/// §10.5's equality: compact serializations, byte-equal.
pub fn compact(document: &Map<String, Value>) -> String {
    serde_json::to_string(document).expect("a JSON map serializes")
}

fn ancestor_orders(value: &Value, path: &str) -> Vec<(String, Vec<String>)> {
    path.match_indices('/')
        .filter_map(|(offset, _)| {
            let parent = &path[..offset];
            let map = value.pointer(parent)?.as_object()?;
            Some((parent.to_owned(), map.keys().cloned().collect()))
        })
        .collect()
}
fn restore_order(value: &mut Value, parent: &str, keys: &[String]) {
    if let Some(new) = value.pointer_mut(parent).and_then(Value::as_object_mut) {
        let known: std::collections::HashSet<&str> = keys.iter().map(String::as_str).collect();
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

/// Apply one revision's ops to `document` with `Plan::patch`'s semantics, unvalidated.
pub fn patch(document: &Map<String, Value>, ops: &[PatchOperation]) -> Result<Value, String> {
    let mut value = Value::Object(document.clone());
    for (index, operation) in ops.iter().enumerate() {
        let path = match operation {
            PatchOperation::Add { path, .. }
            | PatchOperation::Remove { path }
            | PatchOperation::Replace { path, .. }
            | PatchOperation::Move { path, .. }
            | PatchOperation::Copy { path, .. }
            | PatchOperation::Test { path, .. } => path,
        };
        let mut orders = ancestor_orders(&value, path);
        if let PatchOperation::Move { from, .. } = operation {
            orders.extend(ancestor_orders(&value, from));
        }
        let one: json_patch::Patch = serde_json::from_value(
            serde_json::to_value(std::slice::from_ref(operation)).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("ops[{index}]: {e}"))?;
        json_patch::patch(&mut value, &one).map_err(|e| format!("ops[{index}]: {e}"))?;
        for (parent, keys) in orders {
            restore_order(&mut value, &parent, &keys);
        }
    }
    Ok(value)
}

/// Replay every project's history in a schema-1 or schema-2 database.
pub fn replay(sql: &rusqlite::Connection) -> rusqlite::Result<Vec<ProjectHistory>> {
    let mut q = sql.prepare(
        "SELECT p.project_id, p.name, pl.rev, pl.doc FROM plans pl
         JOIN projects p USING(project_id) ORDER BY p.name, p.project_id",
    )?;
    let plans = q
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (project_id, name, current, stored) in plans {
        let mut q =
            sql.prepare("SELECT rev, ops FROM plan_edits WHERE project_id=?1 ORDER BY rev")?;
        let edits = q
            .query_map([&project_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        out.push(replay_project(project_id, name, current, &stored, &edits));
    }
    Ok(out)
}

fn replay_project(
    project_id: String,
    name: String,
    current: i64,
    stored: &str,
    edits: &[(i64, String)],
) -> ProjectHistory {
    let mut history = ProjectHistory {
        project_id,
        name,
        revisions: Vec::new(),
        blockers: Vec::new(),
    };
    let revs: Vec<i64> = edits.iter().map(|(rev, _)| *rev).collect();
    if revs != (1..=current).collect::<Vec<_>>() {
        let missing: Vec<i64> = (1..=current).filter(|r| !revs.contains(r)).collect();
        history.blockers.push(format!(
            "unknown origin: plan_edits has revs {revs:?} for a plan at rev {current} (missing {missing:?})"
        ));
        return history;
    }
    let mut document: Map<String, Value> = serde_json::from_str(r#"{"steps":{}}"#).unwrap();
    for (rev, ops) in edits {
        let ops: Vec<PatchOperation> = match serde_json::from_str(ops) {
            Ok(ops) => ops,
            Err(error) => {
                history
                    .blockers
                    .push(format!("rev {rev}: ops do not decode: {error}"));
                return history;
            }
        };
        if *rev == 1 && !ops.is_empty() {
            history.blockers.push(
                "unknown origin: rev 1 is not the empty initializer (its ops are not [])".into(),
            );
            return history;
        }
        match patch(&document, &ops) {
            Ok(Value::Object(next)) => document = next,
            Ok(_) => {
                history
                    .blockers
                    .push(format!("rev {rev}: the patch leaves no object"));
                return history;
            }
            Err(error) => {
                history
                    .blockers
                    .push(format!("rev {rev}: the patch does not apply: {error}"));
                return history;
            }
        }
        history.revisions.push(Revision {
            rev: *rev as u64,
            document: document.clone(),
        });
    }
    match serde_json::from_str::<Map<String, Value>>(stored) {
        Ok(stored) if compact(&stored) == compact(&document) => {}
        _ => history.blockers.push(format!(
            "rev {current}: the replayed document is not plans.doc"
        )),
    }
    history
}
