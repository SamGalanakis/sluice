#[test]
#[ignore = "G7 private installation rollback through a scratch release"]
fn g7_rollback() {
    rehearse();
}

fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn selected(root: &Path, args: &[&str]) -> Value {
    let out = common::install(root, args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn old_command(
    bin: &Path,
    home: &Path,
    env: &BTreeMap<String, String>,
    name: &str,
    args: Value,
) -> Value {
    let out = common::checked(
        Command::new(bin)
            .args(["tool", name])
            .arg(args.to_string())
            .envs(env)
            .env("SLUICE_HOME", home),
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn old_runner(root: &Path, home: &Path, env: &BTreeMap<String, String>, mode: &str) -> OldProcess {
    let log = fs::File::create(root.join(format!("python-{mode}.log"))).unwrap();
    OldProcess(
        Command::new("/home/sam/.local/bin/uv")
            .args(["run", "--project"])
            .arg(common::workspace())
            .args(["--no-sync", "python"])
            .arg(common::workspace().join("crates/sluice/tests/rehearsal/fixtures/old_runner.py"))
            .arg(root)
            .arg(mode)
            .envs(env)
            .env("SLUICE_HOME", home)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    )
}
fn stop_old(root: &Path, child: &mut OldProcess, mode: &str) {
    common::checked(Command::new("/bin/kill").args(["-TERM", &child.0.id().to_string()]));
    assert!(
        child.0.wait().unwrap().success(),
        "old runner cleanup failed"
    );
    assert!(
        read(root.join(format!("python-{mode}-cleanup.json")))
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["native_empty"] == true && r["wrapper_reaped"] == true)
    );
}
fn rehearse() {
    let scratch = common::scratch();
    let root = scratch.path();
    let mut env = lane::environment(root);
    let old_release = root.join("python-release");
    fs::create_dir_all(old_release.join("bin")).unwrap();
    let old_bin = old_release.join("bin/sluice");
    fs::write(&old_bin,format!("#!/bin/sh\nexport PYTHONPATH='{}'\nexec /home/sam/.local/bin/uv run --project '{}' --no-sync python -m sluice.cli \"$@\"\n",common::workspace().join("src").display(),common::workspace().display())).unwrap();
    fs::set_permissions(&old_bin, fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!(
        "{}:{}",
        old_release.join("bin").display(),
        root.join("bin").display()
    );
    env.insert("PATH".into(), path.clone());
    env.insert("SLUICE_HOST_PATH".into(), path);
    symlink(
        common::workspace().join("target/private-tmux/bin/tmux"),
        old_release.join("bin/tmux"),
    )
    .unwrap();
    fs::write(old_release.join("bin/systemd-run"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(
        old_release.join("bin/systemd-run"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    lane::git(
        root,
        &[
            "init",
            "-q",
            "--bare",
            "--initial-branch=main",
            "remote.git",
        ],
    );
    lane::git(
        root,
        &[
            "clone",
            "-q",
            root.join("remote.git").to_str().unwrap(),
            "repo",
        ],
    );
    let repo = root.join("repo");
    lane::git(&repo, &["config", "user.name", "Rollback fixture"]);
    lane::git(&repo, &["config", "user.email", "rollback@example.invalid"]);
    lane::git(
        &repo,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Create the rollback baseline.",
        ],
    );
    lane::git(&repo, &["push", "-q", "origin", "HEAD:main"]);
    let hook = root.join("remote.git/hooks/post-receive");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nprintf '%s\\n' push >> '{}'\n",
            root.join("pushes").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let python_home = root.join("python-home");
    let mut python = old_runner(root, &python_home, &env, "start");
    common::wait(|| {
        assert!(
            python.0.try_wait().unwrap().is_none(),
            "{}",
            fs::read_to_string(root.join("python-start.log")).unwrap()
        );
        root.join("python-ready.json").is_file()
    });
    let ready = read(root.join("python-ready.json"));
    let session = ready["checkpoint"]["session"].as_str().unwrap();
    stop_old(root, &mut python, "start");
    common::tree(&python_home, &root.join("python-backup"));
    common::tree(&python_home, &root.join("source"));
    fs::write(
        root.join("source/.snapshot-origin.json"),
        json!({"home":python_home}).to_string(),
    )
    .unwrap();
    relocate(&python_home, &root.join("source"));
    let rust_home = root.join("rust-home");
    env.insert("PATH".into(), root.join("bin").to_string_lossy().into());
    env.insert(
        "SLUICE_HOST_PATH".into(),
        root.join("bin").to_string_lossy().into(),
    );
    let report = common::import(root, &rust_home);
    assert_eq!(report["interrupted_runs"], 1);
    let release = common::release_fixture(root);
    selected(root, &["fence", "scratch import switch"]);
    selected(
        root,
        &[
            "select",
            release.to_str().unwrap(),
            rust_home.to_str().unwrap(),
        ],
    );
    env.insert(
        "SLUICE_BIN".into(),
        release.join("bin/sluice").to_string_lossy().into(),
    );
    let mut rust = common::boot(&release.join("bin/sluice"), &rust_home, &env);
    let owned = common::OwnedHome(rust_home.clone());
    let selector = json!({"kind":"name","value":"rollback-fixture"});
    common::ok(
        &rust_home,
        json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":"sluice was upgraded; continue where you left off","reason":"fixture import resume","author":"fixture"}}),
    );
    selected(root, &["unfence"]);
    common::ok(
        &rust_home,
        json!({"command":"project_update","args":{"project":selector,"paused":false,"reason":"restore original pause","author":"fixture"}}),
    );
    env.insert("PATH".into(), root.join("bin").to_string_lossy().into());
    env.insert(
        "SLUICE_HOST_PATH".into(),
        root.join("bin").to_string_lossy().into(),
    );
    let lease = common::scheduler(&rust_home);
    // Submit once per run: a later submission changes the version the guardian's
    // completion journal recorded, and the coordinator refuses that completion.
    let mut submitted = std::collections::BTreeSet::new();
    common::wait(|| {
        let status = common::data(
            &rust_home,
            json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null}}}),
        );
        if status["steps"]["work"]["status"] == "running" {
            let run = status["steps"]["work"]["run_ids"][0].as_str().unwrap();
            if !submitted.contains(run) {
                let reply = common::rpc(
                    &rust_home,
                    json!({"command":"step_submit","args":{"project":status["project"]["project_id"],"step":"work","run":run,"outputs":{"word":"resumed"},"author":"fixture"}}),
                );
                if reply["result"]["status"] == "ok" {
                    submitted.insert(run.to_string());
                }
            }
        }
        status["steps"]["push"]["status"] == "succeeded"
    });
    let status = common::data(
        &rust_home,
        json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null}}}),
    );
    assert_eq!(status["steps"]["work"]["outputs"]["session"], session);
    let sha = status["steps"]["push"]["outputs"]["sha"].as_str().unwrap();
    assert_eq!(
        fs::read_to_string(root.join("pushes"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    // Race cached-socket direct callers with exclusive fence publication. Check
    // calls made after fence returns separately so wall-clock races cannot hide a bug.
    let mut cached =
        std::os::unix::net::UnixStream::connect(rust_home.join("coordinator.sock")).unwrap();
    common::checked(Command::new("/bin/kill").args(["-STOP", &rust.0.id().to_string()]));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(9));
    let calls:Vec<_>=(0..8).map(|_|{let home=rust_home.clone();let b=barrier.clone();std::thread::spawn(move||{b.wait();common::rpc(&home,json!({"command":"fn_call","args":{"name":"core.echo","project":{"kind":"name","value":"rollback-fixture"},"inputs":{"value":1},"direct":true,"wait_seconds":0,"author":"fence-race"}}))})}).collect();
    barrier.wait();
    let fence = selected(root, &["fence", "scratch rollback"]);
    common::checked(Command::new("/bin/kill").args(["-CONT", &rust.0.id().to_string()]));
    for call in calls {
        let result = call.join().unwrap();
        assert_eq!(result["result"]["status"], "error");
        assert_eq!(result["result"]["value"]["error"], "busy");
    }
    let cached_reply = common::rpc_stream(
        &mut cached,
        json!({"command":"fn_call","args":{"name":"core.echo","project":selector,"inputs":{"value":1},"direct":true,"wait_seconds":0,"author":"cached-before-fence"}}),
    );
    assert_eq!(cached_reply["result"]["status"], "error");
    assert_eq!(cached_reply["result"]["value"]["error"], "busy");
    for _ in 0..8 {
        let reply = common::rpc(
            &rust_home,
            json!({"command":"fn_call","args":{"name":"core.echo","project":selector,"inputs":{"value":1},"direct":true,"wait_seconds":0,"author":"after-fence"}}),
        );
        assert_eq!(reply["result"]["status"], "error");
        assert_eq!(reply["result"]["value"]["error"], "busy");
    }
    drop(lease);
    // Fence forbids further claims; explicitly cancel all scratch calls already admitted.
    for _ in 0..30 {
        if common::readonly(&rust_home)
            .query_row(
                "SELECT count(*) FROM runs WHERE finished_at IS NULL",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 0
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        common::readonly(&rust_home)
            .query_row(
                "SELECT count(*) FROM runs WHERE finished_at IS NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    owned.assert_empty();
    rust.0.kill().unwrap();
    rust.0.wait().unwrap();
    let refused = Command::new(common::binary())
        .args(["tool", "fn_call"])
        .arg(json!({"name":"core.echo","inputs":{"value":1},"direct":true}).to_string())
        .envs(&env)
        .env("SLUICE_HOME", &rust_home)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("maintenance"));
    common::tree(&rust_home, &root.join("rust-recovery-snapshot"));
    // All owned runs must be gone before a recovery snapshot and Python selection.
    let restored = root.join("restored-home");
    common::tree(&root.join("python-backup"), &restored);
    relocate(&python_home, &restored);
    selected(
        root,
        &[
            "select",
            old_release.to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
    );
    let pair = selected(root, &["status"]);
    assert_eq!(
        pair["selection"]["release_path"],
        old_release.to_str().unwrap()
    );
    assert_eq!(pair["selection"]["home_path"], restored.to_str().unwrap());
    assert!(pair["fence"].is_object());
    let old_path = format!(
        "{}:{}",
        old_release.join("bin").display(),
        root.join("bin").display()
    );
    env.insert("PATH".into(), old_path.clone());
    env.insert("SLUICE_HOST_PATH".into(), old_path);
    let mut python = old_runner(root, &restored, &env, "restore");
    common::wait(|| {
        assert!(
            python.0.try_wait().unwrap().is_none(),
            "{}",
            fs::read_to_string(root.join("python-restore.log")).unwrap()
        );
        root.join("python-adopted.json").is_file()
    });
    assert_eq!(
        common::readonly(&restored)
            .query_row("SELECT count(*) FROM projects WHERE paused=0", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    old_command(
        &old_bin,
        &restored,
        &env,
        "step_set_input",
        json!({"project":"rollback-fixture","step":"work","input":"session","value":session,"reason":"resume after rollback"}),
    );
    old_command(
        &old_bin,
        &restored,
        &env,
        "thread_post",
        json!({"project":"rollback-fixture","thread":"step-work","to":"work","body":"Continue where you left off after rollback; use this Python run header and callback tools.","needs_reply":false}),
    );
    old_command(
        &old_bin,
        &restored,
        &env,
        "step_retry",
        json!({"project":"rollback-fixture","steps":["work"],"reason":"resume after rollback"}),
    );
    // The committed push survives SQLite restoration. Record that known result
    // through the old owner tool before releasing the original active project.
    // Its upstream was just retried, so old Python requires force; the push then
    // turns stale rather than running again.
    old_command(
        &old_bin,
        &restored,
        &env,
        "step_set_output",
        json!({"project":"rollback-fixture","step":"push","outputs":{"sha":sha},"force":true,"reason":"reconciled Rust push; never repeat"}),
    );
    selected(root, &["unfence"]);
    old_command(
        &old_bin,
        &restored,
        &env,
        "project_update",
        json!({"name":"rollback-fixture","paused":false,"reason":"restore pre-window active state"}),
    );
    common::wait(|| {
        assert!(
            python.0.try_wait().unwrap().is_none(),
            "{}",
            fs::read_to_string(root.join("python-restore.log")).unwrap()
        );
        root.join("python-resumed.json").is_file()
    });
    let resumed = read(root.join("python-resumed.json"));
    assert_eq!(resumed["checkpoint"]["session"], session);
    assert_eq!(resumed["push_status"], "succeeded");
    assert_ne!(resumed["run"], ready["run"]);
    assert!(
        resumed["task"]
            .as_str()
            .unwrap()
            .contains(resumed["run"].as_str().unwrap())
    );
    assert!(
        !resumed["task"]
            .as_str()
            .unwrap()
            .contains(ready["run"].as_str().unwrap())
    );
    assert!(
        common::readonly(&restored)
            .query_row(
                "SELECT paused FROM projects WHERE name='already-paused'",
                [],
                |r| r.get::<_, bool>(0)
            )
            .unwrap()
    );
    assert_eq!(
        fs::read_to_string(root.join("pushes"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    stop_old(root, &mut python, "restore");
    common::evidence(
        "g7_rollback",
        &json!({"old_session":session,"resumed_rust_session":status["steps"]["work"]["outputs"]["session"],"restored_python_session":resumed["checkpoint"]["session"],"rust_pushes":1,"rollback_pushes":0,"fence_generation":fence["generation"],"post_fence_refusals":8,"selected_python_home_verified":true,"already_paused_preserved":true,"owned_work_empty":true}),
    );
}
use crate::{common, lane};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::Command,
    time::Duration,
};

fn relocate(old: &Path, home: &Path) {
    common::checked(
        Command::new("/usr/bin/python3")
            .arg(common::workspace().join("crates/sluice/tests/rehearsal/fixtures/relocate.py"))
            .arg(old)
            .arg(home),
    );
}

struct OldProcess(std::process::Child);
impl Drop for OldProcess {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        let _ = Command::new("/bin/kill")
            .args(["-TERM", &self.0.id().to_string()])
            .output();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while self.0.try_wait().ok().flatten().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
