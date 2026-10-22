//! Shipping chooses slow groups, preserves dry runs and removes temporary build targets.
use crate::executable;
use std::{fs, path::Path, process::Command};

fn git(repo: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Samuel Galanakis",
            "-c",
            "user.email=47306720+SamGalanakis@users.noreply.github.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

// Temporary ships used to leave a new multi-gigabyte Cargo target after every checkout removal.
#[test]
fn slow_ship_cleans_temporary_targets_and_preserves_repository_and_unsafe_targets() {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    let scratch = tempfile::tempdir_in("/tmp").unwrap();
    let targets = tempfile::Builder::new()
        .prefix("ship-gc-test-")
        .tempdir_in("/workspace/.cargo-targets")
        .unwrap();
    let repo = scratch.path().join("repo");
    let bin = scratch.path().join("bin");
    let home = scratch.path().join("home");
    let temporary = scratch.path().join("temporary");
    let own = targets.path().join("repository");
    let outside = scratch.path().join("outside");
    let historical_root = Path::new("/workspace/.cargo-targets/sluice-historical");
    fs::create_dir_all(historical_root).unwrap();
    let historical = tempfile::Builder::new()
        .prefix("ship-protected-")
        .tempdir_in(historical_root)
        .unwrap();
    let historical_target = historical.path().to_path_buf();
    for dir in [
        &repo.join("scripts"),
        &repo.join("crates/sluice-store/src"),
        &bin,
        &home,
        &temporary,
        &own,
        &outside,
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(own.join("keep"), "Repository build artifacts").unwrap();
    fs::write(outside.join("keep"), "Unrelated files").unwrap();
    let script = repo.join("scripts/ship");
    executable::write(&script, include_str!("../../../../scripts/ship"));
    executable::write(
        repo.join("scripts/gate"),
        include_str!("../../../../scripts/gate"),
    );
    executable::write(
        repo.join("scripts/check"),
        include_str!("../../../../scripts/check"),
    );
    executable::write(repo.join("scripts/deploy"), "#!/bin/sh\nexit 0\n");
    fs::create_dir_all(repo.join("crates/sluice/tests/it")).unwrap();
    fs::write(
        repo.join("crates/sluice/tests/it/release.rs"),
        "#[test]\nfn slow_fixture_durability() {}\n",
    )
    .unwrap();
    fs::write(
        repo.join("crates/sluice-store/src/schema.rs"),
        format!(
            "pub const SCHEMA_VERSION: i64 = {};\n",
            sluice_store::schema::SCHEMA_VERSION
        ),
    )
    .unwrap();
    let db = rusqlite::Connection::open(home.join("sluice.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE home_meta(singleton INTEGER, schema_version INTEGER);
         CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY, phase TEXT NOT NULL, request TEXT NOT NULL);
         CREATE TABLE runs(run_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL REFERENCES attempts(attempt_id),
                           finished_at TEXT, project_id TEXT, result TEXT, unit_name TEXT);
         CREATE TABLE projects(project_id TEXT, archived INTEGER);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO home_meta VALUES(1,?1)",
        [sluice_store::schema::SCHEMA_VERSION],
    )
    .unwrap();
    drop(db);
    executable::write(
        bin.join("sluice"),
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\n",
            serde_json::json!({"selection": {"home_path": home}})
        ),
    );
    // All pushes and deployments are fixtures. Git still creates and removes real worktrees.
    executable::write(
        bin.join("git"),
        r#"#!/usr/bin/env python3
import os, pathlib, subprocess, sys
repo, verb = sys.argv[2:4]
def git(*args, data=None):
    return subprocess.check_output(['/usr/bin/git', '-C', repo, *args], input=data, text=True).strip()
if verb == 'fetch': sys.exit(0)
if verb == 'diff' and os.environ['SHIP_FIXTURE_MODE'] == 'success-groups-retry' and pathlib.Path(os.environ['SHIP_FIXTURE_PUSH']).exists():
    print('crates/sluice-store/src/writer.rs')
    sys.exit(0)
if verb == 'push':
    mode = os.environ['SHIP_FIXTURE_MODE']
    record = pathlib.Path(os.environ['SHIP_FIXTURE_PUSH'])
    if 'retry' in mode and not record.exists():
        record.write_text('rejected')
        base = git('rev-parse', 'origin/main')
        tree = git('rev-parse', 'HEAD^{tree}')
        if mode == 'success-tree-retry':
            blob = git('hash-object', '-w', '--stdin', data='Upstream change')
            tree = git('mktree', data=git('ls-tree', 'HEAD') + '\n100644 blob ' + blob + '\tupstream.txt\n')
        commit = git('-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.com',
                     'commit-tree', tree, '-p', base, '-m', 'Concurrent upstream change')
        git('update-ref', 'refs/remotes/origin/main', commit)
        print('[rejected] fetch first', file=sys.stderr)
        sys.exit(1)
    git('update-ref', 'refs/remotes/origin/main', 'HEAD')
    sys.exit(0)
os.execv('/usr/bin/git', ['/usr/bin/git', *sys.argv[1:]])
"#,
    );
    executable::write(
        bin.join("cargo"),
        r#"#!/usr/bin/env python3
import json, os, pathlib, sys
repo = pathlib.Path(os.environ['SHIP_FIXTURE_REPO'])
target = pathlib.Path(os.environ['SHIP_FIXTURE_OWN'] if pathlib.Path.cwd() == repo else os.environ['SHIP_FIXTURE_TARGET'])
arguments = sys.argv[1:]
if arguments[:1] == ['--config']: arguments = arguments[2:]
if arguments[0] == 'metadata':
    if pathlib.Path.cwd() != repo:
        pathlib.Path(os.environ['SHIP_FIXTURE_CHECKOUT']).write_text(str(pathlib.Path.cwd()))
    if os.environ['SHIP_FIXTURE_MODE'] == 'metadata-failure': sys.exit(1)
    print(json.dumps({'target_directory': str(target)}))
else:
    target.mkdir(parents=True, exist_ok=True)
    (target / 'artifact').write_text('Build artifacts')
    with open(os.environ['SHIP_FIXTURE_GATES'], 'a') as record: record.write(arguments[0] + '\n')
    if not os.environ['SHIP_FIXTURE_MODE'].startswith('success'): sys.exit(1)
"#,
    );
    let path =
        std::env::join_paths([bin.as_path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let base = commit(&repo);
    git(&repo, &["update-ref", "refs/remotes/origin/main", &base]);
    fs::write(repo.join("README.md"), "Candidate docs.\n").unwrap();
    let head = commit(&repo);
    git(&repo, &["branch", "candidate", &head]);
    let target = targets.path().join("temporary");
    let symlink = targets.path().join("symlink");
    std::os::unix::fs::symlink(&outside, &symlink).unwrap();
    let checkout_record = scratch.path().join("checkout");
    for (mode, candidate, cargo_target, dry) in [
        ("success", "candidate", &target, false),
        ("success-empty-retry", "candidate", &target, false),
        ("success-tree-retry", "candidate", &target, false),
        ("success-groups-retry", "candidate", &target, false),
        ("failure", "candidate", &target, false),
        ("failure", "candidate", &own, false),
        ("failure", "candidate", &outside, false),
        ("failure", "candidate", &historical_target, false),
        ("failure", "candidate", &symlink, false),
        ("metadata-failure", "candidate", &target, false),
        ("failure", "HEAD", &target, false),
        ("failure", "candidate", &target, true),
    ] {
        git(&repo, &["update-ref", "refs/remotes/origin/main", &base]);
        fs::create_dir_all(cargo_target).unwrap();
        fs::write(cargo_target.join("artifact"), "Build artifacts").unwrap();
        let _ = fs::remove_file(&checkout_record);
        let push_record = scratch.path().join("push");
        let gate_record = scratch.path().join("gates");
        let _ = fs::remove_file(&push_record);
        let _ = fs::remove_file(&gate_record);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = AtomicBool::new(false);
        let result = std::thread::scope(|scope| {
            let stopped = &stopped;
            let server = scope.spawn(move || {
                loop {
                    let (mut stream, _) = listener.accept().unwrap();
                    if stopped.load(Ordering::Relaxed) {
                        break;
                    }
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0; 1024];
                    // Empty port probes are not an adoption request. Keep serving until
                    // the ship finishes, including if its HTTP check needs another try.
                    if stream.read(&mut request).unwrap_or(0) > 0 {
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            });
            let mut command = Command::new(&script);
            command.current_dir(&repo).arg(candidate);
            if dry {
                command.arg("--dry-run");
            }
            let result = command
                .arg("--prefix")
                .arg(scratch.path().join("prefix"))
                .env("PATH", &path)
                .env("TMPDIR", &temporary)
                .env("SLUICE_INSTALL_DIR", scratch.path().join("install"))
                .env("SLUICE_HOME", &home)
                .env("SLUICE_DEPLOY_PORT", address.port().to_string())
                .env("SHIP_FIXTURE_REPO", &repo)
                .env("SHIP_FIXTURE_OWN", &own)
                .env("SHIP_FIXTURE_TARGET", cargo_target)
                .env("SHIP_FIXTURE_CHECKOUT", &checkout_record)
                .env("SHIP_FIXTURE_MODE", mode)
                .env("SHIP_FIXTURE_PUSH", &push_record)
                .env("SHIP_FIXTURE_GATES", &gate_record)
                .output()
                .unwrap();
            stopped.store(true, Ordering::Relaxed);
            // Wake the fixture's accept so it can observe the end of the scenario.
            let _ = TcpStream::connect(address);
            server.join().unwrap();
            result
        });
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert_eq!(
            result.status.success(),
            mode.starts_with("success") || dry,
            "{stderr}"
        );
        if mode.starts_with("success") {
            let gates = fs::read_to_string(&gate_record).unwrap();
            let expected = if mode == "success-tree-retry" { 2 } else { 1 };
            for command in ["fmt", "clippy", "nextest", "test"] {
                assert_eq!(
                    gates.lines().filter(|line| *line == command).count(),
                    if mode == "success-groups-retry" && command == "nextest" {
                        2
                    } else {
                        expected
                    },
                    "{mode}: {stderr}"
                );
            }
            assert_eq!(
                stderr.contains("reusing that gate"),
                mode == "success-empty-retry" || mode == "success-groups-retry",
                "{stderr}"
            );
        }
        if mode == "success-groups-retry" {
            assert!(
                stderr.contains("slow group durability (1 tests)"),
                "{stderr}"
            );
            assert!(stderr.contains("--profile slow"), "{stderr}");
        }
        let removed =
            candidate != "HEAD" && !dry && cargo_target == &target && mode != "metadata-failure";
        assert_eq!(!cargo_target.exists(), removed, "{mode}: {stderr}");
        assert!(own.join("keep").exists(), "{stderr}");
        assert!(outside.join("keep").exists(), "{stderr}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(
            git(&repo, &["worktree", "list", "--porcelain"])
                .matches("worktree ")
                .count(),
            1
        );
        assert_eq!(fs::read_dir(&temporary).unwrap().count(), 0, "{stderr}");
        if checkout_record.exists() {
            let checkout = fs::read_to_string(&checkout_record).unwrap();
            assert!(!Path::new(&checkout).exists(), "{stderr}");
        }
        if dry {
            assert!(stderr.contains("cleanup: would resolve"), "{stderr}");
        }
        if removed {
            assert!(
                stderr.contains("removed temporary cargo target"),
                "{stderr}"
            );
        }
    }
}
fn commit(repo: &Path) -> String {
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", "Fixture change"]);
    git(repo, &["rev-parse", "HEAD"])
}

#[test]
fn ship_dry_run_gates_the_requested_branch_and_preserves_the_checkout() {
    let scratch = tempfile::tempdir_in("/tmp").unwrap();
    let repo = scratch.path().join("repo");
    let home = scratch.path().join("home");
    let bin = scratch.path().join("bin");
    for dir in [
        &repo,
        &home,
        &bin,
        &repo.join("scripts"),
        &repo.join("crates/sluice-store/src"),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    let script = repo.join("scripts/ship");
    executable::write(&script, include_str!("../../../../scripts/ship"));
    executable::write(
        repo.join("scripts/gate"),
        include_str!("../../../../scripts/gate"),
    );
    executable::write(
        repo.join("scripts/check"),
        include_str!("../../../../scripts/check"),
    );
    fs::write(
        repo.join("crates/sluice-store/src/schema.rs"),
        format!(
            "pub const SCHEMA_VERSION: i64 = {};\n",
            sluice_store::schema::SCHEMA_VERSION
        ),
    )
    .unwrap();
    let db = rusqlite::Connection::open(home.join("sluice.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE home_meta(singleton INTEGER, schema_version INTEGER);
         CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY, phase TEXT NOT NULL, request TEXT NOT NULL);
         CREATE TABLE runs(run_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL REFERENCES attempts(attempt_id),
                           finished_at TEXT, project_id TEXT, result TEXT, unit_name TEXT);
         CREATE TABLE projects(project_id TEXT, archived INTEGER);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO home_meta VALUES(1,?1)",
        [sluice_store::schema::SCHEMA_VERSION],
    )
    .unwrap();
    drop(db);
    let status = serde_json::json!({"selection": {"home_path": home}}).to_string();
    executable::write(
        bin.join("sluice"),
        format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", status),
    );
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let base = commit(&repo);
    git(&repo, &["update-ref", "refs/remotes/origin/main", &base]);
    fs::write(repo.join("README.md"), "Only docs.\n").unwrap();
    let docs = commit(&repo);
    git(&repo, &["branch", "docs-candidate", &docs]);
    git(&repo, &["checkout", "-q", "--detach", &base]);
    fs::write(repo.join("scripts/deploy"), "Upstream deployment change.\n").unwrap();
    let upstream = commit(&repo);
    git(
        &repo,
        &["update-ref", "refs/remotes/origin/main", &upstream],
    );
    fs::write(
        repo.join("crates/sluice-store/src/writer.rs"),
        "Branch durability change.\n",
    )
    .unwrap();
    let head = commit(&repo);
    for (candidate, slow) in [("HEAD", true), ("docs-candidate", false)] {
        let result = Command::new(&script)
            .current_dir(&repo)
            .args([candidate, "--dry-run", "--prefix"])
            .arg(scratch.path().join("prefix"))
            .env("PATH", &path)
            .env("SLUICE_INSTALL_DIR", scratch.path().join("install"))
            .env("SLUICE_HOME", &home)
            .output()
            .unwrap();
        let stderr = String::from_utf8(result.stderr).unwrap();
        assert!(result.status.success(), "{stderr}");
        assert!(
            stderr.contains("cargo clippy --workspace --all-targets --locked"),
            "{stderr}"
        );
        assert!(
            stderr.contains("cargo nextest run --workspace --all-targets --locked"),
            "{stderr}"
        );
        assert!(stderr.contains("cargo fmt --all --check"), "{stderr}");
        assert!(
            stderr.contains("cargo test --workspace --doc --locked"),
            "{stderr}"
        );
        assert_eq!(
            stderr.contains(
                "slow group durability (0 tests) required by: crates/sluice-store/src/writer.rs"
            ),
            slow,
            "{stderr}"
        );
        assert!(stderr.contains("dry run: shipped nothing"), "{stderr}");
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&repo, &["rev-parse", "origin/main"]), upstream);
        assert!(git(&repo, &["status", "--porcelain"]).is_empty());
        assert!(!repo.join(".git/FETCH_HEAD").exists());
    }
}

// Catch overbroad schema triggers, missing browser coverage and unmapped new slow scenarios
// through the local entry point that ship shares, without doing a Cargo build in the fixture.
#[test]
fn local_gate_selects_risks_and_refuses_unmapped_slow_tests() {
    let scratch = tempfile::tempdir_in("/tmp").unwrap();
    let repo = scratch.path();
    for dir in [
        "scripts",
        "crates/sluice-store/src",
        "crates/sluice-web/tests",
        "crates/sluice/tests/it",
    ] {
        fs::create_dir_all(repo.join(dir)).unwrap();
    }
    executable::write(
        repo.join("scripts/check"),
        include_str!("../../../../scripts/check"),
    );
    executable::write(
        repo.join("scripts/gate"),
        include_str!("../../../../scripts/gate"),
    );
    fs::write(
        repo.join("crates/sluice-web/tests/browser.rs"),
        "#[test]\nfn slow_browser_fixture() {}\n",
    )
    .unwrap();
    fs::write(
        repo.join("crates/sluice/tests/it/release.rs"),
        "#[test]\nfn slow_release_fixture() {}\n",
    )
    .unwrap();
    let schema = repo.join("crates/sluice-store/src/schema.rs");
    let initial = "pub const SCHEMA_VERSION: i64 = 4;\nconst ADDED_COLUMNS: &[(&str, &str, &str)] = &[];\nconst ADDED_VIEWS: &[(&str, &str)] = &[(\"old\", \"SELECT 1;\")];\n";
    fs::write(&schema, initial).unwrap();
    git(repo, &["init", "-q", "-b", "main"]);
    let base = commit(repo);
    git(repo, &["update-ref", "refs/remotes/origin/main", &base]);
    let added = initial.replace("&[]", "&[(\"runs\", \"new\", \"TEXT\"),]");
    let view = initial.replace("SELECT 1;\")", "SELECT 1;\"), (\"new\", \"SELECT '[;]';\")");
    for (source, durability) in [
        (&added, false),
        (&view, false),
        (&initial.replace("SELECT 1;", "SELECT 2;"), true),
        (&initial.replace("= 4;", "= 5;"), true),
        (
            &initial.replace("&[]", "&[];\nfn changed_writer() {}"),
            true,
        ),
        (&initial.replace("&[(\"old\", \"SELECT 1;\")]", "&[]"), true),
        (
            &added.replace("TEXT\"),]", "TEXT\"), (\"runs\", \"new\", \"INTEGER\"),]"),
            true,
        ),
    ] {
        fs::write(&schema, source).unwrap();
        let output = Command::new(repo.join("scripts/check"))
            .current_dir(repo)
            .arg("--dry-run")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        assert_eq!(
            stderr.contains("slow group durability"),
            durability,
            "{source}\n{stderr}"
        );
        assert_eq!(stderr.contains("--profile slow"), durability, "{stderr}");
        assert!(!stderr.contains("slow group browser"), "{stderr}");
    }
    fs::write(&schema, initial).unwrap();
    fs::write(
        repo.join("crates/sluice-web/page.rs"),
        "// Dashboard change\n",
    )
    .unwrap();
    // Ship uses committed base..head changes, and the local command includes staged changes.
    git(repo, &["add", "."]);
    let output = Command::new(repo.join("scripts/check"))
        .current_dir(repo)
        .arg("--dry-run")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("slow group browser (1 tests)"), "{stderr}");
    assert!(
        stderr.contains("test(/(^|::)(slow_browser_fixture)$/)"),
        "{stderr}"
    );
    assert!(!stderr.contains("slow group durability"), "{stderr}");
    fs::write(
        repo.join("crates/sluice/tests/it/unmapped.rs"),
        "#[test]\nfn slow_unmapped_fixture() {}\n",
    )
    .unwrap();
    let output = Command::new(repo.join("scripts/check"))
        .current_dir(repo)
        .arg("--dry-run")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("slow tests need exactly one group"));
}
