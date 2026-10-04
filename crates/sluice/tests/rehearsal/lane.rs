#[test]
#[ignore = "G7 converted lash lane through fake engines and scratch executables"]
fn g7_lane() {
    run_lane();
}

fn run_lane() {
    let scratch = common::scratch();
    let root = scratch.path();
    let home = root.join("rust-home");
    let env = environment(root);
    git(
        root,
        &[
            "init",
            "-q",
            "--bare",
            "--initial-branch=main",
            "remote.git",
        ],
    );
    git(
        root,
        &[
            "clone",
            "-q",
            root.join("remote.git").to_str().unwrap(),
            "repo",
        ],
    );
    let repo = root.join("repo");
    git(&repo, &["config", "user.name", "Rehearsal fixture"]);
    git(
        &repo,
        &["config", "user.email", "rehearsal@example.invalid"],
    );
    git(
        &repo,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "Create the scratch baseline.",
        ],
    );
    git(&repo, &["push", "-q", "origin", "HEAD:main"]);
    fs::write(root.join("ignore"), "env.sh\n").unwrap();
    fs::create_dir_all(&home).unwrap();
    let _coordinator = common::boot(&common::binary(), &home, &env);
    let owned = common::OwnedHome(home.clone());
    let reply = common::ok(
        &home,
        json!({"command":"project_create","args":{"name":"lash","description":"Scratch lash lane","icon":null,"resources":{"land":1,"lane":1},"author":"fixture"}}),
    );
    let project = reply["data"]["project_id"].as_str().unwrap();
    let fns = home.join("projects").join(project).join("fns");
    let source = common::workspace().join("cutover-staging/projects/lash/fns");
    common::tree(&source, &fns);
    let selector = json!({"kind":"id","value":project});
    let spec = root.join("spec.md");
    fs::write(&spec, "Complete the labelled scratch lane.").unwrap();
    let recipe: Value = serde_json::from_slice(
        &fs::read(common::workspace().join("cutover-staging/projects/lash/recipes/lane.json"))
            .unwrap(),
    )
    .unwrap();
    let serialized = recipe["steps"]
        .to_string()
        .replace("{unit}", "g7")
        .replace("{ticket}", "G7-1")
        .replace("{engine}", "codex")
        .replace("{model}", "sol")
        .replace("{spec}", spec.to_str().unwrap())
        .replace("/workspace/code/lash", repo.to_str().unwrap());
    let steps: Value = serde_json::from_str(&serialized).unwrap();
    common::ok(
        &home,
        json!({"command":"plan_patch","args":{"project":selector,"rev":1,"ops":[{"op":"replace","path":"","value":{"inputs":{},"outputs":{},"steps":steps}}],"start":true,"dry_run":false,"reason":"scratch converted lane","author":"fixture"}}),
    );
    let lease = common::scheduler(&home);
    let mut submitted = BTreeSet::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let status = common::data(
            &home,
            json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null}}}),
        );
        if status["steps"]["g7-rm"]["status"] == "succeeded" {
            break;
        }
        let work = &status["steps"]["g7-work"];
        if work["status"] == "running" {
            let run = work["run_ids"][0].as_str().unwrap();
            if !submitted.contains(run) && root.join("worker-committed").exists() {
                let ready = !submitted.is_empty();
                let reply = common::rpc(
                    &home,
                    json!({"command":"step_submit","args":{"project":project,"step":"g7-work","run":run,"outputs":{"ready":ready,"evidence":"Labelled scratch evidence","unresolved":if ready {""} else {"First land must reject"}},"author":"fixture"}}),
                );
                if reply["result"]["status"] == "ok" {
                    submitted.insert(run.to_string());
                }
            }
        }
        assert!(Instant::now() < deadline, "lane timed out: {status}");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(lease);
    assert_eq!(submitted.len(), 2, "send-back must execute work twice");
    assert!(
        !root.join("fork").exists(),
        "cleanup did not remove the scratch fork"
    );
    let db = common::readonly(&home);
    let action:(String,String)=db.query_row("SELECT result,action_outcome FROM runs WHERE step_id='g7-land' ORDER BY created_at LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&action.0).unwrap()["status"],
        "failed"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&action.1).unwrap()["outcome"],
        "applied"
    );
    let land_leases:i64=db.query_row("SELECT count(*) FROM leases l JOIN runs r USING(run_id) WHERE r.step_id='g7-land' AND resource='land' AND state='released'",[],|r|r.get(0)).unwrap();
    assert!(land_leases > 0, "land did not acquire/release its lease");
    let feedback: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE body LIKE '%Do not re-submit the old commit.%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(feedback, 1);
    let status = common::data(
        &home,
        json!({"command":"status","args":{"project":selector,"selection":{"steps":null,"tags":null}}}),
    );
    for step in [
        "g7-fork",
        "g7-work",
        "g7-land",
        "g7-landed",
        "g7-close",
        "g7-rm",
    ] {
        assert_eq!(status["steps"][step]["status"], "succeeded");
    }
    let sha = status["steps"]["g7-land"]["outputs"]["landed_sha"]
        .as_str()
        .unwrap();
    assert_eq!(
        git(
            root,
            &[
                "--git-dir",
                root.join("remote.git").to_str().unwrap(),
                "rev-parse",
                "main"
            ]
        ),
        sha
    );
    assert_eq!(
        git(
            root,
            &[
                "--git-dir",
                root.join("remote.git").to_str().unwrap(),
                "rev-list",
                "--count",
                "main"
            ]
        ),
        "2"
    );
    owned.assert_empty();
    common::evidence(
        "g7_lane",
        &json!({"worker_attempts":2,"rejected_land_send_back":true,"feedback_messages":feedback,"land_leases":land_leases,"landed_sha":sha,"remote_commits":2,"cleanup":true,"owned_work_empty":true}),
    );
}

