//! The schema cutover's tooling (docs/design/plan-rows.md §10): `scripts/deploy
//! --schema-cutover`, `scripts/compat-check --incompatible --copy`, `scripts/cutover-rehearse`,
//! `scripts/ship` and `scripts/deploy` refusing an incompatible change, and the release
//! manifest's `schema`. Every installation here is a scratch one in test mode: a scratch home,
//! a scratch installation directory and prefix, plain child services, `sluice-test-*` units.
//!
//! The cutover needs two releases: the old one the home runs (schema 1) and the candidate. The
//! old release is the last schema-1 ancestor with the legacy compiler on the first-parent
//! history, built once into the target directory, and the candidate is this build.
#[path = "../../../tests/support/executable.rs"]
mod executable;
#[allow(dead_code)]
#[path = "fixtures/install_support.rs"]
mod support;
use serde_json::{Value, json};
use sluice::{install::Installation, release::Manifest};
use sluice_model::plan_rows::{CutoverReport, RetryAdvice, StopRequest};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const SCHEMA: i64 = sluice_store::schema::SCHEMA_VERSION;
/// The schema the cutover moves to.
const TARGET: u32 = 3;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn target() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .unwrap()
        .to_path_buf()
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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
fn text(output: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
fn wait_for(what: &str, limit: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !predicate() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The schema-1 binary the home runs before the cutover.
fn old_revision() -> &'static str {
    static REVISION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    REVISION.get_or_init(|| {
        let repo = repo();
        git(&repo, &["rev-list", "--first-parent", "HEAD"])
            .lines()
            .find(|sha| {
                git(
                    &repo,
                    &["show", &format!("{sha}:crates/sluice-store/src/schema.rs")],
                )
                .contains("pub const SCHEMA_VERSION: i64 = 1;")
                    && !git(&repo, &["ls-tree", sha, "crates/sluice-model/src/plan.rs"]).is_empty()
            })
            .expect("cutover tests need a schema-1 ancestor with the legacy compiler")
            .to_owned()
    })
}

fn old_binary() -> PathBuf {
    let repo = repo();
    let sha = old_revision();
    let cache = target().join("cutover-old").join(sha);
    std::fs::create_dir_all(&cache).unwrap();
    // One build per schema-1 revision, whichever test asks first.
    let lock = std::fs::File::create(cache.join("lock")).unwrap();
    lock.lock().unwrap();
    // Where cargo put it, as it reports: a cargo wrapper may override the target directory.
    let path = cache.join("binary");
    if let Ok(binary) = std::fs::read_to_string(&path).map(PathBuf::from)
        && binary.is_file()
    {
        return binary;
    }
    let source = cache.join("src");
    let _ = std::fs::remove_dir_all(&source);
    std::fs::create_dir_all(&source).unwrap();
    let mut archive = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["archive", sha])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let status = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(&source)
        .stdin(archive.stdout.take().unwrap())
        .status()
        .unwrap();
    assert!(
        status.success() && archive.wait().unwrap().success(),
        "unpacking {sha}"
    );
    let built = Command::new("cargo")
        .current_dir(&source)
        .arg("--config")
        .arg(format!(
            "build.target-dir={}",
            serde_json::to_string(&cache.join("target")).unwrap()
        ))
        .args([
            "build",
            "-p",
            "sluice",
            "--bin",
            "sluice",
            "--locked",
            "--message-format=json-render-diagnostics",
        ])
        .output()
        .unwrap();
    assert!(built.status.success(), "building {sha}: {}", text(&built));
    let binary = String::from_utf8_lossy(&built.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["target"]["name"] == "sluice")
        .find_map(|message| message["executable"].as_str().map(PathBuf::from))
        .unwrap_or_else(|| panic!("building {sha} names no sluice executable"));
    std::fs::write(&path, binary.to_string_lossy().as_bytes()).unwrap();
    binary
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "__pycache__" {
            continue;
        }
        let path = entry.path();
        if std::fs::metadata(&path).unwrap().is_dir() {
            copy_tree(&path, &to.join(&name));
        } else {
            std::fs::copy(&path, to.join(&name)).unwrap();
        }
    }
}
fn files(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(root, &path, out);
        } else {
            out.insert(
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                sha256(&path),
            );
        }
    }
}
/// A file's SHA-256 in hex, as `sluice_store::artifacts::fingerprint` gives it (by coreutils:
/// an unoptimized test build hashes a debug binary slowly).
fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum").arg(path).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()[..64].to_owned()
}
/// A release as `scripts/build-release` lays it out (the binary, the fn helper, the private
/// tmux and a manifest that verifies), around an already built binary.
fn release(root: &Path, binary: &Path, git_sha: &str, schema: u32) -> PathBuf {
    let stage = tempfile::tempdir_in(root).unwrap().keep();
    std::fs::create_dir_all(stage.join("bin")).unwrap();
    if std::fs::hard_link(binary, stage.join("bin/sluice")).is_err() {
        std::fs::copy(binary, stage.join("bin/sluice")).unwrap();
    }
    copy_tree(&repo().join("python"), &stage.join("python"));
    let tmux = repo().join("target/private-tmux");
    assert!(
        tmux.join("tmux-manifest.json").is_file(),
        "run scripts/build-private-tmux once per checkout"
    );
    copy_tree(&tmux, &stage.join("tmux"));
    let mut digests = BTreeMap::new();
    files(&stage, &stage, &mut digests);
    let release_id = format!(
        "{git_sha}-{}",
        sluice_store::artifacts::fingerprint(&serde_json::to_vec(&digests).unwrap())
    );
    let manifest = Manifest {
        release_id: release_id.clone(),
        git_sha: git_sha.into(),
        guardian_protocol_major: 1,
        guardian_protocol_minor: 0,
        files: digests,
        private_tmux: serde_json::from_slice(
            &std::fs::read(stage.join("tmux/tmux-manifest.json")).unwrap(),
        )
        .unwrap(),
        build_toolchain: BTreeMap::from([("rustc".into(), "fixture".into())]),
        schema,
    };
    std::fs::write(
        stage.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let named = root.join(&release_id);
    std::fs::rename(&stage, &named).unwrap();
    named
}

struct Releases {
    _root: tempfile::TempDir,
    old: PathBuf,
    candidate: PathBuf,
}
impl Releases {
    fn new() -> Self {
        let root = tempfile::tempdir_in(target().join("tmp")).unwrap();
        let repo = repo();
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let old = release(root.path(), &old_binary(), old_revision(), 1);
        let candidate = release(
            root.path(),
            Path::new(env!("CARGO_BIN_EXE_sluice")),
            &head,
            TARGET,
        );
        Self {
            _root: root,
            old,
            candidate,
        }
    }
}

/// A scratch installation whose selected old release serves a scratch home, its coordinator
/// and loop plain children registered in services.json as a fixture deploy records them.
struct Scratch {
    gate: support::Gate,
    releases: Releases,
    prefix: PathBuf,
    port: u16,
    /// More environment for the old services (and so the runs they launch).
    env: Vec<(String, String)>,
}
impl Scratch {
    fn new() -> Self {
        Self::with_env(|_| Vec::new())
    }
    fn with_env(env: impl FnOnce(&Path) -> Vec<(String, String)>) -> Self {
        let gate = support::Gate::new();
        let env = env(gate.root.path());
        let releases = Releases::new();
        let install = Installation::at(gate.install.clone()).unwrap();
        install.fence("first install".into()).unwrap();
        install.select(&releases.old, &gate.home).unwrap();
        install.unfence().unwrap();
        let prefix = gate.root.path().join("prefix");
        std::fs::create_dir_all(&prefix).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut scratch = Self {
            gate,
            releases,
            prefix,
            port,
            env,
        };
        scratch.start_services(true);
        scratch
    }
    fn old(&self) -> PathBuf {
        self.releases.old.join("bin/sluice")
    }
    /// The old coordinator and loop, with or without the fixture fns in their catalog.
    fn start_services(&mut self, fixtures: bool) {
        let old = self.old();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.gate.root.path().join("coordinator.log"))
            .unwrap();
        let mut coordinator = self.gate.command(&old, &["coordinator"]);
        coordinator.envs(self.env.iter().map(|(k, v)| (k, v)));
        if !fixtures {
            coordinator.env_remove("SLUICE_FIXTURE");
        }
        let child = coordinator
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap();
        self.gate.children.push(child);
        let home = self.gate.home.clone();
        wait_for("the old coordinator", Duration::from_secs(60), || {
            std::os::unix::net::UnixStream::connect(home.join("coordinator.sock")).is_ok()
        });
        let child = self
            .gate
            .command(&old, &["loop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.gate.children.push(child);
        let mut services = serde_json::Map::new();
        let n = self.gate.children.len();
        for (name, child) in ["coordinator", "loop"]
            .iter()
            .zip(&self.gate.children[n - 2..])
        {
            services.insert(
                (*name).into(),
                json!({"pid": child.id(), "start": start_time(child.id()), "release": self.releases.old}),
            );
        }
        std::fs::write(
            self.gate.install.join("services.json"),
            serde_json::to_vec(&services).unwrap(),
        )
        .unwrap();
    }
    fn stop_services(&mut self) {
        self.gate.stop();
    }
    /// `sluice tool <name> <args>` with the old release, as the orchestrator calls it.
    fn tool(&self, name: &str, args: Value) -> Value {
        let output = self
            .gate
            .command(&self.old(), &["tool", name, &args.to_string()])
            .env("SLUICE_AUTHOR", "orchestrator")
            .output()
            .unwrap();
        assert!(output.status.success(), "{name}: {}", text(&output));
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn db<T>(&self, read: impl FnOnce(&rusqlite::Connection) -> T) -> T {
        let db = rusqlite::Connection::open_with_flags(
            self.gate.home.join("sluice.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        read(&db)
    }
    fn value(&self, sql: &str) -> Option<String> {
        use rusqlite::types::Value as Sql;
        self.db(|db| match db.query_row(sql, [], |r| r.get::<_, Sql>(0)) {
            Ok(Sql::Text(text)) => Some(text),
            Ok(Sql::Integer(n)) => Some(n.to_string()),
            Ok(Sql::Real(n)) => Some(n.to_string()),
            _ => None,
        })
    }
    /// A project `p` whose steps run the given fns, each started at once; returns when each
    /// `fixture.wait` step has a live run and every other step has succeeded.
    fn project(&self, steps: &[(&str, &str)]) {
        self.tool("project_create", json!({"name": "p"}));
        let ops: Vec<Value> = steps
            .iter()
            .map(|(id, run)| json!({"op":"add","path":format!("/steps/{id}"),"value":{"run":run,"in":{"value":{"default":1}}}}))
            .collect();
        self.tool(
            "plan_patch",
            json!({"project":"p","rev":1,"reason":"cutover fixture","ops":ops}),
        );
        for (id, run) in steps {
            if *run == "fixture.wait" {
                self.live_run(&format!("step_id='{id}'"));
            } else {
                wait_for(id, Duration::from_secs(60), || {
                    self.value(&format!("SELECT status FROM steps WHERE step_id='{id}'"))
                        .as_deref()
                        == Some("succeeded")
                });
            }
        }
    }
    /// The run id of the one run matching `filter`, once it is executing.
    fn live_run(&self, filter: &str) -> String {
        let sql = format!(
            "SELECT r.run_id FROM runs r JOIN attempts a USING(attempt_id) WHERE r.{filter} AND a.phase='executing' AND r.unit_name IS NOT NULL"
        );
        let mut run = None;
        wait_for(filter, Duration::from_secs(60), || {
            run = self.value(&sql);
            run.is_some()
        });
        run.unwrap()
    }
    fn rehearsal(&self) -> PathBuf {
        // A passed rehearsal for this candidate over this old release.
        let path = self.gate.root.path().join("rehearsal.json");
        std::fs::write(
            &path,
            json!({"ok": true, "candidate": self.releases.candidate.file_name().unwrap().to_string_lossy(),
                   "old": self.releases.old.file_name().unwrap().to_string_lossy()})
            .to_string(),
        )
        .unwrap();
        path
    }
    fn cutover(&self, extra: &[&str]) -> Output {
        let mut command = self
            .gate
            .command(&repo().join("scripts/deploy"), &["--prefix"]);
        command.arg(&self.prefix).arg("--schema-cutover");
        if !extra.contains(&"--resume-checks") {
            command
                .arg("--candidate")
                .arg(&self.releases.candidate)
                .arg("--rehearsal")
                .arg(self.rehearsal());
        }
        command
            .args(extra)
            .env("SLUICE_DEPLOY_PROCESS_FIXTURE", "1")
            .env("SLUICE_DEPLOY_PORT", self.port.to_string())
            .env("SLUICE_DEPLOY_READY_SECONDS", "120")
            .output()
            .unwrap()
    }
    fn report(&self) -> CutoverReport {
        let reports: Vec<_> = std::fs::read_dir(&self.gate.install)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let name = p.file_name().unwrap().to_string_lossy();
                name.starts_with("cutover-") && name.ends_with(".json")
            })
            .collect();
        assert_eq!(reports.len(), 1, "{reports:?}");
        serde_json::from_slice(&std::fs::read(&reports[0]).unwrap()).unwrap()
    }
    fn deploy_log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.gate.install.join("deploy.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn fence(&self) -> Option<String> {
        Installation::at(self.gate.install.clone())
            .unwrap()
            .status()
            .unwrap()
            .fence
            .map(|f| f.reason)
    }
    fn unit_active(&self, run: &str) -> bool {
        Command::new("systemctl")
            .args(["--user", "is-active", "--quiet"])
            .arg(format!("sluice-test-{run}.service"))
            .status()
            .unwrap()
            .success()
    }
    fn assert_scheduler_ready(&self) {
        let services: Value = serde_json::from_slice(
            &std::fs::read(self.gate.install.join("services.json")).unwrap(),
        )
        .unwrap();
        for name in ["coordinator", "serve", "loop"] {
            let record = &services[name];
            let pid = record["pid"].as_u64().unwrap();
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .unwrap_or_else(|error| panic!("{name} is absent: {error}"));
            let fields: Vec<_> = stat
                .rsplit(')')
                .next()
                .unwrap()
                .split_whitespace()
                .collect();
            assert_eq!(Some(fields[19]), record["start"].as_str(), "{name}");
            assert_ne!(fields[0], "Z", "{name} exited");
        }
        assert!(
            self.value("SELECT scheduler_owner FROM maintenance WHERE singleton=1")
                .is_some(),
            "the loop must hold the scheduler lease when deploy returns"
        );
    }
}
fn start_time(pid: u32) -> String {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    stat.rsplit(')')
        .next()
        .unwrap()
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_owned()
}

// ---- the release manifest's schema --------------------------------------------------------

#[test]
fn home_schema_is_the_store_schema_and_an_older_manifest_reads_as_schema_1() {
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sluice"))
        .args(["home", "schema"])
        .env("SLUICE_HOME", home.path().join("never-created"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply, json!({"schema": SCHEMA}));
    assert!(!home.path().join("never-created").exists());
    // A manifest written before the field existed is schema 1; a new one round-trips it.
    let root = tempfile::tempdir_in(target().join("tmp")).unwrap();
    let built = release(
        root.path(),
        Path::new(env!("CARGO_BIN_EXE_sluice")),
        "f00d",
        TARGET,
    );
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(built.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["schema"], TARGET);
    manifest.as_object_mut().unwrap().remove("schema");
    std::fs::write(built.join("manifest.json"), manifest.to_string()).unwrap();
    assert_eq!(sluice::release::verify(&built).unwrap().schema, 1);
}

// ---- ship and deploy refuse an incompatible change -----------------------------------------

/// A scratch repository holding `scripts` and a `schema.rs` at `schema`, committed.
fn scratch_repo(root: &Path, schema: i64) -> PathBuf {
    let dir = root.join("repo");
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::create_dir_all(dir.join("crates/sluice-store/src")).unwrap();
    for script in ["ship", "deploy", "build-release", "compat-check"] {
        std::fs::copy(
            repo().join("scripts").join(script),
            dir.join("scripts").join(script),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join("crates/sluice-store/src/schema.rs"),
        format!("pub const SCHEMA_VERSION: i64 = {schema};\n"),
    )
    .unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "fixture"]);
    dir
}
/// A scratch installation selecting a home this build created (at SCHEMA).
fn selected_home(gate: &support::Gate) -> PathBuf {
    assert!(gate.cli(&["query", "SELECT 1"]).status.success());
    let prefix = gate.root.path().join("prefix");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_sluice"), prefix.join("bin/sluice")).unwrap();
    let release = gate.root.path().join("python-release");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_sluice"), release.join("bin/sluice")).unwrap();
    let install = Installation::at(gate.install.clone()).unwrap();
    install.fence("first install".into()).unwrap();
    install.select(&release, &gate.home).unwrap();
    install.unfence().unwrap();
    prefix
}

#[test]
fn ship_refuses_a_ref_whose_schema_differs_from_the_home_before_anything_moves() {
    let gate = support::Gate::new();
    let prefix = selected_home(&gate);
    let source = scratch_repo(gate.root.path(), SCHEMA + 2);
    let origin = gate.root.path().join("origin.git");
    git(
        gate.root.path(),
        &["init", "-q", "--bare", origin.to_str().unwrap()],
    );
    git(
        &source,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    for dry in [true, false] {
        let mut command = gate.command(&source.join("scripts/ship"), &["--prefix"]);
        command.arg(&prefix).current_dir(&source);
        if dry {
            command.arg("--dry-run");
        }
        // No live launcher on PATH: the scratch prefix's is the one ship asks.
        command.env("PATH", "/usr/bin:/bin");
        let output = command.output().unwrap();
        assert!(!output.status.success(), "{}", text(&output));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "is schema {} and the home is schema {SCHEMA}",
                SCHEMA + 2
            )) && stderr.contains("--schema-cutover"),
            "{}",
            text(&output)
        );
    }
    // Nothing was pushed.
    assert_eq!(
        git(
            gate.root.path(),
            &["--git-dir", origin.to_str().unwrap(), "for-each-ref"]
        ),
        ""
    );
}

#[test]
fn deploy_refuses_an_incompatible_commit_before_building_it() {
    let gate = support::Gate::new();
    let prefix = selected_home(&gate);
    let source = scratch_repo(gate.root.path(), SCHEMA + 2);
    let output = gate
        .command(&source.join("scripts/deploy"), &["HEAD", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    assert!(!output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--schema-cutover") && stderr.contains("is schema"),
        "{}",
        text(&output)
    );
    // Refused before the build: no release, no fence, the old selection.
    assert!(!prefix.join("releases").exists());
    let status = Installation::at(gate.install.clone())
        .unwrap()
        .status()
        .unwrap();
    assert!(status.fence.is_none());
    // A compatible commit is not refused for its schema (it fails later, at the build of
    // this fixture's empty tree).
    let compatible = scratch_repo(&gate.root.path().join("same"), SCHEMA);
    let output = gate
        .command(&compatible.join("scripts/deploy"), &["HEAD", "--prefix"])
        .arg(&prefix)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("--schema-cutover"),
        "{}",
        text(&output)
    );
}

// ---- deploy --schema-cutover ---------------------------------------------------------------

#[test]
fn cutover_dry_run_prints_the_notice_and_what_the_deadline_would_stop_and_changes_nothing() {
    let scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    let run = scratch.live_run("step_id='w'");
    let before = std::fs::read(scratch.gate.install.join("services.json")).unwrap();
    let output = scratch.cutover(&["--deadline", "+60m", "--dry-run"]);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("sluice cutover to schema {TARGET} at "))
            && stdout.contains("new work is refused from now")
            && stdout.contains("retry it after the cutover"),
        "{stdout}"
    );
    let line = stdout
        .lines()
        .find(|l| l.contains(&run))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(
        line.contains("p w ") && line.contains("settle=no") && line.contains("step_cancel"),
        "{line}"
    );
    assert!(
        line.contains(&scratch.releases.old.file_name().unwrap().to_string_lossy()[..12]),
        "{line}"
    );
    // Nothing changed: no fence, no drain, the run still live, the same services.
    assert_eq!(scratch.fence(), None);
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("normal")
    );
    assert_eq!(
        scratch.value(&format!(
            "SELECT finished_at FROM runs WHERE run_id='{run}'"
        )),
        None
    );
    assert!(scratch.unit_active(&run));
    assert_eq!(
        std::fs::read(scratch.gate.install.join("services.json")).unwrap(),
        before
    );
    assert!(scratch.deploy_log().is_empty());
}

