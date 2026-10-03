use super::{files, parser::Snapshot, sessions};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sluice_model::ids::{AttemptId, ProjectId, ResultId, RunId};
use std::path::Path;

pub fn identity(snapshot: &Snapshot, src: &Path, staging: &Path) -> Result<String> {
    let mut metadata = Vec::new();
    metadata.push(snapshot.dropped["calls_for_reconciliation"].clone());
    if src.join(".snapshot-origin.json").exists() {
        metadata.push(files::json(&src.join(".snapshot-origin.json"))?);
    }
    for project in &snapshot.projects {
        for step in project.plan["steps"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
        {
            for binding in step["in"].as_object().into_iter().flat_map(|m| m.values()) {
                if let Some(path) = binding["file"].as_str() {
                    let origin = if src.join(".snapshot-origin.json").exists() {
                        files::json(&src.join(".snapshot-origin.json"))?["home"]
                            .as_str()
                            .map(std::path::PathBuf::from)
                    } else {
                        None
                    };
                    let relative = Path::new(path).strip_prefix(src).ok().or_else(|| {
                        origin
                            .as_ref()
                            .and_then(|root| Path::new(path).strip_prefix(root).ok())
                    });
                    if let Some(relative) = relative {
                        metadata.push(json!({"binding":path,"sha256":sluice_store::artifacts::fingerprint(&files::read(&src.join(relative))?)}));
                    }
                }
            }
        }
        metadata.push(json!({"name":project.name,"settings":project.settings,"icon":project.icon.as_ref().map(|b|sluice_store::artifacts::fingerprint(b)),"plan":project.plan,"state":project.state,"questions":project.questions,"submissions":project.submissions}));
        for run in run_ids(&project.state) {
            let input = src
                .join("projects")
                .join(&project.name)
                .join("runs")
                .join(&run)
                .join("input.json");
            if input.exists() {
                metadata.push(json!({"run":run,"input":files::json(&input)?}));
            }
            if let Some(checkpoint) = sessions::checkpoint(src, &project.name, &run)? {
                metadata.push(json!({"run":run,"checkpoint":checkpoint}));
                if checkpoint["engine"] == "codex"
                    && let Some(session) = checkpoint["session"].as_str().filter(|s| !s.is_empty())
                {
                    files::safe_component(session)?;
                    let mapping = files::json(
                        &src.join("codex-native-sessions")
                            .join(format!("{session}.json")),
                    )?;
                    let private = Path::new(
                        mapping["home"]
                            .as_str()
                            .context("Codex mapping has no home")?,
                    )
                    .canonicalize()?;
                    ensure!(
                        private.starts_with(src.join("codex-native-homes").canonicalize()?),
                        "checkpoint home is outside snapshot"
                    );
                    // Root identity is logical. Copy rehearsals rewrite physical
                    // home paths, while bytes and session/cwd remain authoritative.
                    metadata.push(json!({"session":session,"cwd":mapping["cwd"]}));
                    for path in files::entries(&private)? {
                        let name = path
                            .file_name()
                            .context("session filename missing")?
                            .to_str()
                            .context("non-UTF8 filename")?;
                        if ["sessions", "archived_sessions"].contains(&name) {
                            files::identity_files(
                                &path,
                                &format!("session/{session}/{name}"),
                                &mut metadata,
                                0,
                            )?;
                        } else if ["history.jsonl", "session_index.jsonl"].contains(&name)
                            || name.starts_with("state_") && name.contains(".sqlite")
                        {
                            metadata.push(json!({"path":format!("session/{session}/{name}"),"sha256":sluice_store::artifacts::fingerprint(&files::read(&path)?)}));
                        }
                    }
                }
            }
        }
    }
    for scope in
        [src.join("fns"), src.join("recipes")]
            .into_iter()
            .chain(snapshot.projects.iter().flat_map(|p| {
                [
                    src.join("projects").join(&p.name).join("fns"),
                    src.join("projects").join(&p.name).join("recipes"),
                ]
            }))
    {
        if scope.exists() {
            source_assets(
                &scope,
                scope
                    .strip_prefix(src)?
                    .to_str()
                    .context("non-UTF8 source asset")?,
                &mut metadata,
                0,
            )?;
        }
    }
    files::identity_files(staging, "converted", &mut metadata, 0)?;
    // Secrets/config come only from converted staging. The combined identity
    // and private manifest never publish a secret's individual digest.
    Ok(sluice_store::artifacts::fingerprint(
        &sluice_model::hash::canonical_json(&json!(metadata))?,
    ))
}

pub fn allocate(snapshot: &Snapshot, import: &str, source: &Path, destination: &Path) -> Value {
    let mut projects = json!({});
    for project in &snapshot.projects {
        let mut steps = json!({});
        for id in project.plan["steps"]
            .as_object()
            .into_iter()
            .flat_map(|o| o.keys())
        {
            let old = &project.state["steps"][id];
            let mut predecessors = json!({});
            if old["status"] == "running" {
                let count = if project.plan["steps"][id]["scatter"].is_string() {
                    old["run_ids"].as_array().map_or(0, |a| a.len())
                } else {
                    1
                };
                for n in 0..count {
                    let index = if project.plan["steps"][id]["scatter"].is_string() {
                        n as i64
                    } else {
                        -1
                    };
                    predecessors[index.to_string()] =
                        json!({"run":RunId::new(),"attempt":AttemptId::new()});
                }
            }
            steps[id] =
                json!({"generation":1,"result":ResultId::new(),"predecessors":predecessors});
        }
        projects[&project.name] = json!({"id":ProjectId::new(),"original_paused":project.settings["paused"],"archived":project.settings["archived"],"steps":steps});
    }
    json!({"version":1,"import_id":import,"source":source,"destination":destination,"phase":"prepared","projects":projects,"questions":{}})
}

pub fn run_ids(state: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for step in state["steps"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        if step["status"] != "running" {
            continue;
        }
        for id in step["run_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !ids.iter().any(|s| s == id) {
                ids.push(id.into());
            }
        }
    }
    ids
}

fn source_assets(dir: &Path, prefix: &str, metadata: &mut Vec<Value>, depth: usize) -> Result<()> {
    ensure!(depth <= 32, "source asset tree too deep");
    for path in files::entries(dir)? {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("non-UTF8 asset filename")?;
        if name.starts_with('.') || name == "__pycache__" || name.ends_with(".pyc") {
            continue;
        }
        let key = format!("{prefix}/{name}");
        let file = std::fs::symlink_metadata(&path)?;
        ensure!(
            !file.file_type().is_symlink(),
            "source asset symlink needs explicit review: {} -> {}",
            path.display(),
            std::fs::read_link(&path).unwrap_or_default().display()
        );
        if file.is_dir() {
            source_assets(&path, &key, metadata, depth + 1)?;
        } else {
            metadata.push(json!({"source_asset":key,"sha256":sluice_store::artifacts::fingerprint(&files::read(&path)?)}));
        }
    }
    Ok(())
}
