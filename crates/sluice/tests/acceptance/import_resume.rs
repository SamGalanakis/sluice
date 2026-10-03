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
    assert_eq!(row["engine"], json!("devin"));
    devin.close().await.unwrap();
    // Preparing commands starts no engine, no tmux and no coordinator.
    assert!(!context.run_dir.join("tmux.sock").exists());
}

struct PythonRunner(std::process::Child);
impl PythonRunner {
    fn stop(&mut self) {
        if self.0.try_wait().unwrap().is_some() {
            return;
        }
        Command::new("/usr/bin/kill")
            .args(["-TERM", &self.0.id().to_string()])
            .status()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while self.0.try_wait().unwrap().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}
impl Drop for PythonRunner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[tokio::test]
#[ignore = "Real Codex old Python runner -> imported predecessor -> public Rust agent.run"]
async fn g7_codex_import_resume() {
    use crate::{
        engines::{Gate, absolute_tool, credentials, private_write, step_finished},
        support::{git, repo as git_repo},
    };
    use std::time::{Duration, Instant};
    let scratch = Scratch::new();
    let mut env = match credentials(&scratch.0, "codex") {
        Ok(e) => e,
        Err(reason) => {
            println!("g7_codex_import_resume PENDING: {reason}");
            return;
        }
    };
    let ws = repo();
    let cwd = scratch.0.join("work");
    fs::create_dir(&cwd).unwrap();
    git_repo(&cwd);
    let baseline = git(&cwd, &["rev-parse", "HEAD"]);
    let source = scratch.0.join("python-home");
    let destination = scratch.0.join("rust-home");
    let bin = scratch.0.join("python-bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(ws.join("target/private-tmux/bin/tmux"), bin.join("tmux")).unwrap();
    std::os::unix::fs::symlink(absolute_tool("codex"), bin.join("codex")).unwrap();
    // The old runner's unnamed scopes are forbidden here; its process ledger owns cleanup.
    private_write(&bin.join("systemd-run"), b"#!/bin/sh\nexit 1\n");
    fs::set_permissions(bin.join("systemd-run"), fs::Permissions::from_mode(0o700)).unwrap();
    let wrapper = format!(
        "#!/bin/sh\nexec '{}' run --project '{}' --no-sync python -m sluice.cli \"$@\"\n",
        absolute_tool("uv").display(),
        ws.display()
    );
    private_write(&bin.join("sluice"), wrapper.as_bytes());
    fs::set_permissions(bin.join("sluice"), fs::Permissions::from_mode(0o700)).unwrap();
    env.insert(
        "PATH".into(),
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
    );
    env.insert("SLUICE_HOME".into(), source.to_string_lossy().into());
    env.insert("PYTHONPATH".into(), ws.join("src").to_string_lossy().into());
    let logs = fs::File::create(scratch.0.join("python-runner.log")).unwrap();
    let mut runner = PythonRunner(
        Command::new(absolute_tool("uv"))
            .args(["run", "--project"])
            .arg(&ws)
            .args(["--no-sync", "python"])
            .arg(ws.join("crates/sluice/tests/acceptance/fixtures/python_runner.py"))
            .arg(&scratch.0)
            .envs(&env)
            .env_remove("CLAUDECODE")
            .env_remove("AI_AGENT")
            .env_remove("SLUICE_FIXTURE")
            .current_dir(&ws)
            .stdout(logs.try_clone().unwrap())
            .stderr(logs)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(250);
    while !scratch.0.join("python-ready.json").exists() {
        assert!(
            runner.0.try_wait().unwrap().is_none(),
            "Python fixture exited: {}",
            fs::read_to_string(scratch.0.join("python-runner.log")).unwrap()
        );
        assert!(Instant::now() < deadline, "Python waiting turn timed out");
        std::thread::sleep(Duration::from_millis(100));
    }
    let ready: Value =
        serde_json::from_slice(&fs::read(scratch.0.join("python-ready.json")).unwrap()).unwrap();
    let session = ready["checkpoint"]["session"].as_str().unwrap().to_string();
    let old_run = ready["old_run"].as_str().unwrap().to_string();
    println!("g7_codex_import_resume Python waiting: session={session} old_run={old_run}");
    let first_commit = git(&cwd, &["rev-parse", "HEAD"]);
    assert_ne!(first_commit, baseline);
    assert_eq!(
        git(&cwd, &["rev-list", "--count", &format!("{baseline}..HEAD")]),
        "1"
    );
    runner.stop();
    let cleanup: Value = serde_json::from_slice(
        &fs::read(scratch.0.join("python-cleanup.json")).expect("old Python cleanup proof absent"),
    )
    .unwrap();
    assert!(
        cleanup
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["native_empty"] == true && c["wrapper_reaped"] == true)
    );
    let snapshot = fs::read(source.join("sluice.db")).unwrap();
    private_write(
        &scratch.0.join("staging/config.json"),
        b"{\"fn_dirs\": []}\n",
    );
    let out = Command::new(ws.join("target/debug/sluice"))
        .arg("import-python-home")
        .arg(&source)
        .arg(&destination)
        .arg("--staging")
        .arg(scratch.0.join("staging"))
        .envs(&env)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // p7-02 owns owner-facing cutover release/select. v2 release explicitly refuses cutover.
    // This scratch-only writer transition substitutes for that owner step before activation.
    let writer = sluice_store::Writer::open(&destination).unwrap();
    writer.write(sluice_store::RetrySafety::NonIdempotent,|tx|{
        assert_eq!(tx.sql().query_row("SELECT mode FROM maintenance WHERE singleton=1",[],|r|r.get::<_,String>(0))?,"cutover");
        tx.sql().execute("UPDATE maintenance SET mode='normal',owner=NULL,revision=revision+1 WHERE singleton=1",[])?;
        tx.changed(None,"maintenance");Ok(())
    }).await.unwrap();
    writer.shutdown().await.unwrap();
    let mut gate = Gate::new(&scratch.0, destination, env);
    gate.boot();
    let selector = json!({"kind":"name","value":"import-resume"});
    gate.rpc(json!({"command":"project_update","args":{"project":selector,"paused":false,"author":"fixture","reason":"scratch cutover release"}}));
    gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"sluice was upgraded; continue where you left off. Resume the same session, do not create or commit first.txt again. Follow the new task.md and current Rust RunId instructions exclusively. Post an addressed message with exact body G7_UPGRADED to the orchestrator using the current message_post command, then submit word=upgraded using the current step_submit command. Finish this turn.","reason":"scratch G7 cutover","author":"fixture"}}));
    gate.scheduling();
    let done = step_finished(&mut gate, &selector);
    assert_eq!(done["outputs"]["session"], session);
    assert_eq!(done["outputs"]["word"], "upgraded");
    let new_run = done["run_ids"][0].as_str().unwrap();
    assert_ne!(new_run, old_run);
    gate.track(new_run);
    let task = gate.home.join("runs").join(new_run).join("task.md");
    let text = fs::read_to_string(&task).unwrap();
    assert!(text.contains(new_run), "new run header absent");
    assert!(!text.contains(&old_run), "old callback present");
    assert_eq!(git(&cwd, &["rev-parse", "HEAD"]), first_commit);
    assert_eq!(
        git(&cwd, &["rev-list", "--count", &format!("{baseline}..HEAD")]),
        "1"
    );
    let reads = sluice_store::ReadPool::open(&gate.home, 1).unwrap();
    let owned_run = new_run.to_string();
    let (submissions,messages)=reads.snapshot(move |sql|{
        let submitted:String=sql.query_row("SELECT outputs FROM submissions WHERE run_id=?1",[&owned_run],|r|r.get(0))?;
        let messages:i64=sql.query_row("SELECT count(*) FROM messages WHERE run_id=?1 AND body='G7_UPGRADED' AND \"to\"='orchestrator'",[&owned_run],|r|r.get(0))?;
        Ok((submitted,messages))
    }).await.unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&submissions).unwrap()["word"],
        "upgraded"
    );
    assert_eq!(messages, 1, "new-run message_post callback absent");
    let mapping: Value = serde_json::from_slice(
        &fs::read(
            gate.home
                .join("codex-native-sessions")
                .join(format!("{session}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        rollout_contains_task(Path::new(mapping["home"].as_str().unwrap()), &task),
        "resumed transcript lacks current task path and Rust RunId"
    );
    assert_eq!(
        fs::read(source.join("sluice.db")).unwrap(),
        snapshot,
        "Rust callback changed source Python home"
    );
    gate.wait(Duration::from_secs(20), |g| g.groups_empty());
    gate.assert_clean();
    gate.cleanup();
    println!(
        "g7_codex_import_resume PASS before_session={session} after_session={session} old_run={old_run} new_run={new_run} task={} original_commits=1",
        task.display()
    );
}

fn rollout_contains_task(home: &Path, task: &Path) -> bool {
    let mut dirs = vec![home.join("sessions")];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file()
                && entry.path().extension().is_some_and(|e| e == "jsonl")
                && fs::read_to_string(entry.path())
                    .is_ok_and(|text| text.contains(task.to_str().unwrap()))
            {
                return true;
            }
        }
    }
    false
}
