#[path = "../src/import_python_home.rs"]
mod import_python_home;

use import_python_home::{FailurePoint, Options, import};
use rusqlite::Connection;
use serde_json::{Value, json};
use sluice_agents::{
    engines::{
        EngineAdapter,
        claude::Claude,
        codex::{Codex, CodexOptions},
        devin::{Devin, DevinOptions},
    },
    supervisor::{Checkpoint, State},
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    _scratch: tempfile::TempDir,
    source: PathBuf,
    staging: PathBuf,
    destination: PathBuf,
    connection: Connection,
}
impl Fixture {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source");
        let staging = scratch.path().join("staging");
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_home/source"),
            &source,
        );
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_home/staging"),
            &staging,
        );
        let mapping = source.join("codex-native-sessions/fake-session.json");
        let raw = fs::read_to_string(&mapping)
            .unwrap()
            .replace("@SOURCE@", source.to_str().unwrap());
        fs::write(mapping, raw).unwrap();
        let state =
            Connection::open(source.join("codex-native-homes/generation/state_5.sqlite")).unwrap();
        state
            .execute(
                "UPDATE threads SET rollout_path=replace(rollout_path,'@SOURCE@',?1)",
                [source.to_str().unwrap()],
            )
            .unwrap();
        drop(state);
        let connection = Connection::open(source.join("sluice.db")).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        let raw: String = connection
            .query_row("SELECT doc FROM plans WHERE project='fixture'", [], |r| {
                r.get(0)
            })
            .unwrap();
        connection
            .execute(
                "UPDATE plans SET doc=?1 WHERE project='fixture'",
                [raw.replace("@SOURCE@", source.to_str().unwrap())],
            )
            .unwrap();
        let destination = scratch.path().join("destination");
        Self {
            _scratch: scratch,
            source,
            staging,
            destination,
            connection,
        }
    }
    fn options(&self, failure: FailurePoint) -> Options {
        Options {
            source: self.source.clone(),
            destination: self.destination.clone(),
            staging: self.staging.clone(),
            failure,
        }
    }
    fn imported(&self) -> Connection {
        Connection::open(self.destination.join("sluice.db")).unwrap()
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            copy_tree(&path, &dst.join(path.file_name().unwrap()));
        } else {
            fs::copy(&path, dst.join(path.file_name().unwrap())).unwrap();
        }
    }
}
fn fingerprints(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap().path();
            if entry.is_dir() {
                walk(root, &entry, out);
            } else {
                out.insert(
                    entry.strip_prefix(root).unwrap().to_owned(),
                    fs::read(entry).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
fn json_query(db: &Connection, sql: &str) -> Value {
    let raw: String = db.query_row(sql, [], |r| r.get(0)).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn imports_the_contract_table_and_preserves_the_source_including_wal() {
    let fixture = Fixture::new();
    assert!(
        fixture
            .source
            .join("sluice.db-wal")
            .metadata()
            .unwrap()
            .len()
            > 0
    );
    let before = fingerprints(&fixture.source);
    let report = import(&fixture.options(FailurePoint::None)).unwrap();
    assert_eq!(before, fingerprints(&fixture.source));
    assert_eq!(report["projects"], 2);
    assert_eq!(
        report["steps_by_state"],
        json!({"succeeded":2,"failed":1,"stale":1,"skipped":1,"pending":3,"running":2})
    );
    assert_eq!(report["interrupted_runs"], 2);
    assert_eq!(report["sessions_copied"], 2);
    assert_eq!(report["questions_converted"], 1);
    let db = fixture.imported();
    assert_eq!(
        db.query_row("SELECT count(*) FROM projects WHERE paused=1", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(
        db.query_row(
            "SELECT archived FROM projects WHERE name='archived'",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    let ledger: Value =
        serde_json::from_slice(&fs::read(fixture.destination.join("import-ledger.json")).unwrap())
            .unwrap();
    assert_eq!(ledger["projects"]["fixture"]["original_paused"], false);
    assert_eq!(ledger["projects"]["archived"]["original_paused"], true);
    let id: String = db
        .query_row(
            "SELECT project_id FROM projects WHERE name='fixture'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(id.parse::<sluice_model::ids::ProjectId>().is_ok());
    let done: (String, bool, String) = db
        .query_row(
            "SELECT outputs,manual,inputs_hash FROM steps WHERE step_id='done'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&done.0).unwrap(),
        json!({"value":"kept","ok":true})
    );
    assert!(done.1);
    assert_eq!(done.2.len(), 64);
    assert_ne!(done.2, "old-python-hash");
    for (step, status) in [
        ("done", "succeeded"),
        ("failed", "failed"),
        ("stale", "stale"),
        ("skip", "skipped"),
        ("wait", "pending"),
        ("work", "failed"),
        ("scatter", "failed"),
    ] {
        let found: String = db
            .query_row("SELECT status FROM steps WHERE step_id=?1", [step], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(found, status);
    }
    assert_eq!(
        json_query(&db, "SELECT error FROM steps WHERE step_id='work'")["message"],
        import_python_home::INTERRUPTED
    );
    assert_eq!(
        json_query(&db, "SELECT error FROM steps WHERE step_id='failed'")["message"],
        "old failure"
    );
    assert_eq!(
        json_query(&db, "SELECT paused FROM steps WHERE step_id='failed'"),
        "owner review"
    );
    assert_eq!(
        json_query(&db, "SELECT skipped FROM steps WHERE step_id='skip'"),
        json!([{"kind":"boolean","reference":"enabled","value":false,"negate":false}])
    );
    let scatter = json_query(&db, "SELECT instances FROM steps WHERE step_id='scatter'");
    assert_eq!(scatter["0"]["status"], "succeeded");
    assert_eq!(scatter["0"]["outputs"]["final"], "good item");
    assert_eq!(scatter["1"]["status"], "failed");
    assert_eq!(
        db.query_row("SELECT done FROM steps WHERE step_id='scatter'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    let plan = json_query(
        &db,
        "SELECT doc FROM plans JOIN projects USING(project_id) WHERE name='fixture'",
    );
    assert!(plan["steps"]["old-a"].get("when").is_none());
    assert_eq!(plan["steps"]["old-a"]["after"], json!(["done/ok"]));
    assert_eq!(plan["steps"]["wait"]["after"], json!(["done"]));
    assert_eq!(plan["steps"]["tagged"]["tags"], json!(["unit:separate"]));
    assert_eq!(
        plan["steps"]["tagged"]["in"]["from_other"]["source"],
        "old-b/value"
    );
    assert_eq!(
        plan["steps"]["old-a"]["tags"],
        plan["steps"]["old-b"]["tags"]
    );
    for table in [
        "calls",
        "leases",
        "notification_attempts",
        "message_deliveries",
    ] {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM messages WHERE needs_reply=1 AND \"to\"='owner'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM question_attachments WHERE detached_at IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        fs::read_to_string(fixture.destination.join(".env")).unwrap(),
        "FIXTURE_SECRET=synthetic-only\n"
    );
    assert_eq!(
        fs::metadata(fixture.destination.join(".env"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        !fixture
            .destination
            .join("projects")
            .join(&id)
            .join("runs/scalar-run")
            .exists()
    );
    let pool = sluice_store::ReadPool::open(&fixture.destination, 1).unwrap();
    drop(pool);
}

#[test]
fn same_snapshot_is_a_byte_for_byte_noop_and_changed_snapshot_is_refused() {
    let fixture = Fixture::new();
    let report = import(&fixture.options(FailurePoint::None)).unwrap();
    let before = fingerprints(&fixture.destination);
    assert_eq!(
        report,
        import(&fixture.options(FailurePoint::None)).unwrap()
    );
    assert_eq!(before, fingerprints(&fixture.destination));
    fixture
        .connection
        .execute(
            "UPDATE projects SET description='changed' WHERE name='fixture'",
            [],
        )
        .unwrap();
    assert!(
        import(&fixture.options(FailurePoint::None))
            .unwrap_err()
            .to_string()
            .contains("different")
    );
    assert_eq!(before, fingerprints(&fixture.destination));
}

#[test]
fn crash_recovery_reuses_ids_and_rolls_back_partial_rows() {
    for failure in [
        FailurePoint::MidCommit,
        FailurePoint::AfterCommit,
        FailurePoint::BeforePublish,
    ] {
        let fixture = Fixture::new();
        let before = fingerprints(&fixture.source);
        assert!(import(&fixture.options(failure)).is_err());
        assert!(!fixture.destination.exists());
        let work = fixture
            .destination
            .parent()
            .unwrap()
            .join(".destination.python-import");
        let allocated: Value =
            serde_json::from_slice(&fs::read(work.join("ledger.json")).unwrap()).unwrap();
        if failure == FailurePoint::MidCommit {
            let db = Connection::open(work.join("home/sluice.db")).unwrap();
            assert_eq!(
                db.query_row("SELECT count(*) FROM projects", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        import(&fixture.options(FailurePoint::None)).unwrap();
        let committed: Value = serde_json::from_slice(
            &fs::read(fixture.destination.join("import-ledger.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(allocated["projects"], committed["projects"]);
        for step in ["work", "scatter"] {
            for ids in committed["projects"]["fixture"]["steps"][step]["predecessors"]
                .as_object()
                .unwrap()
                .values()
            {
                let directory = fixture
                    .destination
                    .join("runs")
                    .join(ids["run"].as_str().unwrap());
                if let Some(checkpoint) = Checkpoint::read(&directory).unwrap() {
                    assert_eq!(json!(checkpoint.attempt), ids["attempt"]);
                    assert_eq!(json!(checkpoint.invocation), ids["invocation"]);
                } else {
                    let result = json_query(
                        &fixture.imported(),
                        &format!(
                            "SELECT result FROM runs WHERE run_id='{}'",
                            ids["run"].as_str().unwrap()
                        ),
                    );
                    assert_eq!(result["status"], "succeeded");
                }
            }
        }
        assert_eq!(before, fingerprints(&fixture.source));
        assert_eq!(
            fixture
                .imported()
                .query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
    }
}

#[test]
fn interrupted_predecessors_have_resume_data_and_no_process_identity() {
    let fixture = Fixture::new();
    let generation = fixture.source.join("codex-native-homes/generation");
    fs::set_permissions(
        generation.join("sessions"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    fs::set_permissions(
        generation.join("state_5.sqlite"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    let unused = fixture.source.join("codex-native-homes/unused-generation");
    fs::create_dir_all(&unused).unwrap();
    fs::write(unused.join("do-not-copy"), "unrelated session").unwrap();
    import(&fixture.options(FailurePoint::None)).unwrap();
    let db = fixture.imported();
    let mut query = db.prepare("SELECT r.run_id,s.engine,s.cwd,s.session_id,s.metadata,a.request FROM runs r JOIN sessions s USING(run_id) JOIN attempts a USING(attempt_id) WHERE a.phase='terminal' AND r.finished_at IS NOT NULL AND r.guardian_pid IS NULL AND r.guardian_start IS NULL AND r.unit_name IS NULL AND r.cgroup IS NULL AND r.boot_id IS NULL").unwrap();
    let rows = query
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut adapter = Codex::new(CodexOptions::new(
        "unused".into(),
        fixture.source.parent().unwrap().join("unused-owner"),
        fixture.destination.clone(),
    ));
    for (run, engine, cwd, session, metadata, request) in rows {
        assert_eq!(engine, "codex");
        assert_eq!(cwd, fs::canonicalize("/tmp").unwrap().to_str().unwrap());
        assert_eq!(session, "fake-session");
        let metadata: Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(metadata, json!({}));
        let directory = fixture.destination.join("runs").join(&run);
        let checkpoint = Checkpoint::read(&directory).unwrap().unwrap();
        assert_eq!(checkpoint.run.to_string(), run);
        assert_eq!(checkpoint.engine, engine);
        assert_eq!(checkpoint.cwd, Path::new(&cwd));
        assert_eq!(checkpoint.session.as_deref(), Some(session.as_str()));
        assert_eq!(checkpoint.head_before.as_deref(), Some("abcdef"));
        assert_eq!(checkpoint.state, State::Done);
        assert_eq!(checkpoint.internal_attempt, 1);
        assert_eq!(checkpoint.started_ms, 0);
        assert_eq!(checkpoint.live_after.0, 0);
        assert!(checkpoint.delivery.entries.is_empty());
        assert_eq!((checkpoint.nudges, checkpoint.compactions), (0, 0));
        assert!(!checkpoint.reminded);
        let resolved = runtime
            .block_on(adapter.session(&session))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.id, session);
        assert_eq!(resolved.cwd, checkpoint.cwd);
        let private = adapter.session_home(&session).unwrap().unwrap();
        assert_eq!(
            private,
            fixture
                .destination
                .join("codex-native-homes")
                .join(&session)
        );
        assert!(
            private
                .join("sessions/2026/10/03/rollout-fixture-fake-session.jsonl")
                .is_file()
        );
        assert!(private.join("state_5.sqlite").is_file());
        let state = Connection::open(private.join("state_5.sqlite")).unwrap();
        let rollout: String = state
            .query_row(
                "SELECT rollout_path FROM threads WHERE id='fake-session'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(Path::new(&rollout).starts_with(&private));
        assert!(Path::new(&rollout).exists());
        assert!(!private.join("config.toml").exists());
        assert!(!private.join("auth.json").exists());
        assert!(!private.join("session.json").exists());
        assert!(!fixture.destination.join("engine-homes").exists());
        assert!(
            !fixture
                .destination
                .join("codex-native-homes/unused-generation")
                .exists()
        );
        assert_eq!(
            fs::metadata(private.join("sessions"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
        assert_eq!(
            fs::metadata(private.join("state_5.sqlite"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(directory.join("native.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["provenance"]["head_before"], "abcdef");
        assert_eq!(request["inputs"]["cwd"], cwd);
        let attempt: String = db
            .query_row("SELECT attempt_id FROM runs WHERE run_id=?1", [&run], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(checkpoint.attempt.to_string(), attempt);
        if request["item_count"].is_number() {
            assert_eq!(request["item_count"], 2);
            assert_eq!(request["inputs"]["spec"], "second");
            assert_eq!(
                request["effective_inputs"]["spec"],
                json!(["first", "second"])
            );
        }
    }
    assert_eq!(
        json_query(&db, "SELECT outputs FROM submissions WHERE step_id='work'")["final"],
        "submitted progress"
    );
}

#[test]
fn external_predecessors_resolve_through_their_native_adapters() {
    for engine in ["claude", "devin"] {
        for scenario in [
            "valid",
            "missing",
            "cwd_mismatch",
            "unsupported_schema",
            "escaping_transcript",
            "missing_cwd",
        ] {
            if (engine == "claude" && scenario == "unsupported_schema")
                || (engine == "devin" && scenario == "escaping_transcript")
            {
                continue;
            }
            let fixture = Fixture::new();
            let owner = fixture.source.parent().unwrap().join("owner");
            let cwd = if scenario == "missing_cwd" {
                owner.join("missing-cwd")
            } else {
                fs::canonicalize("/tmp").unwrap()
            };
            let session = "imported-external-session";
            let claude_home = owner.join("claude");
            let data_home = owner.join("data");
            if engine == "claude" {
                let project = claude_home.join("projects/scratch");
                fs::create_dir_all(&project).unwrap();
                fs::write(
                    project.join(format!("{session}.jsonl")),
                    format!("{}\n", json!({"type":"user","cwd":cwd,"sessionId":session})),
                )
                .unwrap();
            } else {
                fs::create_dir_all(data_home.join("devin/cli")).unwrap();
                let db = Connection::open(data_home.join("devin/cli/sessions.db")).unwrap();
                db.execute_batch(
                    "CREATE TABLE sessions(id TEXT PRIMARY KEY,working_directory TEXT);",
                )
                .unwrap();
                db.execute(
                    "INSERT INTO sessions VALUES(?1,?2)",
                    (session, cwd.to_str().unwrap()),
                )
                .unwrap();
            }
            for run in ["scalar-run", "scatter-busy"] {
                fs::write(fixture.source.join("projects/fixture/runs").join(run).join("native.json"), json!({"engine":engine,"session":session,"cwd":cwd,"git":{"head_before":"original-head","head_after":"later-head"},"final":"saved progress","pid":999999}).to_string()).unwrap();
            }
            let transcript = claude_home
                .join("projects/scratch")
                .join(format!("{session}.jsonl"));
            if engine == "claude" {
                match scenario {
                    "missing" => fs::remove_file(&transcript).unwrap(),
                    "cwd_mismatch" => {
                        fs::write(&transcript, format!("{}\n", json!({"cwd":fixture.source})))
                            .unwrap()
                    }
                    "escaping_transcript" => {
                        let outside = owner.join("outside.jsonl");
                        fs::rename(&transcript, &outside).unwrap();
                        std::os::unix::fs::symlink(outside, &transcript).unwrap();
                    }
                    _ => {}
                }
            } else {
                let db = Connection::open(data_home.join("devin/cli/sessions.db")).unwrap();
                match scenario {
                    "missing" => {
                        db.execute("DELETE FROM sessions", []).unwrap();
                    }
                    "cwd_mismatch" => {
                        db.execute(
                            "UPDATE sessions SET working_directory=?1",
                            [fixture.source.to_str().unwrap()],
                        )
                        .unwrap();
                    }
                    "unsupported_schema" => db.execute_batch("PRAGMA user_version=7;").unwrap(),
                    _ => {}
                }
            }
            let before = fingerprints(&owner);
            let source_before = fingerprints(&fixture.source);
            let output = Command::new(env!("CARGO_BIN_EXE_sluice"))
                .arg("import-python-home")
                .arg(&fixture.source)
                .arg(&fixture.destination)
                .arg("--staging")
                .arg(&fixture.staging)
                .env("HOME", &owner)
                .env("CLAUDE_CONFIG_DIR", &claude_home)
                .env("XDG_DATA_HOME", &data_home)
                .output()
                .unwrap();
            assert_eq!(before, fingerprints(&owner));
            assert_eq!(source_before, fingerprints(&fixture.source));
            if !["valid", "missing_cwd"].contains(&scenario) {
                assert!(!output.status.success(), "{engine} accepted {scenario}");
                assert!(!fixture.destination.exists());
                let error = String::from_utf8_lossy(&output.stderr);
                let expected = match scenario {
                    "missing" => "engine session is missing",
                    "cwd_mismatch" => "session cwd differs",
                    "unsupported_schema" => "unsupported Devin sessions.db schema",
                    "escaping_transcript" => "session transcript escapes home",
                    _ => unreachable!(),
                };
                assert!(error.contains(expected), "{error}");
                continue;
            }
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["sessions_copied"], 2);
            assert_eq!(report["session_imports"].as_array().unwrap().len(), 2);
            if scenario == "missing_cwd" {
                assert!(!cwd.exists());
                assert!(report["exceptions"].as_array().unwrap().iter().any(|e| {
                    e["resume_validation_error"]
                        .as_str()
                        .is_some_and(|s| s.contains("cwd is missing"))
                }));
            }
            assert!(!fixture.destination.join("codex-native-homes").exists());
            let db = fixture.imported();
            let runs: Vec<String> = db
                .prepare("SELECT run_id FROM sessions WHERE engine=?1")
                .unwrap()
                .query_map([engine], |r| r.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(runs.len(), 2);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            for run in runs {
                let checkpoint = Checkpoint::read(&fixture.destination.join("runs").join(run))
                    .unwrap()
                    .unwrap();
                assert_eq!(checkpoint.engine, engine);
                assert_eq!(checkpoint.cwd, cwd);
                assert_eq!(checkpoint.session.as_deref(), Some(session));
                assert_eq!(checkpoint.head_before.as_deref(), Some("original-head"));
                assert_eq!(checkpoint.final_text, "saved progress");
                assert_eq!(checkpoint.state, State::Done);
                let resolved = if engine == "claude" {
                    let mut adapter = Claude::new(
                        "unused".into(),
                        claude_home.clone(),
                        "unused".into(),
                        checkpoint.run,
                    );
                    if scenario == "missing_cwd" {
                        assert!(runtime.block_on(adapter.session(session)).is_err());
                        adapter.recorded_session(session).unwrap().unwrap()
                    } else {
                        runtime.block_on(adapter.session(session)).unwrap().unwrap()
                    }
                } else {
                    let mut adapter = Devin::new(DevinOptions {
                        data_home: data_home.clone(),
                        ..Default::default()
                    });
                    runtime.block_on(adapter.session(session)).unwrap().unwrap()
                };
                assert_eq!(resolved.id, session);
                assert_eq!(resolved.cwd, cwd);
            }
        }
    }
}

struct ImportHooks;
impl sluice_store::plans::RetryMessages for ImportHooks {
    fn validate_retry(
        &self,
        _tx: &sluice_store::WriteTransaction<'_>,
        _project: sluice_model::ids::ProjectId,
        _steps: &[sluice_model::ids::StepId],
        body: &str,
        _author: &str,
    ) -> sluice_store::Result<()> {
        assert!(!body.is_empty());
        Ok(())
    }
    fn post_retry(
        &mut self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        project: sluice_model::ids::ProjectId,
        step: &sluice_model::ids::StepId,
        body: &str,
        author: &str,
    ) -> sluice_store::Result<()> {
        let post = serde_json::from_value(
            json!({"project":sluice_model::ids::ProjectSelector::Id(project),"to":step,"body":body,"from":author,"needs_reply":false}),
        )?;
        sluice_store::messages::message_post(tx, post, &sluice_store::messages::NoPlanInputs)?;
        Ok(())
    }
}
impl sluice_store::attempts::ExecutionHooks for ImportHooks {
    fn assign(
        &mut self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        id: &sluice_store::attempts::AttemptIdentity,
        cursor: i64,
        exact: Option<&sluice_store::attempts::AssignedRange>,
    ) -> sluice_store::Result<sluice_store::attempts::AssignedRange> {
        assert!(exact.is_none());
        let range = sluice_store::messages::assign_run_range(tx, id.project, id.run, cursor, None)?;
        assert_eq!(range.after.0, cursor);
        Ok(sluice_store::attempts::AssignedRange {
            after: range.after.0,
            through: range.through.0,
        })
    }
    fn started(
        &mut self,
        _tx: &mut sluice_store::WriteTransaction<'_>,
        _id: &sluice_store::attempts::AttemptIdentity,
        _range: &sluice_store::attempts::AssignedRange,
    ) -> sluice_store::Result<()> {
        panic!("data-level test must never start work")
    }
    fn hold(
        &mut self,
        _tx: &mut sluice_store::WriteTransaction<'_>,
        _id: &sluice_store::attempts::AttemptIdentity,
        needs: &[(String, u64)],
        _scatter: bool,
    ) -> sluice_store::Result<()> {
        assert!(needs.is_empty());
        Ok(())
    }
    fn release(
        &mut self,
        _tx: &mut sluice_store::WriteTransaction<'_>,
        _id: &sluice_store::attempts::AttemptIdentity,
    ) -> sluice_store::Result<()> {
        panic!("no process exists to release")
    }
}
struct FixtureSignatures;
impl sluice_model::SignatureProvider for FixtureSignatures {
    fn signature(&self, name: &str) -> Option<sluice_model::FnSignature> {
        if name == "core.external" {
            return Some(sluice_model::FnSignature {
                open: true,
                ..Default::default()
            });
        }
        if name != "fixture.worker" {
            return None;
        }
        Some(sluice_model::FnSignature {
            inputs: [
                ("cwd".into(), sluice_model::types::Type::String),
                ("spec".into(), sluice_model::types::Type::String),
            ]
            .into_iter()
            .collect(),
            outputs: [
                ("final".into(), sluice_model::types::Type::String),
                (
                    "session".into(),
                    sluice_model::types::Type::Optional(Box::new(
                        sluice_model::types::Type::String,
                    )),
                ),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        })
    }
}

#[test]
fn feedback_retry_selects_imported_prev_run_and_never_reserves_a_good_scatter_item() {
    let fixture = Fixture::new();
    import(&fixture.options(FailurePoint::None)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let writer = sluice_store::Writer::open(&fixture.destination).unwrap();
    let imported_home = fixture.destination.clone();
    runtime
        .block_on(
            writer.write(sluice_store::RetrySafety::NonIdempotent, move |tx| {
                use sluice_model::{
                    commands::{StepRetry, StepSelection},
                    ids::{AttemptId, ProjectId, ProjectSelector, Revision, RunId},
                };
                let project: ProjectId = tx
                    .sql()
                    .query_row(
                        "SELECT project_id FROM projects WHERE name='fixture'",
                        [],
                        |r| r.get::<_, String>(0),
                    )?
                    .parse()
                    .unwrap();
                let raw: String = tx.sql().query_row(
                    "SELECT doc FROM plans WHERE project_id=?1",
                    [project.to_string()],
                    |r| r.get(0),
                )?;
                let plan =
                    sluice_model::Plan::parse(&serde_json::from_str(&raw)?, &FixtureSignatures)
                        .unwrap();
                let context = sluice_store::plans::PlanContext {
                    project,
                    revision: Revision(1),
                    plan,
                };
                let mut hooks = ImportHooks;
                sluice_store::plans::step_retry(
                    tx,
                    &context,
                    StepRetry {
                        expected_rev: None,
                        project: ProjectSelector::Id(project),
                        selection: StepSelection {
                            steps: Some(vec!["work".parse().unwrap(), "scatter".parse().unwrap()]),
                            tags: None,
                        },
                        message: Some("sluice was upgraded; continue where you left off".into()),
                        reason: None,
                        author: Some("owner".into()),
                    },
                    &mut hooks,
                )?;
                assert!(tx.sql().query_row(
                    "SELECT paused FROM projects WHERE project_id=?1",
                    [project.to_string()],
                    |r| r.get::<_, bool>(0)
                )?);
                // Simulate the owner's release (cutover step 8): leave the import's cutover
                // maintenance, which fences new reservations.
                tx.sql()
                    .execute("UPDATE maintenance SET mode='normal' WHERE singleton=1", [])?;
                // Release only the scratch project's pause for reservation checks.
                tx.sql().execute(
                    "UPDATE projects SET paused=0 WHERE project_id=?1",
                    [project.to_string()],
                )?;
                for (step, index, count, spec) in [
                    ("work", -1, None, "continue"),
                    ("scatter", 1, Some(2), "second"),
                ] {
                    let step: sluice_model::ids::StepId = step.parse().unwrap();
                    let state = sluice_store::plans::read_state(tx.sql(), project)?;
                    let hash = sluice_model::plan::inputs_hash(
                        &context.plan,
                        &state,
                        &context.plan.steps()[&step],
                    )
                    .unwrap();
                    let prior: String = tx.sql().query_row(
                        "SELECT run_id FROM runs WHERE step_id=?1 AND item_index=?2",
                        rusqlite::params![step.as_str(), index],
                        |r| r.get(0),
                    )?;
                    let reservation = sluice_store::attempts::reserve(
                        tx,
                        &context,
                        sluice_store::attempts::Reserve {
                            step,
                            attempt: AttemptId::new(),
                            run: RunId::new(),
                            item_index: index,
                            item_count: count,
                            inputs: serde_json::from_value(json!({"cwd":"/tmp","spec":spec}))?,
                            inputs_hash: hash,
                            provenance: Default::default(),
                            release_id: "scratch-release".into(),
                            protocol_major: 1,
                        },
                        &mut hooks,
                    )?;
                    assert_eq!(reservation.prev_run.unwrap().to_string(), prior);
                    assert!(reservation.messages.through > reservation.messages.after);
                    let checkpoint =
                        Checkpoint::read(&imported_home.join("runs").join(&prior))?.unwrap();
                    let previous = sluice_agents::supervisor::PreviousSession {
                        engine: checkpoint.engine,
                        cwd: checkpoint.cwd.clone(),
                        session: checkpoint.session,
                    };
                    let assigned = sluice_process::socket::AssignedRange {
                        after: sluice_model::ids::MessageId(reservation.messages.after),
                        through: sluice_model::ids::MessageId(reservation.messages.through),
                    };
                    assert_eq!(
                        sluice_agents::supervisor::select_session(
                            None,
                            Some(&previous),
                            "codex",
                            &checkpoint.cwd,
                            assigned
                        )
                        .as_deref(),
                        Some("fake-session")
                    );
                }
                let state = sluice_store::plans::read_state(tx.sql(), project)?;
                let step: sluice_model::ids::StepId = "scatter".parse().unwrap();
                let hash = sluice_model::plan::inputs_hash(
                    &context.plan,
                    &state,
                    &context.plan.steps()[&step],
                )
                .unwrap();
                let good = sluice_store::attempts::reserve(
                    tx,
                    &context,
                    sluice_store::attempts::Reserve {
                        step,
                        attempt: AttemptId::new(),
                        run: RunId::new(),
                        item_index: 0,
                        item_count: Some(2),
                        inputs: serde_json::from_value(json!({"cwd":"/tmp","spec":"first"}))?,
                        inputs_hash: hash,
                        provenance: Default::default(),
                        release_id: "scratch-release".into(),
                        protocol_major: 1,
                    },
                    &mut hooks,
                );
                assert!(
                    good.unwrap_err()
                        .to_string()
                        .contains("successful scatter item is retained")
                );
                assert_eq!(
                    tx.sql().query_row(
                        "SELECT count(*) FROM runs WHERE item_index=0",
                        [],
                        |r| r.get::<_, i64>(0)
                    )?,
                    1
                );
                Ok(())
            }),
        )
        .unwrap();
    runtime.block_on(writer.shutdown()).unwrap();
}

#[test]
fn refuses_overlap_unrelated_destinations_symlinks_and_missing_checkpoints() {
    let fixture = Fixture::new();
    for destination in [
        fixture.source.clone(),
        fixture.source.join("nested"),
        fixture.source.parent().unwrap().to_owned(),
    ] {
        let options = Options {
            destination,
            ..fixture.options(FailurePoint::None)
        };
        assert!(import(&options).is_err());
    }
    fs::create_dir(&fixture.destination).unwrap();
    fs::write(fixture.destination.join("unrelated"), "preserve").unwrap();
    assert!(import(&fixture.options(FailurePoint::None)).is_err());
    assert_eq!(
        fs::read_to_string(fixture.destination.join("unrelated")).unwrap(),
        "preserve"
    );
    fs::remove_dir_all(&fixture.destination).unwrap();
    let main = fixture
        .staging
        .join("projects/fixture/fns/fixture.worker/main.py");
    fs::remove_file(&main).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", &main).unwrap();
    assert!(import(&fixture.options(FailurePoint::None)).is_err());
    fs::remove_file(main).unwrap();
    fs::remove_file(fixture.source.join(
        "codex-native-homes/generation/sessions/2026/10/03/rollout-fixture-fake-session.jsonl",
    ))
    .unwrap();
    assert!(import(&fixture.options(FailurePoint::None)).is_err());
}

#[test]
fn rejects_invalid_plan_without_coercing_or_committing_any_project() {
    let fixture = Fixture::new();
    let mut plan = json_query(
        &fixture.connection,
        "SELECT doc FROM plans WHERE project='fixture'",
    );
    plan["steps"]["wait"]["when"] = json!("done/value");
    fixture
        .connection
        .execute(
            "UPDATE plans SET doc=?1 WHERE project='fixture'",
            [plan.to_string()],
        )
        .unwrap();
    let error = import(&fixture.options(FailurePoint::None)).unwrap_err();
    assert!(error.to_string().contains("not a boolean"));
    assert!(!fixture.destination.exists());
}

#[test]
fn never_executes_converted_scripts_and_wires_the_cli_mode() {
    let fixture = Fixture::new();
    let output = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .args([
            "import-python-home",
            fixture.source.to_str().unwrap(),
            fixture.destination.to_str().unwrap(),
            "--staging",
            fixture.staging.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["projects"],
        2
    );
    assert!(import_python_home::dispatch_import_python_home().is_none());
}

#[test]
fn missing_published_required_file_prevents_idempotent_success() {
    for path in [
        ".env",
        "codex-native-sessions/fake-session.json",
        "codex-native-homes/fake-session/state_5.sqlite",
        "checkpoint",
    ] {
        let fixture = Fixture::new();
        import(&fixture.options(FailurePoint::None)).unwrap();
        let path = if path == "checkpoint" {
            let run: String = fixture
                .imported()
                .query_row("SELECT run_id FROM sessions LIMIT 1", [], |r| r.get(0))
                .unwrap();
            fixture
                .destination
                .join("runs")
                .join(run)
                .join("native.json")
        } else {
            fixture.destination.join(path)
        };
        fs::remove_file(path).unwrap();
        assert!(import(&fixture.options(FailurePoint::None)).is_err());
    }
}

#[test]
fn interrupted_sessionless_agent_keeps_a_native_checkpoint() {
    let fixture = Fixture::new();
    for run in ["scalar-run", "scatter-busy"] {
        fs::write(
            fixture
                .source
                .join("projects/fixture/runs")
                .join(run)
                .join("native.json"),
            json!({"engine":"codex","cwd":"/tmp","head_before":"baseline","session":null})
                .to_string(),
        )
        .unwrap();
    }
    let report = import(&fixture.options(FailurePoint::None)).unwrap();
    assert_eq!(report["sessions_copied"], 0);
    let db = fixture.imported();
    let runs: Vec<String> = db
        .prepare("SELECT run_id FROM runs WHERE json_extract(result,'$.status')='failed'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(runs.len(), 2);
    for run in runs {
        let checkpoint = Checkpoint::read(&fixture.destination.join("runs").join(run))
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.session, None);
        assert_eq!(checkpoint.head_before.as_deref(), Some("baseline"));
        assert_eq!(checkpoint.state, State::Done);
    }
    assert!(!fixture.destination.join("codex-native-homes").exists());
}

#[test]
fn required_codex_state_symlinks_are_refused() {
    for relative in [
        "state_5.sqlite",
        "sessions/2026/10/03/rollout-fixture-fake-session.jsonl",
    ] {
        let fixture = Fixture::new();
        let original = fixture
            .source
            .join("codex-native-homes/generation")
            .join(relative);
        let outside = fixture.source.parent().unwrap().join("outside-state");
        fs::rename(&original, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &original).unwrap();
        assert!(import(&fixture.options(FailurePoint::None)).is_err());
        assert!(!fixture.destination.exists());
    }
}

#[test]
fn changed_brief_bytes_do_not_change_the_success_hash() {
    let fixture = Fixture::new();
    import(&fixture.options(FailurePoint::None)).unwrap();
    let db = fixture.imported();
    let before: String = db
        .query_row(
            "SELECT inputs_hash FROM steps WHERE step_id='done'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let raw = json_query(
        &db,
        "SELECT doc FROM plans JOIN projects USING(project_id) WHERE name='fixture'",
    );
    let brief = Path::new(
        raw["steps"]["done"]["in"]["brief"]["file"]
            .as_str()
            .unwrap(),
    );
    assert!(brief.is_file());
    fs::write(brief, "edited brief").unwrap();
    let project: sluice_model::ids::ProjectId = db
        .query_row(
            "SELECT project_id FROM projects WHERE name='fixture'",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap()
        .parse()
        .unwrap();
    let state = sluice_store::plans::read_state(&db, project).unwrap();
    let plan = sluice_model::Plan::parse(&serde_json::from_value(raw).unwrap(), &FixtureSignatures)
        .unwrap();
    let step = &plan.steps()[&"done".parse::<sluice_model::ids::StepId>().unwrap()];
    assert_eq!(
        before,
        sluice_model::plan::inputs_hash(&plan, &state, step)
            .unwrap()
            .to_string()
    );
    assert_eq!(
        state.status(&step.id),
        sluice_model::commands::StepStatus::Succeeded
    );
}

#[test]
fn missing_cwd_is_reported_without_losing_the_checkpoint_or_substituting_a_directory() {
    let fixture = Fixture::new();
    let missing = fixture.source.parent().unwrap().join("missing-cwd");
    let mut native: Value = serde_json::from_slice(
        &fs::read(
            fixture
                .source
                .join("projects/fixture/runs/scalar-run/native.json"),
        )
        .unwrap(),
    )
    .unwrap();
    native["cwd"] = json!(missing);
    for run in ["scalar-run", "scatter-busy"] {
        fs::write(
            fixture
                .source
                .join("projects/fixture/runs")
                .join(run)
                .join("native.json"),
            native.to_string(),
        )
        .unwrap();
    }
    fs::write(
        fixture
            .source
            .join("codex-native-sessions/fake-session.json"),
        json!({"home":fixture.source.join("codex-native-homes/generation"),"cwd":missing})
            .to_string(),
    )
    .unwrap();
    let report = import(&fixture.options(FailurePoint::None)).unwrap();
    assert!(report["exceptions"].as_array().unwrap().iter().any(|e| {
        e["resume_validation_error"]
            .as_str()
            .is_some_and(|s| s.contains("cwd is missing"))
    }));
    assert_eq!(
        fixture
            .imported()
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(!missing.exists());
    let run: String = fixture
        .imported()
        .query_row("SELECT run_id FROM sessions LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let checkpoint = Checkpoint::read(&fixture.destination.join("runs").join(run))
        .unwrap()
        .unwrap();
    assert_eq!(checkpoint.cwd, missing);
    assert_eq!(checkpoint.session.as_deref(), Some("fake-session"));
}

#[test]
fn pending_scatter_retains_good_items_and_refuses_an_incompatible_count() {
    for incompatible in [false, true] {
        let fixture = Fixture::new();
        let mut state = json_query(
            &fixture.connection,
            "SELECT doc FROM states WHERE project='fixture'",
        );
        state["steps"]["scatter"] = json!({"status":"pending","kept":{"inputs_hash":"old-python-hash","run_ids":["scatter-good","scatter-busy"],"results":[{"final":"kept after retry"},null]}});
        if incompatible {
            state["steps"]["scatter"]["kept"]["results"]
                .as_array_mut()
                .unwrap()
                .pop();
        }
        fixture
            .connection
            .execute(
                "UPDATE states SET doc=?1 WHERE project='fixture'",
                [state.to_string()],
            )
            .unwrap();
        let result = import(&fixture.options(FailurePoint::None));
        if incompatible {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("retained item count")
            );
            assert!(!fixture.destination.exists());
        } else {
            result.unwrap();
            let db = fixture.imported();
            let instances = json_query(&db, "SELECT instances FROM steps WHERE step_id='scatter'");
            assert_eq!(
                instances["0"]["outputs"],
                json!({"final":"kept after retry"})
            );
            assert_eq!(instances["0"]["status"], "succeeded");
            assert_eq!(
                db.query_row(
                    "SELECT status FROM steps WHERE step_id='scatter'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
                "pending"
            );
            assert_eq!(
                db.query_row("SELECT done FROM steps WHERE step_id='scatter'", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap(),
                1
            );
            assert_eq!(
                db.query_row("SELECT total FROM steps WHERE step_id='scatter'", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                2
            );
        }
    }
}

#[test]
fn saved_pause_states_cover_every_project_and_override_the_stopped_snapshot() {
    for complete in [true, false] {
        let fixture = Fixture::new();
        let flags = if complete {
            json!({"fixture":true,"archived":false})
        } else {
            json!({"fixture":true})
        };
        fs::write(
            fixture.staging.join("plan-conversion.json"),
            json!({"pause_states":flags}).to_string(),
        )
        .unwrap();
        let result = import(&fixture.options(FailurePoint::None));
        if complete {
            assert_eq!(result.unwrap()["pause_state_source"], "pre-window-manifest");
            let ledger: Value = serde_json::from_slice(
                &fs::read(fixture.destination.join("import-ledger.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(ledger["projects"]["fixture"]["original_paused"], true);
            assert_eq!(ledger["projects"]["archived"]["original_paused"], false);
            assert_eq!(
                fixture
                    .imported()
                    .query_row("SELECT count(*) FROM projects WHERE paused=1", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                2
            );
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("cover every imported project")
            );
        }
    }
}

#[test]
#[ignore = "read-only online backup rehearsal of the real home; never runs or resumes work"]
fn stopped_copy_rehearsal() {
    let live = PathBuf::from("/home/sam/.sluice");
    let before = fs::metadata(live.join("sluice.db"))
        .unwrap()
        .modified()
        .unwrap();
    let scratch = tempfile::Builder::new()
        .prefix("sluice-import-rehearsal-")
        .tempdir_in("/tmp")
        .unwrap();
    let backup = scratch.path().join("source");
    let staging = scratch.path().join("staging");
    let helper =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rehearse_python_home.py");
    let output = Command::new("uv")
        .args([
            "run",
            "python",
            helper.to_str().unwrap(),
            backup.to_str().unwrap(),
            staging.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = import(&Options {
        source: backup.clone(),
        destination: scratch.path().join("rust"),
        staging,
        failure: FailurePoint::None,
    })
    .unwrap();
    println!(
        "REHEARSAL_COUNTS {}",
        json!({"projects":report["projects"],"steps_by_state":report["steps_by_state"],"interrupted_runs":report["interrupted_runs"],"sessions_copied":report["sessions_copied"],"questions_converted":report["questions_converted"],"conflicts":report["conflicts"].as_array().unwrap().len()})
    );
    assert!(report["projects"].as_u64().unwrap() > 0);
    assert_eq!(
        before,
        fs::metadata(live.join("sluice.db"))
            .unwrap()
            .modified()
            .unwrap()
    );
    println!("LIVE_DB_MTIME_UNCHANGED {:?}", before);
    println!("REHEARSAL_EXCEPTIONS {}", report["exceptions"]);
    // Only scratch directories become writable for disposal, after all source
    // preservation assertions. Files and external credential targets stay alone.
    fn prepare_disposal(dir: &Path) {
        assert!(dir.starts_with("/tmp"));
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                prepare_disposal(&path);
            }
        }
    }
    prepare_disposal(scratch.path());
}
