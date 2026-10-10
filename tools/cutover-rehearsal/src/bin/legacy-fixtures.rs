//! `legacy-fixtures --out <dir>` builds the legacy homes the schema-3 converter is tested on
//! (`docs/design/plan-rows.md` §12, lane H2), with the old release's own coordinator, and
//! writes each as SQL text beside what it must convert to:
//!
//! - `schema1.sql`: a schema-1 home whose plans have every shape the history can take: absent
//!   root sections and every root order, a section removed and re-added, inputs whose rows the
//!   old projection normalized, a step section replaced whole in another order, a gap, a move,
//!   `test` and `copy` operations, typed tools, a recipe unit, pauses, needs and priorities,
//!   runs whose attempts carry the completion snapshot, an archived and a deleted project,
//!   trimmed records, and one snapshot and one record that disagree with the history
//!   (warnings, not blockers);
//! - `schema1_old.sql`: the same home as a backup from before the later added columns;
//! - `schema2.sql`: the same home as the interim board schema 2 left it;
//! - `live.sql`: a home holding live work (a running step, a held lease and a running call);
//! - `<name>.expected.json`: every plan's document at every revision as the old store stored
//!   it after each edit (checked here against `legacy::replay`, the oracle), and the counts
//!   and warnings the conversion must report.

use cutover_rehearsal::{
    dump, legacy,
    scratch::{Launcher, command, journal, map},
};
use serde_json::{Map, Value, json};
use sluice_model::{commands::CommandReply, ids::ProjectId};
use sluice_process::journal::PayloadResult;
use sluice_runtime::{coordinator::Coordinator, dispatch::Catalog, scheduler::reconcile_project};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// A revision and the document the old store stored for it.
type Stored = (u64, Map<String, Value>);

struct Generator {
    broker: Coordinator<Launcher>,
    host: Launcher,
    home: PathBuf,
    ids: BTreeMap<String, ProjectId>,
    /// Each project's (rev, stored document) after every edit.
    history: BTreeMap<String, Vec<Stored>>,
}

fn selector(id: ProjectId) -> Value {
    json!({"kind": "id", "value": id})
}
fn edit(reason: &str) -> Value {
    json!({"dry_run": false, "reason": reason, "author": "generator"})
}

impl Generator {
    async fn open(home: PathBuf) -> Self {
        std::fs::create_dir_all(&home).unwrap();
        let host = Launcher::default();
        let broker = Coordinator::open(home.clone(), Catalog::fixtures(), host.clone())
            .await
            .unwrap();
        broker.acquire_scheduler("generator".into()).await.unwrap();
        Self {
            broker,
            host,
            home,
            ids: BTreeMap::new(),
            history: BTreeMap::new(),
        }
    }
    async fn stored(&self, name: &str) -> (u64, Map<String, Value>) {
        let id = self.ids[name].to_string();
        self.broker
            .reads()
            .snapshot(move |sql| {
                Ok(sql.query_row(
                    "SELECT rev, doc FROM plans WHERE project_id=?1",
                    [&id],
                    |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?)),
                )?)
            })
            .await
            .map(|(rev, doc)| (rev, serde_json::from_str(&doc).unwrap()))
            .unwrap()
    }
    async fn capture(&mut self, name: &str) {
        let (rev, doc) = self.stored(name).await;
        let history = self.history.entry(name.to_owned()).or_default();
        if history.last().is_none_or(|(last, _)| *last != rev) {
            assert_eq!(
                rev as usize,
                history.len() + 1,
                "{name}: one revision per edit"
            );
            history.push((rev, doc));
        }
    }
    async fn create(&mut self, name: &str, resources: Value) {
        let reply = command(
            &self.broker,
            json!({"command": "project_create", "args": {"name": name, "description": "",
                "icon": null, "resources": resources, "author": "generator"}}),
        )
        .await
        .unwrap();
        let CommandReply::Project(project) = reply else {
            panic!("{name}: {reply:?}")
        };
        self.ids.insert(name.to_owned(), project.project_id);
        self.capture(name).await;
    }
    /// A command on `name`'s project; an edit's new revision is captured.
    async fn run(&mut self, name: &str, tool: &str, mut args: Value) -> CommandReply {
        args["project"] = selector(self.ids[name]);
        let reply = command(&self.broker, json!({"command": tool, "args": args}))
            .await
            .unwrap_or_else(|e| panic!("{name} {tool} {args}: {e}"));
        self.capture(name).await;
        reply
    }
    async fn patch(&mut self, name: &str, ops: Value) {
        let (rev, _) = self.stored(name).await;
        let before = rev;
        self.run(
            name,
            "plan_patch",
            json!({"rev": rev, "ops": ops, "start": true, "dry_run": false,
                "reason": "generated", "author": "generator"}),
        )
        .await;
        assert_eq!(self.stored(name).await.0, before + 1, "{name}: {ops}");
    }
    /// Launch what is ready, then finish each new launch of `steps` as `result` says.
    async fn tick(&mut self, name: &str, finish: &[(&str, PayloadResult)]) {
        let before = self.host.launches().len();
        reconcile_project(&self.broker, self.ids[name], "generator")
            .await
            .unwrap();
        for launch in &self.host.launches()[before..] {
            let step = launch.identity.step.as_ref().map(|s| s.to_string());
            if let Some((_, result)) = finish.iter().find(|(s, _)| Some(*s) == step.as_deref()) {
                self.broker
                    .complete(journal(launch, result.clone()))
                    .await
                    .unwrap();
            }
        }
    }
    async fn sql(&self, statements: &'static str) {
        let ids = self.ids.clone();
        self.broker
            .writer()
            .write(sluice_store::RetrySafety::NonIdempotent, move |tx| {
                let mut text = statements.to_owned();
                for (name, id) in &ids {
                    text = text.replace(&format!("${name}"), &id.to_string());
                }
                tx.sql().execute_batch(&text)?;
                tx.changed(None, "log");
                Ok(())
            })
            .await
            .unwrap();
    }
}

