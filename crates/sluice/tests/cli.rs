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
    path::{Path, PathBuf},
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

/// Dispatched commands spawn a `sluice-test-coordinator-<digest>` systemd unit;
/// stop it when the test ends so no process outlives its deleted home.
struct StopCoordinator(PathBuf);
impl Drop for StopCoordinator {
    fn drop(&mut self) {
        let digest = sluice_store::artifacts::fingerprint(self.0.as_os_str().as_encoded_bytes());
        let unit = format!("sluice-test-coordinator-{}", &digest[..16]);
        for operation in ["stop", "reset-failed"] {
            let _ = Command::new("/usr/bin/systemctl")
                .args(["--user", operation, &unit])
                .output();
        }
    }
}
fn guard(home: &Path) -> StopCoordinator {
    StopCoordinator(home.to_path_buf())
}

#[test]
fn the_first_run_writes_the_default_config_and_lists_the_tools() {
    let home = ScratchHome::new().unwrap();
    let listing = run(home.path(), &["tool"]);
    assert!(listing.status.success());
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join("config.json")).unwrap())
            .unwrap();
    assert_eq!(
        config,
        json!({"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420}, "log_max": 10000})
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
}

#[test]
fn tool_errors_and_bad_arguments_exit_1() {
    let home = ScratchHome::new().unwrap();
    let _guard = guard(home.path());
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
    let listing = run(home.path(), &["query"]);
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("table projects("));
}

#[test]
fn a_project_through_the_tools() {
    let home = ScratchHome::new().unwrap();
    let _guard = guard(home.path());
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

fn post(home: &Path, thread: &str, body: &str, needs_reply: bool) {
    let out = tool(
        home,
        "message_post",
        &json!({"project": "p", "thread": thread, "body": body, "from": "x",
                "needs_reply": needs_reply})
        .to_string(),
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
    let _guard = guard(home.path());
    tool(
        home.path(),
        "project_create",
        r#"{"name":"p","description":""}"#,
    );
    post(home.path(), "q", "before", true);

    // From now: records appended while the watcher runs, on the chosen
    // threads. A watcher resolves its start bounds on open, so appends race
    // it; post until the newest marker lands, which proves it is live.
    let (mut child, rx) = watch(
        home.path(),
        &["watch", "-p", "p", "--kinds", "message", "--threads", "q"],
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
        &["watch", "-p", "p", "--threads", "q", "--since-seq", "0"],
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
    let _guard = guard(home.path());
    tool(
        home.path(),
        "project_create",
        r#"{"name":"p","description":""}"#,
    );
    let (mut child, rx) = watch(
        home.path(),
        &["watch", "-p", "p", "--threads", "q", "--wake", "questions"],
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
    let _guard = guard(home.path());
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
    let _guard = guard(home.path());
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
    let _guard = guard(home.path());
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
                "message_post",
                json!({"project": selector, "thread": "step-work", "from": "work",
                       "to": "orchestrator", "body": "question", "needs_reply": false}),
            ),
            ("status", json!({"project": selector})),
            ("plan_get", json!({"project": selector})),
            ("log_read", json!({"project": selector})),
            ("fn_list", json!({"project": selector})),
            (
                "messages",
                json!({"project": selector, "view": "thread", "thread": "step-work"}),
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
    let posted = tool(
        home.path(),
        "messages",
        &json!({"project": "demo", "view": "thread", "thread": "step-work"}).to_string(),
    );
    assert_eq!(stdout(&posted)["messages"].as_array().unwrap().len(), 3);
    let unknown = sluice_model::ids::ProjectId::new();
    let missing = tool(
        home.path(),
        "message_post",
        &json!({"project": unknown.to_string(), "body": "x", "from": "work",
                "needs_reply": false})
        .to_string(),
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
