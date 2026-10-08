//! `sluice` CLI (SPEC §9): argv validation, stdout/stderr/exit codes, JSON
//! args, id selectors and the `tool` command's dispatch, ported from
//! tests/test_cli.py.
#[allow(dead_code)]
#[path = "../../../tests/support/mod.rs"]
mod support;

use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest},
    rpc::decode_json,
};
use sluice_runtime::{coordinator::Coordinator, dispatch::Catalog, execution::OsHost};
use std::{
    path::Path,
    process::{Command, Output, Stdio},
};
use support::home::ScratchHome;

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home)
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_AUTHOR")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT")
        .args(args)
        .output()
        .unwrap()
}

fn tool(home: &Path, name: &str, args: &str) -> Output {
    run(home, &["tool", name, args])
}

fn stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON: {e}\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn stderr(output: &Output) -> Value {
    serde_json::from_slice(&output.stderr).unwrap_or_else(|e| {
        panic!(
            "stderr is not JSON: {e}\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn tool_errors_and_bad_arguments_exit_1() {
    let home = ScratchHome::new().unwrap();
    let unknown = tool(home.path(), "no_such_tool", "{}");
    assert_eq!(unknown.status.code(), Some(1));
    assert_eq!(stderr(&unknown)["error"], "bad_request");
    let bad = tool(home.path(), "status", "{not json");
    assert_eq!(bad.status.code(), Some(1));
    assert!(
        stderr(&bad)["message"]
            .as_str()
            .unwrap()
            .contains("not JSON")
    );
    let wrong = tool(home.path(), "status", "[1]");
    assert_eq!(wrong.status.code(), Some(1));
    assert!(
        stderr(&wrong)["message"]
            .as_str()
            .unwrap()
            .contains("JSON object")
    );
    let args = tool(home.path(), "plan_patch", r#"{"project":"p"}"#);
    assert_eq!(args.status.code(), Some(1));
    assert!(
        stderr(&args)["message"]
            .as_str()
            .unwrap()
            .contains("missing field")
    );
    let missing = tool(home.path(), "status", r#"{"project":"zz"}"#);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(stderr(&missing)["error"], "not_found");
}

#[test]
fn query_prints_a_table_binds_params_and_lists_the_schema() {
    let home = ScratchHome::new().unwrap();
    let out = run(
        home.path(),
        &["query", "SELECT name, description FROM projects"],
    );
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        json!({"columns": ["name", "description"], "rows": [], "truncated": false})
    );
    let out = run(
        home.path(),
        &[
            "query",
            "SELECT ? AS n, ? AS s UNION ALL SELECT 1, 'z'",
            "null",
            "x y",
        ],
    );
    assert_eq!(
        stdout(&out),
        json!({"columns": ["n", "s"], "rows": [[null, "x y"], [1, "z"]], "truncated": false})
    );
    let bad = run(home.path(), &["query", "DELETE FROM projects"]);
    assert_eq!(bad.status.code(), Some(1));
    assert_eq!(stderr(&bad)["error"], "bad_request");
    let bound = tool(home.path(), "query", &json!({"sql":"SELECT ?,?,?,?,?,?", "params":[null,true,7,2.5,"x'; DROP TABLE projects;--",false]}).to_string());
    assert!(
        bound.status.success(),
        "{}",
        String::from_utf8_lossy(&bound.stderr)
    );
    assert_eq!(
        stdout(&bound)["rows"],
        json!([[null, 1, 7, 2.5, "x'; DROP TABLE projects;--", 0]])
    );
    for sql in [
        "SELECT 1; SELECT 2",
        "SELECT 1; DROP TABLE projects",
        "",
        ";",
        "-- only comment",
        "SELECT 1; ;",
    ] {
        assert!(!run(home.path(), &["query", sql]).status.success(), "{sql}");
    }
    let nul = tool(
        home.path(),
        "query",
        &json!({"sql":"SELECT 1\u{0}"}).to_string(),
    );
    assert!(!nul.status.success());
    for sql in [
        "SELECT ';' AS \"semi;colon\"; -- trailing ;\n /* ; */",
        "SELECT 'it''s ; fine'",
        "/* ; */ SELECT 1; /* trailing */",
    ] {
        let reply = run(home.path(), &["query", sql]);
        assert!(
            reply.status.success(),
            "{sql}: {}",
            String::from_utf8_lossy(&reply.stderr)
        );
        assert_eq!(stdout(&reply)["truncated"], false);
    }
    for limit in [1001, usize::MAX] {
        let out = tool(
            home.path(),
            "query",
            &json!({"sql":"SELECT 1","limit":limit}).to_string(),
        );
        assert_eq!(stderr(&out)["error"], "bad_request");
    }
    for args in [
        json!({"sql":"SELECT ?"}),
        json!({"sql":"SELECT 1","params":[1]}),
        json!({"sql":"SELECT ?","params":[[1]]}),
    ] {
        assert!(
            !tool(home.path(), "query", &args.to_string())
                .status
                .success()
        );
    }
    assert_eq!(
        stdout(&run(
            home.path(),
            &["query", "SELECT count(*) FROM projects"]
        ))["rows"],
        json!([[0]])
    );
    assert!(
        !run(home.path(), &["query", &"SELECT 1;".repeat(11000)])
            .status
            .success()
    );
    let series =
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r LIMIT 1001) SELECT n FROM r";
    for (limit, count, truncated) in [
        (None, 200, true),
        (Some(0), 200, true),
        (Some(2), 2, true),
        (Some(1000), 1000, true),
    ] {
        let mut args = json!({"sql":series});
        if let Some(limit) = limit {
            args["limit"] = json!(limit);
        }
        let out = tool(home.path(), "query", &args.to_string());
        let table = stdout(&out);
        assert_eq!(table["rows"].as_array().unwrap().len(), count);
        assert_eq!(table["truncated"], truncated);
    }
    let exact = run(
        home.path(),
        &["query", "SELECT 1 UNION ALL SELECT 2", "--limit", "2"],
    );
    assert_eq!(stdout(&exact)["rows"], json!([[1], [2]]));
    assert_eq!(stdout(&exact)["truncated"], false);
    let blob = run(home.path(), &["query", "SELECT x'0102' AS icon"]);
    let message = stderr(&blob)["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("binary data")
            && message.contains("hex(\"icon\")")
            && message.contains("length(\"icon\")")
    );
    assert_eq!(
        stdout(&run(
            home.path(),
            &["query", "SELECT hex(x'0102'),length(x'0102')"]
        ))["rows"],
        json!([["0102", 2]])
    );
    for sql in [
        "SELECT CAST(x'ff' AS TEXT)",
        "SELECT 1e999",
        "SELECT missing FROM projects",
        "SELECT (",
        "SELECT json('broken')",
    ] {
        assert_eq!(
            stderr(&run(home.path(), &["query", sql]))["error"],
            "invalid",
            "{sql}"
        );
    }
    let never = home.root().join("never.db");
    for sql in [
        format!("VACUUM INTO '{}'", never.display()),
        "SELECT load_extension('/tmp/never.so')".into(),
        "SELECT LOAD_EXTENSION('/tmp/never.so')".into(),
        "SELECT sqlite_log(1,'never')".into(),
        format!("SELECT writefile('{}','never')", never.display()),
        "SELECT readfile('/etc/passwd')".into(),
        "SELECT fts3_tokenizer('simple')".into(),
        "SELECT eval('DELETE FROM projects')".into(),
    ] {
        assert!(
            !run(home.path(), &["query", &sql]).status.success(),
            "{sql}"
        );
    }
    assert!(!never.exists());
    let began = std::time::Instant::now();
    let bomb = run(
        home.path(),
        &[
            "query",
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r) SELECT sum(n) FROM r",
        ],
    );
    assert_eq!(stderr(&bomb)["error"], "bad_request");
    assert!(
        stderr(&bomb)["message"]
            .as_str()
            .unwrap()
            .contains("VM-operation")
    );
    assert!(began.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(
        stdout(&run(home.path(), &["query", "SELECT 1"]))["rows"],
        json!([[1]])
    );
    let wide = run(
        home.path(),
        &[
            "query",
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r LIMIT 200) SELECT n,hex(randomblob(8000)) FROM r",
        ],
    );
    let table = stdout(&wide);
    assert!(table["truncated"].as_bool().unwrap());
    let rows = table["rows"].as_array().unwrap();
    assert!(!rows.is_empty() && rows.len() < 200);
    assert!(wide.stdout.len() <= sluice_store::query::MAX_BYTES + 1);
    let listing = run(home.path(), &["query"]);
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("table projects("));
}

#[test]
fn a_project_through_the_tools() {
    let home = ScratchHome::new().unwrap();
    let listing = run(home.path(), &["tool"]);
    assert!(listing.status.success());
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        config,
        json!({"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 3065}, "log_max": 10000})
    );
    let names: Vec<String> = String::from_utf8(listing.stdout)
        .unwrap()
        .lines()
        .map(|line| line.split_whitespace().next().unwrap().to_owned())
        .collect();
    for name in ["projects_list", "plan_patch", "fn_call", "verify", "status"] {
        assert!(names.iter().any(|n| n == name), "{name} not listed");
    }
    for gone in ["init", "plan", "fn"] {
        assert_eq!(run(home.path(), &[gone]).status.code(), Some(2), "{gone}");
    }

    let created = tool(
        home.path(),
        "project_create",
        r#"{"name":"demo","description":"cli"}"#,
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let project_id = stdout(&created)["project_id"].as_str().unwrap().to_owned();
    let patched = tool(
        home.path(),
        "plan_patch",
        &json!({
            "project": "demo", "rev": 1, "reason": "plan",
            "ops": [{"op": "add", "path": "/inputs", "value": {"n": "int"}},
                    {"op": "add", "path": "/steps/a",
                     "value": {"run": "core.echo", "in": {"value": {"default": 1}}}}],
        })
        .to_string(),
    );
    assert!(
        patched.status.success(),
        "{}",
        String::from_utf8_lossy(&patched.stderr)
    );
    assert_eq!(stdout(&patched)["rev"], 2);
    let got = tool(home.path(), "plan_get", r#"{"project":"demo"}"#);
    assert!(got.status.success());
    let stale = tool(
        home.path(),
        "plan_patch",
        r#"{"project":"demo","rev":1,"reason":"x","ops":[]}"#,
    );
    assert_eq!(stale.status.code(), Some(1));
    // id selectors reach the same project
    let by_id = tool(
        home.path(),
        "plan_get",
        &json!({"project": format!("id:{project_id}")}).to_string(),
    );
    assert!(
        by_id.status.success(),
        "{}",
        String::from_utf8_lossy(&by_id.stderr)
    );
    let set = tool(
        home.path(),
        "plan_set_input",
        r#"{"project":"demo","name":"n","value":4}"#,
    );
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    // The board: set, read back, refused without a program, cleared with null.
    let board = tool(
        home.path(),
        "board_set",
        r#"{"project":"demo","program":"root = StepStatus(\"a\")","expected_rev":0}"#,
    );
    assert!(
        board.status.success(),
        "{}",
        String::from_utf8_lossy(&board.stderr)
    );
    assert_eq!(stdout(&board), json!({"rev": 1, "warnings": []}));
    let read = tool(home.path(), "board_get", r#"{"project":"demo"}"#);
    assert_eq!(stdout(&read)["program"], "root = StepStatus(\"a\")");
    let missing = tool(home.path(), "board_set", r#"{"project":"demo"}"#);
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing)["message"]
            .as_str()
            .unwrap()
            .contains("null to clear")
    );
    let cleared = tool(
        home.path(),
        "board_set",
        r#"{"project":"demo","program":null}"#,
    );
    assert_eq!(stdout(&cleared), json!({"rev": 2, "warnings": []}));
    // The document needs a Doc() on the board; board_set warns for a step not in the plan.
    let refused = tool(home.path(), "board_doc_read", r#"{"project":"demo"}"#);
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused)["message"]
            .as_str()
            .unwrap()
            .contains("this board's program has no Doc()"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let board = tool(
        home.path(),
        "board_set",
        r#"{"project":"demo","program":"root = Stack([Doc(), StepStatus(\"b\")])"}"#,
    );
    assert_eq!(
        stdout(&board),
        json!({"rev": 3, "warnings": ["line 1: StepStatus names step `b`, which is not in the plan"]})
    );
    // Written from a file by flags, then read with its lines numbered.
    let file = home.path().join("doc.md");
    std::fs::write(&file, "## Phase\nGreen soon.\n## Asks\n- one\n- two\n").unwrap();
    let written = run(
        home.path(),
        &[
            "tool",
            "board_doc_write",
            "--project",
            "demo",
            "--markdown-file",
            file.to_str().unwrap(),
            "--expected-rev",
            "0",
            "--reason",
            "first words",
        ],
    );
    assert!(
        written.status.success(),
        "{}",
        String::from_utf8_lossy(&written.stderr)
    );
    assert_eq!(stdout(&written), json!({"rev": 1, "changed": true}));
    let read = tool(home.path(), "board_doc_read", r#"{"project":"demo"}"#);
    let read = stdout(&read);
    assert_eq!(
        read["numbered"],
        "     1\t## Phase\n     2\tGreen soon.\n     3\t## Asks\n     4\t- one\n     5\t- two\n"
    );
    assert_eq!((&read["rev"], &read["author"]), (&json!(1), &json!("cli")));
    // Several edits at once by rev-1 numbers: replace, delete, insert and append.
    let edited = run(
        home.path(),
        &[
            "tool",
            "board_doc_edit",
            "--project",
            "demo",
            "--expected-rev",
            "1",
            "--edits",
            r###"[{"start":2,"end":2,"text":"Green: **11** red left."},{"start":4,"end":4,"text":""},{"start":3,"end":2,"text":"Lanes cut."},{"start":6,"end":5,"text":"## Figments\n- none"}]"###,
        ],
    );
    assert!(
        edited.status.success(),
        "{}",
        String::from_utf8_lossy(&edited.stderr)
    );
    assert_eq!(stdout(&edited), json!({"rev": 2, "changed": true}));
    let read = stdout(&tool(
        home.path(),
        "board_doc_read",
        r#"{"project":"demo"}"#,
    ));
    assert_eq!(
        read["markdown"],
        "## Phase\nGreen: **11** red left.\nLanes cut.\n## Asks\n- two\n## Figments\n- none\n"
    );
    // A stale rev is a conflict naming the current one, and nothing changes.
    let stale = tool(
        home.path(),
        "board_doc_edit",
        r#"{"project":"demo","expected_rev":1,"edits":[{"start":1,"end":1,"text":"x"}]}"#,
    );
    assert_eq!(stale.status.code(), Some(1));
    assert_eq!(
        (&stderr(&stale)["error"], &stderr(&stale)["current_rev"]),
        (&json!("conflict"), &json!(2))
    );
    let stale = tool(
        home.path(),
        "board_doc_write",
        r#"{"project":"demo","markdown":"x","expected_rev":1}"#,
    );
    assert_eq!(stderr(&stale)["error"], "conflict");
    // An overlap or a range past the end is invalid, naming the edit.
    let overlap = tool(
        home.path(),
        "board_doc_edit",
        r#"{"project":"demo","expected_rev":2,"edits":[{"start":1,"end":2,"text":"x"},{"start":2,"end":3,"text":"y"}]}"#,
    );
    assert_eq!(stderr(&overlap)["error"], "invalid");
    assert!(
        stderr(&overlap)["message"]
            .as_str()
            .unwrap()
            .contains("edits[1] (start 2, end 3) overlaps edits[0] (start 1, end 2)")
    );
    let past = tool(
        home.path(),
        "board_doc_edit",
        r#"{"project":"demo","expected_rev":2,"edits":[{"start":9,"end":9,"text":"x"}]}"#,
    );
    assert!(
        stderr(&past)["message"]
            .as_str()
            .unwrap()
            .contains("edits[0] (start 9, end 9): start is past the end")
    );
    let read = stdout(&tool(
        home.path(),
        "board_doc_read",
        r#"{"project":"demo"}"#,
    ));
    assert_eq!(read["rev"], 2, "nothing changed");
    // The same text again changes nothing.
    let same = tool(
        home.path(),
        "board_doc_write",
        &json!({"project":"demo","markdown":read["markdown"]}).to_string(),
    );
    assert_eq!(stdout(&same), json!({"rev": 2, "changed": false}));
    let missing = tool(home.path(), "board_doc_write", r#"{"project":"demo"}"#);
    assert!(
        stderr(&missing)["message"]
            .as_str()
            .unwrap()
            .contains("board_doc_write needs markdown")
    );
}

#[test]
fn tool_rpc_sends_one_raw_request_and_prints_the_unshaped_reply() {
    // Host gates (G1a's direct caller among them) drive the coordinator this way.
    let home = ScratchHome::new().unwrap();
    let created = tool(
        home.path(),
        "rpc",
        r#"{"command":"project_create","args":{"name":"raw","description":"","icon":null,"resources":{},"author":"gate"}}"#,
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let CommandReply::Project(project) = decode_json(&created.stdout).unwrap() else {
        panic!("{}", String::from_utf8_lossy(&created.stdout))
    };
    let listed = tool(home.path(), "rpc", r#"{"command":"projects_list"}"#);
    assert!(listed.status.success());
    let listed: CommandReply = decode_json(&listed.stdout).unwrap();
    assert!(
        serde_json::to_string(&listed)
            .unwrap()
            .contains(&project.project_id.to_string())
    );
    let bad = tool(home.path(), "rpc", r#"{"command":"no_such_command"}"#);
    assert_eq!(bad.status.code(), Some(1));
    assert_eq!(stderr(&bad)["error"], "bad_request");
}

#[test]
fn a_coordinator_exits_once_its_home_is_removed() {
    let home = ScratchHome::new().unwrap();
    let mut coordinator = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home.path())
        .arg("coordinator")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::os::unix::net::UnixStream::connect(home.path().join("coordinator.sock")).is_err() {
        if coordinator.try_wait().unwrap().is_some() || std::time::Instant::now() > deadline {
            let _ = coordinator.kill();
            panic!("coordinator never served");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::fs::remove_dir_all(home.path()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = coordinator.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = coordinator.kill();
            let _ = coordinator.wait();
            panic!("coordinator outlived its removed home");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
}

/// What a coordinator writes to stderr from start to a SIGTERM, with `SLUICE_LOG` set to
/// `level` (unset when None).
fn coordinator_stderr(level: Option<&str>) -> String {
    let home = ScratchHome::new().unwrap();
    let log = tempfile::NamedTempFile::new().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_sluice"));
    command
        .env("SLUICE_HOME", home.path())
        .env_remove("SLUICE_LOG")
        .env_remove("JOURNAL_STREAM")
        .arg("coordinator")
        .stdout(Stdio::null())
        .stderr(log.reopen().unwrap());
    if let Some(level) = level {
        command.env("SLUICE_LOG", level);
    }
    let mut coordinator = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::os::unix::net::UnixStream::connect(home.path().join("coordinator.sock")).is_err() {
        if coordinator.try_wait().unwrap().is_some() || std::time::Instant::now() > deadline {
            let _ = coordinator.kill();
            panic!(
                "coordinator never served: {}",
                std::fs::read_to_string(log.path()).unwrap_or_default()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let killed = Command::new("/usr/bin/kill")
        .args(["-TERM", &coordinator.id().to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    let status = coordinator.wait().unwrap();
    assert!(status.success(), "{status}");
    std::fs::read_to_string(log.path()).unwrap()
}

#[test]
fn a_coordinator_logs_its_start_and_stop_to_stderr_at_info() {
    let logged = coordinator_stderr(None);
    let started = logged
        .lines()
        .find(|line| line.contains("sluice started"))
        .unwrap_or_else(|| panic!("no startup line in {logged:?}"));
    assert!(started.contains("INFO"), "{started}");
    assert!(started.contains("mode=\"coordinator\""), "{started}");
    assert!(started.contains("release="), "{started}");
    assert!(started.contains("home="), "{started}");
    assert!(logged.contains("startup adoption done"), "{logged}");
    assert!(logged.contains("sluice stopped"), "{logged}");
}

#[test]
fn sluice_log_warn_keeps_a_quiet_coordinator_silent() {
    let logged = coordinator_stderr(Some("warn"));
    assert!(!logged.contains("sluice started"), "{logged}");
    assert!(!logged.contains("INFO"), "{logged}");
}

#[tokio::test]
async fn dispatched_commands_round_trip_through_a_coordinator() {
    let home = ScratchHome::new().unwrap();
    let program = Path::new(env!("CARGO_BIN_EXE_sluice")).to_path_buf();
    let broker = Coordinator::open(
        home.path().to_path_buf(),
        Catalog::core(),
        OsHost {
            home: home.path().to_path_buf(),
            program,
        },
    )
    .await
    .unwrap();
    let command = |request: CommandRequest| {
        let broker = broker.clone();
        async move { broker.command(request).await }
    };
    let create: CommandRequest = decode_json(
        br#"{"command":"project_create","args":{"name":"demo","description":"","resources":{}}}"#,
    )
    .unwrap();
    assert!(matches!(
        command(create).await.unwrap(),
        CommandReply::Project(_)
    ));
    let read: CommandRequest = decode_json(
        br#"{"command":"log_read","args":{"project":{"kind":"name","value":"demo"},"since_seq":0,"limit":200}}"#,
    )
    .unwrap();
    match command(read).await.unwrap() {
        CommandReply::Records(page) => assert!(!page.records.is_empty()),
        reply => panic!("log_read: {reply:?}"),
    }
}

/// A `sluice watch` subprocess, with a reader thread pushing its stdout lines
/// onto a channel the test can poll. stderr is inherited for diagnosis.
fn watch(home: &Path, args: &[&str]) -> (std::process::Child, std::sync::mpsc::Receiver<Value>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home)
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_AUTHOR")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT")
        .env_remove("SLUICE_PROJECT_ID")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut text = String::new();
        let _ = std::io::BufReader::new(stderr).read_to_string(&mut text);
        if !text.is_empty() {
            eprintln!("watch stderr: {text}");
        }
    });
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            match line {
                Ok(line)
                    if tx
                        .send(serde_json::from_str::<Value>(&line).unwrap())
                        .is_ok() => {}
                _ => break,
            }
        }
    });
    (child, rx)
}

/// Project `p` with pending steps `q` and `r` (work done outside sluice, so they stay
/// pending): their threads are `step-q` and `step-r`.
fn project_with_steps(home: &Path) {
    tool(home, "project_create", r#"{"name":"p","description":""}"#);
    let patched = tool(
        home,
        "plan_patch",
        r#"{"project":"p","rev":1,"reason":"threads","ops":[
            {"op":"add","path":"/steps/q","value":{"run":"core.external","outputs":{"ok":"boolean"}}},
            {"op":"add","path":"/steps/r","value":{"run":"core.external","outputs":{"ok":"boolean"}}}]}"#,
    );
    assert!(
        patched.status.success(),
        "{}",
        String::from_utf8_lossy(&patched.stderr)
    );
}

/// The orchestrator asks (or tells) step `step`, on its thread `step-<step>`.
fn post(home: &Path, step: &str, body: &str, question: bool) {
    let out = tool(
        home,
        if question { "ask" } else { "say" },
        &json!({"project": "p", "to": step, "body": body}).to_string(),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn watch_prints_matching_records_from_now_or_since_a_seq() {
    let home = ScratchHome::new().unwrap();
    project_with_steps(home.path());
    post(home.path(), "q", "before", true);

    // From now: records appended while the watcher runs, on the chosen
    // threads. A watcher resolves its start bounds on open, so appends race
    // it; post until the newest marker lands, which proves it is live.
    let (mut child, rx) = watch(
        home.path(),
        &[
            "watch",
            "-p",
            "p",
            "--kinds",
            "message",
            "--threads",
            "step-q",
        ],
    );
    let mut marker = String::new();
    for i in 0..40 {
        marker = format!("after-{i}");
        post(home.path(), "q", &marker, true);
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(line) if line["body"] == marker => break,
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("watch died: {e}"),
        }
        if i == 39 {
            panic!("watch never printed an appended record");
        }
    }
    assert!(marker.starts_with("after-"));
    post(home.path(), "r", "other thread", true);
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(2)).is_err(),
        "a post on another thread must not print"
    );
    child.kill().unwrap();
    child.wait().unwrap();

    // From --since-seq 0: the whole thread's history, in order.
    let (mut child, rx) = watch(
        home.path(),
        &[
            "watch",
            "-p",
            "p",
            "--threads",
            "step-q",
            "--since-seq",
            "0",
        ],
    );
    let mut bodies = Vec::new();
    while let Ok(line) = rx.recv_timeout(std::time::Duration::from_secs(15)) {
        let body = line["body"].as_str().unwrap().to_owned();
        let done = body == marker;
        bodies.push(body);
        if done {
            break;
        }
    }
    assert_eq!(bodies[0], "before");
    assert_eq!(bodies.last().unwrap(), &marker);
    assert!(bodies[1..].iter().all(|b| b.starts_with("after-")));
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn watch_holds_notes_until_a_question_under_wake_questions() {
    let home = ScratchHome::new().unwrap();
    project_with_steps(home.path());
    let (mut child, rx) = watch(
        home.path(),
        &[
            "watch",
            "-p",
            "p",
            "--threads",
            "step-q",
            "--wake",
            "questions",
        ],
    );
    // Bring the watcher live with a question that must print; anything posted
    // before it resolves its start bounds never reaches it.
    let mut live = false;
    for i in 0..40 {
        let marker = format!("ready-{i}");
        post(home.path(), "q", &marker, true);
        match rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(line) if line["body"] == marker => {
                live = true;
                break;
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(e) => panic!("watch died: {e}"),
        }
    }
    assert!(live, "watch never came up");
    post(home.path(), "q", "fyi", false);
    assert!(
        rx.recv_timeout(std::time::Duration::from_secs(2)).is_err(),
        "a note is held, not printed"
    );
    post(home.path(), "q", "which db?", true);
    let fyi = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
    let question = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
    assert_eq!(fyi["body"], "fyi");
    assert_eq!(question["body"], "which db?");
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn watch_refuses_an_unknown_project_or_kind() {
    let home = ScratchHome::new().unwrap();
    tool(
        home.path(),
        "project_create",
        r#"{"name":"p","description":""}"#,
    );
    let out = run(home.path(), &["watch", "-p", "zz"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stderr(&out)["error"], "not_found");
    let out = run(home.path(), &["watch", "--kinds", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    let message = stderr(&out)["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("unknown") && message.contains("nope"),
        "{message}"
    );
}

#[test]
fn next_times_out_and_wakes_on_everything_with_all() {
    let home = ScratchHome::new().unwrap();
    tool(
        home.path(),
        "project_create",
        r#"{"name":"p","description":""}"#,
    );
    let patched = tool(
        home.path(),
        "plan_patch",
        r#"{"project":"p","rev":1,"reason":"plan",
            "ops":[{"op":"add","path":"/steps/a",
                    "value":{"run":"core.echo","in":{"value":{"default":1}}}}]}"#,
    );
    assert!(patched.status.success());
    // Nothing a coordinator wakes on arrived yet: the request times out.
    let out = run(
        home.path(),
        &[
            "next",
            "-p",
            "p",
            "--since-seq",
            "0",
            "--timeout",
            "2",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let last: Value = serde_json::from_str(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(last["timed_out"], true);
    // --all wakes on anything: the plan edit the patch recorded.
    let out = run(
        home.path(),
        &[
            "next",
            "-p",
            "p",
            "--since-seq",
            "0",
            "--all",
            "--timeout",
            "10",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(lines.last().unwrap()["timed_out"], false);
    assert!(
        lines[..lines.len() - 1]
            .iter()
            .any(|r| r["kind"] == "plan.edit")
    );
}

/// Agents see their project as a bare id (SLUICE_PROJECT_ID) and are told `id:<uuid>`; both
/// reach the project in every tool, as the name does.
#[test]
fn a_bare_id_an_id_selector_and_the_name_reach_the_same_project() {
    let home = ScratchHome::new().unwrap();
    let created = tool(
        home.path(),
        "project_create",
        r#"{"name":"demo","description":""}"#,
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let id = stdout(&created)["project_id"].as_str().unwrap().to_owned();
    let run = sluice_model::ids::RunId::new().to_string();
    for selector in [id.clone(), format!("id:{id}"), "demo".into()] {
        for (name, args) in [
            (
                "say",
                json!({"project": selector, "to": "owner", "body": "note"}),
            ),
            ("status", json!({"project": selector})),
            ("plan_get", json!({"project": selector})),
            ("log_read", json!({"project": selector})),
            ("fn_list", json!({"project": selector})),
            (
                "messages",
                json!({"project": selector, "view": "thread", "thread": "owner"}),
            ),
        ] {
            let out = tool(home.path(), name, &args.to_string());
            assert!(
                out.status.success(),
                "{name} with {selector}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        // No such run, but the project resolves: the refusal is about the run, not the project.
        let submit = tool(
            home.path(),
            "step_submit",
            &json!({"project": selector, "step": "work", "run": run, "outputs": {}}).to_string(),
        );
        assert_eq!(submit.status.code(), Some(1));
        let message = stderr(&submit)["message"].as_str().unwrap().to_owned();
        assert_eq!(message, "stale submission", "{selector}");
    }
    // A run's task names its project by id, so its submit never reads the store: a binary
    // pinned to another schema still reaches the coordinator. A name still needs the store.
    let mark = |version: i64| {
        rusqlite::Connection::open(home.path().join("sluice.db"))
            .unwrap()
            .execute("UPDATE home_meta SET schema_version=?1", [version])
            .unwrap();
    };
    mark(99);
    for (selector, refusal) in [
        (format!("id:{id}"), "stale submission"),
        ("demo".into(), "unsupported schema version 99; expected 1"),
    ] {
        let submit = tool(
            home.path(),
            "step_submit",
            &json!({"project": selector, "step": "work", "run": run, "outputs": {}}).to_string(),
        );
        assert_eq!(stderr(&submit)["message"], refusal, "{selector}");
    }
    mark(1);
    let posted = tool(
        home.path(),
        "messages",
        &json!({"project": "demo", "view": "thread", "thread": "owner"}).to_string(),
    );
    assert_eq!(stdout(&posted)["messages"].as_array().unwrap().len(), 3);
    let unknown = sluice_model::ids::ProjectId::new();
    let missing = tool(
        home.path(),
        "say",
        &json!({"project": unknown.to_string(), "to": "owner", "body": "x"}).to_string(),
    );
    assert_eq!(stderr(&missing)["error"], "not_found");
    assert!(
        stderr(&missing)["message"]
            .as_str()
            .unwrap()
            .contains(&format!("id:{unknown}"))
    );
    // A name that looks like an id would be ambiguous, so it is refused.
    let refused = tool(
        home.path(),
        "project_create",
        &json!({"name": unknown.to_string(), "description": ""}).to_string(),
    );
    assert_eq!(stderr(&refused)["error"], "bad_request");
}

/// A `sluice serve --no-runner` child, killed on drop.
struct Served(std::process::Child);
impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn serve(home: &Path, args: &[&str]) -> Served {
    Served(
        Command::new(env!("CARGO_BIN_EXE_sluice"))
            .env("SLUICE_HOME", home)
            .arg("serve")
            .arg("--no-runner")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}
/// The dashboard answers on `port` while `served` keeps running.
fn answers(served: &mut Served, port: u16) {
    use std::io::{Read, Write};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Some(status) = served.0.try_wait().unwrap() {
            let mut text = String::new();
            let _ = served.0.stderr.take().unwrap().read_to_string(&mut text);
            panic!("serve exited {status}: {text}");
        }
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            stream
                .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .unwrap();
            let mut reply = String::new();
            let _ = stream.read_to_string(&mut reply);
            assert!(
                reply.starts_with("HTTP/1.0 200") || reply.starts_with("HTTP/1.1 200"),
                "{reply}"
            );
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "serve never bound {port}"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn serve_binds_config_http_unless_the_command_line_names_host_and_port() {
    let home = ScratchHome::new().unwrap();
    let mut coordinator = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home.path())
        .arg("coordinator")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::os::unix::net::UnixStream::connect(home.path().join("coordinator.sock")).is_err() {
        assert!(coordinator.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let config = |http: Value| {
        std::fs::write(
            home.path().join("config.json"),
            json!({"fn_dirs": [], "http": http, "log_max": 10000}).to_string(),
        )
        .unwrap()
    };
    let port = |listener: std::net::TcpListener| listener.local_addr().unwrap().port();
    let from_config = port(support::free_port::free_port().unwrap());
    config(json!({"host": "127.0.0.1", "port": from_config}));
    {
        let mut served = serve(home.path(), &[]);
        answers(&mut served, from_config);
    }
    // A non-loopback host from config.json is refused like one from --host...
    config(json!({"host": "192.0.2.1", "port": from_config}));
    let refused = run(home.path(), &["serve", "--no-runner"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("loopback"));
    // ...and the command line wins over it.
    let given = port(support::free_port::free_port().unwrap());
    let mut served = serve(
        home.path(),
        &["--host", "127.0.0.1", "--port", &given.to_string()],
    );
    answers(&mut served, given);
    drop(served);
    let _ = coordinator.kill();
    let _ = coordinator.wait();
}

/// `sluice mcp` serves the MCP tools over stdio (newline-delimited JSON-RPC), backed by the
/// home's coordinator, and exits when its input closes.
#[test]
fn mcp_mode_serves_the_tools_over_stdio() {
    use std::io::{BufRead, BufReader, Write};
    let home = ScratchHome::new().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home.path())
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_AUTHOR")
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let (lines, received) = std::sync::mpsc::channel::<Value>();
    let output = BufReader::new(child.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in output.lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str(&line)
                && lines.send(value).is_err()
            {
                break;
            }
        }
    });
    let send = |input: &mut std::process::ChildStdin, message: Value| {
        writeln!(input, "{message}").unwrap();
    };
    let reply = |id: u64| loop {
        let message = received
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("an MCP reply");
        if message["id"] == id {
            return message;
        }
    };
    send(
        &mut input,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"stdio-test","version":"0"}}}),
    );
    let init = reply(1);
    assert_eq!(init["result"]["serverInfo"]["name"], "sluice", "{init}");
    assert!(init["result"]["instructions"].is_string());
    send(
        &mut input,
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    );
    send(
        &mut input,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    );
    let listed = reply(2);
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for name in ["projects_list", "project_create", "next", "docs"] {
        assert!(names.contains(&name), "{name} missing from {names:?}");
    }
    send(
        &mut input,
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"project_create","arguments":{"name":"demo"}}}),
    );
    let created = reply(3);
    assert_eq!(
        created["result"]["structuredContent"]["name"], "demo",
        "{created}"
    );
    send(
        &mut input,
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{
        "name":"projects_list","arguments":{}}}),
    );
    let listed = reply(4);
    assert_eq!(
        listed["result"]["structuredContent"]["result"][0]["name"], "demo",
        "{listed}"
    );
    drop(input);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "mcp did not exit");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `recipe_list`, `fn_list`, `log_wait` and `step_wait` answer through the coordinator, so the
/// CLI prints what MCP returns; `step_set_input` takes the flat `steps`/`tags` selection.
#[test]
fn tool_results_match_the_mcp_shapes() {
    let home = ScratchHome::new().unwrap();
    let ok = |out: Output| {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout(&out)
    };
    ok(tool(home.path(), "project_create", r#"{"name":"demo"}"#));
    std::fs::create_dir_all(home.path().join("recipes")).unwrap();
    std::fs::write(
        home.path().join("recipes/lane.json"),
        json!({"name":"lane","doc":"one lane","params":{"x":"string"},
               "steps":{"{unit}-a":{"run":"core.echo","in":{"value":{"default":"{x}"}}}}})
        .to_string(),
    )
    .unwrap();
    let recipes = ok(tool(home.path(), "recipe_list", r#"{"project":"demo"}"#));
    assert_eq!(recipes[0]["name"], "lane", "{recipes}");
    assert_eq!(recipes[0]["scope"], "global");
    assert_eq!(recipes[0]["doc"], "one lane");
    assert_eq!(recipes[0]["params"]["x"], "string");
    let fns = ok(tool(home.path(), "fn_list", r#"{"project":"demo"}"#));
    assert!(
        fns.as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "core.echo"),
        "{fns}"
    );
    ok(tool(
        home.path(),
        "step_add",
        &json!({"project":"demo","step":"a","start":false,
                "spec":{"run":"core.echo","tags":["t"],"in":{"value":{"default":1}}}})
        .to_string(),
    ));
    ok(tool(
        home.path(),
        "step_set_input",
        r#"{"project":"demo","steps":"a","inputs":{"value":2}}"#,
    ));
    let plan = ok(tool(home.path(), "plan_get", r#"{"project":"demo"}"#));
    assert_eq!(
        plan["plan"]["steps"]["a"]["in"]["value"],
        json!({"default":2})
    );
    ok(tool(
        home.path(),
        "step_set_input",
        r#"{"project":"demo","tags":["t"],"inputs":{"value":3}}"#,
    ));
    let plan = ok(tool(home.path(), "plan_get", r#"{"project":"demo"}"#));
    assert_eq!(
        plan["plan"]["steps"]["a"]["in"]["value"],
        json!({"default":3})
    );
    let waited = ok(tool(
        home.path(),
        "log_wait",
        r#"{"project":"demo","since_seq":0,"timeout":5}"#,
    ));
    let mut keys: Vec<&String> = waited.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["last_seq", "records"], "{waited}");
    assert!(!waited["records"].as_array().unwrap().is_empty());
    // The log filters ride in log_wait's read; only the plan edits pass here.
    let edits = ok(tool(
        home.path(),
        "log_wait",
        r#"{"project":"demo","since_seq":0,"kinds":["step.status","plan.edit"],"statuses":["failed"],"timeout":5}"#,
    ));
    let records = edits["records"].as_array().unwrap();
    assert!(!records.is_empty(), "{edits}");
    assert!(records.iter().all(|r| r["kind"] == "plan.edit"), "{edits}");
    // `a` was added without starting, so it is held: settled at once.
    let settled = ok(tool(
        home.path(),
        "step_wait",
        r#"{"project":"demo","steps":"a","until":"settled","timeout":5}"#,
    ));
    assert_eq!(settled["met"], true, "{settled}");
    assert_eq!(settled["steps"], json!({"a":"pending"}));
    assert!(settled["seq"].as_i64().unwrap() > 0, "{settled}");
}

/// `sluice tool ask/say/reply`: the orchestrator speaks (no run identity), the thread and
/// sender are derived, each call prints its receipt, a bad recipient is `invalid` with
/// nothing stored, and the retired `message_post` is refused without a run.
#[test]
fn ask_say_and_reply_through_sluice_tool() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project_with_steps(home);
    let listing = String::from_utf8(run(home, &["tool"]).stdout).unwrap();
    let listed: Vec<&str> = listing
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    for name in ["ask", "say", "reply", "messages"] {
        assert!(listed.contains(&name), "{name} not listed");
    }
    assert!(!listed.contains(&"message_post"));
    let ok = |out: Output| {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout(&out)
    };
    let said = ok(tool(
        home,
        "say",
        r#"{"project":"p","to":"q","body":"start with the parser"}"#,
    ));
    assert_eq!(said["to"], "q");
    assert_eq!(said["thread"], "step-q");
    assert_eq!(said["delivery"], "queued");
    assert!(said.get("run").is_none());
    let asked = ok(tool(
        home,
        "ask",
        r#"{"project":"p","to":"owner","body":"ship it?","title":"Ship"}"#,
    ));
    assert_eq!(
        (asked["thread"].as_str(), asked["delivery"].as_str()),
        (Some("owner"), Some("delivered"))
    );
    // The owner's question (the dashboard's, sent raw) is answered by the orchestrator.
    let owner_ask = json!({"command":"ask","args":{"project":{"kind":"name","value":"p"},"to":"orchestrator","body":"which db?","owner":true}});
    let raw = ok(run(home, &["tool", "rpc", &owner_ask.to_string()]));
    let question = raw["data"]["id"].clone();
    let replied = ok(tool(
        home,
        "reply",
        &json!({"project":"p","to_message":question,"body":"postgres"}).to_string(),
    ));
    assert_eq!(
        (replied["to"].as_str(), replied["thread"].as_str()),
        (Some("owner"), Some("owner"))
    );
    let thread = ok(tool(
        home,
        "messages",
        r#"{"project":"p","view":"thread","thread":"owner"}"#,
    ));
    let rows: Vec<(String, String, String, Value)> = thread["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["verb"].as_str().unwrap().into(),
                m["from"].as_str().unwrap().into(),
                m["to"].as_str().unwrap().into(),
                m["state"].clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (
                "ask".into(),
                "orchestrator".into(),
                "owner".into(),
                json!("open")
            ),
            (
                "ask".into(),
                "owner".into(),
                "orchestrator".into(),
                json!("answered")
            ),
            (
                "reply".into(),
                "orchestrator".into(),
                "owner".into(),
                Value::Null
            ),
        ]
    );
    assert_eq!(thread["messages"][1]["answered_by"], replied["id"]);
    // The orchestrator's inbox is what is addressed to it; the owner's is the owner's.
    let inbox = ok(tool(home, "messages", r#"{"project":"p","view":"inbox"}"#));
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 0);
    let owners = ok(tool(
        home,
        "messages",
        r#"{"project":"p","view":"inbox","owner":true}"#,
    ));
    assert_eq!(owners["messages"].as_array().unwrap().len(), 2);
    let count = || {
        ok(tool(
            home,
            "query",
            r#"{"sql":"SELECT count(*) AS n FROM messages"}"#,
        ))["rows"][0]["n"]
            .clone()
    };
    let before = count();
    for (name, args, error) in [
        (
            "say",
            json!({"project":"p","to":"nobody","body":"x"}),
            "invalid",
        ),
        ("ask", json!({"project":"p","body":"x"}), "invalid"),
        (
            "say",
            json!({"project":"p","to":"orchestrator","body":"x"}),
            "invalid",
        ),
        (
            "ask",
            json!({"project":"p","to":"q","body":"x","owner":true}),
            "bad_request",
        ),
        (
            "say",
            json!({"project":"p","to":"q","body":"x","thread":"mine"}),
            "bad_request",
        ),
        (
            "message_post",
            json!({"project":"p","body":"x","needs_reply":false}),
            "invalid",
        ),
        (
            "message_post",
            json!({"project":"p","body":"x","run":sluice_model::ids::RunId::new()}),
            "not_found",
        ),
    ] {
        let out = tool(home, name, &args.to_string());
        assert_eq!(out.status.code(), Some(1), "{name} {args}");
        assert_eq!(stderr(&out)["error"], error, "{name} {args}");
    }
    assert_eq!(count(), before);
}

/// `sluice tool` with an environment and, optionally, stdin.
fn tool_with(home: &Path, args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Output {
    use std::io::Write;
    let mut command = Command::new(env!("CARGO_BIN_EXE_sluice"));
    command
        .env("SLUICE_HOME", home)
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_AUTHOR")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT")
        .env_remove("SLUICE_PROJECT_ID")
        .envs(env.iter().copied())
        .arg("tool")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        input.write_all(text.as_bytes()).unwrap();
    }
    drop(input);
    child.wait_with_output().unwrap()
}

fn ok(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout(&out)
}

fn refused(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        stderr(out)["error"],
        "bad_request",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stderr(out)["message"].as_str().unwrap().to_owned()
}

/// A message id, seq, limit or wait given as a string of digits is that integer; anything
/// else is refused naming the field.
#[test]
fn tool_takes_decimal_strings_for_integer_arguments() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project_with_steps(home);
    let owner_ask = json!({"command":"ask","args":{"project":{"kind":"name","value":"p"},"to":"orchestrator","body":"which db?","owner":true}});
    let question = ok(run(home, &["tool", "rpc", &owner_ask.to_string()]))["data"]["id"]
        .as_i64()
        .unwrap();
    let replied = ok(tool(
        home,
        "reply",
        &json!({"project":"p","to_message":question.to_string(),"body":"postgres"}).to_string(),
    ));
    assert_eq!(replied["thread"], "owner");
    let thread = ok(tool(
        home,
        "messages",
        r#"{"project":"p","view":"thread","thread":"owner","since":"0"}"#,
    ));
    assert_eq!(thread["messages"][0]["state"], "answered", "{thread}");
    let page = ok(tool(
        home,
        "log_read",
        r#"{"project":"p","since_seq":"0","limit":"2"}"#,
    ));
    assert_eq!(page["records"].as_array().unwrap().len(), 2);
    for (args, field) in [
        (
            json!({"project":"p","to_message":"abc","body":"x"}),
            "to_message",
        ),
        (
            json!({"project":"p","to_message":"-3","body":"x"}),
            "to_message",
        ),
    ] {
        let message = refused(&tool(home, "reply", &args.to_string()));
        assert!(message.starts_with(field), "{message}");
    }
    let message = refused(&tool(home, "log_read", r#"{"project":"p","limit":"lots"}"#));
    assert!(message.starts_with("limit"), "{message}");
}

/// An unknown tool or field names itself and the nearest valid names.
#[test]
fn tool_suggests_the_nearest_tool_and_field() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    let message = refused(&tool(home, "step_contxt", "{}"));
    assert_eq!(
        message,
        "unknown tool step_contxt; did you mean step_context?"
    );
    let message = refused(&tool(home, "step_submit", r#"{"output":{}}"#));
    assert_eq!(
        message,
        "step_submit takes no argument 'output'; did you mean outputs?"
    );
    let message = refused(&tool_with(
        home,
        &["step_submit", "--output-file", "-"],
        &[],
        Some("{}"),
    ));
    assert_eq!(
        message,
        "step_submit takes no argument 'output'; did you mean outputs?"
    );
    let message = refused(&tool_with(home, &["reply", "--message", "3"], &[], None));
    assert!(message.contains("did you mean to_message?"), "{message}");
    let message = refused(&tool(home, "nothing_like_it", "{}"));
    assert_eq!(
        message,
        "unknown tool nothing_like_it (sluice tool lists them)"
    );
}

/// `--field value` flags, `--field-file PATH` and `-` for stdin: a string field takes the
/// text as is, any other value its JSON, and flags win over the JSON object.
#[test]
fn tool_takes_flags_files_and_stdin() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project_with_steps(home);
    let body = "it's \"quoted\" $HOME `ls` {not json}\n";
    let said = ok(tool_with(
        home,
        &["say", "--project", "p", "--to", "q", "--body-file", "-"],
        &[],
        Some(body),
    ));
    assert_eq!(said["to"], "q");
    let file = home.join("body.md");
    std::fs::write(&file, "42").unwrap();
    // The JSON object first, then flags over it: --to wins; a numeric text stays a string body.
    ok(tool_with(
        home,
        &[
            "say",
            r#"{"project":"p","to":"r","body":"x"}"#,
            "--to=q",
            &format!("--body-file={}", file.display()),
        ],
        &[],
        None,
    ));
    let owner_ask = json!({"command":"ask","args":{"project":{"kind":"name","value":"p"},"to":"orchestrator","body":"which db?","owner":true}});
    let question =
        ok(run(home, &["tool", "rpc", &owner_ask.to_string()]))["data"]["id"].to_string();
    let replied = ok(tool_with(
        home,
        &[
            "reply",
            "--project",
            "p",
            "--to_message",
            &question,
            "--body",
            "owner",
        ],
        &[],
        None,
    ));
    assert_eq!(replied["to"], "owner");
    // A boolean's flag alone means true; the JSON object can come from stdin with `-`.
    let inbox = ok(tool_with(
        home,
        &["messages", "-", "--owner"],
        &[],
        Some(r#"{"project":"p","view":"thread","thread":"owner"}"#),
    ));
    let bodies: Vec<&str> = inbox["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["which db?", "owner"]);
    let thread = ok(tool_with(
        home,
        &[
            "messages",
            "--project",
            "p",
            "--view",
            "thread",
            "--thread",
            "step-q",
        ],
        &[],
        None,
    ));
    let bodies: Vec<&str> = thread["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, [body, "42"]);
    // A field that takes only objects must be JSON; a flag must be the tool's.
    let message = refused(&tool_with(
        home,
        &[
            "step_set_output",
            "--project",
            "p",
            "--step",
            "q",
            "--outputs",
            "{ok",
        ],
        &[],
        None,
    ));
    assert!(message.starts_with("--outputs: not JSON"), "{message}");
    let message = refused(&tool_with(home, &["status", "--project"], &[], None));
    assert_eq!(message, "--project needs a value");
}

/// In a run, a call that leaves out project, run or (step_submit, step_context) step gets
/// the run's own; a value the call gives is never replaced.
#[test]
fn tool_defaults_project_run_and_step_inside_a_run() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project_with_steps(home);
    let other = ok(tool(home, "project_create", r#"{"name":"other"}"#));
    let project = stdout(&tool(home, "status", r#"{"project":"p"}"#))["project"]["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let run_id = sluice_model::ids::RunId::new().to_string();
    let env = [
        ("SLUICE_PROJECT_ID", project.as_str()),
        ("SLUICE_RUN_ID", run_id.as_str()),
        ("SLUICE_STEP", "q"),
    ];
    // Outside a run nothing is filled in.
    let message = refused(&tool_with(home, &["status"], &[], None));
    assert!(message.contains("missing field `project`"), "{message}");
    let status = ok(tool_with(home, &["status"], &env, None));
    assert_eq!(status["project"]["name"], "p");
    let status = ok(tool_with(
        home,
        &["status", "--project", "other"],
        &env,
        None,
    ));
    assert_eq!(status["project"]["project_id"], other["project_id"]);
    let context = ok(tool_with(home, &["step_context"], &env, None));
    assert_eq!(
        (
            context["project"]["project_id"].as_str(),
            context["step"].as_str()
        ),
        (Some(project.as_str()), Some("q"))
    );
    let context = ok(tool_with(
        home,
        &["step_context", "--step", "r"],
        &env,
        None,
    ));
    assert_eq!(context["step"], "r");
    // The run speaks as its step: this run is not one of the project's, so it is refused.
    let out = tool_with(home, &["say", "--to", "owner", "--body", "hi"], &env, None);
    assert_eq!(
        stderr(&out)["error"],
        "not_found",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // An explicit null run keeps the orchestrator's voice.
    let said = ok(tool_with(
        home,
        &["say", r#"{"run":null}"#, "--to", "owner", "--body", "hi"],
        &env,
        None,
    ));
    assert_eq!(said["thread"], "owner");
    // step_submit gets project, step and run: the coordinator sees this run's submission.
    let out = tool_with(home, &["step_submit", "--outputs", "{}"], &env, None);
    assert_eq!(
        stderr(&out)["message"],
        "stale submission",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Without SLUICE_RUN_ID it is not a run: no defaults.
    let message = refused(&tool_with(
        home,
        &["step_submit", "--outputs", "{}"],
        &env[..1],
        None,
    ));
    assert!(message.contains("missing field"), "{message}");
}

/// `sluice tool <name> --help` lists the tool's fields from its schema.
#[test]
fn tool_help_lists_the_fields_with_types_and_descriptions() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    let help = tool_with(home, &["reply", "--help"], &[], None);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(help.starts_with("usage: sluice tool reply "), "{help}");
    assert!(help.contains("Reply to a message"), "{help}");
    assert!(
        help.contains(
            "  --to-message <integer>\n      the id of the message replied to; omit when using to."
        ),
        "{help}"
    );
    assert!(help.contains("  --to <string>\n      the sender whose single open question addressed to you is answered."), "{help}");
    assert!(help.contains("Give exactly one selector."), "{help}");
    assert!(help.contains("  --body <string>  (default \"\")"), "{help}");
    assert!(help.contains("--<field>-file PATH"), "{help}");
    let help = String::from_utf8(tool_with(home, &["messages", "-h"], &[], None).stdout).unwrap();
    assert!(
        help.contains("--view <inbox|questions|history|thread>  (default \"inbox\")"),
        "{help}"
    );
    // A tool MCP does not offer still lists its fields.
    let help =
        String::from_utf8(tool_with(home, &["submission", "--help"], &[], None).stdout).unwrap();
    assert!(help.contains("--run <string>"), "{help}");
    let usage = String::from_utf8(tool_with(home, &["--help"], &[], None).stdout).unwrap();
    assert!(usage.starts_with("usage: sluice tool"), "{usage}");
    assert!(usage.contains("step_submit"), "{usage}");
}

/// step_wait and the log's statuses and recipients filters get the same treatment: flags
/// (a word for a list of strings is a one-item list), decimal strings, suggestions and the
/// run's project.
#[test]
fn step_wait_and_the_log_filters_take_flags_strings_and_run_defaults() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    project_with_steps(home);
    post(home, "q", "to q", false);
    post(home, "r", "to r", false);
    // q is a pending core.external step: settled at once. The timeout is a JSON string.
    let waited = ok(tool_with(
        home,
        &[
            "step_wait",
            "--project",
            "p",
            "--steps",
            "q",
            "--until",
            "settled",
            "--timeout",
            "\"5\"",
        ],
        &[],
        None,
    ));
    assert_eq!(waited["met"], true, "{waited}");
    assert_eq!(waited["steps"]["q"], "pending", "{waited}");
    let waited = ok(tool_with(
        home,
        &[
            "step_wait",
            "--project",
            "p",
            "--steps",
            "q",
            "--until",
            r#"{"any_of":["failed"]}"#,
            "--timeout",
            "0",
        ],
        &[],
        None,
    ));
    assert_eq!(waited["met"], false, "{waited}");
    let message = refused(&tool_with(
        home,
        &["step_wait", "--project", "p", "--untill", "settled"],
        &[],
        None,
    ));
    assert_eq!(
        message,
        "step_wait takes no argument 'untill'; did you mean until?"
    );
    let message = refused(&tool_with(home, &["step_wiat"], &[], None));
    assert_eq!(message, "unknown tool step_wiat; did you mean step_wait?");
    // recipients narrows messages to those to q; a word is a one-item list.
    let page = ok(tool_with(
        home,
        &[
            "log_read",
            "--project",
            "p",
            "--kinds",
            "message",
            "--recipients",
            "q",
            "--since-seq",
            "0",
            "--limit",
            "50",
        ],
        &[],
        None,
    ));
    let bodies: Vec<&str> = page["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["to q"]);
    let page = ok(tool_with(
        home,
        &[
            "log_read",
            "--project",
            "p",
            "--kinds",
            "step.status",
            "--statuses",
            "failed",
            "--since-seq",
            "0",
        ],
        &[],
        None,
    ));
    assert_eq!(page["records"], json!([]));
    let message = refused(&tool_with(
        home,
        &["log_read", "--project", "p", "--status", "failed"],
        &[],
        None,
    ));
    assert_eq!(
        message,
        "log_read takes no argument 'status'; did you mean statuses?"
    );
    // In a run, step_wait waits in the run's project; its steps are always named.
    let project = stdout(&tool(home, "status", r#"{"project":"p"}"#))["project"]["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let run_id = sluice_model::ids::RunId::new().to_string();
    let env = [
        ("SLUICE_PROJECT_ID", project.as_str()),
        ("SLUICE_RUN_ID", run_id.as_str()),
        ("SLUICE_STEP", "q"),
    ];
    let waited = ok(tool_with(
        home,
        &[
            "step_wait",
            "--steps",
            "r",
            "--until",
            "settled",
            "--timeout",
            "0",
        ],
        &env,
        None,
    ));
    assert_eq!(waited["met"], true, "{waited}");
    let out = tool_with(
        home,
        &["step_wait", "--until", "settled", "--timeout", "0"],
        &env,
        None,
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "no step is defaulted for step_wait"
    );
    assert_eq!(stderr(&out)["error"], "invalid");
}

/// step_settle gets the schema-driven treatment: --help from its schema, flags, and the
/// nearest tool or field for a typo.
#[test]
fn step_settle_takes_flags_help_and_suggestions() {
    let home = ScratchHome::new().unwrap();
    let home = home.path();
    let help = tool_with(home, &["step_settle", "--help"], &[], None);
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(
        help.starts_with("usage: sluice tool step_settle "),
        "{help}"
    );
    assert!(
        help.contains("Settle a finishing step on its submission"),
        "{help}"
    );
    assert!(help.contains("  --step <string>  (required)"), "{help}");
    assert!(
        help.contains("  --reason <string>  (default \"\")"),
        "{help}"
    );
    let message = refused(&tool_with(home, &["step_setle"], &[], None));
    assert!(
        message.starts_with("unknown tool step_setle; did you mean step_settle"),
        "{message}"
    );
    let message = refused(&tool_with(
        home,
        &["step_settle", "--projet", "p", "--step", "w"],
        &[],
        None,
    ));
    assert_eq!(
        message,
        "step_settle takes no argument 'projet'; did you mean project?"
    );
    // Flags decode into the command: an unknown project is not_found, not a bad request.
    let out = tool_with(
        home,
        &[
            "step_settle",
            "--project",
            "nope",
            "--step",
            "w",
            "--reason",
            "old release",
        ],
        &[],
        None,
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not_found"));
}

/// `unit_add` takes `after` as the documented map of recipe step suffix to step ids, from the
/// JSON argument and from `--after`, and a single id for a suffix as a one-item list.
#[test]
fn unit_add_takes_its_after_map() {
    let home = ScratchHome::new().unwrap();
    let ok = |out: Output| {
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout(&out)
    };
    ok(tool(home.path(), "project_create", r#"{"name":"demo"}"#));
    std::fs::create_dir_all(home.path().join("recipes")).unwrap();
    std::fs::write(
        home.path().join("recipes/lane.json"),
        json!({"name":"lane","params":{},
               "steps":{"{unit}-a":{"run":"core.echo","in":{"value":{"default":1}}},
                        "{unit}-b":{"run":"core.echo","in":{"value":{"default":2}},"after":["{unit}-a"]}}})
        .to_string(),
    )
    .unwrap();
    ok(tool(
        home.path(),
        "step_add",
        &json!({"project":"demo","step":"gate","start":false,
                "spec":{"run":"core.echo","in":{"value":{"default":0}}}})
        .to_string(),
    ));
    ok(tool(
        home.path(),
        "unit_add",
        r#"{"project":"demo","recipe":"lane","params":{"unit":"u1"},"start":false,"after":{"a":["gate"]}}"#,
    ));
    ok(run(
        home.path(),
        &[
            "tool",
            "unit_add",
            "--project",
            "demo",
            "--recipe",
            "lane",
            "--params",
            r#"{"unit":"u2"}"#,
            "--start",
            "false",
            "--after",
            r#"{"b":"gate"}"#,
        ],
    ));
    let plan = ok(tool(home.path(), "plan_get", r#"{"project":"demo"}"#));
    let steps = &plan["plan"]["steps"];
    assert_eq!(steps["u1-a"]["after"], json!(["gate"]), "{plan}");
    assert_eq!(steps["u2-b"]["after"], json!(["u2-a", "gate"]), "{plan}");
}