#[test]
fn cutover_takes_only_a_passed_rehearsal_of_this_candidate_over_the_selected_release() {
    let scratch = Scratch::new();
    let candidate = scratch
        .releases
        .candidate
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let old = scratch
        .releases
        .old
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    for (rehearsal, refusal) in [
        (
            json!({"ok": false, "candidate": candidate, "old": old, "failure": "1 cancel refused"}),
            "is a failed rehearsal: 1 cancel refused",
        ),
        (
            json!({"ok": true, "candidate": "another", "old": old}),
            "rehearsed candidate another",
        ),
    ] {
        let path = scratch.gate.root.path().join("other-rehearsal.json");
        std::fs::write(&path, rehearsal.to_string()).unwrap();
        let output = scratch
            .gate
            .command(&repo().join("scripts/deploy"), &["--prefix"])
            .arg(&scratch.prefix)
            .args(["--schema-cutover", "--deadline", "+0s", "--candidate"])
            .arg(&scratch.releases.candidate)
            .arg("--rehearsal")
            .arg(&path)
            .env("SLUICE_DEPLOY_PROCESS_FIXTURE", "1")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{}", text(&output));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(refusal),
            "{}",
            text(&output)
        );
    }
    // Refused before the notice: nothing drained, nothing fenced.
    assert_eq!(scratch.fence(), None);
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("normal")
    );
    // A candidate of the home's own schema is no cutover, and --skip-compat has no place in one.
    let output = scratch
        .gate
        .command(&repo().join("scripts/deploy"), &["--prefix"])
        .arg(&scratch.prefix)
        .args(["--schema-cutover", "--deadline", "+0s", "--candidate"])
        .arg(&scratch.releases.old)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a schema change"),
        "{}",
        text(&output)
    );
    let output = scratch
        .gate
        .command(&repo().join("scripts/deploy"), &["--prefix"])
        .arg(&scratch.prefix)
        .args([
            "--schema-cutover",
            "--deadline",
            "+0s",
            "--skip-compat",
            "why",
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--skip-compat does not apply"),
        "{}",
        text(&output)
    );
}