fn succeeded(value: Value) -> PayloadResult {
    PayloadResult::Succeeded(map(json!({ "value": value })))
}

async fn alpha(g: &mut Generator) {
    g.create("alpha", json!({"cpu": 2})).await;
    let recipes = g
        .home
        .join("projects")
        .join(g.ids["alpha"].to_string())
        .join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::write(
        recipes.join("lane.json"),
        json!({"name": "lane", "steps": {
            "{unit}-fork": {"run": "fixture.echo", "in": {"value": {"default": "fork"}}},
            "{unit}-work": {"run": "fixture.echo", "after": ["{unit}-fork"],
                "in": {"value": {"source": "{unit}-fork/value"}}}}})
        .to_string(),
    )
    .unwrap();
    // rev 2: the first step, into the origin's only section.
    g.patch(
        "alpha",
        json!([{"op": "add", "path": "/steps/a", "value": {"run": "fixture.echo",
        "tags": ["unit:build"], "in": {"value": {"default": 1}}}}]),
    )
    .await;
    // rev 3: inputs after steps, one declared by type alone and one with a doc; the old
    // projection stores both as {"type": …, "doc": …}.
    g.patch(
        "alpha",
        json!([{"op": "add", "path": "/inputs", "value": {"repo": "string",
        "limit": {"type": "int", "doc": "How many"}}}]),
    )
    .await;
    // rev 4: a handoff, an exit, a pause with a reason, a priority and needs.
    g.patch("alpha", json!([
        {"op": "add", "path": "/steps/b", "value": {"run": "fixture.echo",
            "tags": ["unit:build", "exit"], "after": ["a"], "in": {"value": {"source": "a/value"}}}},
        {"op": "add", "path": "/steps/c", "value": {"run": "fixture.echo", "paused": "after review",
            "priority": 5, "needs": {"cpu": 1}, "in": {"value": {"source": "repo"}}}}
    ]))
    .await;
    g.run(
        "alpha",
        "plan_set_input",
        json!({"name": "repo", "value": "/srv/repo",
        "edit": edit("input")}),
    )
    .await;
    g.run(
        "alpha",
        "plan_set_input",
        json!({"name": "limit", "value": 3, "edit": edit("input")}),
    )
    .await;
    g.tick("alpha", &[("a", succeeded(json!(1)))]).await;
    g.tick("alpha", &[("b", succeeded(json!(1)))]).await;
    // rev 5: outputs, the third section.
    g.patch(
        "alpha",
        json!([{"op": "add", "path": "/outputs", "value": {"result":
        {"source": "b/value"}}}]),
    )
    .await;
    // rev 6: the step section replaced whole in another order, with a new step first among
    // survivors' neighbours.
    let (_, doc) = g.stored("alpha").await;
    let steps = &doc["steps"];
    g.patch("alpha", json!([{"op": "replace", "path": "/steps", "value": {
        "c": steps["c"], "a": steps["a"], "d": {"run": "fixture.echo", "in": {"value": {"default": "d"}}},
        "b": steps["b"]}}]))
        .await;
    // rev 7: a removal leaves a gap.
    g.run(
        "alpha",
        "step_remove",
        json!({"selection": {"steps": ["d"], "tags": null},
        "edit": edit("remove d")}),
    )
    .await;
    // rev 8: a scatter step added paused (start false).
    g.run(
        "alpha",
        "step_add",
        json!({"step": "e", "spec": {"run": "fixture.echo",
        "scatter": "value", "in": {"value": {"default": [1, 2, 3]}}}, "start": false,
        "edit": edit("add e")}),
    )
    .await;
    // rev 9: a move renames e to f.
    g.patch(
        "alpha",
        json!([{"op": "move", "from": "/steps/e", "path": "/steps/f"}]),
    )
    .await;
    // rev 10 and 11: the outputs section removed, then present and empty.
    g.patch("alpha", json!([{"op": "remove", "path": "/outputs"}]))
        .await;
    g.patch(
        "alpha",
        json!([{"op": "add", "path": "/outputs", "value": {}}]),
    )
    .await;
    // rev 12: an input's declaration replaced in place.
    g.patch(
        "alpha",
        json!([{"op": "replace", "path": "/inputs/repo", "value":
        {"type": "string", "doc": "The repo"}}]),
    )
    .await;
    // rev 13: a unit gate.
    g.run(
        "alpha",
        "step_add",
        json!({"step": "g", "spec": {"run": "fixture.echo",
        "after": ["unit:build?"], "in": {"value": {"source": "c/value"}}}, "start": true,
        "edit": edit("add g")}),
    )
    .await;
    // rev 14: the whole document replaced with its sections in another order (a pure
    // reorder commits nothing in schema 1, so one declaration changes with it).
    let (_, doc) = g.stored("alpha").await;
    let mut steps = doc["steps"].clone();
    steps["g"]["doc"] = json!("Gate on build");
    g.patch(
        "alpha",
        json!([{"op": "replace", "path": "", "value": {
        "inputs": doc["inputs"], "outputs": doc["outputs"], "steps": steps}}]),
    )
    .await;
    // rev 15: a test, then a new key inside a declaration.
    g.patch(
        "alpha",
        json!([
            {"op": "test", "path": "/steps/g/run", "value": "fixture.echo"},
            {"op": "add", "path": "/steps/g/tags", "value": ["late"]}
        ]),
    )
    .await;
    // rev 16: a copy.
    g.patch(
        "alpha",
        json!([{"op": "copy", "from": "/steps/g", "path": "/steps/h"}]),
    )
    .await;
    // rev 17 to 21: typed tools.
    g.run(
        "alpha",
        "unit_add",
        json!({"recipe": "lane", "unit": "fig-1", "params": {},
        "after": {}, "start": true, "edit": edit("unit")}),
    )
    .await;
    g.run(
        "alpha",
        "step_pause",
        json!({"selection": {"steps": ["b"], "tags": null},
        "paused": true, "edit": edit("hold b")}),
    )
    .await;
    g.run(
        "alpha",
        "unit_tag",
        json!({"unit": "fig-1", "add": ["wave-3"], "remove": [],
        "edit": edit("tag")}),
    )
    .await;
    g.run(
        "alpha",
        "edge_add",
        json!({"step": "h", "after": ["a"], "edit": edit("gate h")}),
    )
    .await;
    g.run(
        "alpha",
        "step_update",
        json!({"step": "c", "changes": {"priority": 7},
        "edit": edit("prioritize c")}),
    )
    .await;
    // A run lost by its guardian: a terminal attempt carrying the completion snapshot.
    g.tick(
        "alpha",
        &[("fig-1-fork", PayloadResult::Lost("guardian gone".into()))],
    )
    .await;
}

