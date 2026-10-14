//! Shipping chooses durability gates from the requested branch and never mutates a dry run.
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
    db.execute_batch("CREATE TABLE home_meta(singleton INTEGER, schema_version INTEGER); CREATE TABLE runs(run_id TEXT,finished_at TEXT,project_id TEXT); CREATE TABLE projects(project_id TEXT,archived INTEGER);").unwrap();
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
