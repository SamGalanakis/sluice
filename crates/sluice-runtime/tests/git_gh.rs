//! git.* and gh.* builtins against real scratch repositories and fake `git`/`gh`
//! executables, porting packs/git/tests/test_git.py. Every repo lives under a TempDir —
//! nothing touches this repository's own git state, and the fakes mean real GitHub is
//! never contacted.

use indexmap::IndexMap;
use serde_json::{Value, json};
use sluice_model::rpc::JsonMap;
use sluice_runtime::builtins::{
    gh,
    git::{self, BuiltinCtx, FnFailure},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use tempfile::TempDir;

// ---- fixture git (real, on scratch repos only) --------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
fn git_code(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

struct Repo {
    path: PathBuf,
    origin: PathBuf,
}

/// A repo on `main` with one commit, wired to a bare `origin` remote.
fn repo(tmp: &TempDir) -> Repo {
    let path = tmp.path().join("repo");
    fs::create_dir(&path).unwrap();
    git(&path, &["init", "-b", "main"]);
    git(&path, &["config", "user.email", "t@example.com"]);
    git(&path, &["config", "user.name", "T"]);
    fs::write(path.join("f.txt"), "one\n").unwrap();
    git(&path, &["add", "."]);
    git(&path, &["commit", "-m", "init"]);
    let origin = tmp.path().join("origin.git");
    let out = Command::new("git")
        .args(["init", "--bare"])
        .arg(&origin)
        .output()
        .unwrap();
    assert!(out.status.success());
    git(
        &path,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&path, &["push", "-u", "origin", "main"]);
    Repo { path, origin }
}

/// The run env fn_env would produce: ambient vars, SLUICE_HOST_* records, and — when a
/// bin dir is given — it prepended to PATH and recorded as SLUICE_HOST_PATH, so the
/// fakes are what child_env restores.
fn ctx(run_dir: &Path, bin: Option<&Path>) -> BuiltinCtx {
    fs::create_dir_all(run_dir).unwrap();
    let mut env: IndexMap<String, String> = std::env::vars().collect();
    let path = match bin {
        Some(dir) => {
            let mut dirs = vec![dir.to_path_buf()];
            if let Some(existing) = env.get("PATH") {
                dirs.extend(std::env::split_paths(existing));
            }
            std::env::join_paths(&dirs)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        }
        None => env.get("PATH").cloned().unwrap_or_default(),
    };
    env.insert("PATH".into(), path.clone());
    env.insert("SLUICE_HOST_PATH".into(), path);
    env.insert(
        "SLUICE_HOST_PYTHONPATH".into(),
        env.get("PYTHONPATH").cloned().unwrap_or_default(),
    );
    env.insert(
        "SLUICE_HOST_VIRTUAL_ENV".into(),
        env.get("VIRTUAL_ENV").cloned().unwrap_or_default(),
    );
    env.insert(
        "SLUICE_RUN_DIR".into(),
        run_dir.to_string_lossy().into_owned(),
    );
    BuiltinCtx::new(env, run_dir)
}

fn inputs(value: Value) -> JsonMap {
    serde_json::from_value(value).unwrap()
}
fn get<'a>(out: &'a JsonMap, name: &str) -> &'a Value {
    out.0.get(name).unwrap().as_value()
}
fn tmp() -> TempDir {
    TempDir::new().unwrap()
}
fn run_dir(tmp: &TempDir) -> PathBuf {
    tmp.path().join("run")
}

// ---- fake tools (sh scripts on a scratch PATH) ---------------------------------

/// Write `body` as an executable named `name` inside {tmp}/bin; returns the bin dir.
fn fake_bin(tmp: &TempDir, name: &str, body: &str) -> PathBuf {
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let file = bin.join(name);
    fs::write(&file, body).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Decode one line of NUL-separated argv per recorded call.
fn read_calls(path: &Path) -> Vec<Vec<String>> {
    fs::read(path)
        .unwrap()
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        })
        .collect()
}

/// A fake gh answering `pr list`/`pr view` with canned JSON and recording argv.
fn make_gh(tmp: &TempDir, list_json: &str, view_json: &str) -> (PathBuf, PathBuf) {
    let argv_file = tmp.path().join("gh.argv");
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\0' \"$@\" >> \"{0}\"\n\
         printf '\\n' >> \"{0}\"\n\
         if [ \"$1\" = \"pr\" ] && [ \"$2\" = \"list\" ]; then\n\
         \x20 printf '%s\\n' '{1}'\n\
         elif [ \"$1\" = \"pr\" ] && [ \"$2\" = \"view\" ]; then\n\
         \x20 printf '%s\\n' '{2}'\n\
         fi\n\
         exit 0\n",
        argv_file.display(),
        list_json,
        view_json
    );
    (fake_bin(tmp, "gh", &script), argv_file)
}