#[test]
fn cutover_with_catalog_drift_stops_fenced_with_the_refusal_named_and_stops_no_unit() {
    let mut scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    let run = scratch.live_run("step_id='w'");
    let attempt = scratch
        .value(&format!("SELECT attempt_id FROM runs WHERE run_id='{run}'"))
        .unwrap();
    // The catalog drifts under the running step: the old coordinator comes back without the
    // fixture fns, so the old release's step_cancel, which compiles the plan, refuses.
    scratch.stop_services();
    scratch.start_services(false);
    let output = scratch.cutover(&[
        "--deadline",
        "+0s",
        "--cancel-grace",
        "2",
        "--settle-timeout",
        "5",
    ]);
    assert!(!output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cutover stopped fenced: 1 cancels refused"),
        "{}",
        text(&output)
    );
    let report = scratch.report();
    assert!(report.stopped.is_empty(), "{report:?}");
    assert_eq!(report.refused.len(), 1, "{report:?}");
    let refused = serde_json::to_value(&report.refused[0]).unwrap();
    assert_eq!(refused["project"], "p");
    assert_eq!(refused["step"], "w");
    assert_eq!(refused["runs"], json!([run]));
    assert_eq!(refused["attempts"], json!([attempt]));
    assert_eq!(refused["error"]["error"], "invalid");
    assert_eq!(refused["error"]["message"], "invalid stored plan");
    assert!(
        refused["error"]["errors"][0]
            .as_str()
            .unwrap()
            .contains("unknown fn"),
        "{refused}"
    );
    assert_eq!(
        report.reason,
        format!(
            "schema-{TARGET} cutover at {}: stopped at the deadline; retry it after the cutover",
            report.deadline
        )
    );
    // Stopped still fenced and drained, the run untouched and its unit still active.
    assert_eq!(scratch.fence(), Some(format!("schema-{TARGET} cutover")));
    assert_eq!(
        scratch
            .value("SELECT mode || ' ' || owner FROM maintenance")
            .as_deref(),
        Some("drain cutover")
    );
    assert_eq!(
        scratch.value(&format!(
            "SELECT finished_at FROM runs WHERE run_id='{run}'"
        )),
        None
    );
    assert_eq!(
        scratch.value(&format!(
            "SELECT cancel_requested FROM attempts WHERE attempt_id='{attempt}'"
        )),
        Some("0".into())
    );
    assert!(scratch.unit_active(&run));
    assert!(
        scratch
            .deploy_log()
            .iter()
            .any(|l| l["cutover"] == "cutover stopped fenced: 1 cancels refused"),
        "{:?}",
        scratch.deploy_log()
    );
}

