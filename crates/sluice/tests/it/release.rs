use crate::executable;
use crate::install_support as support;
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
const SCHEMA: i64 = sluice_store::schema::SCHEMA_VERSION;

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
        schema: 1,
    };
    std::fs::write(
        release.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    release
}
#[test]
fn slow_release_schema_integrity_protocol_and_doctor_tamper() {
    let gate = support::Gate::new();
    let release = synthetic_release(gate.root.path());
    {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let account = PathBuf::from(std::env::var_os("HOME").unwrap());
        for script in ["build-release", "deploy"] {
            let mut command = gate.command(&repo.join("scripts").join(script), &[]);
            if script == "build-release" {
                command.arg(account.join(".local/bin"));
            } else {
                command
                    .args(["HEAD", "--prefix"])
                    .arg(account.join(".local/share/sluice"));
            }
            let output = command.output().unwrap();
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("Refusing")
                    || String::from_utf8_lossy(&output.stderr).contains("refuse")
            );
        }
    }
    verify(&release).unwrap();
    let schema = gate
        .command(Path::new(env!("CARGO_BIN_EXE_sluice")), &["home", "schema"])
        .env("SLUICE_HOME", gate.root.path().join("never-created"))
        .env("TOKIO_WORKER_THREADS", "2")
        .output()
        .unwrap();
    assert!(schema.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&schema.stdout).unwrap(),
        json!({"schema":SCHEMA})
    );
    assert!(!gate.root.path().join("never-created").exists());
    let path = release.join("manifest.json");
    let manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut legacy = manifest.clone();
    legacy.as_object_mut().unwrap().remove("schema");
    std::fs::write(&path, legacy.to_string()).unwrap();
    assert_eq!(verify(&release).unwrap().schema, 1);
    std::fs::write(&path, manifest.to_string()).unwrap();
    {
        let path = release.join("manifest.json");
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        manifest.files.insert("../escape".into(), "0".repeat(64));
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(verify(&release).is_err());
        manifest.files.remove("../escape");
        manifest.guardian_protocol_major += 1;
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(verify(&release).is_err());
        manifest.guardian_protocol_major -= 1;
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    }
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
// A release carries the licence of every face the dashboard serves, and the manifest of their
// bytes: `licenses/fonts/<family>/LICENSE` and `licenses/fonts/SHA256SUMS`, as the source tree
// has them, each in the release manifest. Cheap: `cargo` is a stub on PATH and the prefix holds
// a stub binary and private tmux, so only the packaging runs.

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
    // The binary's own schema, asked of it at build time.
    assert_eq!(
        i64::from(manifest.schema),
        sluice_store::schema::SCHEMA_VERSION
    );
    for path in [
        "bin/sluice",
        "python/sluice_fn/__init__.py",
        "python/inline_python.py",
        "tmux/bin/tmux",
        "assets",
        "licenses/fonts/SHA256SUMS",
        "licenses/fonts/inter/LICENSE",
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
    // A prebuilt entry can be copied to the public bin directory. Its default
    // control path belongs to the runtime HOME, not the build machine prefix.
    let owner = gate.root.path().join("fake-owner");
    std::fs::create_dir_all(owner.join(".local/bin")).unwrap();
    std::fs::create_dir_all(owner.join(".local/share/sluice")).unwrap();
    std::os::unix::fs::symlink(&gate.install, owner.join(".local/share/sluice/install")).unwrap();
    let relocated = owner.join(".local/bin/sluice");
    std::fs::copy(prefix.join("bin/sluice"), &relocated).unwrap();
    let relocated_status = gate
        .command(&relocated, &["install", "status"])
        .env_remove("SLUICE_INSTALL_DIR")
        .env("HOME", &owner)
        .output()
        .unwrap();
    assert!(relocated_status.status.success(), "{:?}", relocated_status);
    let version = gate
        .command(&release.join("bin/sluice"), &["--version"])
        .env_remove("PYTHONPATH")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(version.status.success());
    gate.boot(&release.join("bin/sluice"), false);
    let sluice_model::commands::CommandReply::Project(project) = gate.rpc(json!({"command":"project_create","args":{"name":"release-fixture","description":"","icon":null,"resources":{},"author":"release-test"}})).unwrap() else { panic!("project") };
    let selector = json!({"kind":"id","value":project.project_id});
    gate.rpc(json!({"command":"plan_edit","args":{"project":selector,"rev":1,"ops":[{"op":"step.add","step":"wait","spec":{"run":"fixture.wait","in":{"value":{"default":7}}}}],"start":true,"dry_run":false,"reason":"pinned fixture","author":"release-test"}})).unwrap();
    let scheduler = gate
        .command(&release.join("bin/sluice"), &["loop"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    gate.children.push(scheduler);
    let mut run = String::new();
    support::wait(|| {
        let sql = rusqlite::Connection::open(gate.home.join("sluice.db")).unwrap();
        if let Ok(id) = sql.query_row("SELECT run_id FROM runs WHERE step_id='wait'", [], |r| {
            r.get::<_, String>(0)
        }) {
            run = id;
            gate.home
                .join("runs")
                .join(&run)
                .join("dispatch-count")
                .exists()
        } else {
            false
        }
    });
    let run_dir = gate.home.join("runs").join(&run);
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
    let mut managed = serde_json::Map::new();
    for (name, child) in ["coordinator", "loop"].iter().zip(&gate.children) {
        let pid = child.id();
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let start = stat
            .rsplit(')')
            .next()
            .unwrap()
            .split_whitespace()
            .nth(19)
            .unwrap();
        managed.insert((*name).into(), json!({"pid":pid,"start":start}));
    }
    std::fs::write(
        gate.install.join("services.json"),
        serde_json::to_vec(&managed).unwrap(),
    )
    .unwrap();
    // Put the pinned release beyond the last three before the next deploy.
    let timestamp = std::time::SystemTime::now();
    for i in 0..4 {
        let directory = prefix
            .join("releases")
            .join(format!("retention-fixture-{i}"));
        std::fs::create_dir(&directory).unwrap();
        std::fs::File::open(directory)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(timestamp + std::time::Duration::from_millis(i)),
            )
            .unwrap();
    }
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
    assert!(!prefix.join("releases/retention-fixture-0").exists());
    assert!(!prefix.join("releases/retention-fixture-1").exists());
    assert!(prefix.join("releases/retention-fixture-2").exists());
    assert!(prefix.join("releases/retention-fixture-3").exists());
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
            "SELECT status='succeeded' FROM steps WHERE project_id=?1 AND step_id='wait'",
            [project.project_id.to_string()],
            |r| r.get::<_, bool>(0),
        )
        .unwrap()
    });
    assert_eq!(
        std::fs::read_to_string(run_dir.join("dispatch-count")).unwrap(),
        "dispatch\n"
    );
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
    // The selected old Python artifact cannot parse the Rust internal entry flag.
    // The retained Rust dispatcher must apply the restored home and remove it.
    let rollback = gate.root.path().join("python-backup");
    let restored = gate.root.path().join("restored-home");
    std::fs::create_dir_all(rollback.join("bin")).unwrap();
    std::fs::create_dir(&restored).unwrap();
    let old = rollback.join("bin/sluice");
    executable::write(
        &old,
        "#!/usr/bin/python3\nimport json, os, sys\nprint(json.dumps({'home':os.environ['SLUICE_HOME'],'args':sys.argv[1:]}))\n",
    );
    let dispatcher = std::fs::read_link(gate.install.join("entry")).unwrap();
    assert!(
        gate.cli(&["install", "fence", "rollback fixture"])
            .status
            .success()
    );
    assert!(
        gate.cli(&[
            "install",
            "select",
            rollback.to_str().unwrap(),
            restored.to_str().unwrap()
        ])
        .status
        .success()
    );
    assert_eq!(
        std::fs::read_link(gate.install.join("entry")).unwrap(),
        dispatcher
    );
    let output = gate
        .command(&prefix.join("bin/sluice"), &["fixture-rollback"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output["home"], restored.to_string_lossy().as_ref());
    assert_eq!(output["args"], json!(["fixture-rollback"]));
    assert!(gate.cli(&["install", "unfence"]).status.success());
    std::fs::write(release.join("python/inline_python.py"), "tampered").unwrap();
    assert!(verify(&release).is_err());
    gate.root.disable_cleanup(false);
}
#[test]
fn doctor_probes_engines_on_path_without_their_homes() {
    use std::os::unix::fs::PermissionsExt;
    let gate = support::Gate::new();
    let release = synthetic_release(gate.root.path());
    let install = Installation::at(gate.install.clone()).unwrap();
    install.fence("test".into()).unwrap();
    install.select(&release, &gate.home).unwrap();
    let bin = gate.root.path().join("engines");
    std::fs::create_dir(&bin).unwrap();
    let seen = gate.root.path().join("probe-homes");
    // codex is newer than tested (accepted untested), claude is older than its floor (refused),
    // devin is not installed.
    let codex_help = sluice_agents::engines::codex::protocol::FIXTURE_HELP;
    let server_help = sluice_agents::engines::codex::protocol::FIXTURE_APP_SERVER_HELP;
    for (name, version) in [
        ("codex", "codex-cli 0.161.0"),
        ("claude", "2.1.282 (Claude Code)"),
    ] {
        let path = bin.join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"{name}|$*|$HOME|$CODEX_HOME|$CLAUDE_CONFIG_DIR\" >> '{}'\nif [ \"$1\" = --help ]; then printf '%s' '{codex_help}'; exit; fi\nif [ \"$1 $2\" = 'app-server --help' ]; then printf '%s' '{server_help}'; exit; fi\necho '{version}'\n",
                seen.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let doctor = gate
        .command(
            Path::new(env!("CARGO_BIN_EXE_sluice")),
            &["doctor", "--json"],
        )
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert!(doctor.status.success(), "{doctor:?}");
    let report: Value = serde_json::from_slice(&doctor.stdout).unwrap();
    let engine = |name: &str| {
        report["engines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["engine"] == name)
            .unwrap_or_else(|| panic!("{name}: {report}"))
            .clone()
    };
    let codex = engine("codex");
    assert_eq!(codex["executable"], json!(bin.join("codex")));
    assert_eq!(codex["version"], "codex-cli 0.161.0");
    assert_eq!(codex["supported"], true, "{codex}");
    assert_eq!(codex["status"], "untested, accepted", "{codex}");
    assert_eq!(codex["tested"], json!(["0.160.0", "0.160.1"]));
    assert_eq!(codex["floor"], "0.160.0");
    assert_eq!(codex["newer_major"], "accepted");
    assert_eq!(codex["probes"]["missing"], json!([]), "{codex}");
    for capability in ["app-server", "resume", "--remote", "unix-websocket"] {
        assert!(
            codex["probes"]["verified"]
                .as_array()
                .unwrap()
                .contains(&json!(capability)),
            "{capability}: {codex}"
        );
    }
    // The fake has no protocol schema to generate: the methods are assumed, not missing.
    assert!(
        codex["probes"]["assumed"]
            .as_array()
            .unwrap()
            .contains(&json!("turn/steer")),
        "{codex}"
    );
    let claude = engine("claude");
    assert_eq!(claude["version"], "2.1.282 (Claude Code)");
    assert_eq!(claude["supported"], false);
    assert_eq!(claude["status"], "refused");
    assert_eq!(claude["floor"], "2.1.283");
    assert!(
        claude["error"]
            .as_str()
            .unwrap()
            .starts_with("claude 2.1.282 is older than 2.1.283"),
        "{claude}"
    );
    let devin = engine("devin");
    assert_eq!(devin["executable"], Value::Null);
    assert_eq!(devin["version"], Value::Null);
    assert_eq!(devin["status"], "not found");
    assert_eq!(devin["newer_major"], "refused");
    assert_eq!(devin["error"], "executable not found on PATH");
    // Only version and help probes ran, in scratch homes that are gone afterwards: no session
    // started and the owner's engine homes were never pointed at.
    let probes = std::fs::read_to_string(&seen).unwrap();
    let ran: Vec<(&str, &str)> = probes
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split('|').collect();
            (f[0], f[1])
        })
        .collect();
    assert_eq!(
        ran,
        [
            ("codex", "--version"),
            ("codex", "--help"),
            ("codex", "app-server --help"),
            ("claude", "--version"),
        ],
        "{probes}"
    );
    for line in probes.lines() {
        let fields: Vec<&str> = line.split('|').collect();
        let probe_home = Path::new(fields[2]);
        assert!(probe_home.starts_with(std::env::temp_dir()), "{line}");
        assert!(Path::new(fields[4]) == probe_home, "{line}");
        let codex_home = Path::new(fields[3]);
        assert!(codex_home.starts_with(std::env::temp_dir()), "{line}");
        assert!(!probe_home.exists() && !codex_home.exists(), "{line}");
    }
    let text = gate
        .command(Path::new(env!("CARGO_BIN_EXE_sluice")), &["doctor"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.lines().any(|l| l.starts_with("warn engine codex")
            && l.contains("codex-cli 0.161.0")
            && l.contains(": untested, accepted (tested versions 0.160.0, 0.160.1; floor 0.160.0; newer major accepted)")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.trim_start().starts_with("capabilities: ")
                && l.contains("verified; assumed: ")),
        "{text}"
    );
    assert!(
        text.lines().any(|l| l.starts_with("warn engine claude")
            && l.contains(": refused: claude 2.1.282 is older than 2.1.283")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("warn engine devin") && l.contains("not found on PATH")),
        "{text}"
    );
}

// ---- packaging and deploying without a cargo build -----------------------------------------
//
// `cargo` is a stub on PATH and the prefix holds the binary build-release packages (a shell
// wrapper that execs this build's sluice) and this checkout's private tmux, so build-release
// runs only its packaging and the release it makes verifies for real.

// The commit whose build-release ordered a release's files by `Path` (by component), so a
// tree holding `a/x` and `a-b/x` got a content id that `release::verify` never recomputes.
const PATH_ORDER_BUILD_RELEASE: &str = "2f3823ef561bf06142cb2f6e2076a764afd3bfd1";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
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
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}
fn text(output: &std::process::Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "__pycache__" {
            continue;
        }
        let path = entry.path();
        if std::fs::metadata(&path).unwrap().is_dir() {
            copy_tree(&path, &to.join(entry.file_name()));
        } else {
            std::fs::copy(&path, to.join(entry.file_name())).unwrap();
        }
    }
}
// The stub `cargo` directory and a prefix whose build cache holds this checkout's private tmux.
fn packaging(root: &Path) -> (PathBuf, PathBuf) {
    let stubs = root.join("stubs");
    std::fs::create_dir_all(&stubs).unwrap();
    executable::write(stubs.join("cargo"), "#!/bin/sh\necho 'cargo stub'\n");
    let prefix = root.join("prefix");
    let tmux = repo().join("target/private-tmux");
    assert!(
        tmux.join("tmux-manifest.json").is_file(),
        "run scripts/build-private-tmux once per checkout"
    );
    copy_tree(&tmux, &prefix.join(".build/private-tmux"));
    (stubs, prefix)
}
fn stub_path(stubs: &Path) -> std::ffi::OsString {
    std::env::join_paths(
        std::iter::once(stubs.to_path_buf()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )),
    )
    .unwrap()
}
// The binary the next build-release packages: a wrapper that marks its processes with
// `FIXTURE_RELEASE=<label>`, runs `before` and execs this build's sluice.
fn wrapper(prefix: &Path, label: &str, before: &str) {
    let target = prefix.join(".build/target/release");
    std::fs::create_dir_all(&target).unwrap();
    executable::write(
        target.join("sluice"),
        format!(
            "#!/bin/sh\nexport FIXTURE_RELEASE='{label}'\n{before}\nexec '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_sluice")
        ),
    );
}
// A source tree build-release and deploy can run from: the given build-release, this
// checkout's deploy and fn helper, this build's schema, and one font family per name (each
// with its licence, beside a SHA256SUMS).
fn source_tree(root: &Path, families: &[&str], build_release: &str) -> PathBuf {
    let source = root.join("source");
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    executable::write(source.join("scripts/build-release"), build_release);
    executable::write(
        source.join("scripts/deploy"),
        std::fs::read(repo().join("scripts/deploy")).unwrap(),
    );
    std::fs::copy(
        repo().join("scripts/deploy_host.py"),
        source.join("scripts/deploy_host.py"),
    )
    .unwrap();
    copy_tree(&repo().join("python"), &source.join("python"));
    std::fs::create_dir_all(source.join("crates/sluice-store/src")).unwrap();
    std::fs::write(
        source.join("crates/sluice-store/src/schema.rs"),
        format!(
            "pub const SCHEMA_VERSION: i64 = {};\n",
            sluice_store::schema::SCHEMA_VERSION
        ),
    )
    .unwrap();
    let fonts = source.join("crates/sluice-web/assets/fonts");
    for family in families {
        std::fs::create_dir_all(fonts.join(family)).unwrap();
        std::fs::write(
            fonts.join(family).join("LICENSE"),
            format!("{family} licence"),
        )
        .unwrap();
    }
    std::fs::write(fonts.join("SHA256SUMS"), "").unwrap();
    source
}
fn current_build_release() -> String {
    std::fs::read_to_string(repo().join("scripts/build-release")).unwrap()
}
fn path_order_build_release() -> String {
    git(
        &repo(),
        &[
            "show",
            &format!("{PATH_ORDER_BUILD_RELEASE}:scripts/build-release"),
        ],
    )
}
// Runs `source`'s build-release into `prefix` and returns the release it printed.
fn build(source: &Path, prefix: &Path, stubs: &Path, sha: &str) -> PathBuf {
    let output = Command::new(source.join("scripts/build-release"))
        .arg(prefix)
        .env("PATH", stub_path(stubs))
        .env("SLUICE_BUILD_GIT_SHA", sha)
        .output()
        .unwrap();
    assert!(output.status.success(), "build-release: {}", text(&output));
    PathBuf::from(
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .lines()
            .last()
            .unwrap(),
    )
}