/// A fake gh answering each `pr view` with the next canned JSON, then the last.
fn make_gh_seq(tmp: &TempDir, responses: &[String]) -> (PathBuf, PathBuf) {
    let argv_file = tmp.path().join("gh.argv");
    let resp = tmp.path().join("gh-resp");
    fs::create_dir_all(&resp).unwrap();
    for (i, body) in responses.iter().enumerate() {
        fs::write(resp.join(format!("{i}.json")), body).unwrap();
    }
    fs::write(resp.join("n"), "0").unwrap();
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\0' \"$@\" >> \"{0}\"\n\
         printf '\\n' >> \"{0}\"\n\
         n=0\n\
         [ -f \"{1}/n\" ] && n=$(cat \"{1}/n\")\n\
         echo $((n + 1)) > \"{1}/n\"\n\
         f=\"{1}/$n.json\"\n\
         while [ ! -f \"$f\" ]; do n=$((n - 1)); f=\"{1}/$n.json\"; done\n\
         cat \"$f\"\n",
        argv_file.display(),
        resp.display()
    );
    (fake_bin(tmp, "gh", &script), argv_file)
}

/// A fake gh that fails every call on stderr.
fn make_gh_failing(tmp: &TempDir, msg: &str) -> (PathBuf, PathBuf) {
    let argv_file = tmp.path().join("gh.argv");
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\0' \"$@\" >> \"{0}\"\n\
         printf '\\n' >> \"{0}\"\n\
         echo \"{1}\" >&2\n\
         exit 1\n",
        argv_file.display(),
        msg
    );
    (fake_bin(tmp, "gh", &script), argv_file)
}

/// A fake gh answering `run list`/`run view`/`run cancel` from canned bodies.
fn make_gh_runs(
    tmp: &TempDir,
    list_body: &str,
    jobs_body: &str,
    cancel_code: i32,
    cancel_err: &str,
) -> (PathBuf, PathBuf) {
    let argv_file = tmp.path().join("gh.argv");
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\0' \"$@\" >> \"{0}\"\n\
         printf '\\n' >> \"{0}\"\n\
         if [ \"$1\" = \"run\" ] && [ \"$2\" = \"list\" ]; then\n\
         \x20 printf '%s\\n' '{1}'\n\
         elif [ \"$1\" = \"run\" ] && [ \"$2\" = \"view\" ]; then\n\
         \x20 printf '%s\\n' '{2}'\n\
         elif [ \"$1\" = \"run\" ] && [ \"$2\" = \"cancel\" ]; then\n\
         \x20 printf '%s' '{3}' >&2\n\
         \x20 exit {4}\n\
         fi\n\
         exit 0\n",
        argv_file.display(),
        list_body,
        jobs_body,
        cancel_err,
        cancel_code
    );
    (fake_bin(tmp, "gh", &script), argv_file)
}

/// A fake git that records argv; argument-safety tests prove it is never invoked.
fn make_git(tmp: &TempDir) -> (PathBuf, PathBuf) {
    let argv_file = tmp.path().join("git.argv");
    let script = format!(
        "#!/bin/sh\n\
         printf '%s\\0' \"$@\" >> \"{0}\"\n\
         printf '\\n' >> \"{0}\"\n",
        argv_file.display()
    );
    (fake_bin(tmp, "git", &script), argv_file)
}

fn pr_json(state: &str, mergeable: &str, sha: &str, url: &str, rollup: Value) -> String {
    json!({
        "state": state,
        "mergeable": mergeable,
        "headRefOid": sha,
        "url": url,
        "statusCheckRollup": rollup,
    })
    .to_string()
}
fn check(name: &str, status: &str, conclusion: Option<&str>) -> Value {
    json!({"name": name, "status": status, "conclusion": conclusion})
}
fn context(name: &str, state: &str) -> Value {
    json!({"context": name, "state": state})
}
fn run_json() -> String {
    json!([{
        "databaseId": 991,
        "headSha": "feed42",
        "status": "completed",
        "conclusion": "success",
        "url": "https://example.test/run/991",
        "workflowName": "CI",
    }])
    .to_string()
}

async fn pr_wait(
    bin: &Path,
    tmp: &TempDir,
    until: &str,
    extra: &[(&str, Value)],
) -> Result<JsonMap, FnFailure> {
    let mut inp = json!({"path": tmp.path(), "pr": "3", "until": until, "interval": 0});
    for (k, v) in extra {
        inp.as_object_mut()
            .unwrap()
            .insert(k.to_string(), v.clone());
    }
    gh::pr_wait(&inputs(inp), &ctx(&run_dir(tmp), Some(bin))).await
}

// ---- git.worktree / worktree_rm / head ------------------------------------------

