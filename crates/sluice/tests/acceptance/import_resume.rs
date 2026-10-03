//! Imported-session fixtures are compiled by the agents acceptance executable.
//! All importer calls use this worktree's absolute binary and synthetic Python homes.
use crate::support::Scratch;
use serde_json::{Value, json};
use sluice_agents::engines::{
    EngineAdapter, EngineContext,
    claude::{profile as claude_profile, state::Claude},
    devin::{Devin, DevinOptions},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn python(root: &Path, mode: &str, path: &Path) -> Value {
    let repo = repo();
    let out = Command::new("uv")
        .args(["run", "--project"])
        .arg(&repo)
        .args(["--no-sync", "python"])
        .arg(repo.join("crates/sluice/tests/acceptance/fixtures/python_home.py"))
        .arg(mode)
        .arg(path)
        .current_dir(&repo)
        .env("SLUICE_HOME", root.join("python-home"))
        .env("PYTHONPATH", repo.join("src"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if out.stdout.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&out.stdout).unwrap()
    }
}
#[tokio::test]
async fn imported_claude_and_devin_predecessors_resolve_and_prepare_resume_commands() {
    let scratch = Scratch::new();
    python(&scratch.0, "build", &scratch.0);
    let owner = scratch.0.join("owner");
    let source = scratch.0.join("python-home");
    let destination = scratch.0.join("rust-home");
    let repo = repo();
    let out = Command::new(repo.join("target/debug/sluice"))
        .arg("import-python-home")
        .arg(&source)
        .arg(&destination)
        .arg("--staging")
        .arg(scratch.0.join("staging"))
        .env("HOME", &owner)
        .env("CLAUDE_CONFIG_DIR", owner.join(".claude"))
        .env("SLUICE_HOME", &destination)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let imported = python(&scratch.0, "inspect", &destination);
    let cwd = scratch.0.join("work");
    let context = EngineContext {
        run_dir: scratch.0.join("adapter-run"),
        cwd: cwd.clone(),
        model: None,
        effort: None,
        tmux_binary: Some(repo.join("target/private-tmux/bin/tmux")),
    };
    fs::create_dir(&context.run_dir).unwrap();
    let claude_binary = scratch.0.join("claude");
    fs::write(&claude_binary, "#!/bin/sh\necho '2.1.284 (Claude Code)'\n").unwrap();
    fs::set_permissions(&claude_binary, fs::Permissions::from_mode(0o700)).unwrap();
    let mut claude = Claude::new(
        claude_binary,
        owner.join(".claude"),
        repo.join("target/debug/sluice"),
        sluice_model::ids::RunId::new(),
    );
    let row = imported
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["engine"] == "claude")
        .unwrap();
    let sid = row["session"].as_str().unwrap();
    assert_eq!(claude.session(sid).await.unwrap().unwrap().cwd, cwd);
    let launch = claude.prepare(&context, Some(sid)).await.unwrap().unwrap();
    assert!(launch.argv.windows(2).any(|a| a == ["--resume", sid]));
    assert_eq!(
        launch.argv,
        claude_profile::argv(
            &scratch.0.join("claude"),
            &context.run_dir.join("claude-settings.json"),
            Some(sid),
            None
        )
    );
    claude.close().await.unwrap();
    let devin_binary = scratch.0.join("devin");
    fs::write(&devin_binary,"#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'devin 3000.11.3'; else echo '--config --export --model --resume --respect-workspace-trust'; fi\n").unwrap();
    fs::set_permissions(&devin_binary, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(scratch.0.join("devin-config.json"), "{}").unwrap();
    let opts = DevinOptions {
        binary: devin_binary,
        config: scratch.0.join("devin-config.json"),
        data_home: owner.join(".local/share"),
        hook_binary: repo.join("target/debug/sluice"),
        environment: Default::default(),
        ..Default::default()
    };
    let mut devin = Devin::new(opts);
    let row = imported
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["engine"] == "devin")
        .unwrap();
    let sid = row["session"].as_str().unwrap();
    assert_eq!(devin.session(sid).await.unwrap().unwrap().cwd, cwd);
    let launch = devin.prepare(&context, Some(sid)).await.unwrap().unwrap();
    assert!(launch.argv.windows(2).any(|a| a == ["--resume", sid]));
    assert_eq!(row["metadata"]["engine"], json!("devin"));
    devin.close().await.unwrap();
    // Preparing commands starts no engine, no tmux and no coordinator.
    assert!(!context.run_dir.join("tmux.sock").exists());
}