#[test]
fn cutover_cancels_the_running_steps_stops_the_calls_settles_and_reports_how_each_ended() {
    let scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    let step_run = scratch.live_run("step_id='w'");
    let call: Value = scratch.tool(
        "fn_call",
        json!({"name": "fixture.wait", "inputs": {"value": 2}, "project": "p"}),
    );
    let call = call["call"].as_str().unwrap().to_owned();
    let call_run = scratch.live_run(&format!("run_id='{call}'"));
    let output = scratch.cutover(&[
        "--deadline",
        "+0s",
        "--cancel-grace",
        "3",
        "--settle-timeout",
        "120",
    ]);
    eprintln!("{}", text(&output));
    // Whatever the candidate does next, the report already says how each run ended.
    let report = scratch.report();
    assert_eq!(report.schema, TARGET);
    assert!(report.refused.is_empty(), "{report:?}");
    assert_eq!(report.stopped.len(), 2, "{report:?}");
    let step = report
        .stopped
        .iter()
        .find(|r| r.run.to_string() == step_run)
        .unwrap();
    assert_eq!(step.requested, StopRequest::Cancel);
    assert_eq!(step.project.as_deref(), Some("p"));
    assert_eq!(
        step.step.as_ref().map(|s| s.to_string()).as_deref(),
        Some("w")
    );
    assert_eq!(
        serde_json::to_value(&step.outcome).unwrap(),
        json!({"status":"failed","error":"cancelled"})
    );
    assert_eq!(step.step_error.as_deref(), Some("cancelled"));
    assert_eq!(step.advice, RetryAdvice::Retry);
    let stopped_call = report
        .stopped
        .iter()
        .find(|r| r.run.to_string() == call_run)
        .unwrap();
    assert_eq!(stopped_call.requested, StopRequest::Stop);
    assert_eq!(stopped_call.call.as_deref(), Some(call.as_str()));
    assert_eq!(
        serde_json::to_value(&stopped_call.outcome).unwrap(),
        json!({"status":"failed","error":"process_lost"})
    );
    assert_eq!(stopped_call.advice, RetryAdvice::CallAgain);
    // The cancel's record names the cutover; nothing is live; the units are gone.
    assert_eq!(
        scratch
            .value("SELECT json_extract(payload,'$.author') || ' ' || json_extract(payload,'$.reason') FROM records WHERE kind='step.cancel'")
            .as_deref(),
        Some(format!("cutover {}", report.reason).as_str())
    );
    assert!(!scratch.unit_active(&step_run) && !scratch.unit_active(&call_run));
    let backups: Vec<_> = std::fs::read_dir(scratch.gate.install.join("backups"))
        .unwrap()
        .collect();
    assert_eq!(backups.len(), 1);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!(
            "schema {TARGET} · 1 projects · 2 revisions converted · cancel requested for 1 runs · 1 calls stopped"
        )),
        "{stdout}"
    );
    assert!(stdout.contains(&format!(
        "stopped p w {step_run} requested=cancel outcome=failed:cancelled advice=retry"
    )));
    assert!(stdout.contains(&format!(
        "stopped - call:{call} {call_run} requested=stop outcome=failed:process_lost advice=call-again"
    )) || stdout.contains(&format!(
        "stopped p call:{call} {call_run} requested=stop outcome=failed:process_lost advice=call-again"
    )));
    // Unfenced, released, the candidate selected and the home at its schema.
    scratch.assert_scheduler_ready();
    assert_eq!(scratch.fence(), None);
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("normal")
    );
    assert_eq!(
        scratch.value("SELECT paused FROM projects WHERE name='p'"),
        Some("0".into())
    );
    let selection = Installation::at(scratch.gate.install.clone())
        .unwrap()
        .status()
        .unwrap()
        .selection
        .unwrap();
    assert_eq!(selection.release_path, scratch.releases.candidate);
    assert_eq!(
        scratch.db(|db| db
            .query_row("SELECT schema_version FROM home_meta", [], |r| r
                .get::<_, i64>(0))
            .unwrap()),
        i64::from(TARGET)
    );
}

