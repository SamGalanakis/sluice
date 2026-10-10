//! Lane H2's compatibility gates for `sluice home migrate` (`docs/design/plan-rows.md` §10.2,
//! §10.7, §10.8): the binary converts a legacy home in place and reports it, the converted
//! home serves `plan_get` and `plan_history` back to rev 1, `--dry-run` changes nothing, a
//! blocked home is refused and left as it was, and a conversion killed at any moment leaves
//! the home either untouched or converted whole, never between (one transaction). The legacy
//! homes are `tests/fixtures/legacy/`, built by the old release.
#[path = "../../../tests/support/plan_rows.rs"]
mod plan_rows;
#[allow(dead_code)]
#[path = "../../../tests/support/mod.rs"]
mod support;

use plan_rows::{compact, load_dump, logical_dump};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};
use support::home::ScratchHome;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/legacy")
}
fn read(name: &str) -> String {
    std::fs::read_to_string(fixtures().join(name)).unwrap()
}
fn sluice(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sluice"));
    command
        .env("SLUICE_HOME", home)
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_AUTHOR")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT");
    command
}
fn run(home: &Path, args: &[&str]) -> Output {
    sluice(home).args(args).output().unwrap()
}
fn json_of(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
/// A scratch home holding a legacy fixture's database, in WAL mode as a real home is.
fn legacy_home(name: &str, defect: Option<&str>) -> ScratchHome {
    let home = ScratchHome::new().unwrap();
    let database = home.path().join("sluice.db");
    load_dump(&database, &read(&format!("{name}.sql"))).unwrap();
    let sql = Connection::open(&database).unwrap();
    if let Some(defect) = defect {
        sql.execute_batch(defect).unwrap();
    }
    let _: String = sql
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .unwrap();
    home
}
fn dump(home: &Path) -> String {
    logical_dump(&Connection::open(home.join("sluice.db")).unwrap()).unwrap()
}

#[test]
fn home_migrate_converts_a_legacy_home_in_place_and_it_serves_its_plans_and_history() {
    let expected: Value = serde_json::from_str(&read("schema1.expected.json")).unwrap();
    let home = legacy_home("schema1", None);
    let migrated = run(home.path(), &["home", "migrate", "--json"]);
    assert!(migrated.status.success(), "{migrated:?}");
    let report = json_of(&migrated);
    assert_eq!(report["from_schema"], 1);
    let reported = report["projects"].as_array().unwrap();
    for project in expected["projects"].as_array().unwrap() {
        let name = project["name"].as_str().unwrap();
        let line = reported
            .iter()
            .find(|p| p["name"] == name)
            .unwrap_or_else(|| panic!("{name} in {report}"));
        let revisions = project["revisions"].as_array().unwrap();
        assert_eq!(line["revisions"], revisions.len(), "{name}");
        for count in ["steps", "inputs", "outputs"] {
            assert_eq!(line[count], project[count], "{name} {count}");
        }
        assert_eq!(
            line["attempt_snapshots_removed"], project["attempt_snapshots"],
            "{name}"
        );
        assert!(line["project_id"].is_string() && line["records_rewritten"].is_u64());

        // plan_get exports the last revision exactly; plan_history reaches rev 1.
        let got = json_of(&run(
            home.path(),
            &["tool", "plan_get", &json!({"project": name}).to_string()],
        ));
        assert_eq!(got["rev"], revisions.len());
        assert_eq!(
            compact(got["plan"].as_object().unwrap()),
            compact(revisions.last().unwrap()["document"].as_object().unwrap()),
            "{name}: plan_get"
        );
        let mut after: Option<u64> = None;
        let mut revs = Vec::new();
        loop {
            let mut args = json!({"project": name, "limit": 7});
            if let Some(seq) = after {
                args["after_seq"] = json!(seq);
            }
            let page = json_of(&run(
                home.path(),
                &["tool", "plan_history", &args.to_string()],
            ));
            for entry in page["entries"].as_array().unwrap() {
                if entry["kind"] == "plan.edit" {
                    revs.push(entry["rev"].as_u64().unwrap());
                    if entry["rev"] == 1 {
                        assert_eq!(
                            entry["changes"],
                            json!([{"op": "header.put", "root_order": ["steps"]}])
                        );
                    }
                }
            }
            match page["next_after_seq"].as_u64() {
                Some(seq) => after = Some(seq),
                None => break,
            }
        }
        assert_eq!(
            revs,
            (1..=revisions.len() as u64).collect::<Vec<_>>(),
            "{name}: every revision, oldest first, never trimmed"
        );
    }
    let warnings = report["warnings"].to_string();
    for warned in expected["warnings"].as_array().unwrap() {
        assert!(
            warnings.contains(warned["project"].as_str().unwrap()),
            "a warning about {}: {warnings}",
            warned["about"]
        );
    }
    // A second run finds nothing to convert and changes nothing.
    let before = dump(home.path());
    let again = run(home.path(), &["home", "migrate", "--json"]);
    assert_eq!(dump(home.path()), before, "{again:?}");
}

#[test]
fn home_migrate_dry_run_reports_the_conversion_and_changes_nothing() {
    let home = legacy_home("schema2", None);
    let database = home.path().join("sluice.db");
    let bytes = std::fs::read(&database).unwrap();
    let before = dump(home.path());
    let dry = run(home.path(), &["home", "migrate", "--dry-run", "--json"]);
    assert!(dry.status.success(), "{dry:?}");
    let report = json_of(&dry);
    assert_eq!(report["from_schema"], 2);
    assert_eq!(report["projects"].as_array().unwrap().len(), 3);
    assert_eq!(dump(home.path()), before);
    assert_eq!(
        std::fs::read(&database).unwrap(),
        bytes,
        "not a byte changed"
    );
}

#[test]
fn home_migrate_refuses_a_blocked_home_and_live_work_and_leaves_each_as_it_was() {
    let defects: Vec<Value> = serde_json::from_str(&read("defects.json")).unwrap();
    for defect in defects {
        let case = defect["case"].as_str().unwrap();
        let home = legacy_home("schema1", defect["sql"].as_str());
        let before = dump(home.path());
        let refused = run(home.path(), &["home", "migrate", "--json"]);
        assert!(!refused.status.success(), "{case}: refused");
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&refused.stdout),
            String::from_utf8_lossy(&refused.stderr)
        );
        let project = defect["project"].as_str().unwrap();
        assert!(
            said.contains(project) && said.contains(&defect["rev"].to_string()),
            "{case}: names {project} and rev {}: {said}",
            defect["rev"]
        );
        assert_eq!(dump(home.path()), before, "{case}: left as it was");
    }
    let live: Value = serde_json::from_str(&read("live.expected.json")).unwrap();
    let home = legacy_home("live", None);
    let before = dump(home.path());
    let refused = run(home.path(), &["home", "migrate", "--json"]);
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr).into_owned()
        + &String::from_utf8_lossy(&refused.stdout);
    for (what, count) in [
        ("attempts", &live["live"]["attempts"]),
        ("runs", &live["live"]["runs"]),
        ("leases", &live["live"]["leases"]),
        ("calls", &live["live"]["calls"]),
    ] {
        assert!(
            said.contains(&format!("{count} {what}")) || said.contains(what),
            "{said}"
        );
    }
    assert_eq!(dump(home.path()), before);
}