async fn others(g: &mut Generator) {
    // beta: only its origin, a board and a retired board slot.
    g.create("beta", json!({})).await;
    g.run(
        "beta",
        "board_set",
        json!({"program": "root = Doc(\"beta\")", "expected_rev": null,
        "reason": null, "author": "generator"}),
    )
    .await;
    // gamma: archived, with steps before inputs.
    g.create("gamma", json!({})).await;
    g.patch(
        "gamma",
        json!([{"op": "add", "path": "/steps/x", "value": {"run": "fixture.submit",
        "in": {"value": {"default": true}}, "outputs": {"extra": "string"}}}]),
    )
    .await;
    g.patch(
        "gamma",
        json!([{"op": "add", "path": "/inputs", "value": {"k": "boolean"}}]),
    )
    .await;
    g.patch(
        "gamma",
        json!([{"op": "replace", "path": "/steps/x/in/value", "value":
        {"source": "k"}}]),
    )
    .await;
    g.patch(
        "gamma",
        json!([{"op": "add", "path": "/outputs", "value": {"out":
        {"source": "x/value"}}}]),
    )
    .await;
    g.run(
        "gamma",
        "project_update",
        json!({"archived": true, "author": "generator"}),
    )
    .await;
    // delta: deleted, so it has no plan left to convert.
    g.create("delta", json!({})).await;
    g.patch(
        "delta",
        json!([{"op": "add", "path": "/steps/y", "value": {"run": "fixture.echo",
        "in": {"value": {"default": 0}}}}]),
    )
    .await;
    g.run(
        "delta",
        "project_update",
        json!({"archived": true, "author": "generator"}),
    )
    .await;
    let settings: i64 = {
        let id = g.ids["delta"].to_string();
        g.broker
            .reads()
            .snapshot(move |sql| {
                Ok(sql.query_row(
                    "SELECT settings_rev FROM projects WHERE project_id=?1",
                    [&id],
                    |r| r.get(0),
                )?)
            })
            .await
            .unwrap()
    };
    let id = g.ids["delta"];
    command(
        &g.broker,
        json!({"command": "project_delete", "args": {"project": selector(id),
        "confirm_name": "delta", "expected_settings_rev": settings, "author": "generator"}}),
    )
    .await
    .unwrap();
    g.history.remove("delta");
}