#[tokio::test]
async fn worktree_new_branch_default_path() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let out = git::worktree(
        &inputs(json!({"repo": &repo.path, "base": "main", "branch": "feat"})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    let expected = tmp
        .path()
        .join("repo-wt")
        .join("feat")
        .canonicalize()
        .unwrap();
    assert_eq!(get(&out, "path"), &json!(expected.to_str().unwrap()));
    assert_eq!(get(&out, "branch"), "feat");
    assert_eq!(
        get(&out, "sha"),
        &json!(git(&repo.path, &["rev-parse", "main"]))
    );
    assert_eq!(
        git(&expected, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "feat"
    );
}

#[tokio::test]
async fn worktree_explicit_path() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let wt = tmp.path().join("wt-here");
    let out = git::worktree(
        &inputs(json!({"repo": &repo.path, "base": "main", "branch": "feat", "path": &wt})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(
        get(&out, "path"),
        &json!(wt.canonicalize().unwrap().to_str().unwrap())
    );
    assert!(wt.join("f.txt").exists());
}

#[tokio::test]
async fn worktree_existing_branch() {
    let tmp = tmp();
    let repo = repo(&tmp);
    git(&repo.path, &["branch", "exists"]);
    let sha = git(&repo.path, &["rev-parse", "exists"]);
    let wt = tmp.path().join("wt2");
    let out = git::worktree(
        &inputs(json!({"repo": &repo.path, "base": "main", "branch": "exists", "path": &wt})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "sha"), &json!(sha));
    assert_eq!(get(&out, "branch"), "exists");
}

#[tokio::test]
async fn worktree_rm() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let wt = tmp.path().join("wt-gone");
    let ctx = ctx(&run_dir(&tmp), None);
    git::worktree(
        &inputs(json!({"repo": &repo.path, "base": "main", "branch": "feat", "path": &wt})),
        &ctx,
    )
    .await
    .unwrap();
    let out = git::worktree_rm(&inputs(json!({"repo": &repo.path, "path": &wt})), &ctx)
        .await
        .unwrap();
    assert_eq!(get(&out, "removed"), &json!(true));
    assert!(!wt.exists());
    let out = git::worktree_rm(&inputs(json!({"repo": &repo.path, "path": &wt})), &ctx)
        .await
        .unwrap();
    assert_eq!(get(&out, "removed"), &json!(false));
}

#[tokio::test]
async fn worktree_rm_needs_force() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let wt = tmp.path().join("wt-dirty");
    let ctx = ctx(&run_dir(&tmp), None);
    git::worktree(
        &inputs(json!({"repo": &repo.path, "base": "main", "branch": "feat", "path": &wt})),
        &ctx,
    )
    .await
    .unwrap();
    fs::write(wt.join("f.txt"), "dirty\n").unwrap();
    // git refuses to remove a dirty worktree
    git::worktree_rm(&inputs(json!({"repo": &repo.path, "path": &wt})), &ctx)
        .await
        .unwrap_err();
    let out = git::worktree_rm(
        &inputs(json!({"repo": &repo.path, "path": &wt, "force": true})),
        &ctx,
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "removed"), &json!(true));
}

#[tokio::test]
async fn worktree_rm_plain_dir_is_not_a_worktree() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let plain = tmp.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let out = git::worktree_rm(
        &inputs(json!({"repo": &repo.path, "path": &plain})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "removed"), &json!(false));
    assert!(plain.exists());
}

#[tokio::test]
async fn head() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let out = git::head(
        &inputs(json!({"path": &repo.path})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "branch"), "main",);
    assert_eq!(
        get(&out, "sha"),
        &json!(git(&repo.path, &["rev-parse", "HEAD"]))
    );
}

// ---- git.merge / git.rebase / git.push -------------------------------------------

/// feature work on a side branch, main back on the base commit.
fn feature_branch(repo: &Repo) {
    git(&repo.path, &["checkout", "-b", "feature"]);
    fs::write(repo.path.join("g.txt"), "feature\n").unwrap();
    git(&repo.path, &["add", "."]);
    git(&repo.path, &["commit", "-m", "feature work"]);
    git(&repo.path, &["checkout", "main"]);
}

#[tokio::test]
async fn merge_detached_target() {
    // target is not checked out anywhere: plain `worktree add <wt> target`.
    let tmp = tmp();
    let repo = repo(&tmp);
    feature_branch(&repo);
    git(&repo.path, &["branch", "release"]); // target exists, not checked out
    let out = git::merge(
        &inputs(json!({
            "repo": &repo.path, "source": "feature", "target": "release",
            "message": "merge feature into release",
        })),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "merged"), &json!(true));
    assert_eq!(get(&out, "conflicts"), &json!([]));
    assert_eq!(
        get(&out, "sha"),
        &json!(git(&repo.path, &["rev-parse", "release"]))
    );
    assert!(git(&repo.path, &["ls-tree", "--name-only", "release"]).contains("g.txt"));
    // merge commit has two parents
    assert_eq!(
        git(&repo.path, &["rev-list", "--parents", "-n", "1", "release"])
            .split_whitespace()
            .count(),
        3
    );
    // temporary worktree is gone
    assert_eq!(git(&repo.path, &["worktree", "list"]).lines().count(), 1);
}

