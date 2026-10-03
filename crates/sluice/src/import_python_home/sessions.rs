//! Continuation metadata only. No Python lock, shim, PID or callback survives.
use super::files;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub fn checkpoint(src: &Path, project: &str, run: &str) -> Result<Option<Value>> {
    files::safe_component(project)?;
    files::safe_component(run)?;
    let path = src
        .join("projects")
        .join(project)
        .join("runs")
        .join(run)
        .join("native.json");
    if !path.exists() {
        return Ok(None);
    }
    let old = files::json(&path)?;
    let mut checkpoint = json!({});
    for key in [
        "engine",
        "session",
        "cwd",
        "head_before",
        "head_after",
        "git",
        "final",
    ] {
        if let Some(value) = old.get(key) {
            checkpoint[key] = value.clone();
        }
    }
    Ok(Some(checkpoint))
}

pub fn copy(
    src: &Path,
    home: &Path,
    destination: &Path,
    run: &str,
    checkpoint: &Value,
) -> Result<Option<Value>> {
    let Some(session) = checkpoint["session"].as_str().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let engine = checkpoint["engine"]
        .as_str()
        .context("checkpoint has no engine")?;
    let cwd = PathBuf::from(
        checkpoint["cwd"]
            .as_str()
            .context("checkpoint has no cwd")?,
    );
    let available = cwd.is_dir();
    let cwd = if available { cwd.canonicalize()? } else { cwd };
    let mut metadata = checkpoint.clone();
    if !available {
        // Keep the predecessor and checkpoint so the owner's later retry fails
        // with the actual missing cwd, never silently starts a fresh worker.
        metadata["validation_error"] = json!(format!("session cwd is missing: {}", cwd.display()));
    }
    match engine {
        "codex" => {
            files::safe_component(session)?;
            let mapping = files::json(
                &src.join("codex-native-sessions")
                    .join(format!("{session}.json")),
            )?;
            let mapped_cwd = recorded_cwd(
                mapping["cwd"]
                    .as_str()
                    .context("Codex mapping has no cwd")?,
            );
            ensure!(
                cwd == mapped_cwd,
                "Codex checkpoint cwd differs from session mapping"
            );
            let old_home = PathBuf::from(
                mapping["home"]
                    .as_str()
                    .context("Codex mapping has no home")?,
            )
            .canonicalize()?;
            ensure!(
                old_home.starts_with(src.join("codex-native-homes").canonicalize()?),
                "Codex private home escapes source snapshot"
            );
            let relative = format!("engine-homes/{run}");
            let private = home.join(&relative);
            files::directory(&private)?;
            let mut found = false;
            for path in files::entries(&old_home)? {
                let name = path.file_name().context("engine filename missing")?;
                let text = name.to_str().context("non-UTF8 engine filename")?;
                let required = [
                    "sessions",
                    "archived_sessions",
                    "session_index.jsonl",
                    "history.jsonl",
                ]
                .contains(&text)
                    || text.starts_with("state_")
                        && (text.ends_with(".sqlite")
                            || text.ends_with(".sqlite-wal")
                            || text.ends_with(".sqlite-shm"));
                if !required {
                    continue;
                }
                let meta = fs::symlink_metadata(&path)?;
                ensure!(
                    !meta.file_type().is_symlink(),
                    "required session state is a symlink: {}",
                    path.display()
                );
                if meta.is_dir() {
                    files::tree(&path, &private.join(name), &[], 0)?;
                } else {
                    files::write(
                        &private.join(name),
                        &files::read(&path)?,
                        meta.permissions().mode(),
                    )?;
                }
                if text == "sessions" {
                    found = contains_rollout(&path, session, 0)?;
                }
            }
            ensure!(
                found,
                "Codex session has no matching saved rollout: {session}"
            );
            rewrite_state_paths(&private, &old_home, &destination.join(&relative))?;
            // The runtime builds current configuration/hooks and takes a fresh
            // lock. Existing auth/skills links are inspected but never copied.
            let mut ownership = json!({});
            for name in [
                "auth.json",
                "skills",
                "memories",
                "rules",
                "prompts",
                "plugins",
            ] {
                let path = old_home.join(name);
                if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                    let target = fs::read_link(&path)?;
                    let absolute = if target.is_absolute() {
                        target
                    } else {
                        old_home.join(target)
                    };
                    if name == "auth.json" {
                        let resolved = absolute.canonicalize()?;
                        ensure!(
                            !resolved.starts_with(src),
                            "credential link is not externally owned"
                        );
                        ownership[name] = json!(resolved);
                    }
                }
            }
            metadata["external_credentials"] = ownership;
            metadata["private_home"] = json!(destination.join(&relative));
            metadata["python_mapping"] = json!({"home":destination.join(&relative),"cwd":cwd});
            files::write(
                &private.join("session.json"),
                &serde_json::to_vec_pretty(
                    &json!({"home":destination.join(&relative),"cwd":cwd,"session":session}),
                )?,
                0o600,
            )?;
        }
        "claude" => {
            files::safe_component(session)?;
            let config = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| owner_home().join(".claude"));
            let mut found = None;
            for project in files::entries(&config.join("projects"))? {
                let transcript = project.join(format!("{session}.jsonl"));
                if !transcript.is_file() {
                    continue;
                }
                for line in files::read(&transcript)?.split(|b| *b == b'\n') {
                    if let Ok(rec) = serde_json::from_slice::<Value>(line)
                        && let Some(old_cwd) = rec["cwd"].as_str()
                    {
                        ensure!(recorded_cwd(old_cwd) == cwd, "Claude session cwd differs");
                        found = Some(transcript.clone());
                        break;
                    }
                }
            }
            metadata["external_store"] = json!(found.context("Claude session transcript missing")?);
        }
        "devin" => {
            let db = owner_home().join(".local/share/devin/cli/sessions.db");
            // This is an external engine-owned store, not the Python home.
            // Validate only its one session row through the narrowly versioned
            // read-only adapter; copying the entire store leaks unrelated data.
            let con = rusqlite::Connection::open_with_flags(
                &db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )?;
            let key = session
                .rsplit('/')
                .next()
                .context("invalid Devin session")?;
            let recorded: String = con.query_row(
                "SELECT working_directory FROM sessions WHERE id=?1",
                [key],
                |r| r.get(0),
            )?;
            ensure!(recorded_cwd(&recorded) == cwd, "Devin session cwd differs");
            drop(con);
            metadata["external_store"] = json!(db);
        }
        _ => bail!("unsupported checkpoint engine {engine}"),
    }
    metadata["imported"] = json!(true);
    Ok(Some(
        json!({"engine":engine,"cwd":cwd,"session":session,"metadata":metadata}),
    ))
}

