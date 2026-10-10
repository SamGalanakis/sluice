//! The rehearsal end to end on a scratch home with live work (`docs/design/plan-rows.md`
//! §10.10): a rolling run, a run holding a resource lease, a scatter step whose first
//! instance already failed and a running direct call. On a backup-API copy, the old release's
//! own drain, cancel, intent check and adoption pass bring the copy to zero blockers and the
//! report says how each run actually ended and what to do about it; the original home and a
//! real unit named like a copied run are left as they were. A cancel the old catalog refuses
//! stops the rehearsal with the refusal named and nothing adopted. The report's types are the
//! contract's, and the old release refuses a schema-3 home before changing a byte.

use cutover_rehearsal::{
    dump,
    rehearse::{self, Options},
    report::{CancelRefusal, CutoverReport, RetryAdvice, StopRequest},
    scratch::{Launcher, command, data, journal, map},
};
use serde_json::{Value, json};
use sluice_model::{commands::StepStatus, error::PublicError, ids::ProjectId};
use sluice_process::journal::PayloadResult;
use sluice_runtime::{
    coordinator::Coordinator, dispatch::Catalog, execution::Launch, scheduler::reconcile_project,
};
use std::{path::Path, process::Command};

const DEADLINE: &str = "2026-10-12T18:00:00Z";

struct Live {
    _root: tempfile::TempDir,
    home: std::path::PathBuf,
    launches: Vec<Launch>,
    call: String,
}

fn selector(project: ProjectId) -> Value {
    json!({"kind": "id", "value": project})
}

/// A scratch home, built and left with live work by the old coordinator.
async fn live_home() -> Live {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let host = Launcher::default();
    let broker = Coordinator::open(home.clone(), Catalog::fixtures(), host.clone())
        .await
        .unwrap();
    let sluice_model::commands::CommandReply::Project(project) = command(
        &broker,
        json!({"command": "project_create", "args": {"name": "lash", "description": "",
            "icon": null, "resources": {"cpu": 1}, "author": "test"}}),
    )
    .await
    .unwrap() else {
        panic!("a project")
    };
    let project = project.project_id;
    command(&broker, json!({"command": "plan_patch", "args": {"project": selector(project),
        "rev": 1, "ops": [{"op": "replace", "path": "", "value": {"steps": {
            "tests-main": {"run": "fixture.echo", "tags": ["unit:tests-main", "rolling"],
                "in": {"value": {"default": 1}}},
            "build": {"run": "fixture.echo", "needs": {"cpu": 1}, "in": {"value": {"default": 2}}},
            "fan": {"run": "fixture.echo", "scatter": "value", "in": {"value": {"default": [1, 2]}}},
            "done": {"run": "fixture.echo", "in": {"value": {"default": 3}}}
        }}}], "start": true, "dry_run": false, "reason": "test", "author": "test"}}))
    .await
    .unwrap();
    broker.acquire_scheduler("s".into()).await.unwrap();
    reconcile_project(&broker, project, "s").await.unwrap();
    let launches = host.launches();
    let step = |launch: &Launch| launch.identity.step.as_ref().map(|s| s.to_string());
    assert_eq!(launches.len(), 5, "tests-main, build, fan twice and done");
    for launch in &launches {
        match step(launch).as_deref() {
            Some("done") => broker
                .complete(journal(
                    launch,
                    PayloadResult::Succeeded(map(json!({"value": 3}))),
                ))
                .await
                .map(|_| ())
                .unwrap(),
            Some("fan") if launch.invocation.inputs.0["value"].as_value() == &json!(1) => broker
                .complete(journal(
                    launch,
                    PayloadResult::Failed(PublicError::FnFailure {
                        message: "the first instance failed by itself".into(),
                    }),
                ))
                .await
                .map(|_| ())
                .unwrap(),
            _ => {}
        }
    }
    let call = data(
        &broker,
        json!({"command": "fn_call", "args": {"name": "fixture.wait",
        "inputs": {"value": 9}, "project": selector(project), "wait_seconds": 0, "direct": true,
        "author": "test"}}),
    )
    .await;
    let call = call["call"]
        .as_str()
        .or(call["call_id"].as_str())
        .unwrap_or_else(|| panic!("{call}"))
        .to_owned();
    broker.release_scheduler("s".into()).await.unwrap();
    let status = sluice_runtime::drain::status(broker.reads()).await.unwrap();
    assert!(
        status.blockers.iter().any(|b| b.kind == "lease.held"),
        "{:?}",
        status.blockers
    );
    let launches = host.launches();
    drop(broker);
    Live {
        _root: root,
        home,
        launches,
        call,
    }
}