// ---- compat-check --incompatible --copy -------------------------------------------------------

#[test]
fn cutover_preserves_an_imported_revision_and_checks_its_baseline() {
    let scratch = Scratch::new();
    scratch.tool("project_create", json!({"name": "p"}));
    // The importer's only logged edit is revision 7, with no completion snapshot.
    let db = rusqlite::Connection::open(scratch.gate.home.join("sluice.db")).unwrap();
    db.execute_batch(
        "UPDATE plans SET rev=7;
         UPDATE plan_edits SET rev=7;
         UPDATE records SET payload=json_set(payload,'$.rev',7) WHERE kind='plan.edit';
         UPDATE records SET seq=seq+1000;
         UPDATE plan_edits SET seq=seq+1000;",
    )
    .unwrap();
    // Retained dynamic records precede the baseline and fill more than one history page.
    for seq in 1..=201 {
        db.execute(
            "INSERT INTO records(seq,project_id,kind,at,payload,payload_version)
             SELECT ?1, project_id, 'plan.input', at, ?2, 1 FROM plan_edits LIMIT 1",
            rusqlite::params![seq, json!({"kind":"plan.input","rev":1,"name":"old","value":seq,"author":"fixture","reason":"old input"}).to_string()],
        ).unwrap();
    }
    drop(db);
    let output = scratch.cutover(&["--deadline", "+0s"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(scratch.value("SELECT rev FROM plans").as_deref(), Some("7"));
    assert_eq!(
        scratch.value("SELECT min(rev) FROM plan_edits").as_deref(),
        Some("7")
    );
    assert_eq!(scratch.report().revisions, 1);
    assert_eq!(scratch.fence(), None);
    let next = scratch
        .gate
        .command(
            &scratch.releases.candidate.join("bin/sluice"),
            &[
                "tool",
                "plan_edit",
                r#"{"project":"p","rev":7,"reason":"after cutover","ops":[{"op":"input.put","name":"n","declaration":"int"}]}"#,
            ],
        )
        .output()
        .unwrap();
    assert!(next.status.success(), "{}", text(&next));
    assert_eq!(scratch.value("SELECT rev FROM plans").as_deref(), Some("8"));
    assert_eq!(
        scratch.value("SELECT max(rev) FROM plan_edits").as_deref(),
        Some("8")
    );
}

#[test]
fn cutover_resumes_failed_checks_after_repair_and_refuses_the_wrong_fence_or_schema() {
    let mut scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    let stopped_run = scratch.live_run("step_id='w'");
    // A candidate that serves the real home but returns a wrong plan while the fault marker
    // exists. The release manifest includes both the wrapper and its real binary.
    let candidate = &scratch.releases.candidate;
    std::fs::rename(
        candidate.join("bin/sluice"),
        candidate.join("bin/sluice-real"),
    )
    .unwrap();
    executable::write(
        candidate.join("bin/sluice"),
        r#"#!/bin/sh
if [ "$1" = tool ] && [ "$2" = plan_get ] && [ -f "$SLUICE_HOME/fail-plan-check" ]; then
  printf '%s\n' '{"rev":0,"plan":{}}'
  exit 0
fi
# Reproduce the live loop's fenced activation refusal deterministically.
if [ "$1" = loop ] && [ -f "$SLUICE_INSTALL_DIR/fence.json" ]; then
  echo 'busy: maintenance: schema-3 cutover' >&2
  exit 1
fi
if [ "$1" = loop ] && [ -f "$SLUICE_HOME/loop-without-lease" ]; then
  exec /bin/sleep 120
fi
if [ "$1" = loop ] && [ -f "$SLUICE_HOME/lose-coordinator-on-loop" ]; then
  exec /usr/bin/python3 - "$SLUICE_INSTALL_DIR/services.json" <<'PY'
import json, os, pathlib, signal, sys, time
pid = json.loads(pathlib.Path(sys.argv[1]).read_text())['coordinator']['pid']
os.kill(pid, signal.SIGTERM)
time.sleep(120)
PY
fi
if [ "$1" = drain ] && [ "$2" = --release ] && [ -f "$SLUICE_HOME/lose-release-reply" ]; then
  "$(dirname "$0")/sluice-real" "$@" >/dev/null || exit $?
  exec /usr/bin/python3 - "$SLUICE_INSTALL_DIR/services.json" <<'PY'
import json, os, pathlib, signal, sys, time
pid = json.loads(pathlib.Path(sys.argv[1]).read_text())['coordinator']['pid']
os.kill(pid, signal.SIGTERM)
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    try:
        state = pathlib.Path('/proc', str(pid), 'stat').read_text().rsplit(')', 1)[1].split()[0]
        if state == 'Z': break
    except FileNotFoundError: break
    time.sleep(.05)
print(json.dumps({'error':'storage', 'message':'fixture release reply lost'}), file=sys.stderr)
sys.exit(1)
PY
fi
if [ "$1" = drain ] && [ "$2" = --release ] && [ -f "$SLUICE_HOME/lose-services-on-release" ]; then
  "$(dirname "$0")/sluice-real" "$@" || exit $?
  exec /usr/bin/python3 - "$SLUICE_INSTALL_DIR/services.json" <<'PY'
import json, os, pathlib, signal, sys, time
path = pathlib.Path(sys.argv[1])
services = json.loads(path.read_text())
for record in services.values():
    try: os.kill(record['pid'], signal.SIGTERM)
    except ProcessLookupError: pass
deadline = time.monotonic() + 10
while True:
    active = []
    for record in services.values():
        try:
            state = pathlib.Path('/proc', str(record['pid']), 'stat').read_text().rsplit(')', 1)[1].split()[0]
            if state != 'Z': active.append(record['pid'])
        except FileNotFoundError: pass
    if not active: break
    if time.monotonic() >= deadline: sys.exit('fixture services did not stop')
    time.sleep(.05)
# Keep a dead loop record even when deploy correctly deferred starting it.
services.setdefault('loop', services['serve'])
path.write_text(json.dumps(services))
PY
fi
exec "$(dirname "$0")/sluice-real" "$@"
"#,
    );
    let mut manifest: Manifest =
        serde_json::from_slice(&std::fs::read(candidate.join("manifest.json")).unwrap()).unwrap();
    let mut digests = BTreeMap::new();
    files(candidate, candidate, &mut digests);
    digests.remove("manifest.json");
    manifest.release_id = format!(
        "{}-{}",
        manifest.git_sha,
        sluice_store::artifacts::fingerprint(&serde_json::to_vec(&digests).unwrap())
    );
    manifest.files = digests;
    std::fs::write(
        candidate.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let renamed = candidate.parent().unwrap().join(&manifest.release_id);
    std::fs::rename(candidate, &renamed).unwrap();
    scratch.releases.candidate = renamed;
    let marker = scratch.gate.home.join("fail-plan-check");
    std::fs::write(&marker, "fault").unwrap();
    let output = scratch.cutover(&["--deadline", "+0s", "--cancel-grace", "2"]);
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("plan_get p differs"),
        "{}",
        text(&output)
    );
    assert_eq!(
        scratch
            .value("SELECT schema_version FROM home_meta")
            .as_deref(),
        Some("3")
    );
    assert_eq!(scratch.fence().as_deref(), Some("schema-3 cutover"));
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("drain")
    );
    let original = serde_json::to_value(scratch.report()).unwrap();
    let checkpoint = scratch.gate.install.join("schema-cutover-checkpoint.json");
    let recorded = std::fs::read(&checkpoint).unwrap();
    std::fs::remove_file(&checkpoint).unwrap();
    let output = scratch.cutover(&["--resume-checks"]);
    assert!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("recorded checkpoint"),
        "{}",
        text(&output)
    );
    std::fs::write(&checkpoint, recorded).unwrap();
    let install = Installation::at(scratch.gate.install.clone()).unwrap();
    install.fence("ordinary deploy".into()).unwrap();
    let output = scratch.cutover(&["--resume-checks"]);
    assert!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("fence reason schema-3 cutover"),
        "{}",
        text(&output)
    );
    install.fence("schema-3 cutover".into()).unwrap();
    let db = rusqlite::Connection::open(scratch.gate.home.join("sluice.db")).unwrap();
    db.execute("UPDATE home_meta SET schema_version=1", [])
        .unwrap();
    let output = scratch.cutover(&["--resume-checks"]);
    assert!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("home and candidate at schema 3"),
        "{}",
        text(&output)
    );
    db.execute("UPDATE home_meta SET schema_version=3", [])
        .unwrap();
    drop(db);
    // An unrepaired check failure must leave the installation fenced and drained again.
    let output = scratch.cutover(&["--resume-checks"]);
    assert!(!output.status.success(), "{}", text(&output));
    assert_eq!(scratch.fence().as_deref(), Some("schema-3 cutover"));
    std::fs::remove_file(marker).unwrap();
    // A live loop process alone is insufficient. A lease timeout must preserve
    // recovery through the same checkpoint and restore the drain after release.
    let no_lease = scratch.gate.home.join("loop-without-lease");
    std::fs::write(&no_lease, "fault").unwrap();
    let output = scratch.cutover(&["--resume-checks"]);
    assert!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr)
                .contains("loop did not hold the scheduler lease within 60 s"),
        "{}",
        text(&output)
    );
    assert_eq!(scratch.fence().as_deref(), Some("schema-3 cutover"));
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("drain")
    );
    let progress: Value = serde_json::from_slice(&std::fs::read(&checkpoint).unwrap()).unwrap();
    assert_ne!(progress["completed"], true);
    std::fs::remove_file(no_lease).unwrap();
    // Recovery must re-create a maintenance coordinator through the restored
    // installation fence before it can restore the released drain.
    let lose_coordinator = scratch.gate.home.join("lose-coordinator-on-loop");
    std::fs::write(&lose_coordinator, "fault").unwrap();
    let output = scratch.cutover(&["--resume-checks"]);
    assert!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr)
                .contains("coordinator exited after cutover release")
            && !String::from_utf8_lossy(&output.stderr).contains("drain restoration failed"),
        "{}",
        text(&output)
    );
    assert_eq!(scratch.fence().as_deref(), Some("schema-3 cutover"));
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("drain")
    );
    std::fs::remove_file(lose_coordinator).unwrap();
    // A committed release can lose its acknowledgment. Restore the drain even
    // when the CLI reports failure, rather than treating that as no release.
    let lose_reply = scratch.gate.home.join("lose-release-reply");
    std::fs::write(&lose_reply, "fault").unwrap();
    let output = scratch.cutover(&["--resume-checks"]);
    assert!(
        !output.status.success()
            && String::from_utf8_lossy(&output.stderr).contains("fixture release reply lost")
            && !String::from_utf8_lossy(&output.stderr).contains("drain restoration failed"),
        "{}",
        text(&output)
    );
    assert_eq!(scratch.fence().as_deref(), Some("schema-3 cutover"));
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("drain")
    );
    std::fs::remove_file(lose_reply).unwrap();
    // A --collect service that already exited must not prevent service re-creation.
    let services_path = scratch.gate.install.join("services.json");
    let mut services: Value =
        serde_json::from_slice(&std::fs::read(&services_path).unwrap()).unwrap();
    services["collected"] = json!({"unit": format!("sluice-test-collected-{}.service", sluice_model::ids::InvocationId::new()), "release":scratch.releases.candidate});
    std::fs::write(&services_path, serde_json::to_vec(&services).unwrap()).unwrap();
    std::fs::write(scratch.gate.home.join("lose-services-on-release"), "fault").unwrap();
    let output = scratch.cutover(&["--resume-checks"]);
    assert!(output.status.success(), "{}", text(&output));
    scratch.assert_scheduler_ready();
    assert_eq!(scratch.fence(), None);
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("normal")
    );
    assert_eq!(serde_json::to_value(scratch.report()).unwrap(), original);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("2 revisions converted"),
        "{}",
        text(&output)
    );
    assert_eq!(
        std::fs::read_dir(scratch.gate.install.join("backups"))
            .unwrap()
            .count(),
        1
    );
    // Hold the project paused so retry visibly returns its step to pending. The
    // recovered loop must admit it as soon as the project is unpaused.
    for (name, args) in [
        ("project_update", json!({"project":"p","paused":true})),
        (
            "step_retry",
            json!({"project":"p","steps":"w","reason":"retry after cutover"}),
        ),
    ] {
        let output = scratch
            .gate
            .command(
                &scratch.releases.candidate.join("bin/sluice"),
                &["tool", name, &args.to_string()],
            )
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", text(&output));
    }
    assert_eq!(
        scratch
            .value("SELECT status FROM steps WHERE step_id='w'")
            .as_deref(),
        Some("pending")
    );
    let output = scratch
        .gate
        .command(
            &scratch.releases.candidate.join("bin/sluice"),
            &[
                "tool",
                "project_update",
                r#"{"project":"p","paused":false}"#,
            ],
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let admitted = scratch.live_run("step_id='w'");
    assert_ne!(admitted, stopped_run);
    assert!(scratch.unit_active(&admitted));
    // A completed cutover is not a resumable fenced cutover.
    assert!(!scratch.cutover(&["--resume-checks"]).status.success());
}