/// A plan with `revisions` revisions, each adding one step, inserted beside the fixture's
/// projects so the conversion takes long enough to be killed part way.
fn bulk(revisions: usize) -> String {
    let project = "0199c3a4-5b6e-7f80-9a1b-000000000b01";
    let mut sql = format!(
        "INSERT INTO projects(project_id, name, created_at) VALUES ('{project}', 'bulk', '2026-10-01T00:00:00Z');\n"
    );
    let mut steps = serde_json::Map::new();
    for rev in 1..=revisions {
        let ops = if rev == 1 {
            json!([])
        } else {
            let step = format!("s{rev:05}");
            let declaration = json!({"run": "fixture.echo", "in": {"value": {"default": rev}}});
            steps.insert(step.clone(), declaration.clone());
            sql.push_str(&format!(
                "INSERT INTO steps(project_id, step_id, position, generation, declaration) VALUES ('{project}', '{step}', {}, {rev}, '{}');\n",
                rev - 2,
                declaration
            ));
            json!([{"op": "add", "path": format!("/steps/{step}"), "value": declaration}])
        };
        sql.push_str(&format!(
            "INSERT INTO plan_edits(project_id, rev, seq, at, author, reason, ops) VALUES ('{project}', {rev}, {}, '2026-10-01T00:00:00Z', 'bulk', '', '{ops}');\n",
            900_000 + rev
        ));
    }
    sql.push_str(&format!(
        "INSERT INTO plans(project_id, rev, doc) VALUES ('{project}', {revisions}, '{}');\n",
        json!({"steps": steps})
    ));
    sql
}

#[test]
fn a_conversion_killed_at_any_moment_leaves_the_home_untouched_or_converted_whole() {
    let home = legacy_home("schema1", Some(&bulk(1500)));
    let original = dump(home.path());
    let mut delay = Duration::from_millis(2);
    let mut interrupted = 0;
    loop {
        let mut child = sluice(home.path())
            .args(["home", "migrate", "--json"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(delay);
        let finished = child.try_wait().unwrap().is_some();
        if !finished {
            child.kill().unwrap();
        }
        child.wait().unwrap();
        let sql = Connection::open(home.path().join("sluice.db")).unwrap();
        let version: i64 = sql
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        match version {
            1 => {
                assert_eq!(
                    dump(home.path()),
                    original,
                    "killed after {delay:?}: untouched"
                );
                interrupted += 1;
            }
            3 => {
                let rev: i64 = sql
                    .query_row(
                        "SELECT rev FROM plans JOIN projects USING(project_id) WHERE name='bulk'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                let edits: i64 = sql
                    .query_row(
                        "SELECT count(*) FROM plan_edits JOIN projects USING(project_id) WHERE name='bulk'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!((rev, edits), (1500, 1500), "converted whole");
                break;
            }
            other => panic!("killed after {delay:?}: the home is at schema {other}"),
        }
        assert!(
            delay < Duration::from_secs(120),
            "the conversion never finished"
        );
        delay = delay * 3 / 2 + Duration::from_millis(1);
    }
    assert!(interrupted > 0, "at least one run was killed part way");
    eprintln!("{interrupted} conversions killed part way, each leaving the home untouched");
}