/// A private copy as the helper takes it: the database through the backup API into `/tmp/cr.*`.
fn copy(home: &Path) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::Builder::new()
        .prefix("cr.")
        .tempdir_in("/tmp")
        .unwrap();
    let copy = root.path().join("home");
    std::fs::create_dir(&copy).unwrap();
    dump::backup_copy(&home.join("sluice.db"), &copy.join("sluice.db")).unwrap();
    (root, copy)
}

fn blockers(home: &Path) -> i64 {
    let sql = rusqlite::Connection::open_with_flags(
        home.join("sluice.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    sql.query_row(
        "SELECT (SELECT count(*) FROM attempts WHERE phase<>'terminal')
              + (SELECT count(*) FROM runs WHERE finished_at IS NULL)
              + (SELECT count(*) FROM leases WHERE state IN ('waiting','held'))
              + (SELECT count(*) FROM calls WHERE status='running')",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// A real unit named as the adoption pass would look a copied run up, stopped when dropped.
struct Unit(String);
impl Unit {
    fn start(name: String) -> Self {
        assert!(name.starts_with("sluice-test-"));
        let out = Command::new("systemd-run")
            .args([
                "--user",
                "--quiet",
                "--collect",
                "--unit",
                &name,
                "/usr/bin/sleep",
                "600",
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        Self(name)
    }
    fn state(&self) -> String {
        let out = Command::new("systemctl")
            .args(["--user", "show", "-p", "ActiveState,MainPID", &self.0])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}
impl Drop for Unit {
    fn drop(&mut self) {
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &self.0])
            .output();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copy_with_live_work_reaches_zero_blockers_through_the_old_cancel_and_adoption() {
    let live = live_home().await;
    let original = blockers(&live.home);
    assert!(
        original >= 6,
        "five live runs, a held lease and a running call: {original}"
    );
    let (_root, copy) = copy(&live.home);
    let rolling = live
        .launches
        .iter()
        .find(|l| {
            l.identity
                .step
                .as_ref()
                .is_some_and(|s| s.as_str() == "tests-main")
        })
        .unwrap();
    let unit = Unit::start(
        sluice_process::systemd::TransientService::for_launch(rolling.identity.run)
            .name()
            .to_owned(),
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let before = unit.state();
    assert!(before.contains("ActiveState=active"), "{before}");

    let options = Options {
        deadline: DEADLINE.into(),
        sha: "4f1c2d3e".into(),
    };
    let (rehearsal, host) = rehearse::rehearse(&copy, Catalog::fixtures(), &options)
        .await
        .unwrap();
    assert!(rehearsal.ok(), "{rehearsal:#?}");
    assert_eq!(blockers(&copy), 0);
    assert_eq!(
        unit.state(),
        before,
        "a unit named like a copied run is untouched"
    );
    assert_eq!(
        blockers(&live.home),
        original,
        "the original home is untouched"
    );
    {
        let asked = host.asked();
        assert!(asked.launches.is_empty() && asked.invocations.is_empty());
        assert_eq!(asked.reconciled.len(), 4, "every live run reconciled once");
    }

    let report = &rehearsal.report;
    assert_eq!(report.reason, rehearse::reason(DEADLINE));
    assert_eq!(
        (report.projects, report.revisions, report.schema),
        (1, 2, 3)
    );
    assert!(report.refused.is_empty());
    let by = |what: &str| {
        report
            .stopped
            .iter()
            .find(|r| {
                r.step.as_ref().map(|s| s.to_string()).as_deref() == Some(what)
                    || r.call.as_deref() == Some(what)
            })
            .unwrap_or_else(|| panic!("{what} in {:#?}", report.stopped))
    };
    for step in ["tests-main", "build"] {
        let run = by(step);
        assert_eq!(run.requested, StopRequest::Cancel);
        assert_eq!(run.outcome.status, StepStatus::Failed, "{run:?}");
        assert_eq!(run.outcome.error.as_deref(), Some("cancelled"), "{run:?}");
        assert_eq!(run.step_status, Some(StepStatus::Failed));
        assert_eq!(run.step_error.as_deref(), Some("cancelled"));
        assert_eq!(run.advice, RetryAdvice::Retry);
        assert_eq!(run.project.as_deref(), Some("lash"));
    }
    // The fan's live instance is cancelled, but the step keeps its first instance's failure.
    let fan = by("fan");
    assert_eq!(fan.outcome.error.as_deref(), Some("cancelled"), "{fan:?}");
    assert_eq!(fan.step_status, Some(StepStatus::Failed));
    assert_eq!(fan.step_error.as_deref(), Some("fn_failure"), "{fan:?}");
    assert_eq!(fan.advice, RetryAdvice::ReadThenRetry);
    let call = by(&live.call);
    assert_eq!(call.requested, StopRequest::Stop);
    assert_eq!(call.outcome.status, StepStatus::Failed, "{call:?}");
    assert_eq!(
        call.outcome.error.as_deref(),
        Some("process_lost"),
        "{call:?}"
    );
    assert_eq!(call.advice, RetryAdvice::CallAgain);
    assert_eq!(report.stopped.len(), 4);
    let line = cutover_rehearsal::report::line(by("tests-main"));
    assert!(
        line.starts_with("stopped lash tests-main ")
            && line.ends_with("requested=cancel outcome=failed:cancelled advice=retry"),
        "{line}"
    );

    // The drain is the cutover's: every project paused by `cutover`.
    let sql = rusqlite::Connection::open(copy.join("sluice.db")).unwrap();
    let (mode, owner): (String, String) = sql
        .query_row("SELECT mode, owner FROM maintenance", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((mode.as_str(), owner.as_str()), ("drain", "cutover"));
    let cancels: i64 = sql
        .query_row(
            "SELECT count(*) FROM records WHERE kind='step.cancel'
             AND json_extract(payload,'$.author')='cutover' AND json_extract(payload,'$.reason')=?1",
            [rehearse::reason(DEADLINE)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cancels, 3, "one cancel per (project, step)");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_the_old_catalog_refuses_stops_the_rehearsal_with_each_run_named() {
    let live = live_home().await;
    let (_root, copy) = copy(&live.home);
    let before = blockers(&copy);
    // The core catalog has no fixture fns: the old dispatch compiles the plan before it
    // cancels, so every cancel is refused before any intent is stored.
    let (rehearsal, host) = rehearse::rehearse(
        &copy,
        Catalog::core(),
        &Options {
            deadline: DEADLINE.into(),
            sha: String::new(),
        },
    )
    .await
    .unwrap();
    assert!(!rehearsal.ok());
    assert!(rehearsal.report.stopped.is_empty());
    assert!(host.asked().reconciled.is_empty(), "nothing adopted");
    assert_eq!(blockers(&copy), before, "no live work ended");
    assert!(!rehearsal.blockers.is_empty());
    let steps: Vec<String> = rehearsal
        .report
        .refused
        .iter()
        .map(|r| r.step.to_string())
        .collect();
    assert_eq!(steps, ["build", "fan", "tests-main"]);
    let fan = &rehearsal.report.refused[1];
    assert_eq!(
        (fan.project.as_str(), fan.runs.len(), fan.attempts.len()),
        ("lash", 1, 1)
    );
    assert!(
        matches!(&fan.error, PublicError::Invalid { message, .. } if message == "invalid stored plan"),
        "{:?}",
        fan.error
    );
}

#[tokio::test]
async fn the_rehearsal_refuses_a_served_home_and_a_copy_holding_a_dotenv() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("coordinator.sock"), "").unwrap();
    let options = Options {
        deadline: DEADLINE.into(),
        sha: String::new(),
    };
    let served = rehearse::rehearse(root.path(), Catalog::core(), &options).await;
    assert!(matches!(served, Err(PublicError::BadRequest { .. })));
    std::fs::remove_file(root.path().join("coordinator.sock")).unwrap();
    std::fs::create_dir_all(root.path().join("fns/x")).unwrap();
    std::fs::write(root.path().join("fns/x/.env"), "TOKEN=secret").unwrap();
    let secret = rehearse::rehearse(root.path(), Catalog::core(), &options).await;
    assert!(
        matches!(&secret, Err(PublicError::BadRequest { message }) if message.contains(".env")),
        "{:?}",
        secret.err()
    );
    std::fs::remove_file(root.path().join("fns/x/.env")).unwrap();
    std::fs::write(
        root.path().join("config.json"),
        r#"{"fn_dirs": ["/home/elsewhere"]}"#,
    )
    .unwrap();
    let outside = rehearse::rehearse(root.path(), Catalog::core(), &options).await;
    assert!(matches!(outside, Err(PublicError::BadRequest { .. })));
}

#[test]
fn the_report_types_round_trip_the_contracts_fixtures() {
    let fixture = |name: &str| -> Value {
        serde_json::from_slice(
            &std::fs::read(format!(
                "{}/tests/fixtures/{name}.json",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap(),
        )
        .unwrap()
    };
    let report = fixture("cutover.report");
    let typed: CutoverReport = serde_json::from_value(report.clone()).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), report);
    let advice: Vec<RetryAdvice> = typed.stopped.iter().map(|r| r.advice).collect();
    assert_eq!(
        advice,
        [
            RetryAdvice::Retry,
            RetryAdvice::None,
            RetryAdvice::ReadThenRetry,
            RetryAdvice::CallAgain
        ]
    );
    for run in &typed.stopped {
        assert_eq!(
            cutover_rehearsal::report::advice(
                run.call.is_some(),
                &run.outcome,
                run.step_status.as_ref(),
                run.step_error.as_deref()
            ),
            run.advice,
            "{run:?}"
        );
    }
    let refused = fixture("cutover.refused");
    let typed: CancelRefusal = serde_json::from_value(refused.clone()).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), refused);
}

/// The four tables schema 3 adds (`docs/design/plan-rows.md` §2.2), enough for an old
/// binary to meet a converted home.
const SCHEMA3_TABLES: &str = "
CREATE TABLE plan_outputs (project_id TEXT NOT NULL, name TEXT NOT NULL,
  position INTEGER NOT NULL, binding TEXT NOT NULL, PRIMARY KEY (project_id, name)) STRICT;
CREATE TABLE plan_refs (project_id TEXT NOT NULL, consumer_kind TEXT NOT NULL,
  consumer_id TEXT NOT NULL, slot TEXT NOT NULL, ordinal INTEGER NOT NULL, kind TEXT NOT NULL,
  source_kind TEXT NOT NULL, source_id TEXT NOT NULL, source_port TEXT NOT NULL DEFAULT '',
  source_path TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (project_id, consumer_kind, consumer_id, slot, ordinal)) STRICT;
CREATE TABLE plan_edges (project_id TEXT NOT NULL, source_step TEXT NOT NULL,
  target_step TEXT NOT NULL, kind TEXT NOT NULL, via_unit TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (project_id, source_step, target_step, kind, via_unit)) STRICT;
CREATE TABLE step_tags (project_id TEXT NOT NULL, step_id TEXT NOT NULL, tag TEXT NOT NULL,
  PRIMARY KEY (project_id, step_id, tag)) STRICT;
UPDATE home_meta SET schema_version=3;
PRAGMA user_version=3;
";

#[test]
fn the_old_release_refuses_a_schema_3_home_before_changing_a_byte() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    drop(sluice_store::Writer::open(&home).unwrap());
    let database = home.join("sluice.db");
    {
        let sql = rusqlite::Connection::open(&database).unwrap();
        sql.execute_batch(SCHEMA3_TABLES).unwrap();
        sql.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
    }
    let bytes = std::fs::read(&database).unwrap();
    let expected = "unsupported schema version 3; expected 1";
    let writer = match sluice_store::Writer::open(&home) {
        Ok(_) => panic!("the writer refuses"),
        Err(error) => error,
    };
    assert_eq!(writer.to_string(), expected);
    let reader = match sluice_store::ReadPool::open(&home, 1) {
        Ok(_) => panic!("readers refuse"),
        Err(error) => error,
    };
    assert_eq!(reader.to_string(), expected);
    let restored = sluice_store::backup::restore_into_fresh_home(&database, &root.path().join("r"))
        .expect_err("a restore refuses");
    assert_eq!(restored.to_string(), expected);
    assert_eq!(
        std::fs::read(&database).unwrap(),
        bytes,
        "not a byte changed"
    );
    let wal = home.join("sluice.db-wal");
    assert!(
        !wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0,
        "the write-ahead log holds nothing"
    );
}
