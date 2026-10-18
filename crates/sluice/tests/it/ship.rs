//! Shipping chooses durability gates, preserves dry runs and removes temporary build targets.
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
fn ship_cleans_temporary_targets_and_preserves_repository_and_unsafe_targets() {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
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
    executable::write(repo.join("scripts/deploy"), "#!/bin/sh\nexit 0\n");
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
        "#!/bin/sh\ncase \"$3\" in\nfetch) exit 0;;\npush) exec /usr/bin/git -C \"$2\" update-ref refs/remotes/origin/main HEAD;;\nesac\nexec /usr/bin/git \"$@\"\n",
    );
    executable::write(
        bin.join("cargo"),
        r#"#!/usr/bin/env python3
import json, os, pathlib, sys
repo = pathlib.Path(os.environ['SHIP_FIXTURE_REPO'])
target = pathlib.Path(os.environ['SHIP_FIXTURE_OWN'] if pathlib.Path.cwd() == repo else os.environ['SHIP_FIXTURE_TARGET'])
if sys.argv[1] == 'metadata':
    if pathlib.Path.cwd() != repo:
        pathlib.Path(os.environ['SHIP_FIXTURE_CHECKOUT']).write_text(str(pathlib.Path.cwd()))
    if os.environ['SHIP_FIXTURE_MODE'] == 'metadata-failure': sys.exit(1)
    print(json.dumps({'target_directory': str(target)}))
else:
    target.mkdir(parents=True, exist_ok=True)
    (target / 'artifact').write_text('Build artifacts')
    if os.environ['SHIP_FIXTURE_MODE'] != 'success': sys.exit(1)
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
        ("failure", "candidate", &target, false),
        ("failure", "candidate", &own, false),
        ("failure", "candidate", &outside, false),
        ("failure", "candidate", &symlink, false),
        ("metadata-failure", "candidate", &target, false),
        ("failure", "HEAD", &target, false),
        ("failure", "candidate", &target, true),
    ] {
        git(&repo, &["update-ref", "refs/remotes/origin/main", &base]);
        fs::create_dir_all(cargo_target).unwrap();
        fs::write(cargo_target.join("artifact"), "Build artifacts").unwrap();
        let _ = fs::remove_file(&checkout_record);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let result = std::thread::scope(|scope| {
            let server = scope.spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0; 1024];
                if stream.read(&mut request).unwrap_or(0) > 0 {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
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
                .output()
                .unwrap();
            // Wake the fixture if this scenario stopped before the adoption HTTP request.
            let _ = TcpStream::connect(address);
            server.join().unwrap();
            result
        });
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert_eq!(
            result.status.success(),
            mode == "success" || dry,
            "{stderr}"
        );
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
        assert_eq!(stderr.contains("--profile slow"), slow, "{stderr}");
        assert_eq!(
            stderr.contains(
                "slow tier required by durability changes: crates/sluice-store/src/writer.rs"
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