pub fn git(cwd: &Path, args: &[&str]) -> String {
    String::from_utf8(
        common::checked(Command::new("/usr/bin/git").args(args).current_dir(cwd)).stdout,
    )
    .unwrap()
    .trim()
    .into()
}

pub fn environment(root: &Path) -> BTreeMap<String, String> {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(root.join("owner/.codex")).unwrap();
    fs::write(root.join("owner/.codex/auth.json"), "fixture credential").unwrap();
    for (name, path) in [
        ("uv", "/home/sam/.local/bin/uv"),
        ("python3", "/usr/bin/python3"),
        ("bash", "/usr/bin/bash"),
        ("sh", "/usr/bin/sh"),
        ("sleep", "/usr/bin/sleep"),
    ] {
        symlink(path, bin.join(name)).unwrap();
    }
    symlink(common::binary(), bin.join("sluice")).unwrap();
    let fixture = common::workspace().join("target/debug/fixture");
    assert!(fixture.is_file());
    for name in ["kiln", "linear", "gh", "git", "codex"] {
        let script = format!(
            "#!/bin/sh\nexport G7_ROOT='{}'\nexport G7_FIXTURE='{}'\nexec /usr/bin/python3 '{}' \"$@\"\n",
            root.display(),
            fixture.display(),
            common::workspace()
                .join("crates/sluice/tests/rehearsal/fixtures/lane_tools.py")
                .display()
        );
        // argv[0] is the Python script path, so use a per-name symlink to it.
        let helper = bin.join(format!("{name}.py"));
        symlink(
            common::workspace().join("crates/sluice/tests/rehearsal/fixtures/lane_tools.py"),
            &helper,
        )
        .unwrap();
        let script = script.replace(
            &format!(
                "'{}'",
                common::workspace()
                    .join("crates/sluice/tests/rehearsal/fixtures/lane_tools.py")
                    .display()
            ),
            &format!("'{}'", helper.display()),
        );
        fs::write(bin.join(name), script).unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    BTreeMap::from([
        ("HOME".into(), root.join("owner").to_string_lossy().into()),
        (
            "SLUICE_CODEX_CLI".into(),
            root.join("bin/codex").to_string_lossy().into(),
        ),
        (
            "CODEX_HOME".into(),
            root.join("owner/.codex").to_string_lossy().into(),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            root.join("owner/.config").to_string_lossy().into(),
        ),
        (
            "XDG_DATA_HOME".into(),
            root.join("owner/.local/share").to_string_lossy().into(),
        ),
        ("PATH".into(), bin.to_string_lossy().into()),
        ("SLUICE_HOST_PATH".into(), bin.to_string_lossy().into()),
        (
            "SLUICE_HOME".into(),
            root.join("rust-home").to_string_lossy().into(),
        ),
        (
            "SLUICE_INSTALL_DIR".into(),
            root.join("install").to_string_lossy().into(),
        ),
        (
            "SLUICE_BIN".into(),
            common::binary().to_string_lossy().into(),
        ),
        (
            "SLUICE_PYTHON_DIR".into(),
            common::workspace().join("python").to_string_lossy().into(),
        ),
        ("SLUICE_UV_BIN".into(), "/home/sam/.local/bin/uv".into()),
        (
            "SLUICE_TMUX_PREFIX".into(),
            common::workspace()
                .join("target/private-tmux")
                .to_string_lossy()
                .into(),
        ),
        (
            "SLUICE_CODEX_BIN".into(),
            bin.join("codex").to_string_lossy().into(),
        ),
        ("SLUICE_AGENT_SETTLE_S".into(), "0.1".into()),
        ("SLUICE_AGENT_GRACE_MIN".into(), "0.02".into()),
        ("SLUICE_AGENT_WORK_MIN".into(), "0.2".into()),
        ("SLUICE_FIXTURE".into(), "1".into()),
        ("SLUICE_BACKOFF".into(), "0".into()),
    ])
}
use crate::common;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