// The release id is the sha256 of `files` as `release::verify` serialises its
// `BTreeMap<String, String>`: keys in byte order. A tree with a directory beside a longer
// sibling of the same prefix (`a/x` and `a-b/x`, as the fonts `overpass/` and
// `overpass-mono/` are) sorts differently by `Path` components, which build-release used, and
// every such release failed `install select` with "release content id mismatch". The odd
// names hold build-release's JSON to serde_json's escaping as well.
#[test]
fn slow_release_packaging_hashes_names_and_ships_licences() {
    let root = tempfile::tempdir().unwrap();
    let source = source_tree(root.path(), &["a", "a-b"], &current_build_release());
    let assets = source.join("crates/sluice-web/assets");
    for name in [
        "a/x",
        "a-b/x",
        "a.b/x",
        "a0/x",
        "odd/q\"uote",
        "odd/back\\slash",
        "odd/tab\there",
        "odd/del\u{7f}",
        "odd/\u{1}control",
        "odd/é",
        "odd/日本",
        "odd/\u{2028}separator",
    ] {
        let path = assets.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, name).unwrap();
    }
    let (stubs, prefix) = packaging(root.path());
    wrapper(&prefix, "release-order", "");
    let release = build(&source, &prefix, &stubs, &"1".repeat(40));
    let manifest = verify(&release).unwrap_or_else(|e| panic!("{e:?}"));
    assert!(release.ends_with(&manifest.release_id));
    for name in [
        "assets/a/x",
        "assets/a-b/x",
        "assets/fonts/a/LICENSE",
        "assets/fonts/a-b/LICENSE",
        "licenses/fonts/a/LICENSE",
        "licenses/fonts/a-b/LICENSE",
        "assets/odd/tab\there",
    ] {
        assert!(manifest.files.contains_key(name), "{name}");
    }
    // The manifest's own order is the byte order too.
    let written: Value =
        serde_json::from_slice(&std::fs::read(release.join("manifest.json")).unwrap()).unwrap();
    let keys: Vec<&String> = written["files"].as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    for family in ["a", "a-b"] {
        let name = format!("licenses/fonts/{family}/LICENSE");
        assert_eq!(
            std::fs::read(release.join(&name)).unwrap(),
            std::fs::read(
                source
                    .join("crates/sluice-web/assets/fonts")
                    .join(family)
                    .join("LICENSE")
            )
            .unwrap()
        );
        assert!(manifest.files.contains_key(&name));
    }
    assert_eq!(
        std::fs::read(release.join("licenses/fonts/SHA256SUMS")).unwrap(),
        std::fs::read(source.join("crates/sluice-web/assets/fonts/SHA256SUMS")).unwrap()
    );
    assert!(manifest.files.contains_key("licenses/fonts/SHA256SUMS"));
}

