#[path = "fixtures/install_support.rs"]
mod support;
use serde_json::{Value, json};
use sluice::{
    install::Installation,
    release::{Manifest, verify},
};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
fn synthetic_release(root: &Path) -> PathBuf {
    let release = root.join("release");
    for dir in ["bin", "python/sluice_fn", "tmux/bin", "assets"] {
        std::fs::create_dir_all(release.join(dir)).unwrap();
    }
    for file in [
        "bin/sluice",
        "python/sluice_fn/__init__.py",
        "python/inline_python.py",
        "tmux/bin/tmux",
    ] {
        std::fs::write(release.join(file), file).unwrap();
    }
    let tmux_digest = sluice_store::artifacts::fingerprint(
        &std::fs::read(release.join("tmux/bin/tmux")).unwrap(),
    );
    let tmux = json!({"schema_version":1,"version":"3.7c","source_url":sluice_process::tmux::SOURCE_URL,"source_sha256":sluice_process::tmux::SOURCE_SHA256,
        "configure_flags":["--disable-systemd","--disable-cgroups","--prefix=/scratch","--sysconfdir=/scratch/etc"],"generated_config_lines":["DEFS = -DHAVE_CONFIG_H"],"binary_path":"bin/tmux","binary_sha256":tmux_digest,
        "libevent":{"version":"2.1.12-stable","source_url":sluice_process::tmux::LIBEVENT_URL,"source_sha256":sluice_process::tmux::LIBEVENT_SHA256,"configure_flags":["--disable-shared","--enable-static","--disable-openssl","--prefix=/scratch/libevent"],"linkage":"static"},
        "ncurses":{"linkage":"dynamic","packages":["ncursesw","tinfo"],"version":"6"},"parser":"release cmd-parse.c","build_host":"fixture","built_at":"fixture"});
    std::fs::write(
        release.join("tmux/tmux-manifest.json"),
        serde_json::to_vec(&tmux).unwrap(),
    )
    .unwrap();
    let mut files = BTreeMap::new();
    for name in [
        "bin/sluice",
        "python/sluice_fn/__init__.py",
        "python/inline_python.py",
        "tmux/bin/tmux",
        "tmux/tmux-manifest.json",
    ] {
        files.insert(
            name.to_string(),
            sluice_store::artifacts::fingerprint(&std::fs::read(release.join(name)).unwrap()),
        );
    }
    let git_sha = "a".repeat(40);
    let release_id = format!(
        "{}-{}",
        git_sha,
        sluice_store::artifacts::fingerprint(&serde_json::to_vec(&files).unwrap())
    );
    let manifest = Manifest {
        release_id,
        git_sha,
        guardian_protocol_major: 1,
        guardian_protocol_minor: 0,
        files,
        private_tmux: serde_json::from_value(tmux).unwrap(),
        build_toolchain: BTreeMap::from([("rustc".into(), "fixture".into())]),
    };
    std::fs::write(
        release.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    release
}
#[test]
fn p7_release_manifest_verifies_bytes_and_doctor_reports_tampered_helper() {
    let gate = support::Gate::new();
    let release = synthetic_release(gate.root.path());
    verify(&release).unwrap();
    let install = Installation::at(gate.install.clone()).unwrap();
    install.fence("test".into()).unwrap();
    install.select(&release, &gate.home).unwrap();
    let doctor = gate.cli(&["doctor", "--json"]);
    assert!(doctor.status.success(), "{:?}", doctor);
    let report: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["manifest_ok"], true);
    assert!(report["installation"]["fence"].is_object());
    std::fs::write(release.join("python/sluice_fn/__init__.py"), "tampered").unwrap();
    let doctor = gate.cli(&["doctor", "--json"]);
    assert!(!doctor.status.success());
    let report: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(report["manifest_ok"], false);
    assert!(
        report["error"]["message"]
            .as_str()
            .unwrap()
            .contains("python/sluice_fn")
    );
}
#[test]
fn p7_release_manifest_paths_and_protocol_are_checked_before_use() {
    let gate = support::Gate::new();
    let release = synthetic_release(gate.root.path());
    let path = release.join("manifest.json");
    let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest.files.insert("../escape".into(), "0".repeat(64));
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(verify(&release).is_err());
    manifest.files.remove("../escape");
    manifest.guardian_protocol_major += 1;
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(verify(&release).is_err());
}
#[test]
fn p7_release_release_scripts_refuse_protected_test_paths_before_building() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let gate = support::Gate::new();
    for script in ["build-release", "deploy"] {
        let mut command = gate.command(&repo.join("scripts").join(script), &[]);
        if script == "build-release" {
            command.arg("/home/sam/.local/bin");
        } else {
            command.args(["HEAD", "--prefix", "/home/sam/.local/share/sluice"]);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("Refusing")
                || String::from_utf8_lossy(&output.stderr).contains("refuse")
        );
    }
}
fn helper_fixture(gate: &support::Gate, release: &Path) {
    let python = release.join("python");
    let fixture = gate.root.path().join("helper-fixture.py");
    std::fs::write(&fixture, "from sluice_fn import run\nimport importlib.util\nassert importlib.util.find_spec('sluice') is None\nrun(lambda inputs, ctx: {'value': inputs['value'] + 1})\n").unwrap();
    let mut child = Command::new("/usr/bin/python3")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("PYTHONPATH", python)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(fixture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let context = json!({"project_id":"019a2b3c-4d5e-7f01-8234-56789abcdef0","run_id":"run","attempt_id":"attempt","invocation_id":"invocation","run_dir":gate.home,"home":gate.home,"fn_dir":gate.home,"project_dir":gate.home});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::to_string(&json!({"protocol":1,"inputs":{"value":41},"context":context}))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["outputs"]["value"], 42);
}
#[test]
#[ignore = "P7 real locked release build and delegated fixture guardian across deploy; scratch prefix only"]
fn p7_release_build_helper_launcher_and_deploy_adopt_the_pinned_guardian() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let mut gate = support::Gate::new();
    gate.root.disable_cleanup(true);
    std::fs::create_dir_all(repo.join("target/p7-02-evidence")).unwrap();
    std::fs::write(
        repo.join("target/p7-02-evidence/package-root.txt"),
        gate.root.path().to_string_lossy().as_bytes(),
    )
    .unwrap();
    eprintln!("P7 package scratch root: {}", gate.root.path().display());
    let prefix = gate.root.path().join("prefix");
    let build = gate
        .command(&repo.join("scripts/build-release"), &[])
        .arg(&prefix)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let release = PathBuf::from(String::from_utf8(build.stdout).unwrap().trim());
    let manifest = verify(&release).unwrap();
    assert!(release.ends_with(&manifest.release_id));
    for path in [
        "bin/sluice",
        "python/sluice_fn/__init__.py",
        "python/inline_python.py",
        "tmux/bin/tmux",
        "assets",
        "manifest.json",
    ] {
        assert!(release.join(path).exists(), "{path}");
    }
    helper_fixture(&gate, &release);
    let install = Installation::at(gate.install.clone()).unwrap();
    install.fence("first install".into()).unwrap();
    install.select(&release, &gate.home).unwrap();
    install.unfence().unwrap();
    let launcher = gate
        .command(&prefix.join("bin/sluice"), &["install", "status"])
        .output()
        .unwrap();
    assert!(launcher.status.success(), "{:?}", launcher);
    let selected: Value = serde_json::from_slice(&launcher.stdout).unwrap();
    assert_eq!(
        selected["selection"]["home_path"],
        gate.home.to_string_lossy().as_ref()
    );
    let version = gate
        .command(&release.join("bin/sluice"), &["--version"])
        .env_remove("PYTHONPATH")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(version.status.success());
    gate.boot(&release.join("bin/sluice"), false);
    let reply = gate.rpc(json!({"command":"fn_call","args":{"name":"fixture.wait","inputs":{"value":7},"project":null,"direct":true,"wait_seconds":0,"author":"release-test"}})).unwrap();
    let value = serde_json::to_value(reply).unwrap();
    // CommandReply::Data carries the call status.
    let run = value["data"]["call"]
        .as_str()
        .or_else(|| value["data"]["call_id"].as_str())
        .expect("call id")
        .to_owned();
    let run_dir = gate.home.join("runs").join(&run);
    support::wait(|| run_dir.join("dispatch-count").exists());
    let db = rusqlite::Connection::open(gate.home.join("sluice.db")).unwrap();
    let pid: u32 = db
        .query_row(
            "SELECT guardian_pid FROM runs WHERE run_id=?1",
            [&run],
            |r| r.get(0),
        )
        .unwrap();
    drop(db);
    assert_eq!(
        std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
        release.join("bin/sluice")
    );
    // Register this test-owned broker so deploy stops only its recorded identity.
    let broker_pid = gate.children[0].id();
    let stat = std::fs::read_to_string(format!("/proc/{broker_pid}/stat")).unwrap();
    let start = stat
        .rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .nth(19)
        .unwrap();
    std::fs::write(
        gate.install.join("services.json"),
        serde_json::to_vec(&json!({"coordinator":{"pid":broker_pid,"start":start}})).unwrap(),
    )
    .unwrap();
    // Commit an identical tree in an isolated repository with a new commit id.
    let source = gate.root.path().join("source");
    assert!(
        Command::new("git")
            .args(["clone", "--quiet", "--local", "--no-hardlinks"])
            .arg(&repo)
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .current_dir(&source)
            .args([
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "Build a second fixture release."
            ])
            .status()
            .unwrap()
            .success()
    );
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&source)
        .output()
        .unwrap();
    let sha = String::from_utf8(sha.stdout).unwrap().trim().to_owned();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
        .to_string();
    let deploy = gate
        .command(&source.join("scripts/deploy"), &[&sha, "--prefix"])
        .arg(&prefix)
        .env("SLUICE_DEPLOY_PROCESS_FIXTURE", "1")
        .env("SLUICE_DEPLOY_PORT", port)
        .output()
        .unwrap();
    assert!(
        deploy.status.success(),
        "deploy: {}",
        String::from_utf8_lossy(&deploy.stderr)
    );
    let selected = install.status().unwrap().selection.unwrap();
    assert_ne!(selected.release_path, release);
    assert_eq!(
        std::fs::read_link(format!("/proc/{pid}/exe")).unwrap(),
        release.join("bin/sluice")
    );
    assert!(release.exists());
    assert!(selected.release_path.exists());
    assert!(
        gate.cli(&["install", "fence", "completion during maintenance"])
            .status
            .success()
    );
    std::fs::write(run_dir.join("finish"), "finish").unwrap();
    support::wait(|| {
        let db = rusqlite::Connection::open(gate.home.join("sluice.db")).unwrap();
        db.query_row(
            "SELECT status='succeeded' FROM calls WHERE call_id=?1",
            [&run],
            |r| r.get::<_, bool>(0),
        )
        .unwrap()
    });
    assert!(gate.cli(&["install", "unfence"]).status.success());
    // Stop only the processes that this scratch deploy recorded.
    let services: Value =
        serde_json::from_slice(&std::fs::read(gate.install.join("services.json")).unwrap())
            .unwrap();
    for record in services.as_object().unwrap().values() {
        let pid = record["pid"].as_u64().unwrap().to_string();
        Command::new("/bin/kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap();
    }
    support::wait(|| !gate.home.join("coordinator.sock").exists());
    std::fs::write(release.join("python/inline_python.py"), "tampered").unwrap();
    assert!(verify(&release).is_err());
    gate.root.disable_cleanup(false);
}