/// One snapshot and one record that disagree with the history (§10.4: warnings), retired
/// board slots, and alpha's oldest records trimmed.
async fn wear(g: &mut Generator) {
    g.sql(
        "UPDATE attempts SET
           request = json_set(request, '$.provenance.runtime.completion.document.steps.a.doc', 'tampered'),
           provenance = json_set(provenance, '$.runtime.completion.document.steps.a.doc', 'tampered')
         WHERE project_id = '$alpha' AND step_id = 'a';
         UPDATE records SET payload = json_set(payload, '$.author', 'someone-else')
         WHERE project_id = '$gamma' AND kind = 'plan.edit' AND json_extract(payload, '$.rev') = 3;
         UPDATE projects SET board_slots = '{\"notes\":{\"markdown\":\"m\",\"at\":\"2026-10-01T00:00:00Z\",\"author\":\"owner\"}}'
         WHERE project_id = '$beta';",
    )
    .await;
    let alpha = g.ids["alpha"];
    g.broker
        .writer()
        .write(sluice_store::RetrySafety::NonIdempotent, move |tx| {
            sluice_store::records::trim_to(tx, Some(alpha), 30, 30)?;
            Ok(())
        })
        .await
        .unwrap();
}

/// What a conversion of this home must reproduce and report.
async fn expected(g: &Generator, schema: i64) -> Value {
    let mut projects = Vec::new();
    for (name, history) in &g.history {
        let id = g.ids[name].to_string();
        let (records, snapshots, archived): (i64, i64, bool) = g
            .broker
            .reads()
            .snapshot(move |sql| {
                Ok((
                    sql.query_row(
                        "SELECT count(*) FROM records WHERE project_id=?1 AND kind='plan.edit'",
                        [&id],
                        |r| r.get(0),
                    )?,
                    sql.query_row(
                        "SELECT count(*) FROM attempts WHERE project_id=?1
                         AND json_type(request, '$.provenance.runtime.completion') IS NOT NULL",
                        [&id],
                        |r| r.get(0),
                    )?,
                    sql.query_row(
                        "SELECT archived FROM projects WHERE project_id=?1",
                        [&id],
                        |r| r.get(0),
                    )?,
                ))
            })
            .await
            .unwrap();
        let last = &history.last().unwrap().1;
        let count = |section: &str| {
            last.get(section)
                .and_then(Value::as_object)
                .map_or(0, Map::len)
        };
        projects.push(json!({
            "name": name,
            "archived": archived,
            "revisions": history.iter().map(|(rev, doc)| json!({"rev": rev, "document": doc})).collect::<Vec<_>>(),
            "steps": count("steps"),
            "inputs": count("inputs"),
            "outputs": count("outputs"),
            "plan_edit_records": records,
            "attempt_snapshots": snapshots,
        }));
    }
    json!({
        "from_schema": schema,
        "projects": projects,
        "deleted": ["delta"],
        "warnings": [
            {"project": "alpha", "about": "the completion snapshot of an attempt of step a"},
            {"project": "gamma", "about": "the plan.edit record of rev 3 (author)"}
        ]
    })
}

