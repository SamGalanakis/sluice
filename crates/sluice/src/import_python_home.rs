//! Temporary Python-home import. Delete this module and its readers after cutover.
//! Usage: sluice import-python-home SRC DST [--staging CUTOVER_STAGING].
//! Staging contains projects/<name>/{fns,recipes,config.json,.env}, optional global
//! {fns,recipes,config.json,.env}, builtins.json (descriptor array), and optional
//! plan-conversion.json {"units": {project: {unit: [step,...]}},
//! "pause_states": {project: boolean}}. The latter captures pre-window pauses.
//! Files come from staging, never Python fn installations. No execution API is used.

#[path = "import_python_home/builtins.rs"]
mod builtins;
#[path = "import_python_home/commit.rs"]
mod commit;
#[path = "import_python_home/convert.rs"]
mod convert;
#[path = "import_python_home/files.rs"]
mod files;
#[path = "import_python_home/ledger.rs"]
mod ledger;
#[path = "import_python_home/parser.rs"]
mod parser;
#[path = "import_python_home/sessions.rs"]
mod sessions;
#[path = "import_python_home/state.rs"]
mod state;

use anyhow::{Context, Result, bail, ensure};
use fs4::FileExt;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sluice_model::{Plan, StateSnapshot, error::PublicError};
use sluice_store::{
    RetrySafety, Writer,
    artifacts::{self, Bundle},
    projects::Icon,
};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub const INTERRUPTED: &str = "interrupted by the sluice upgrade";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailurePoint {
    None,
    MidCommit,
    AfterCommit,
    BeforePublish,
}

pub struct Options {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub staging: PathBuf,
    pub failure: FailurePoint,
}

struct Prepared {
    old: parser::OldProject,
    plan: Plan,
    state: StateSnapshot,
    signatures: convert::Signatures,
    icon: Option<Icon>,
    bundle: Option<Bundle>,
    predecessors: Vec<Value>,
    questions: Vec<i64>,
    rewrites: Vec<(String, String)>,
}

/// Single CLI wiring entry point, before the normal home/dispatch parser.
pub fn dispatch_import_python_home() -> Option<std::result::Result<(), PublicError>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_none_or(|s| s != "import-python-home") {
        return None;
    }
    let result = (|| -> Result<()> {
        ensure!(
            args.len() == 3 || args.len() == 5 && args[3] == "--staging",
            "usage: sluice import-python-home SRC DST [--staging CUTOVER_STAGING]"
        );
        let staging = if args.len() == 5 {
            PathBuf::from(&args[4])
        } else {
            std::env::var_os("SLUICE_CUTOVER_STAGING")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("cutover-staging"))
        };
        let report = import(&Options {
            source: PathBuf::from(&args[1]),
            destination: PathBuf::from(&args[2]),
            staging,
            failure: FailurePoint::None,
        })?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        Ok(())
    })();
    Some(result.map_err(|e| PublicError::Invalid {
        message: "Python home import refused".into(),
        errors: vec![format!("{e:#}")],
    }))
}