impl Scratch {
    fn compat_check(&self, args: &[&str]) -> Output {
        self.gate
            .command(&repo().join("scripts/compat-check"), &["--prefix"])
            .arg(&self.prefix)
            .args(args)
            .output()
            .unwrap()
    }
    /// A private copy of the home as compat-check --copy takes one: the database by the
    /// backup API, and the config.
    fn copy_home(&self) -> PathBuf {
        let copy = self.gate.root.path().join("copy");
        std::fs::create_dir_all(&copy).unwrap();
        let source = rusqlite::Connection::open_with_flags(
            self.gate.home.join("sluice.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        source
            .backup(rusqlite::MAIN_DB, copy.join("sluice.db"), None)
            .unwrap();
        std::fs::copy(self.gate.home.join("config.json"), copy.join("config.json")).unwrap();
        copy
    }
}

#[test]
fn compat_check_incompatible_refuses_a_copy_with_live_pins_before_converting_it() {
    let scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    let run = scratch.live_run("step_id='w'");
    // Chosen by itself: the candidate's manifest says schema 3, the home is schema 1.
    let home = scratch.gate.home.to_str().unwrap().to_owned();
    let release = scratch.releases.candidate.to_str().unwrap().to_owned();
    let output = scratch.compat_check(&["--release", &release, "--home", &home]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("compat: incompatible: candidate"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "compat: pinned: live run {run} is pinned to release"
        )) && stdout.contains("scripts/cutover-rehearse"),
        "{stdout}"
    );
    // The same for a prepared copy, which stays the caller's.
    let copy = scratch.copy_home();
    let copy_arg = copy.to_str().unwrap().to_owned();
    let output =
        scratch.compat_check(&["--release", &release, "--copy", &copy_arg, "--incompatible"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("pinned"));
    assert!(copy.join("sluice.db").is_file());
    let schema: i64 = rusqlite::Connection::open(copy.join("sluice.db"))
        .unwrap()
        .query_row("SELECT schema_version FROM home_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(schema, 1, "nothing was converted");
    // --incompatible between two releases of one schema is refused outright.
    let old = scratch.releases.old.to_str().unwrap().to_owned();
    let output = scratch.compat_check(&[
        "--release",
        &old,
        "--home",
        &home,
        "--incompatible",
        "--old",
        &old,
    ]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("both schema"),
        "{}",
        text(&output)
    );
}

#[test]
fn compat_check_with_a_prepared_copy_runs_the_compatible_check_and_leaves_the_copy() {
    let scratch = Scratch::new();
    scratch.project(&[("w", "fixture.echo")]);
    let copy = scratch.copy_home();
    let copy_arg = copy.to_str().unwrap().to_owned();
    let old = scratch.releases.old.to_str().unwrap().to_owned();
    let output = scratch.compat_check(&["--release", &old, "--copy", &copy_arg]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("releases ok"),
        "{}",
        text(&output)
    );
    assert!(copy.join("sluice.db").is_file());
}

#[test]
fn compat_check_incompatible_converts_a_drained_copy_and_the_old_release_refuses_it_unchanged() {
    let scratch = Scratch::new();
    scratch.project(&[("w", "fixture.echo")]);
    wait_for("w to succeed", Duration::from_secs(60), || {
        scratch
            .value("SELECT status FROM steps WHERE step_id='w'")
            .as_deref()
            == Some("succeeded")
    });
    let copy = scratch.copy_home();
    let copy_arg = copy.to_str().unwrap().to_owned();
    let candidate = scratch.releases.candidate.to_str().unwrap().to_owned();
    let old = scratch.releases.old.to_str().unwrap().to_owned();
    let output =
        scratch.compat_check(&["--release", &candidate, "--copy", &copy_arg, "--old", &old]);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("compat: converted from schema 1"),
        "{stdout}"
    );
    assert!(stdout.contains("incompatible ok"), "{stdout}");
    let schema: i64 = rusqlite::Connection::open_with_flags(
        copy.join("sluice.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row("SELECT schema_version FROM home_meta", [], |r| r.get(0))
    .unwrap();
    assert_eq!(schema, i64::from(TARGET));
}

// ---- scripts/cutover-rehearse ------------------------------------------------------------------

impl Scratch {
    fn rehearse(&self, harness: Option<&Path>) -> (Output, Value) {
        let mut command = self.gate.command(
            &repo().join("scripts/cutover-rehearse"),
            &["--json", "--candidate"],
        );
        command
            .arg(&self.releases.candidate)
            .arg("--prefix")
            .arg(&self.prefix)
            .args(["--deadline", "2026-10-12T18:00:00Z"]);
        if let Some(harness) = harness {
            command.arg("--harness").arg(harness);
        }
        let output = command.output().unwrap();
        let report = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("no JSON report: {}", text(&output)));
        (output, report)
    }
    /// A stand-in for the harness: records its arguments and environment, prints `reply`
    /// and exits with `code`, ending nothing.
    fn stand_in(&self, reply: Value, code: i32) -> PathBuf {
        let path = self.gate.root.path().join("harness");
        let record = self.gate.root.path().join("harness.args");
        executable::write(
            &path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho \"SLUICE_HOME=$SLUICE_HOME DBUS=$DBUS_SESSION_BUS_ADDRESS\" >> '{}'\ncat <<'JSON'\n{reply}\nJSON\nexit {code}\n",
                record.display(),
                record.display()
            ),
        );
        path
    }
}

#[test]
fn rehearse_reports_the_old_release_refusing_a_cancel_and_converts_nothing() {
    let scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    let run = scratch.live_run("step_id='w'");
    let attempt = scratch
        .value(&format!("SELECT attempt_id FROM runs WHERE run_id='{run}'"))
        .unwrap();
    let refusal = json!({"project":"p","step":"w","runs":[run],"attempts":[attempt],
        "error":{"error":"invalid","message":"invalid stored plan","errors":["steps.w.run: unknown fn fixture.wait"]}});
    let harness = scratch.stand_in(json!({"refused":[refusal],"blockers":[]}), 1);
    let (output, report) = scratch.rehearse(Some(&harness));
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert_eq!(report["ok"], false);
    assert!(
        report["failure"]
            .as_str()
            .unwrap()
            .contains("refused 1 cancel(s) on the copy"),
        "{report}"
    );
    let cutover: CutoverReport = serde_json::from_value(report["report"].clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&cutover.refused).unwrap(),
        json!([refusal])
    );
    assert!(cutover.stopped.is_empty());
    assert_eq!(
        cutover.reason,
        format!(
            "schema-{TARGET} cutover at 2026-10-12T18:00:00Z: stopped at the deadline; retry it after the cutover"
        )
    );
    assert_eq!(report["conversion"], Value::Null);
    // The harness played on the copy, cut off from the user's service manager, and the copy
    // is gone; the live home is untouched.
    let args = std::fs::read_to_string(scratch.gate.root.path().join("harness.args")).unwrap();
    let copy = report["copy"].as_str().unwrap();
    assert!(
        args.starts_with(&format!("rehearse\n--home\n{copy}\n"))
            && args.contains("--deadline\n2026-10-12T18:00:00Z\n")
            && args.contains(&format!("SLUICE_HOME={copy} DBUS=unix:path=")),
        "{args}"
    );
    assert!(!Path::new(copy).exists());
    assert!(scratch.unit_active(&run));
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("normal")
    );
}