// A scratch installation selecting a release built from a scratch source, its coordinator and
// loop running as the test's children and registered in services.json as a deploy records
// them, and the deploy that follows in fixture mode (plain child services).
struct Deployment {
    gate: support::Gate,
    stubs: PathBuf,
    prefix: PathBuf,
    old: PathBuf,
    port: u16,
}
impl Deployment {
    fn new() -> Self {
        let mut gate = support::Gate::new();
        let (stubs, prefix) = packaging(gate.root.path());
        let label = Self::label_of(&gate, "old");
        wrapper(&prefix, &label, "");
        let old_source = source_tree(
            &gate.root.path().join("old"),
            &["a", "a-b"],
            &current_build_release(),
        );
        let old = build(&old_source, &prefix, &stubs, &"0".repeat(40))
            .canonicalize()
            .unwrap();
        let install = Installation::at(gate.install.clone()).unwrap();
        install.fence("first install".into()).unwrap();
        install.select(&old, &gate.home).unwrap();
        install.unfence().unwrap();
        gate.boot(&old.join("bin/sluice"), false);
        let looping = gate
            .command(&old.join("bin/sluice"), &["loop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        gate.children.push(looping);
        let mut services = serde_json::Map::new();
        for (name, child) in ["coordinator", "loop"].iter().zip(&gate.children) {
            services.insert(
                (*name).into(),
                json!({"pid": child.id(), "start": start_time(child.id()).unwrap(), "release": old}),
            );
        }
        std::fs::write(
            gate.install.join("services.json"),
            serde_json::to_vec(&services).unwrap(),
        )
        .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Self {
            gate,
            stubs,
            prefix,
            old,
            port,
        }
    }
    fn label_of(gate: &support::Gate, name: &str) -> String {
        format!("{name}:{}", gate.root.path().display())
    }
    fn label(&self, name: &str) -> String {
        Self::label_of(&self.gate, name)
    }
    /// A committed source tree whose release the deploy builds around the candidate wrapper.
    fn candidate(&self, families: &[&str], build_release: &str, before: &str) -> PathBuf {
        let candidate_root = tempfile::tempdir_in(self.gate.root.path()).unwrap().keep();
        let source = source_tree(&candidate_root, families, build_release);
        git(&source, &["init", "-q", "-b", "main"]);
        git(&source, &["add", "."]);
        git(&source, &["commit", "-q", "-m", "candidate"]);
        wrapper(&self.prefix, &self.label("candidate"), before);
        source
    }
    fn deploy(&self, source: &Path) -> std::process::Output {
        self.gate
            .command(&source.join("scripts/deploy"), &["HEAD", "--prefix"])
            .arg(&self.prefix)
            .args(["--skip-compat", "fixture: nothing is pinned"])
            .env("SLUICE_DEPLOY_PROCESS_FIXTURE", "1")
            .env("SLUICE_DEPLOY_PORT", self.port.to_string())
            .env("SLUICE_DEPLOY_READY_SECONDS", "120")
            .env("PATH", stub_path(&self.stubs))
            .output()
            .unwrap()
    }
    fn status(&self) -> sluice::install::Status {
        Installation::at(self.gate.install.clone())
            .unwrap()
            .status()
            .unwrap()
    }
    fn services(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.gate.install.join("services.json")).unwrap())
            .unwrap()
    }
    fn deploy_log(&self) -> String {
        std::fs::read_to_string(self.gate.install.join("deploy.log")).unwrap_or_default()
    }
    /// Live processes marked with this label (a zombie's environment reads empty).
    fn running(&self, name: &str) -> Vec<u32> {
        let mark = format!("FIXTURE_RELEASE={}", self.label(name));
        std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| {
                std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
                    environ.split(|b| *b == 0).any(|var| var == mark.as_bytes())
                })
            })
            .collect()
    }
    /// The deploy's coordinator, serve and loop are alive, from `release`, marked `name_of`.
    fn assert_services(&self, release: &Path, name_of: &str) {
        let services = self.services();
        for name in ["coordinator", "serve", "loop"] {
            let record = &services[name];
            assert_eq!(record["release"], json!(release), "{name}: {services}");
            let pid = record["pid"].as_u64().unwrap() as u32;
            assert_eq!(
                start_time(pid).as_deref(),
                record["start"].as_str(),
                "{name} is not running"
            );
            assert!(
                self.running(name_of).contains(&pid),
                "{name} is not {name_of}"
            );
        }
    }
    fn dashboard(&self) -> String {
        use std::io::Read;
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
        reply.lines().next().unwrap_or_default().to_owned()
    }
    fn old_services_running(&mut self) -> bool {
        self.gate
            .children
            .iter_mut()
            .all(|child| child.try_wait().unwrap().is_none())
    }
}
fn start_time(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.rsplit(')').next()?.split_whitespace().collect();
    (fields[0] != "Z").then(|| fields[19].to_owned())
}