fn rewrite_state_paths(private: &Path, old: &Path, destination: &Path) -> Result<()> {
    for path in files::entries(private)? {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("state filename missing")?;
        if !name.starts_with("state_") || !name.ends_with(".sqlite") {
            continue;
        }
        let mut permissions = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let item = PathBuf::from(format!("{}{suffix}", path.display()));
            if !item.exists() {
                continue;
            }
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(0x20000)
                .open(&item)?;
            let mode = file.metadata()?.permissions();
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            permissions.push((file, mode));
        }
        let result = (|| -> Result<()> {
            let db = rusqlite::Connection::open(&path)?;
            let columns: Vec<String> = db
                .prepare("SELECT name FROM pragma_table_info('threads')")?
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            if columns.iter().any(|c| c == "rollout_path") {
                let prefix = format!("{}/", old.display());
                let replacement = format!("{}/", destination.display());
                db.execute("UPDATE threads SET rollout_path=?2 || substr(rollout_path,length(?1)+1) WHERE substr(rollout_path,1,length(?1))=?1",(&prefix,&replacement))?;
                // Python may have copied a pending generation to a named home
                // without updating engine-owned absolute rollout paths. Rewrite
                // only paths whose relative rollout actually exists in our copy.
                let paths: Vec<(String, String)> = db
                    .prepare("SELECT id,rollout_path FROM threads")?
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                for (id, path) in paths {
                    if let Some((_, relative)) = path.split_once("/sessions/") {
                        let relative = Path::new("sessions").join(relative);
                        ensure!(
                            relative
                                .components()
                                .all(|c| matches!(c, std::path::Component::Normal(_))),
                            "invalid engine rollout path"
                        );
                        if private.join(&relative).is_file() {
                            db.execute(
                                "UPDATE threads SET rollout_path=?2 WHERE id=?1",
                                (
                                    &id,
                                    destination
                                        .join(relative)
                                        .to_str()
                                        .context("non-UTF8 engine path")?,
                                ),
                            )?;
                        }
                    }
                }
            }
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            Ok(())
        })();
        for (file, permissions) in permissions {
            file.set_permissions(permissions)?;
        }
        result.with_context(|| format!("rewrite copied Codex state {}", path.display()))?;
    }
    Ok(())
}

fn contains_rollout(root: &Path, session: &str, depth: usize) -> Result<bool> {
    ensure!(depth <= 16, "rollout tree too deep");
    for path in files::entries(root)? {
        let meta = fs::symlink_metadata(&path)?;
        ensure!(!meta.file_type().is_symlink(), "rollout is a symlink");
        if meta.is_dir() {
            if contains_rollout(&path, session, depth + 1)? {
                return Ok(true);
            }
        } else if path.file_name().is_some_and(|s| {
            s.to_str()
                .is_some_and(|s| s.ends_with(&format!("-{session}.jsonl")))
        }) {
            let bytes = files::read(&path)?;
            for line in bytes.split(|b| *b == b'\n') {
                if let Ok(rec) = serde_json::from_slice::<Value>(line)
                    && rec["type"] == "session_meta"
                {
                    ensure!(
                        rec["payload"]["id"] == session,
                        "rollout session identity differs"
                    );
                    return Ok(true);
                }
            }
            bail!("matching rollout has no session metadata");
        }
    }
    Ok(false)
}
fn owner_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/sam"))
}
fn recorded_cwd(value: &str) -> PathBuf {
    Path::new(value)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(value))
}