#[tokio::test]
async fn merge_checked_out_target_and_push() {
    // target is checked out in the main worktree: merge detached, then move ref.
    let tmp = tmp();
    let repo = repo(&tmp);
    feature_branch(&repo);
    let old_main = git(&repo.path, &["rev-parse", "main"]);
    let out = git::merge(
        &inputs(json!({
            "repo": &repo.path, "source": "feature", "target": "main", "push": true,
        })),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "merged"), &json!(true));
    let sha = get(&out, "sha").as_str().unwrap();
    assert_ne!(sha, old_main);
    assert_eq!(git(&repo.path, &["rev-parse", "main"]), sha);
    let origin_main = Command::new("git")
        .arg("--git-dir")
        .arg(&repo.origin)
        .args(["rev-parse", "main"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(String::from_utf8_lossy(&origin_main).trim(), sha);
    assert_eq!(git(&repo.path, &["worktree", "list"]).lines().count(), 1);
}

#[tokio::test]
async fn merge_conflict() {
    let tmp = tmp();
    let repo = repo(&tmp);
    git(&repo.path, &["checkout", "-b", "feature"]);
    fs::write(repo.path.join("f.txt"), "feature version\n").unwrap();
    git(&repo.path, &["commit", "-am", "feature edit"]);
    git(&repo.path, &["checkout", "main"]);
    fs::write(repo.path.join("f.txt"), "main version\n").unwrap();
    git(&repo.path, &["commit", "-am", "main edit"]);
    let old_main = git(&repo.path, &["rev-parse", "main"]);
    let out = git::merge(
        &inputs(json!({"repo": &repo.path, "source": "feature", "target": "main"})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "merged"), &json!(false));
    assert_eq!(get(&out, "sha"), &Value::Null);
    assert_eq!(get(&out, "conflicts"), &json!(["f.txt"]));
    assert_eq!(git(&repo.path, &["rev-parse", "main"]), old_main);
    assert_eq!(git(&repo.path, &["worktree", "list"]).lines().count(), 1);
}

#[tokio::test]
async fn rebase_ok() {
    let tmp = tmp();
    let repo = repo(&tmp);
    git(&repo.path, &["checkout", "-b", "topic"]);
    fs::write(repo.path.join("t.txt"), "topic\n").unwrap();
    git(&repo.path, &["add", "."]);
    git(&repo.path, &["commit", "-m", "topic work"]);
    git(&repo.path, &["checkout", "main"]);
    fs::write(repo.path.join("m.txt"), "main\n").unwrap();
    git(&repo.path, &["add", "."]);
    git(&repo.path, &["commit", "-m", "main work"]);
    git(&repo.path, &["checkout", "topic"]);
    let out = git::rebase(
        &inputs(json!({"path": &repo.path, "onto": "main"})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "ok"), &json!(true));
    assert_eq!(
        get(&out, "sha"),
        &json!(git(&repo.path, &["rev-parse", "HEAD"]))
    );
    assert_eq!(get(&out, "conflicts"), &json!([]));
    git(
        &repo.path,
        &["merge-base", "--is-ancestor", "main", "topic"],
    );
    assert!(repo.path.join("m.txt").exists());
    assert!(repo.path.join("t.txt").exists());
}

#[tokio::test]
async fn rebase_conflict() {
    let tmp = tmp();
    let repo = repo(&tmp);
    git(&repo.path, &["checkout", "-b", "topic"]);
    fs::write(repo.path.join("f.txt"), "topic version\n").unwrap();
    git(&repo.path, &["commit", "-am", "topic edit"]);
    let topic_sha = git(&repo.path, &["rev-parse", "HEAD"]);
    git(&repo.path, &["checkout", "main"]);
    fs::write(repo.path.join("f.txt"), "main version\n").unwrap();
    git(&repo.path, &["commit", "-am", "main edit"]);
    git(&repo.path, &["checkout", "topic"]);
    let out = git::rebase(
        &inputs(json!({"path": &repo.path, "onto": "main"})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "ok"), &json!(false));
    assert_eq!(get(&out, "sha"), &json!(topic_sha));
    assert_eq!(get(&out, "conflicts"), &json!(["f.txt"]));
    // rebase fully aborted
    assert_eq!(git(&repo.path, &["rev-parse", "HEAD"]), topic_sha);
    assert_eq!(
        git_code(&repo.path, &["rev-parse", "-q", "--verify", "REBASE_HEAD"]).1,
        ""
    );
    assert_eq!(git(&repo.path, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn push() {
    let tmp = tmp();
    let repo = repo(&tmp);
    fs::write(repo.path.join("n.txt"), "new\n").unwrap();
    git(&repo.path, &["add", "."]);
    git(&repo.path, &["commit", "-m", "new work"]);
    let head = git(&repo.path, &["rev-parse", "HEAD"]);
    let out = git::push(
        &inputs(json!({"path": &repo.path, "branch": "main"})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "sha"), &json!(head));
    let origin_main = Command::new("git")
        .arg("--git-dir")
        .arg(&repo.origin)
        .args(["rev-parse", "main"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(String::from_utf8_lossy(&origin_main).trim(), head);
}

#[tokio::test]
async fn push_force_with_lease() {
    let tmp = tmp();
    let repo = repo(&tmp);
    fs::write(repo.path.join("f.txt"), "amended\n").unwrap();
    git(
        &repo.path,
        &["commit", "-am", "amended", "--amend", "--no-edit"],
    );
    let head = git(&repo.path, &["rev-parse", "HEAD"]);
    let out = git::push(
        &inputs(json!({"path": &repo.path, "branch": "main", "force_with_lease": true})),
        &ctx(&run_dir(&tmp), None),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "sha"), &json!(head));
    let origin_main = Command::new("git")
        .arg("--git-dir")
        .arg(&repo.origin)
        .args(["rev-parse", "main"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(String::from_utf8_lossy(&origin_main).trim(), head);
}

// ---- gh.pr ----------------------------------------------------------------------

const VIEW_JSON: &str = r#"{"number": 12, "url": "https://example.test/pr/12"}"#;

#[tokio::test]
async fn gh_pr_create() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let (bin, argv_file) = make_gh(&tmp, "[]", VIEW_JSON);
    let out = gh::pr(
        &inputs(json!({
            "path": &repo.path, "base": "main", "head": "feature",
            "title": "My title", "body": "My body", "draft": true,
        })),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "number"), &json!(12),);
    assert_eq!(get(&out, "url"), "https://example.test/pr/12");
    let calls = read_calls(&argv_file);
    assert_eq!(
        calls,
        [
            vec!["pr", "list", "--head", "feature", "--json", "number,url"],
            vec![
                "pr", "create", "--base", "main", "--head", "feature", "--title", "My title",
                "--body", "My body", "--draft"
            ],
            vec!["pr", "view", "feature", "--json", "number,url"],
        ]
        .map(|v| v.into_iter().map(String::from).collect::<Vec<_>>())
    );
}

#[tokio::test]
async fn gh_pr_edit_existing() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let view = r#"{"number": 7, "url": "https://example.test/pr/7"}"#;
    let list = r#"[{"number": 7, "url": "https://example.test/pr/7"}]"#;
    let (bin, argv_file) = make_gh(&tmp, list, view);
    let out = gh::pr(
        &inputs(json!({
            "path": &repo.path, "base": "main", "head": "feature",
            "title": "New title", "body": "New body",
        })),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "number"), &json!(7));
    assert_eq!(get(&out, "url"), "https://example.test/pr/7");
    let calls = read_calls(&argv_file);
    assert_eq!(
        calls[1],
        [
            "pr",
            "edit",
            "7",
            "--title",
            "New title",
            "--body",
            "New body"
        ]
    );
    assert!(!calls.iter().any(|c| c[1] == "create"));
    assert!(!calls[1].contains(&"--draft".to_string()));
}

#[tokio::test]
async fn gh_pr_create_not_draft() {
    let tmp = tmp();
    let repo = repo(&tmp);
    let (bin, argv_file) = make_gh(&tmp, "[]", VIEW_JSON);
    gh::pr(
        &inputs(json!({
            "path": &repo.path, "base": "main", "head": "feature",
            "title": "T", "body": "B",
        })),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap();
    let calls = read_calls(&argv_file);
    assert!(!calls[1].contains(&"--draft".to_string()));
}

// ---- gh.pr_wait -------------------------------------------------------------------

#[tokio::test]
async fn pr_wait_green() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_seq(
        &tmp,
        &[pr_json(
            "OPEN",
            "MERGEABLE",
            "abc123",
            "https://example.test/pr/3",
            json!([
                check("build", "COMPLETED", Some("SUCCESS")),
                context("ci/ok", "SUCCESS")
            ]),
        )],
    );
    let out = pr_wait(bin.as_path(), &tmp, "checks", &[]).await.unwrap();
    assert_eq!(get(&out, "state"), "green");
    assert_eq!(get(&out, "sha"), "abc123");
    assert_eq!(get(&out, "url"), "https://example.test/pr/3");
    assert_eq!(get(&out, "failed"), &json!([]));
    assert_eq!(
        read_calls(&argv_file),
        [vec![
            "pr",
            "view",
            "3",
            "--json",
            "state,mergeable,headRefOid,url,statusCheckRollup"
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>()]
    );
}

#[tokio::test]
async fn pr_wait_pending_then_green() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_seq(
        &tmp,
        &[
            pr_json(
                "OPEN",
                "MERGEABLE",
                "abc123",
                "https://example.test/pr/3",
                json!([
                    check("build", "IN_PROGRESS", None),
                    context("ci/ok", "PENDING"),
                ]),
            ),
            pr_json(
                "OPEN",
                "MERGEABLE",
                "abc123",
                "https://example.test/pr/3",
                json!([
                    check("build", "COMPLETED", Some("SUCCESS")),
                    context("ci/ok", "SUCCESS")
                ]),
            ),
        ],
    );
    let out = pr_wait(bin.as_path(), &tmp, "checks", &[]).await.unwrap();
    assert_eq!(get(&out, "state"), "green");
    assert_eq!(read_calls(&argv_file).len(), 2);
}

#[tokio::test]
async fn pr_wait_red() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_seq(
        &tmp,
        &[pr_json(
            "OPEN",
            "MERGEABLE",
            "abc123",
            "https://example.test/pr/3",
            json!([
                check("build", "COMPLETED", Some("FAILURE")),
                context("ci/x", "ERROR"),
                check("test", "COMPLETED", Some("SUCCESS")),
            ]),
        )],
    );
    let out = pr_wait(bin.as_path(), &tmp, "checks", &[]).await.unwrap();
    assert_eq!(get(&out, "state"), "red");
    assert_eq!(get(&out, "failed"), &json!(["build", "ci/x"]));
    assert!(argv_file.exists(), "real gh ran");
}

#[tokio::test]
async fn pr_wait_failure_while_pending_keeps_polling() {
    // a failed check while others still run is not yet red
    let tmp = tmp();
    let (bin, argv_file) = make_gh_seq(
        &tmp,
        &[
            pr_json(
                "OPEN",
                "MERGEABLE",
                "abc123",
                "https://example.test/pr/3",
                json!([
                    check("a", "COMPLETED", Some("FAILURE")),
                    check("b", "IN_PROGRESS", None),
                ]),
            ),
            pr_json(
                "OPEN",
                "MERGEABLE",
                "abc123",
                "https://example.test/pr/3",
                json!([
                    check("a", "COMPLETED", Some("TIMED_OUT")),
                    check("b", "COMPLETED", Some("SUCCESS")),
                ]),
            ),
        ],
    );
    let out = pr_wait(bin.as_path(), &tmp, "checks", &[]).await.unwrap();
    assert_eq!(get(&out, "state"), "red");
    assert_eq!(get(&out, "failed"), &json!(["a"]));
    assert_eq!(read_calls(&argv_file).len(), 2);
}

#[tokio::test]
async fn pr_wait_no_checks_is_green() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_seq(
        &tmp,
        &[pr_json(
            "OPEN",
            "MERGEABLE",
            "abc123",
            "https://example.test/pr/3",
            json!([]),
        )],
    );
    let out = pr_wait(bin.as_path(), &tmp, "checks", &[]).await.unwrap();
    assert_eq!(get(&out, "state"), "green");
    assert!(argv_file.exists(), "real gh ran");
}

#[tokio::test]
async fn pr_wait_terminal_states() {
    for (gh_state, mergeable, state) in [
        ("MERGED", "MERGEABLE", "merged"),
        ("CLOSED", "MERGEABLE", "closed"),
        ("OPEN", "CONFLICTING", "conflicting"),
    ] {
        for until in ["checks", "merged"] {
            let tmp = tmp();
            let (bin, argv_file) = make_gh_seq(
                &tmp,
                &[pr_json(
                    gh_state,
                    mergeable,
                    "abc123",
                    "https://example.test/pr/3",
                    json!([check("build", "COMPLETED", Some("SUCCESS"))]),
                )],
            );
            let out = pr_wait(bin.as_path(), &tmp, until, &[]).await.unwrap();
            assert_eq!(get(&out, "state"), &json!(state), "{gh_state}/{until}");
            assert!(argv_file.exists(), "real gh ran");
        }
    }
}

#[tokio::test]
async fn pr_wait_until_merged_waits_past_green() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_seq(
        &tmp,
        &[
            pr_json(
                "OPEN",
                "MERGEABLE",
                "abc123",
                "https://example.test/pr/3",
                json!([check("build", "COMPLETED", Some("SUCCESS"))]),
            ),
            pr_json(
                "MERGED",
                "MERGEABLE",
                "abc123",
                "https://example.test/pr/3",
                json!([check("build", "COMPLETED", Some("SUCCESS"))]),
            ),
        ],
    );
    let out = pr_wait(bin.as_path(), &tmp, "merged", &[]).await.unwrap();
    assert_eq!(get(&out, "state"), "merged");
    assert_eq!(read_calls(&argv_file).len(), 2);
}