#[test]
fn rehearse_checks_the_blockers_itself_and_never_converts_a_copy_with_live_work() {
    let scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    scratch.live_run("step_id='w'");
    // A harness that claims success but ended nothing.
    let harness = scratch.stand_in(json!({"refused":[],"blockers":[]}), 0);
    let (output, report) = scratch.rehearse(Some(&harness));
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        report["failure"]
            .as_str()
            .unwrap()
            .contains("did not reach zero blockers: 1 attempts, 1 runs"),
        "{report}"
    );
    assert_eq!(report["conversion"], Value::Null);
}

#[test]
fn rehearse_plays_the_deadline_with_the_old_release_converts_the_copy_and_passes_the_compat_check()
{
    let scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    let run = scratch.live_run("step_id='w'");
    let (output, report) = scratch.rehearse(None);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(report["ok"], true, "{report}");
    let cutover: CutoverReport = serde_json::from_value(report["report"].clone()).unwrap();
    assert!(cutover.refused.is_empty());
    let stopped = cutover
        .stopped
        .iter()
        .find(|r| r.run.to_string() == run)
        .unwrap();
    assert_eq!(stopped.advice, RetryAdvice::Retry);
    assert!(
        report["compat"]
            .as_str()
            .unwrap()
            .contains("incompatible ok")
    );
    // The live home and its run are untouched.
    assert!(scratch.unit_active(&run));
    assert_eq!(
        scratch.value("SELECT mode FROM maintenance").as_deref(),
        Some("normal")
    );
}