/// The oracle must replay exactly what the old store stored after each edit.
fn check_oracle(database: &Path, expected: &Value) {
    let sql = rusqlite::Connection::open(database).unwrap();
    let replayed = legacy::replay(&sql).unwrap();
    let projects = expected["projects"].as_array().unwrap();
    assert_eq!(replayed.len(), projects.len());
    for (oracle, stored) in replayed.iter().zip(projects) {
        assert_eq!(oracle.name, stored["name"].as_str().unwrap());
        assert!(
            oracle.blockers.is_empty(),
            "{}: {:?}",
            oracle.name,
            oracle.blockers
        );
        let stored = stored["revisions"].as_array().unwrap();
        assert_eq!(oracle.revisions.len(), stored.len(), "{}", oracle.name);
        for (replayed, stored) in oracle.revisions.iter().zip(stored) {
            assert_eq!(
                legacy::compact(&replayed.document),
                legacy::compact(stored["document"].as_object().unwrap()),
                "{} rev {}: the oracle's replay differs from what the old store stored",
                oracle.name,
                replayed.rev
            );
        }
    }
}

/// Later-added columns, dropped to make a home an older one (as of the return to schema 1).
const OLDER: &str = "
DROP VIEW board_slots;
ALTER TABLE projects DROP COLUMN board_slots;
ALTER TABLE projects DROP COLUMN board_doc;
ALTER TABLE projects DROP COLUMN board_doc_rev;
ALTER TABLE projects DROP COLUMN board_doc_at;
ALTER TABLE projects DROP COLUMN board_doc_author;
ALTER TABLE projects DROP COLUMN prune_done_after;
ALTER TABLE projects DROP COLUMN prune_keep;
ALTER TABLE steps DROP COLUMN progress;
ALTER TABLE steps DROP COLUMN progress_at;
ALTER TABLE steps DROP COLUMN progress_run;
ALTER TABLE messages DROP COLUMN read_at;
ALTER TABLE runs DROP COLUMN stopped;
";

fn write(out: &Path, name: &str, database: &Path, expected: &Value) {
    let sql = rusqlite::Connection::open(database).unwrap();
    std::fs::write(out.join(format!("{name}.sql")), dump::dump(&sql).unwrap()).unwrap();
    std::fs::write(
        out.join(format!("{name}.expected.json")),
        serde_json::to_string_pretty(expected).unwrap() + "\n",
    )
    .unwrap();
    // The text loads back into the same database.
    let reloaded = out.join(format!(".{name}.check.db"));
    let _ = std::fs::remove_file(&reloaded);
    let again = dump::load(&reloaded, &dump::dump(&sql).unwrap()).unwrap();
    assert_eq!(
        dump::dump(&again).unwrap(),
        dump::dump(&sql).unwrap(),
        "{name} reloads"
    );
    drop(again);
    std::fs::remove_file(&reloaded).unwrap();
}