#[tokio::test]
async fn pr_wait_until_merged_red_stops() {
    // red checks end a merged wait too, so the caller can act
    let tmp = tmp();
    let (bin, argv_file) = make_gh_seq(
        &tmp,
        &[pr_json(
            "OPEN",
            "MERGEABLE",
            "abc123",
            "https://example.test/pr/3",
            json!([check("build", "COMPLETED", Some("FAILURE"))]),
        )],
    );
    let out = pr_wait(bin.as_path(), &tmp, "merged", &[]).await.unwrap();
    assert_eq!(get(&out, "state"), "red");
    assert_eq!(get(&out, "failed"), &json!(["build"]));
    assert!(argv_file.exists(), "real gh ran");
}

#[tokio::test]
async fn pr_wait_timeout() {
    for until in ["checks", "merged"] {
        let tmp = tmp();
        // green-but-unmerged also keeps polling for `merged`
        let rollup = if until == "merged" {
            json!([check("build", "COMPLETED", Some("SUCCESS"))])
        } else {
            json!([check("build", "IN_PROGRESS", None)])
        };
        let (bin, argv_file) = make_gh_seq(
            &tmp,
            &[pr_json(
                "OPEN",
                "MERGEABLE",
                "abc123",
                "https://example.test/pr/3",
                rollup,
            )],
        );
        let out = pr_wait(bin.as_path(), &tmp, until, &[("timeout", json!(0))])
            .await
            .unwrap();
        assert_eq!(get(&out, "state"), "timeout");
        assert_eq!(get(&out, "sha"), "abc123"); // the last sha seen
        assert_eq!(read_calls(&argv_file).len(), 1);
    }
}