/// The fake agent engine of the composition tests: with `wait_message` it stays busy until a
/// message comes, so its run lives on after a submission until it is told to stop.
const FAKE_ENGINE: &str = r#"#!/usr/bin/python3
import os, sys, json, pathlib
config = json.loads(pathlib.Path(sys.argv[2]).read_text())
state = {'status':'starting','turns_started':0,'turns_completed':0,'waiting':None,'background_work':[],'compactions':0,'final_text':'fake done','session_id':'fake-session','acknowledged':[],'not_accepted':[],'progress':0,'error':None}
for line in sys.stdin:
    req = json.loads(line)
    if req['operation'] == 'command':
        cmd = req['command']
        if cmd['command'] in ('start_fresh','resume'): state['status'] = 'idle'
        if cmd['command'] in ('deliver_text','steer'):
            state['acknowledged'].append(cmd['id']); state['turns_started'] += 1; state['status'] = 'busy'; state['progress'] += 1
        if cmd['command'] == 'request_exit': state['status'] = 'exited'
    print(json.dumps({'observation':state,'outcome':'acknowledged','error':None,'hook':None}), flush=True)
"#;

/// Resumes a stopped guardian however the test ends.
struct Frozen(u32);
impl Drop for Frozen {
    fn drop(&mut self) {
        let _ = Command::new("/bin/kill")
            .args(["-CONT", &self.0.to_string()])
            .stderr(Stdio::null())
            .status();
    }
}

#[test]
fn cutover_reports_a_run_settled_before_the_deadline_as_succeeded_with_no_advice() {
    let scratch = Scratch::with_env(|root| {
        let engine = root.join("fake-engine");
        executable::write(&engine, FAKE_ENGINE);
        let script = root.join("fake-script.json");
        std::fs::write(&script, json!({"wait_message": true}).to_string()).unwrap();
        vec![
            (
                "SLUICE_FAKE_ENGINE_BIN".into(),
                engine.to_string_lossy().into(),
            ),
            (
                "SLUICE_FAKE_ENGINE_SCRIPT".into(),
                script.to_string_lossy().into(),
            ),
        ]
    });
    scratch.tool("project_create", json!({"name": "p"}));
    let cwd = scratch.gate.root.path().to_string_lossy().into_owned();
    scratch.tool(
        "plan_patch",
        json!({"project":"p","rev":1,"reason":"cutover fixture","ops":[{"op":"add","path":"/steps/a","value":
            {"run":"agent.run","in":{"engine":{"default":"fake"},"cwd":{"default":cwd},"spec":{"default":"Summarize"}},
             "outputs":{"summary":"string"}}}]}),
    );
    let run = scratch.live_run("step_id='a'");
    scratch.tool(
        "step_submit",
        json!({"project":"p","step":"a","run":run,"outputs":{"summary":"settled before the deadline"}}),
    );
    // Its guardian is held still, so the settled run is live at the deadline: the cutover's
    // cancel finds settle intent already stored, and its unit is stopped after the grace.
    let guardian: u32 = scratch
        .value(&format!(
            "SELECT guardian_pid FROM runs WHERE run_id='{run}'"
        ))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        Command::new("/bin/kill")
            .args(["-STOP", &guardian.to_string()])
            .status()
            .unwrap()
            .success()
    );
    let _frozen = Frozen(guardian);
    scratch.tool(
        "step_settle",
        json!({"project":"p","step":"a","reason":"its agent has submitted"}),
    );
    assert_eq!(
        scratch.value(&format!(
            "SELECT json_type(a.request,'$.settle') IS NOT NULL FROM attempts a JOIN runs r USING(attempt_id) WHERE r.run_id='{run}'"
        )).as_deref(),
        Some("1")
    );
    let output = scratch.cutover(&[
        "--deadline",
        "+0s",
        "--cancel-grace",
        "2",
        "--settle-timeout",
        "120",
    ]);
    eprintln!("{}", text(&output));
    let report = scratch.report();
    assert!(report.refused.is_empty(), "{report:?}");
    assert_eq!(report.stopped.len(), 1, "{report:?}");
    let settled = &report.stopped[0];
    assert_eq!(settled.run.to_string(), run);
    assert_eq!(settled.requested, StopRequest::Cancel);
    assert_eq!(
        serde_json::to_value(&settled.outcome).unwrap(),
        json!({"status":"succeeded"})
    );
    assert_eq!(
        serde_json::to_value(&settled.step_status).unwrap(),
        json!("succeeded")
    );
    assert_eq!(settled.advice, RetryAdvice::None);
}

// ---- sluice home migrate -----------------------------------------------------------------------

fn migrate(gate: &support::Gate, args: &[&str]) -> Output {
    let mut all = vec!["home", "migrate"];
    all.extend_from_slice(args);
    gate.command(Path::new(env!("CARGO_BIN_EXE_sluice")), &all)
        .output()
        .unwrap()
}

#[test]
fn home_migrate_converts_a_drained_schema_1_home_only_with_the_writer_lock_free() {
    let mut scratch = Scratch::new();
    scratch.project(&[("w", "fixture.wait")]);
    let run = scratch.live_run("step_id='w'");
    std::fs::write(
        scratch.gate.home.join("runs").join(&run).join("finish"),
        "finish",
    )
    .unwrap();
    wait_for("w to succeed", Duration::from_secs(60), || {
        scratch
            .value("SELECT status FROM steps WHERE step_id='w'")
            .as_deref()
            == Some("succeeded")
    });
    // Refused while the old coordinator holds the home's writer lock.
    let output = migrate(&scratch.gate, &["--json"]);
    assert!(!output.status.success(), "{}", text(&output));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"], "busy", "{error}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("coordinator.lock"),
        "{error}"
    );
    scratch.stop_services();
    let database = scratch.gate.home.join("sluice.db");
    let before = std::fs::read(&database).unwrap();
    // A dry run converts a copy and leaves the home's bytes as they were.
    let output = migrate(&scratch.gate, &["--dry-run", "--json"]);
    assert!(output.status.success(), "{}", text(&output));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["from_schema"], 1, "{report}");
    assert_eq!(report["projects"][0]["name"], "p", "{report}");
    assert_eq!(report["projects"][0]["revisions"], 2, "{report}");
    assert_eq!(report["projects"][0]["anchor"], Value::Null, "{report}");
    assert_eq!(report["anchored"], json!([]), "{report}");
    assert_eq!(std::fs::read(&database).unwrap(), before);
    assert_eq!(
        scratch
            .value("SELECT schema_version FROM home_meta")
            .as_deref(),
        Some("1")
    );
    // The conversion itself, in words.
    let output = migrate(&scratch.gate, &[]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).starts_with(&format!(
            "converted schema 1 to {SCHEMA}: 1 projects, 2 revisions, 0 anchored, 0 warnings"
        )),
        "{}",
        text(&output)
    );
    assert_eq!(
        scratch
            .value("SELECT schema_version FROM home_meta")
            .as_deref(),
        Some(SCHEMA.to_string().as_str())
    );
}
