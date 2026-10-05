//! `sluice me` and the `step_context` tool (SPEC §9): the answer to "where do
//! I stand" for the agent inside a step — ported from tests/test_me.py.
#[allow(dead_code)]
#[path = "../../../tests/support/mod.rs"]
mod support;

use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};
use support::home::ScratchHome;

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home)
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_AUTHOR")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT")
        .env_remove("SLUICE_PROJECT_ID")
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

fn project(home: &Path) {
    let created = tool(
        home,
        "project_create",
        r#"{"name":"demo","description":"me"}"#,
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let patched = tool(
        home,
        "plan_patch",
        &json!({
            "project": "demo", "rev": 1, "reason": "plan",
            "ops": [{"op": "add", "path": "/steps/a",
                     "value": {"run": "core.echo", "in": {"value": {"default": 1}}, "doc": "adds"}},
                    {"op": "add", "path": "/steps/b",
                     "value": {"run": "core.external",
                               "in": {"seen": {"source": "a/value"}},
                               "outputs": {"word": "string"}, "doc": "b"}}],
        })
        .to_string(),
    );
    assert!(
        patched.status.success(),
        "{}",
        String::from_utf8_lossy(&patched.stderr)
    );
}

#[test]
fn outside_a_step_it_says_so_and_exits_1() {
    let home = ScratchHome::new().unwrap();
    let out = run(home.path(), &["me"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out)["message"]
            .as_str()
            .unwrap()
            .contains("--project")
    );
}

#[test]
fn me_with_flags_works_outside_a_steps_env() {
    let home = ScratchHome::new().unwrap();
    project(home.path());
    let out = run(home.path(), &["me", "--project", "demo", "--step", "a"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("step a (core.echo) — pending"), "{text}");
    assert!(text.contains("doc: adds"), "{text}");
    let out = run(home.path(), &["me", "--project", "demo", "--step", "b"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    // no run yet: the submit command shows the placeholder
    assert!(text.contains(r#""run":"<run>""#), "{text}");
    assert!(text.contains(r#""word":"<string>""#), "{text}");
    assert!(text.contains("upstream a (core.echo): pending"), "{text}");
}

#[test]
fn me_from_the_steps_environment() {
    let home = ScratchHome::new().unwrap();
    project(home.path());
    let project_id = stdout(&tool(home.path(), "projects_list", "{}"))
        .pointer("/0/project_id")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned();
    let out = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home.path())
        .env("SLUICE_PROJECT_ID", &project_id)
        .env("SLUICE_STEP", "a")
        .env_remove("SLUICE_PROJECT")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_AUTHOR")
        .arg("me")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .starts_with("step a (core.echo) — pending")
    );
}

#[test]
fn the_step_context_tools_shape() {
    let home = ScratchHome::new().unwrap();
    project(home.path());
    let out = tool(
        home.path(),
        "step_context",
        r#"{"project":"demo","step":"a"}"#,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let context = stdout(&out);
    let mut keys: Vec<_> = context.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "ask", "attempt", "doc", "elapsed", "finished", "fn", "inputs", "messages", "project",
            "run", "started", "status", "step", "submit", "thread", "upstream"
        ]
    );
    assert_eq!(context["step"], "a");
    assert_eq!(context["fn"], "core.echo");
    assert_eq!(context["status"], "pending");
    assert_eq!(context["thread"], "step-a");
    let missing = tool(
        home.path(),
        "step_context",
        r#"{"project":"demo","step":"zz"}"#,
    );
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(stderr(&missing)["error"], "not_found");
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(repo)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A step whose `cwd` is a git repository with two uncommitted paths.
fn dirty_project(home: &Path) -> std::path::PathBuf {
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("kept.txt"), "kept\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    std::fs::write(repo.join("kept.txt"), "left behind\n").unwrap();
    std::fs::write(repo.join("scratch.md"), "notes\n").unwrap();
    let created = tool(
        home,
        "project_create",
        r#"{"name":"demo","description":"me"}"#,
    );
    assert!(created.status.success());
    let patched = tool(
        home,
        "plan_patch",
        &json!({
            "project": "demo", "rev": 1, "reason": "plan",
            "ops": [{"op": "add", "path": "/steps/w",
                     "value": {"run": "core.external", "in": {"cwd": {"default": repo}},
                               "outputs": {"word": "string"}}}],
        })
        .to_string(),
    );
    assert!(
        patched.status.success(),
        "{}",
        String::from_utf8_lossy(&patched.stderr)
    );
    repo
}

/// step_context (and `sluice me`) says which attempt this is: before any run, the first, with
/// the git status of the step's cwd now.
#[test]
fn step_context_names_the_first_attempt_and_the_dirty_work_tree() {
    let home = ScratchHome::new().unwrap();
    let repo = dirty_project(home.path());
    let context = stdout(&tool(
        home.path(),
        "step_context",
        r#"{"project":"demo","step":"w"}"#,
    ));
    let attempt = &context["attempt"];
    assert_eq!(attempt["number"], 1, "{context}");
    assert!(attempt.get("previous").is_none(), "{attempt}");
    assert_eq!(
        attempt["worktree"],
        json!({"cwd": repo, "git": "dirty", "count": 2, "paths": ["M kept.txt", "?? scratch.md"]})
    );
    let note = attempt["note"].as_str().unwrap();
    assert!(
        note.contains("None: this is the first attempt at this step."),
        "{note}"
    );
    assert!(
        note.contains(&format!(
            "The working directory {} has 2 uncommitted paths now: M kept.txt, ?? scratch.md.",
            repo.display()
        )),
        "{note}"
    );
    let me = run(home.path(), &["me", "--project", "demo", "--step", "w"]);
    assert!(me.status.success());
    assert!(
        String::from_utf8(me.stdout)
            .unwrap()
            .contains("None: this is the first attempt at this step.")
    );
}

/// Without git the context still answers; the work tree says why it is unknown.
#[test]
fn step_context_without_git_says_the_work_tree_is_unavailable() {
    let home = ScratchHome::new().unwrap();
    dirty_project(home.path());
    let out = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .env("SLUICE_HOME", home.path())
        .env("PATH", home.path().join("no-bin"))
        .env_remove("SLUICE_STEP")
        .env_remove("SLUICE_RUN_ID")
        .env_remove("SLUICE_PROJECT")
        .env_remove("SLUICE_PROJECT_ID")
        .args(["tool", "step_context", r#"{"project":"demo","step":"w"}"#])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let context = stdout(&out);
    assert_eq!(
        context["attempt"]["worktree"]["git"], "unavailable",
        "{context}"
    );
    assert!(
        context["attempt"]["note"]
            .as_str()
            .unwrap()
            .contains("is unavailable: git is not installed or not on PATH"),
        "{context}"
    );
}