#[tokio::test]
async fn pr_wait_gh_failure_is_transient() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_failing(&tmp, "gh: network unreachable");
    let err = pr_wait(bin.as_path(), &tmp, "checks", &[])
        .await
        .unwrap_err();
    // the helper retried before giving up: the poll failure is Transient
    assert!(err.is_transient());
    assert!(
        err.to_string()
            .contains("gh pr view failed: gh exited 1: gh: network unreachable"),
        "{err}"
    );
    assert!(argv_file.exists(), "real gh ran");
}

// ---- gh.run_latest / gh.run_cancel -------------------------------------------------

#[tokio::test]
async fn run_latest_success() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_runs(&tmp, &run_json(), "{}", 0, "");
    let out = gh::run_latest(
        &inputs(json!({"path": tmp.path()})),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "run_id"), &json!(991));
    assert_eq!(get(&out, "sha"), "feed42");
    assert_eq!(get(&out, "status"), "completed");
    assert_eq!(get(&out, "conclusion"), "success");
    assert_eq!(get(&out, "url"), "https://example.test/run/991");
    assert_eq!(get(&out, "workflow"), "CI");
    assert_eq!(get(&out, "failed_jobs"), &json!([]));
    assert_eq!(
        read_calls(&argv_file),
        [vec![
            "run",
            "list",
            "--branch",
            "main",
            "--limit",
            "1",
            "--json",
            "databaseId,headSha,status,conclusion,url,workflowName"
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>()]
    );
}