// A release its own binary refuses (here: one built by the build-release that ordered files
// by `Path`, around fonts `a/` and `a-b/`, as 2f3823e's deploy was) stops the deploy before
// the fence: the old services keep running, nothing is fenced and nothing is selected.

// A failure between the stop and the unfence brings the previous release back: whatever the
// deploy started is stopped, the previous release runs the same three services, the fence is
// gone, the dashboard answers, and deploy exits 3 saying so.
fn deploy_rolls_back(deployment: &mut Deployment, inject: &str, error: &str) {
    let source = deployment.candidate(&["a", "a-b"], &current_build_release(), inject);
    let output = deployment.deploy(&source);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "deploy rolled back to {}: {error}",
            "0".repeat(12)
        )),
        "{}",
        text(&output)
    );
    // The old children were stopped; the previous release runs as the deploy runs it.
    assert!(!deployment.old_services_running());
    let old = deployment.old.clone();
    deployment.assert_services(&old, "old");
    assert_eq!(deployment.running("candidate"), Vec::<u32>::new());
    let status = deployment.status();
    assert!(status.fence.is_none(), "{status:?}");
    assert_eq!(status.selection.unwrap().release_path, old);
    assert!(
        deployment.dashboard().contains(" 200"),
        "{}",
        deployment.dashboard()
    );
    assert!(
        deployment
            .deploy_log()
            .contains(r#""outcome": "rolled back""#),
        "{}",
        deployment.deploy_log()
    );
}