pub fn import(options: &Options) -> Result<Value> {
    let src = options.source.canonicalize()?;
    let staging = options.staging.canonicalize()?;
    ensure!(
        src.is_dir() && staging.is_dir(),
        "source/staging must be directories"
    );
    let parent = options
        .destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let name = options
        .destination
        .file_name()
        .context("destination needs a name")?;
    let dst = parent.join(name);
    let actual_dst = if dst.exists() {
        dst.canonicalize()?
    } else {
        dst.clone()
    };
    ensure!(
        src != actual_dst && !src.starts_with(&actual_dst) && !actual_dst.starts_with(&src),
        "source/destination overlap"
    );
    ensure!(
        !staging.starts_with(&actual_dst) && !actual_dst.starts_with(&staging),
        "staging/destination overlap"
    );
    ensure!(
        !fs::symlink_metadata(&dst).is_ok_and(|m| m.file_type().is_symlink()),
        "destination is a symlink"
    );
    let leaf = name.to_str().context("destination must have a UTF8 name")?;
    let lock_path = parent.join(format!(".{leaf}.python-import.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(0x20000)
        .open(&lock_path)?;
    FileExt::try_lock(&lock).context("another importer owns this destination")?;
    let work = parent.join(format!(".{leaf}.python-import"));
    let marker = work.join("owner.json");
    if work.exists() {
        ensure!(
            !fs::symlink_metadata(&work)?.file_type().is_symlink(),
            "import staging is a symlink"
        );
        let owner = files::json(&marker).context("refusing unrelated import staging")?;
        ensure!(
            owner == json!({"version":1,"source":src,"destination":dst}),
            "staging belongs to a different import"
        );
    } else {
        files::directory(&work)?;
        files::atomic_json(
            &marker,
            &json!({"version":1,"source":src,"destination":dst}),
        )?;
    }
    let source_copy = work.join("source");
    if source_copy.exists() {
        fs::remove_dir_all(&source_copy)?;
    }
    let mut snapshot = parser::snapshot(&src, &source_copy)?;
    state::collect_scatter(&mut snapshot, &src)?;
    let mut import_id = ledger::identity(&snapshot, &src, &staging)?;
    let conflict_list = conflicts(&src, &staging)?;
    import_id = artifacts::fingerprint(&sluice_model::hash::canonical_json(
        &json!({"snapshot":import_id,"conflicts":conflict_list,"native_predecessors":1,"fn_sources":1}),
    )?);
    if dst.exists() && !files::entries(&dst)?.is_empty() {
        let saved = files::json(&dst.join("import-ledger.json"))
            .context("refusing unrelated nonempty destination")?;
        ensure!(
            saved["import_id"] == import_id && saved["phase"] == "complete",
            "destination belongs to a different or incomplete snapshot"
        );
        verify_published(&dst, &saved)?;
        return files::json(&dst.join("import-report.json"));
    }
    let ledger_path = work.join("ledger.json");
    let mut ledger = if ledger_path.exists() {
        let ledger = files::json(&ledger_path)?;
        ensure!(
            ledger["import_id"] == import_id,
            "different snapshot refused; use a fresh destination"
        );
        ledger
    } else {
        let mut ledger = ledger::allocate(&snapshot, &import_id, &src, &dst);
        ledger["pause_state_source"] = json!("stopped-snapshot");
        if staging.join("plan-conversion.json").exists() {
            let manifest = files::json(&staging.join("plan-conversion.json"))?;
            if !manifest["pause_states"].is_null() {
                ensure!(
                    manifest["pause_states"].is_object(),
                    "pause_states must be an object"
                );
                ledger["pause_state_source"] = json!("pre-window-manifest");
                ensure!(
                    manifest["pause_states"]
                        .as_object()
                        .is_some_and(|flags| flags.len() == snapshot.projects.len()),
                    "saved pause_states must cover every imported project"
                );
            }
            for project in &snapshot.projects {
                if let Some(paused) = manifest["pause_states"].get(&project.name) {
                    ensure!(paused.is_boolean(), "pause_states must contain booleans");
                    ledger["projects"][&project.name]["original_paused"] = paused.clone();
                } else {
                    ensure!(
                        manifest["pause_states"].is_null(),
                        "saved pause_states missing project {}",
                        project.name
                    );
                }
            }
        }
        files::atomic_json(&ledger_path, &ledger)?;
        ledger
    };
    let home = work.join("home");
    if home.exists() {
        fs::remove_dir_all(&home)?;
    }
    files::directory(&home)?;
    let mut signatures = if staging.join("builtins.json").exists() {
        let mut signatures = convert::Signatures::default();
        for doc in files::json(&staging.join("builtins.json"))?
            .as_array()
            .context("builtins must be descriptors")?
        {
            signatures.add(doc.clone())?;
        }
        signatures
    } else {
        builtins::signatures()?
    };
    signatures.load(&staging.join("fns"))?;
    let mut rewrites = vec![(
        src.to_string_lossy().into_owned(),
        dst.to_string_lossy().into_owned(),
    )];
    for project in &snapshot.projects {
        let id = ledger["projects"][&project.name]["id"]
            .as_str()
            .context("missing project id")?;
        rewrites.insert(
            0,
            (
                src.join("projects")
                    .join(&project.name)
                    .to_string_lossy()
                    .into_owned(),
                dst.join("projects").join(id).to_string_lossy().into_owned(),
            ),
        );
    }
    if src.join(".snapshot-origin.json").exists() {
        let origin = files::json(&src.join(".snapshot-origin.json"))?;
        let origin = PathBuf::from(
            origin["home"]
                .as_str()
                .context("snapshot origin missing home")?,
        );
        for project in &snapshot.projects {
            let id = ledger["projects"][&project.name]["id"]
                .as_str()
                .context("missing project id")?;
            rewrites.insert(
                0,
                (
                    origin
                        .join("projects")
                        .join(&project.name)
                        .to_string_lossy()
                        .into_owned(),
                    dst.join("projects").join(id).to_string_lossy().into_owned(),
                ),
            );
        }
        rewrites.push((
            origin.to_string_lossy().into_owned(),
            dst.to_string_lossy().into_owned(),
        ));
    }
    let manifest = if staging.join("plan-conversion.json").exists() {
        files::json(&staging.join("plan-conversion.json"))?
    } else {
        json!({})
    };
    let global = copy_assets(&staging, &home, &rewrites)?;
    require_private_config(&src, &staging)?;
    let mut report = json!({"projects":snapshot.projects.len(),"steps_by_state":{},"interrupted_runs":0,"sessions_copied":0,"session_imports":[],"questions_converted":0,"conflicts":conflict_list,"conversions":[],"exceptions":[],"dropped":snapshot.dropped,"operationally_paused":true,"pause_state_source":ledger["pause_state_source"]});
    let mut prepared = Vec::new();
    for old in snapshot.projects {
        let id = ledger["projects"][&old.name]["id"]
            .as_str()
            .context("project id missing")?
            .to_owned();
        let dir = home.join("projects").join(&id);
        files::directory(&dir)?;
        let converted = staging.join("projects").join(&old.name);
        require_private_config(&src.join("projects").join(&old.name), &converted)?;
        let bundle = copy_assets(&converted, &dir, &rewrites)?;
        let mut sig = signatures.clone();
        sig.load(&converted.join("fns"))?;
        let (plan, notes) =
            match convert::document(&old.plan, manifest["units"].get(&old.name), &rewrites, &sig) {
                Ok(plan) => plan,
                Err(e) => {
                    files::atomic_json(
                        &work.join("conversion-errors.json"),
                        &json!({"project":old.name,"error":e.to_string()}),
                    )?;
                    bail!("{} plan conversion failed: {e}", old.name);
                }
            };
        report["conversions"].as_array_mut().expect("array").extend(
            notes
                .into_iter()
                .map(|s| json!(format!("{}: {s}", old.name))),
        );
        let state = state::snapshot(&old, &plan, &rewrites)?;
        for (step_id, step) in plan.steps() {
            for (name, binding) in &step.bindings {
                let sluice_model::Binding::File(converted_path) = binding else {
                    continue;
                };
                let converted_path = Path::new(converted_path);
                if let Ok(relative) = converted_path.strip_prefix(&dst) {
                    ensure!(
                        relative
                            .components()
                            .all(|c| matches!(c, std::path::Component::Normal(_))),
                        "file binding escapes imported home"
                    );
                    let original = old.plan["steps"][step_id.as_str()]["in"][name]["file"]
                        .as_str()
                        .context("original file binding missing")?;
                    let source_path = if let Ok(relative) = Path::new(original).strip_prefix(&src) {
                        src.join(relative)
                    } else if src.join(".snapshot-origin.json").exists() {
                        let origin = files::json(&src.join(".snapshot-origin.json"))?;
                        src.join(Path::new(original).strip_prefix(
                            origin["home"].as_str().context("origin home missing")?,
                        )?)
                    } else {
                        bail!("file binding was rewritten without a source snapshot path");
                    };
                    let bytes = files::read(&source_path)?;
                    let target = home.join(relative);
                    if target.exists() {
                        ensure!(
                            files::read(&target)? == bytes,
                            "file binding collides with converted assets"
                        );
                    } else {
                        files::write(&target, &bytes, fs::metadata(&source_path)?.mode())?;
                    }
                }
            }
        }
        let limits = old.settings["resources"]
            .as_object()
            .context("resources must be an object")?
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    value["capacity"]
                        .as_u64()
                        .or_else(|| value.as_u64())
                        .map(sluice_model::plan::ResourceLimit::Fixed)
                        .unwrap_or(sluice_model::plan::ResourceLimit::Dynamic),
                )
            })
            .collect();
        sluice_model::plan::validate_changed_needs(None, &plan, &limits).map_err(|e| {
            anyhow::anyhow!(
                "{}",
                e.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;
        let icon = if let Some(bytes) = old.icon.clone() {
            Some(Icon::image(bytes)?)
        } else {
            old.settings["icon_text"]
                .as_str()
                .map(Icon::text)
                .transpose()?
        };
        let mut predecessors = vec![];
        for (step_id, step) in plan.steps() {
            ledger["projects"][&old.name]["steps"][step_id.as_str()]["provenance"] =
                state::provenance(&src, &old, step)?;
            let entry = &old.state["steps"][step_id.as_str()];
            let status = entry["status"].as_str().unwrap_or("pending");
            let count = report["steps_by_state"][status].as_u64().unwrap_or(0);
            report["steps_by_state"][status] = json!(count + 1);
            let effective = state::effective(&plan, &state, step)?;
            if effective.is_none() && status != "pending" && status != "skipped" {
                report["exceptions"].as_array_mut().expect("array").push(json!(format!("{}/{step_id}: effective inputs unknown; retained status and manual marker with unknown hash",old.name)));
            }
            if status != "running" {
                continue;
            }
            let effective = effective.context("running step effective inputs unavailable")?;
            if let Some(scatter) = &step.scatter {
                let count = effective[scatter]
                    .as_array()
                    .context("scatter input is not an array")?
                    .len();
                ensure!(
                    entry["run_ids"]
                        .as_array()
                        .is_some_and(|a| a.len() == count),
                    "running scatter count mismatch"
                );
            }
            for (index, ids) in
                ledger["projects"][&old.name]["steps"][step_id.as_str()]["predecessors"]
                    .as_object()
                    .context("predecessor mapping missing")?
            {
                let index: i64 = index.parse()?;
                let mut pred = state::predecessor(&src, &old, step, index, &effective, &rewrites)?;
                pred["index"] = json!(index);
                pred["run"] = ids["run"].clone();
                pred["attempt"] = ids["attempt"].clone();
                pred["invocation"] = ids["invocation"].clone();
                pred["step"] = json!(step_id);
                let good = index >= 0
                    && entry["results"]
                        .get(index as usize)
                        .is_some_and(Value::is_object);
                pred["good"] = json!(good);
                if !good {
                    increment(&mut report, "interrupted_runs");
                    if pred["checkpoint"].is_object() {
                        let run = pred["run"].as_str().context("missing run id")?.to_owned();
                        pred["session"] = sessions::copy(&src, &home, &dst, &pred["checkpoint"])?
                            .unwrap_or(Value::Null);
                        if pred["session"].is_object() {
                            increment(&mut report, "sessions_copied");
                            report["session_imports"].as_array_mut().expect("array").push(json!({"run":run,"engine":pred["session"]["engine"],"session":pred["session"]["session"],"cwd":pred["session"]["cwd"],"details":pred["session"]["metadata"]}));
                        }
                        if let Some(error) = pred["session"]["metadata"].get("validation_error") {
                            report["exceptions"].as_array_mut().expect("array").push(json!({"predecessor":format!("{}/{step_id}[{index}]",old.name),"resume_validation_error":error}));
                        }
                        sessions::write_checkpoint(&home, &pred)?;
                    }
                    if !pred["session"].is_object() {
                        report["exceptions"].as_array_mut().expect("array").push(json!(format!("{}/{step_id}[{index}]: interrupted predecessor has no engine session; reconcile external effects before retry",old.name)));
                    }
                }
                predecessors.push(pred);
            }
        }
        let mut questions = Vec::new();
        for q in &old.questions {
            let supported = !q["title"].as_str().is_none_or(|s| s.trim().is_empty())
                && q["input"]
                    .as_str()
                    .is_none_or(|name| plan.inputs().contains_key(name));
            if supported {
                questions.push(q["n"].as_i64().context("question id missing")?);
                increment(&mut report, "questions_converted");
            } else {
                report["exceptions"].as_array_mut().expect("array").push(json!({"unsupported_inbox":format!("{}:{}",old.name,q["n"]),"reason":"missing title or unknown plan input; repost explicitly"}));
            }
        }
        prepared.push(Prepared {
            old,
            plan,
            state,
            signatures: sig,
            icon,
            bundle,
            predecessors,
            questions,
            rewrites: rewrites.clone(),
        });
    }
    // Re-read all relevant data/files before committing a stopped snapshot.
    let second = work.join("verify-source");
    if second.exists() {
        fs::remove_dir_all(&second)?;
    }
    let mut check = parser::snapshot(&src, &second)?;
    state::collect_scatter(&mut check, &src)?;
    let check_id = ledger::identity(&check, &src, &staging)?;
    ensure!(
        artifacts::fingerprint(&sluice_model::hash::canonical_json(
            &json!({"snapshot":check_id,"conflicts":conflicts(&src,&staging)?,"native_predecessors":1,"fn_sources":1})
        )?) == import_id,
        "source/staging changed while importing"
    );
    files::atomic_json(&ledger_path, &ledger)?;
    files::sync_tree(&home)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let writer = Writer::open(&home)?;
    let fail = options.failure == FailurePoint::MidCommit;
    let committed = runtime.block_on(writer.write(RetrySafety::Idempotent, move |tx| {
        commit::commit(tx, prepared, ledger, global, fail)
    }));
    if committed.is_err() {
        runtime.block_on(writer.shutdown())?;
    }
    ledger = committed?;
    if options.failure == FailurePoint::AfterCommit {
        runtime.block_on(writer.shutdown())?;
        bail!("injected after-commit failure");
    }
    runtime.block_on(artifacts::recover(&writer, &home))?;
    runtime.block_on(writer.shutdown())?;
    files::atomic_json(&home.join("import-report.json"), &report)?;
    files::atomic_json(
        &home.join("import-generation.json"),
        &json!({"version":1,"import_id":import_id}),
    )?;
    let mut file_manifest = Vec::new();
    files::identity_files(&home, "", &mut file_manifest, 0)?;
    file_manifest.retain(|f| {
        ![
            "/sluice.db",
            "/sluice.db-wal",
            "/sluice.db-shm",
            "/coordinator.lock",
            "/artifact-worker.lock",
        ]
        .contains(&f["path"].as_str().unwrap_or(""))
    });
    ledger["files"] = json!(file_manifest);
    files::atomic_json(&home.join("import-ledger.json"), &ledger)?;
    files::sync_tree(&home)?;
    if options.failure == FailurePoint::BeforePublish {
        bail!("injected before-publish failure");
    }
    if dst.exists() {
        ensure!(
            files::entries(&dst)?.is_empty(),
            "destination changed before publication"
        );
        fs::remove_dir(&dst)?;
    }
    fs::rename(&home, &dst)?;
    File::open(&parent)?.sync_all()?;
    files::atomic_json(&ledger_path, &ledger)?;
    verify_published(&dst, &ledger)?;
    Ok(report)
}

fn increment(report: &mut Value, key: &str) {
    report[key] = json!(report[key].as_u64().unwrap_or(0) + 1);
}

fn require_private_config(source: &Path, converted: &Path) -> Result<()> {
    for name in ["config.json", ".env"] {
        ensure!(
            !source.join(name).exists() || converted.join(name).is_file(),
            "{name} needs a reviewed staging copy for {}",
            source.display()
        );
    }
    Ok(())
}

fn copy_assets(
    source: &Path,
    dest: &Path,
    rewrites: &[(String, String)],
) -> Result<Option<Bundle>> {
    let recipes = source.join("recipes");
    if recipes.exists() {
        files::tree(&recipes, &dest.join("recipes"), &[], 0)?;
        rewrite_json_tree(&dest.join("recipes"), rewrites)?;
    }
    for name in ["config.json", ".env"] {
        let src = source.join(name);
        if src.exists() {
            let mut bytes = files::read(&src)?;
            if name == "config.json" {
                let mut doc = files::json(&src)?;
                convert::rewrite_paths(&mut doc, rewrites);
                bytes = serde_json::to_vec_pretty(&doc)?;
            } else {
                let mut text = String::from_utf8(bytes)?;
                for (old, new) in rewrites {
                    text = text.replace(old, new);
                }
                bytes = text.into_bytes();
            }
            files::write(&dest.join(name), &bytes, 0o600)?;
        }
    }
    let fns = source.join("fns");
    if !fns.exists() {
        return Ok(None);
    }
    let target = dest.join("fns");
    files::tree(&fns, &target, &[], 0)?;
    let mut map = BTreeMap::new();
    collect_bundle(&target, &target, &mut map, 0)?;
    Ok(Some(Bundle::new(map)?))
}
fn collect_bundle(
    root: &Path,
    dir: &Path,
    map: &mut BTreeMap<String, Vec<u8>>,
    depth: usize,
) -> Result<()> {
    ensure!(depth <= 32, "fn tree too deep");
    for path in files::entries(dir)? {
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            !meta.file_type().is_symlink(),
            "converted fn contains symlink {} -> {}",
            path.display(),
            fs::read_link(&path).unwrap_or_default().display()
        );
        if meta.is_dir() {
            collect_bundle(root, &path, map, depth + 1)?;
        } else {
            map.insert(
                path.strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 fn name")?
                    .into(),
                files::read(&path)?,
            );
        }
    }
    Ok(())
}
fn rewrite_json_tree(dir: &Path, rewrites: &[(String, String)]) -> Result<()> {
    for path in files::entries(dir)? {
        if path.is_dir() {
            rewrite_json_tree(&path, rewrites)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            let mut doc = files::json(&path)?;
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("recipe name missing")?;
            sluice_model::recipe::Recipe::parse(name, &doc).map_err(|e| {
                anyhow::anyhow!(
                    "{}",
                    e.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })?;
            convert::rewrite_paths(&mut doc, rewrites);
            files::atomic_json(&path, &doc)?;
        }
    }
    Ok(())
}

fn conflicts(source: &Path, staging: &Path) -> Result<Vec<Value>> {
    let mut conflicts = vec![];
    let shipped = builtins::shipped();
    let helpers = builtins::shipped_helpers();
    for scope in [source.join("fns")]
        .into_iter()
        .chain(if source.join("projects").exists() {
            files::entries(&source.join("projects"))?
                .into_iter()
                .map(|p| p.join("fns"))
                .collect()
        } else {
            Vec::new()
        })
    {
        if !scope.exists() {
            continue;
        }
        for (relative, hash) in &helpers {
            let file = scope.join(relative);
            if file.exists() && artifacts::fingerprint(&files::read(&file)?) != *hash {
                conflicts.push(json!({"helper":relative,"scope":scope.strip_prefix(source)?,"reason":"locally edited shipped helper; native builtin replacement requires owner review"}));
            }
        }
        for path in files::entries(&scope)? {
            let name = path
                .file_name()
                .context("fn name missing")?
                .to_str()
                .context("non-UTF8 fn name")?;
            if name.starts_with('_') || name.starts_with('.') || !path.join("fn.json").exists() {
                continue;
            }
            if let Some(expected) = shipped.get(name) {
                for file in files::entries(&path)? {
                    let relative = file
                        .file_name()
                        .context("fn filename missing")?
                        .to_str()
                        .context("non-UTF8 fn filename")?;
                    if relative == "__pycache__"
                        || relative.starts_with('.')
                        || relative.ends_with(".pyc")
                    {
                        continue;
                    }
                    if !expected.contains_key(relative) {
                        conflicts.push(json!({"fn":name,"file":relative,"reason":"extra file in shipped copy; native builtin replacement requires owner review"}));
                    }
                }
                for (relative, hash) in expected {
                    let file = path.join(relative);
                    if !file.is_file() || artifacts::fingerprint(&files::read(&file)?) != *hash {
                        conflicts.push(json!({"fn":name,"file":relative,"reason":"locally edited shipped copy; native builtin replacement requires owner review"}));
                    }
                }
            } else {
                let relative = path.strip_prefix(source)?;
                if !staging.join(relative).join("fn.json").exists() {
                    conflicts.push(json!({"fn":name,"scope":relative,"reason":"custom fn has no converted staging copy"}));
                }
            }
        }
    }
    for scope in [source.join("recipes")]
        .into_iter()
        .chain(if source.join("projects").exists() {
            files::entries(&source.join("projects"))?
                .into_iter()
                .map(|p| p.join("recipes"))
                .collect()
        } else {
            Vec::new()
        })
    {
        if !scope.exists() {
            continue;
        }
        for path in files::entries(&scope)? {
            if path.extension().is_some_and(|e| e == "json")
                && !staging.join(path.strip_prefix(source)?).exists()
            {
                conflicts.push(json!({"recipe":path.strip_prefix(source)?,"reason":"recipe has no converted staging copy; repost or stage explicitly"}));
            }
        }
    }
    Ok(conflicts)
}

fn verify_published(dst: &Path, ledger: &Value) -> Result<()> {
    let db = Connection::open_with_flags(dst.join("sluice.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let stored: String = db.query_row(
        "SELECT maintenance_settings FROM home_meta WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let stored: Value = serde_json::from_str(&stored)?;
    ensure!(
        stored["python_import"]["phase"] == "complete"
            && stored["python_import"]["import_id"] == ledger["import_id"],
        "database import completion is missing"
    );
    ensure!(
        stored["python_import"]["projects"] == ledger["projects"]
            && stored["python_import"]["questions"] == ledger["questions"],
        "database import mappings differ from the ledger"
    );
    for entry in ledger["files"]
        .as_array()
        .context("import file manifest missing")?
    {
        let relative = entry["path"]
            .as_str()
            .context("manifest path missing")?
            .trim_start_matches('/');
        ensure!(
            Path::new(relative)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_))),
            "invalid manifest path"
        );
        let file = dst.join(relative);
        ensure!(
            artifacts::fingerprint(&files::read(&file)?)
                == entry["sha256"]
                    .as_str()
                    .context("manifest fingerprint missing")?,
            "required import file missing or changed: {relative}"
        );
        ensure!(
            fs::metadata(file)?.mode() & 0o777
                == entry["mode"].as_u64().context("manifest mode missing")? as u32,
            "import file permissions changed"
        );
    }
    Ok(())
}