#[tokio::test]
async fn run_latest_branch_and_workflow() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_runs(&tmp, &run_json(), "{}", 0, "");
    gh::run_latest(
        &inputs(json!({"path": tmp.path(), "branch": "dev", "workflow": "ci.yml"})),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap();
    let calls = read_calls(&argv_file);
    assert_eq!(
        calls[0][..4],
        ["run", "list", "--branch", "dev"].map(String::from)
    );
    assert_eq!(calls[0][4..6], ["--workflow", "ci.yml"].map(String::from));
}

#[tokio::test]
async fn run_latest_failed_jobs() {
    let tmp = tmp();
    let jobs = json!({"jobs": [
        {"name": "build", "conclusion": "failure"},
        {"name": "lint", "conclusion": "success"},
        {"name": "docs", "conclusion": "skipped"},
        {"name": "mac", "conclusion": "cancelled"},
        {"name": "slow", "conclusion": "timed_out"},
    ]});
    let list = json!([{
        "databaseId": 991, "headSha": "feed42", "status": "completed",
        "conclusion": "failure", "url": "https://example.test/run/991",
        "workflowName": "CI",
    }])
    .to_string();
    let (bin, argv_file) = make_gh_runs(&tmp, &list, &jobs.to_string(), 0, "");
    let out = gh::run_latest(
        &inputs(json!({"path": tmp.path()})),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "conclusion"), "failure");
    assert_eq!(get(&out, "failed_jobs"), &json!(["build", "mac", "slow"]));
    let calls = read_calls(&argv_file);
    assert_eq!(calls[1], ["run", "view", "991", "--json", "jobs"]);
}

#[tokio::test]
async fn run_latest_no_runs() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_runs(&tmp, "[]", "{}", 0, "");
    let err = gh::run_latest(
        &inputs(json!({"path": tmp.path(), "branch": "dev"})),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap_err();
    assert!(!err.is_transient());
    assert!(err.to_string().contains("no runs on dev"), "{err}");
    assert!(argv_file.exists(), "real gh ran");
}

#[tokio::test]
async fn run_cancel() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_runs(&tmp, "[]", "{}", 0, "");
    let out = gh::run_cancel(
        &inputs(json!({"path": tmp.path(), "run_id": 991})),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "cancelled"), &json!(true));
    assert_eq!(
        read_calls(&argv_file),
        [vec!["run", "cancel", "991"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()]
    );
}

#[tokio::test]
async fn run_cancel_already_completed() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_runs(
        &tmp,
        "[]",
        "{}",
        1,
        "cannot cancel a workflow run that is completed",
    );
    let out = gh::run_cancel(
        &inputs(json!({"path": tmp.path(), "run_id": 991})),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap();
    assert_eq!(get(&out, "cancelled"), &json!(false));
    assert!(argv_file.exists(), "real gh ran");
}

#[tokio::test]
async fn run_cancel_other_failure() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_runs(&tmp, "[]", "{}", 1, "HTTP 404: not found");
    let err = gh::run_cancel(
        &inputs(json!({"path": tmp.path(), "run_id": 991})),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap_err();
    assert!(!err.is_transient());
    assert!(argv_file.exists(), "real gh ran");
}

// ---- a plan-supplied ref that looks like an option is refused before the tool runs ---

#[tokio::test]
async fn git_fns_refuse_refs_that_look_like_options() {
    let cases: [(&str, Value); 7] = [
        (
            "push",
            json!({"path": ".", "branch": "--upload-pack=touch-pwned"}),
        ),
        (
            "push",
            json!({"path": ".", "branch": "main", "remote": "--all"}),
        ),
        ("rebase", json!({"path": ".", "onto": "--exec=touch-pwned"})),
        (
            "merge",
            json!({"repo": ".", "source": "-m pwned", "target": "main"}),
        ),
        (
            "merge",
            json!({"repo": ".", "source": "feat", "target": "--detach"}),
        ),
        (
            "worktree",
            json!({"repo": ".", "base": "main", "branch": "-b"}),
        ),
        (
            "worktree",
            json!({"repo": ".", "base": "--orphan", "branch": "feat"}),
        ),
    ];
    for (name, inp) in cases {
        let tmp = tmp();
        let (bin, argv_file) = make_git(&tmp);
        let ctx = ctx(&run_dir(&tmp), Some(bin.as_path()));
        let err = git::dispatch(&format!("git.{name}"), &inputs(inp), &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("may not start with '-'"),
            "{name}: {err}"
        );
        assert!(!argv_file.exists(), "{name}: git ran");
    }
}