// A rollback that fails itself (the previous release no longer verifies, so its doctor
// refuses it) keeps the installation fenced and prints the commands that finish it by hand.

// A candidate built before `install verify` (its CLI refuses the subcommand) is checked by its
// own `install select` on a scratch installation instead, and deploys as before.
#[test]
fn slow_deploy_checks_a_candidate_without_install_verify_by_a_scratch_select() {
    let deployment = Deployment::new();
    let source = deployment.candidate(
        &["a", "b"],
        &path_order_build_release(),
        r#"if [ "$1 $2" = "install verify" ]; then echo "error: unrecognized subcommand 'verify'" >&2; exit 2; fi"#,
    );
    let output = deployment.deploy(&source);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("predates install verify"),
        "{}",
        text(&output)
    );
    let status = deployment.status();
    assert!(status.fence.is_none(), "{status:?}");
    let selected = status.selection.unwrap().release_path;
    assert_ne!(selected, deployment.old);
    deployment.assert_services(&selected, "candidate");
}

#[test]
fn slow_deploy_verification_rollback_and_failed_recovery() {
    let mut deployment = Deployment::new();
    {
        let source = deployment.candidate(&["a", "a-b"], &path_order_build_release(), "");
        let services = std::fs::read(deployment.gate.install.join("services.json")).unwrap();
        let output = deployment.deploy(&source);
        assert!(!output.status.success(), "{}", text(&output));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("does not verify: release content id mismatch")
                && stderr.contains("Stopped before the fence: nothing live changed"),
            "{}",
            text(&output)
        );
        assert!(deployment.old_services_running());
        let status = deployment.status();
        assert!(status.fence.is_none(), "{status:?}");
        assert_eq!(status.selection.unwrap().release_path, deployment.old);
        assert_eq!(
            std::fs::read(deployment.gate.install.join("services.json")).unwrap(),
            services
        );
        assert!(
            deployment.deploy_log().contains(r#""verify": "failed""#),
            "{}",
            deployment.deploy_log()
        );
    }
    {
        deploy_rolls_back(
            &mut deployment,
            r#"if [ "$1 $2" = "install select" ]; then echo '{"error":"storage","message":"injected select failure"}'; exit 1; fi"#,
            "install select: injected select failure",
        );
    }
    {
        // The candidate's coordinator and serve start; its loop exits at once.
        deploy_rolls_back(
            &mut deployment,
            r#"if [ "$1" = loop ]; then exit 1; fi"#,
            "loop exited during restart",
        );
    }
    {
        let source = deployment.candidate(
        &["a", "a-b"],
        &current_build_release(),
        r#"if [ "$1 $2" = "install select" ]; then echo '{"error":"storage","message":"injected select failure"}'; exit 1; fi"#,
    );
        std::fs::write(deployment.old.join("python/inline_python.py"), "tampered").unwrap();
        let output = deployment.deploy(&source);
        assert!(!output.status.success(), "{}", text(&output));
        assert_ne!(output.status.code(), Some(3), "{}", text(&output));
        let stderr = String::from_utf8_lossy(&output.stderr);
        let sluice = deployment.old.join("bin/sluice");
        for expected in [
            format!("the rollback to {} failed", "0".repeat(12)),
            "The installation stays fenced".into(),
            format!(
                "{} install select {} {}",
                sluice.display(),
                deployment.old.display(),
                deployment.gate.home.display()
            ),
            format!("{} install unfence", sluice.display()),
            "deploy failed: install select: injected select failure".into(),
        ] {
            assert!(stderr.contains(&expected), "{expected}\n{}", text(&output));
        }
        let status = deployment.status();
        assert!(
            status.fence.as_ref().unwrap().reason.starts_with("deploy "),
            "{status:?}"
        );
        assert!(
            deployment
                .deploy_log()
                .contains(r#""outcome": "rollback failed""#),
            "{}",
            deployment.deploy_log()
        );
    }
}

// A lane's coordinator rejected a copied 104 GiB execution budget against the lane's
// 24 GiB cap. Existing compat checks ran without a capped caller and never caught it.
#[test]
fn slow_compat_check_escapes_caller_limits_and_reports_full_failure() {
    let mut gate = support::Gate::new();
    gate.boot(Path::new(env!("CARGO_BIN_EXE_sluice")), false);
    gate.rpc(json!({"command":"project_create","args":{"name":"compat-capped","description":"","icon":null,"resources":{},"author":"test"}})).unwrap();
    gate.stop();
    std::fs::write(
        gate.home.join("config.json"),
        json!({"execution":{"total":{"memory":"2GiB","cpus":1}}}).to_string(),
    )
    .unwrap();
    let release = gate.root.path().join("candidate");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::fs::write(
        release.join("manifest.json"),
        json!({"schema":SCHEMA}).to_string(),
    )
    .unwrap();
    let binary = release.join("bin/sluice");
    executable::write(
        &binary,
        format!(
            "#!/bin/sh\nif /usr/bin/systemctl --user show-environment >/dev/null 2>&1; then exit 90; fi\nif [ -n \"${{RUSTFLAGS+x}}\" ]; then exit 91; fi\nexec '{}' \"$@\"\n",
            env!("CARGO_BIN_EXE_sluice")
        ),
    );
    let runtime = format!(
        "/run/user/{}",
        std::os::unix::fs::MetadataExt::uid(&std::fs::metadata("/proc/self").unwrap())
    );
    let check = |poison: bool, prepared_copy: bool| {
        Command::new("/usr/bin/systemd-run")
            .args([
                "--user",
                "--quiet",
                "--wait",
                "--pipe",
                "--collect",
                "--property=MemoryMax=1G",
                "--property=MemorySwapMax=0",
                "--",
            ])
            .arg("/usr/bin/env")
            .arg("PATH=/usr/bin:/bin")
            .arg(if poison {
                "TOKIO_WORKER_THREADS=invalid"
            } else {
                "TOKIO_WORKER_THREADS=2"
            })
            .arg(if poison {
                "RUSTFLAGS=--invalid-inherited-setting"
            } else {
                "COMPAT_TEST=clean-runtime"
            })
            .arg("SLUICE_TEST=1")
            .arg(format!("SLUICE_HOME={}", gate.home.display()))
            .arg("SLUICE_HOST_UNIT=sluice-fake.service")
            .arg("XDG_RUNTIME_DIR=/nonexistent-compat-bus")
            .arg("DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent-compat-bus/none")
            .arg(repo().join("scripts/compat-check"))
            .arg("--release")
            .arg(&release)
            .arg(if prepared_copy { "--copy" } else { "--home" })
            .arg(&gate.home)
            .arg("--prefix")
            .arg(gate.root.path().join("prefix"))
            .env("XDG_RUNTIME_DIR", &runtime)
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={runtime}/bus"),
            )
            .output()
            .unwrap()
    };
    let output = check(false, false);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("compat: 1 releases ok"),
        "{}",
        text(&output)
    );
    let output = check(true, false);
    assert!(output.status.success(), "{}", text(&output));
    // A failed candidate must show multiline diagnostics and a copyable rerun, and remove
    // its scratch directory after stopping every process it started.
    executable::write(
        &binary,
        "#!/bin/sh\necho 'first line' >&2\necho 'second line' >&2\necho 'last diagnostic' >&2\nexit 1\n",
    );
    let output = check(true, false);
    assert!(!output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("first line\nsecond line\nlast diagnostic"),
        "{stderr}"
    );
    assert!(
        stderr.contains("compat: rerun: cd ") && stderr.contains("--release"),
        "{stderr}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let scratch = stdout
        .split("(scratch ")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap();
    assert!(
        !Path::new(scratch).exists(),
        "scratch left behind: {scratch}"
    );
    let output = check(true, true);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(String::from_utf8_lossy(&output.stderr).contains("compat: rerun:"));
    assert!(gate.home.join("sluice.db").is_file());
}