async fn live(root: &Path) -> (PathBuf, Value) {
    let mut g = Generator::open(root.join("live")).await;
    g.create("live", json!({"cpu": 1})).await;
    g.patch(
        "live",
        json!([
            {"op": "add", "path": "/steps/tests-main", "value": {"run": "fixture.echo",
                "tags": ["rolling"], "in": {"value": {"default": 1}}}},
            {"op": "add", "path": "/steps/build", "value": {"run": "fixture.echo",
                "needs": {"cpu": 1}, "in": {"value": {"default": 2}}}}
        ]),
    )
    .await;
    g.tick("live", &[]).await;
    let id = g.ids["live"];
    command(
        &g.broker,
        json!({"command": "fn_call", "args": {"name": "fixture.wait",
        "inputs": {"value": 9}, "project": selector(id), "wait_seconds": 0, "direct": true,
        "author": "generator"}}),
    )
    .await
    .unwrap();
    let counts: (i64, i64, i64, i64) = g
        .broker
        .reads()
        .snapshot(|sql| {
            Ok(sql.query_row(
                "SELECT (SELECT count(*) FROM attempts WHERE phase<>'terminal'),
                        (SELECT count(*) FROM runs WHERE finished_at IS NULL),
                        (SELECT count(*) FROM leases WHERE state IN ('waiting','held')),
                        (SELECT count(*) FROM calls WHERE status='running')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?)
        })
        .await
        .unwrap();
    let mut expected = expected(&g, 1).await;
    expected["deleted"] = json!([]);
    expected["warnings"] = json!([]);
    expected["live"] = json!({"attempts": counts.0, "runs": counts.1, "leases": counts.2,
        "calls": counts.3});
    let database = g.home.join("sluice.db");
    drop(g);
    (database, expected)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = match args.as_slice() {
        [_, flag, out] if flag == "--out" => PathBuf::from(out),
        _ => {
            eprintln!("usage: legacy-fixtures --out <dir>");
            std::process::exit(2);
        }
    };
    std::fs::create_dir_all(&out).unwrap();
    let scratch = std::env::temp_dir().join(format!("legacy-fixtures.{}", std::process::id()));
    let runtime = sluice_runtime::coordinator::executor().unwrap();
    runtime.block_on(async {
        let mut g = Generator::open(scratch.join("schema1")).await;
        alpha(&mut g).await;
        others(&mut g).await;
        wear(&mut g).await;
        let expected1 = expected(&g, 1).await;
        let database = g.home.join("sluice.db");
        let snapshot = scratch.join("schema1.db");
        dump::backup_copy(&database, &snapshot).unwrap();
        drop(g);
        check_oracle(&snapshot, &expected1);
        write(&out, "schema1", &snapshot, &expected1);

        let older = scratch.join("schema1_old.db");
        dump::backup_copy(&snapshot, &older).unwrap();
        rusqlite::Connection::open(&older)
            .unwrap()
            .execute_batch(OLDER)
            .unwrap();
        write(&out, "schema1_old", &older, &expected1);

        let interim = scratch.join("schema2.db");
        dump::backup_copy(&older, &interim).unwrap();
        rusqlite::Connection::open(&interim)
            .unwrap()
            .execute_batch("UPDATE home_meta SET schema_version=2; PRAGMA user_version=2;")
            .unwrap();
        let mut expected2 = expected1.clone();
        expected2["from_schema"] = json!(2);
        write(&out, "schema2", &interim, &expected2);

        let (database, expected) = live(&scratch).await;
        let snapshot = scratch.join("live.db");
        dump::backup_copy(&database, &snapshot).unwrap();
        write(&out, "live", &snapshot, &expected);
    });
    std::fs::remove_dir_all(&scratch).unwrap();
    eprintln!("wrote the legacy fixtures to {}", out.display());
}