#[tokio::test]
async fn gh_fns_refuse_refs_that_look_like_options() {
    let cases: [(&str, Value); 4] = [
        (
            "pr",
            json!({"path": ".", "base": "main", "head": "--repo=o/r",
                   "title": "t", "body": "b"}),
        ),
        (
            "pr",
            json!({"path": ".", "base": "--repo=o/r", "head": "feat",
                   "title": "t", "body": "b"}),
        ),
        (
            "pr_wait",
            json!({"path": ".", "pr": "--repo=o/r", "until": "checks", "timeout": 0}),
        ),
        ("run_latest", json!({"path": ".", "branch": "--limit"})),
    ];
    for (name, inp) in cases {
        let tmp = tmp();
        let (bin, argv_file) = make_gh(&tmp, "[]", VIEW_JSON);
        let ctx = ctx(&run_dir(&tmp), Some(bin.as_path()));
        let err = gh::dispatch(&format!("gh.{name}"), &inputs(inp), &ctx)
            .await
            .unwrap_err();
        assert!(!err.is_transient(), "{name}: {err}");
        assert!(
            err.to_string().contains("may not start with '-'"),
            "{name}: {err}"
        );
        assert!(!argv_file.exists(), "{name}: gh ran");
    }
}

#[tokio::test]
async fn run_cancel_refuses_a_negative_run_id() {
    let tmp = tmp();
    let (bin, argv_file) = make_gh_runs(&tmp, "[]", "{}", 0, "");
    let err = gh::run_cancel(
        &inputs(json!({"path": tmp.path(), "run_id": -1})),
        &ctx(&run_dir(&tmp), Some(bin.as_path())),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("may not start with '-'"), "{err}");
    assert!(!argv_file.exists()); // gh never ran
}

// ---- beyond the Python suite --------------------------------------------------------

/// Cancellation is explicit: dropping the wait future kills the in-flight gh child
/// (kill_on_drop), which stops the fake's heartbeat writes.
#[tokio::test]
async fn cancelling_the_wait_kills_the_in_flight_gh() {
    let tmp = tmp();
    let heartbeat = tmp.path().join("hb");
    let script = format!(
        "#!/bin/sh\nwhile :; do echo tick >> \"{}\"; sleep 0.05; done\n",
        heartbeat.display()
    );
    let bin = fake_bin(&tmp, "gh", &script);
    let ctx = ctx(&run_dir(&tmp), Some(bin.as_path()));
    let task = tokio::spawn(async move {
        gh::pr_wait(
            &inputs(json!({"path": ".", "pr": "3", "until": "checks", "interval": 1})),
            &ctx,
        )
        .await
    });
    for _ in 0..200 {
        if heartbeat.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(heartbeat.exists(), "fake gh never started");
    task.abort();
    let _ = task.await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let snapshot = fs::read_to_string(&heartbeat).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        fs::read_to_string(&heartbeat).unwrap(),
        snapshot,
        "gh child survived cancellation"
    );
}

#[tokio::test]
async fn dispatch_routes_by_name_and_rejects_unknown_names() {
    let tmp = tmp();
    let ctx = ctx(&run_dir(&tmp), None);
    let err = git::dispatch("git.bogus", &inputs(json!({})), &ctx)
        .await
        .unwrap_err();
    assert!(!err.is_transient());
    assert!(err.to_string().contains("git.bogus"));
    let err = gh::dispatch("gh.bogus", &inputs(json!({})), &ctx)
        .await
        .unwrap_err();
    assert!(!err.is_transient());
    assert!(err.to_string().contains("gh.bogus"));
}

#[test]
fn descriptors_match_the_pack_fn_json() {
    let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/git");
    let mut names = Vec::new();
    let mut retries = Vec::new();
    for descriptor in git::descriptors().into_iter().chain(gh::descriptors()) {
        names.push(descriptor.name);
        retries.push((descriptor.name, descriptor.retries));
        let manifest: Value = serde_json::from_str(
            &fs::read_to_string(pack.join(descriptor.name).join("fn.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["name"], descriptor.name);
        assert_eq!(manifest["doc"], descriptor.doc);
        for (declared, key) in [
            (&descriptor.inputs, "inputs"),
            (&descriptor.outputs, "outputs"),
        ] {
            let fields = manifest[key].as_object().unwrap();
            assert_eq!(
                declared
                    .iter()
                    .map(|(name, ty)| (*name, ty.form()))
                    .collect::<Vec<_>>(),
                fields
                    .iter()
                    .map(|(name, ty)| (name.as_str(), ty.clone()))
                    .collect::<Vec<_>>(),
                "{}.{key}",
                descriptor.name
            );
        }
    }
    assert_eq!(
        names,
        [
            "git.worktree",
            "git.worktree_rm",
            "git.head",
            "git.merge",
            "git.rebase",
            "git.push",
            "gh.pr",
            "gh.pr_wait",
            "gh.run_cancel",
            "gh.run_latest",
        ]
    );
    // Only gh.pr_wait declares run(main, retries=3); everything else is run(main).
    assert_eq!(
        retries,
        [
            ("git.worktree", 0),
            ("git.worktree_rm", 0),
            ("git.head", 0),
            ("git.merge", 0),
            ("git.rebase", 0),
            ("git.push", 0),
            ("gh.pr", 0),
            ("gh.pr_wait", 3),
            ("gh.run_cancel", 0),
            ("gh.run_latest", 0),
        ]
        .into_iter()
        .collect::<Vec<_>>()
    );
}
