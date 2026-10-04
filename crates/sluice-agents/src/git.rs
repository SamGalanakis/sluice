//! Git observations preserve the original baseline through every internal retry.
use serde::{Deserialize, Serialize};
use sluice_model::hash::ExecutionProvenance;
use std::{fs, io, path::Path, process::Stdio, time::Duration};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitFacts {
    pub head_before: String,
    pub head_after: String,
    pub commits: u64,
    pub dirty: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSample {
    pub head: String,
    pub status: Vec<u8>,
    pub fingerprint: String,
    pub tracked: String,
}
async fn git(cwd: &Path, args: &[&str]) -> io::Result<std::process::Output> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        // Observation must never take index.lock from under an agent's own git commands.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(10), cmd.output())
        .await
        .map_err(|_| io::Error::other("git probe timed out"))?
}
pub async fn head(cwd: &Path) -> io::Result<Option<String>> {
    let out = git(cwd, &["rev-parse", "--verify", "HEAD"]).await?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(
        String::from_utf8(out.stdout)
            .map_err(io::Error::other)?
            .trim()
            .into(),
    ))
}
pub async fn sample(cwd: &Path) -> io::Result<Option<GitSample>> {
    let Some(head) = head(cwd).await? else {
        return Ok(None);
    };
    let out = git(
        cwd,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .await?;
    if !out.status.success() {
        return Err(io::Error::other("git status failed"));
    }
    let mut fingerprint = Vec::new();
    let mut tracked = Vec::new();
    let mut parts = out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let Some(part) = parts.next() {
        if part.len() < 4 {
            return Err(io::Error::other("malformed git status"));
        }
        if !part.starts_with(b"??") {
            tracked.push(String::from_utf8_lossy(part).into_owned());
        }
        let path = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&part[3..]);
        fingerprint.extend_from_slice(part);
        if let Ok(meta) = fs::symlink_metadata(cwd.join(path)) {
            use std::os::unix::fs::MetadataExt;
            fingerprint.extend_from_slice(&meta.size().to_le_bytes());
            fingerprint.extend_from_slice(&meta.mtime().to_le_bytes());
            fingerprint.extend_from_slice(&meta.mtime_nsec().to_le_bytes());
        }
        if part[0] == b'R' || part[0] == b'C' {
            let _ = parts.next();
        }
    }
    Ok(Some(GitSample {
        head,
        status: out.stdout,
        fingerprint: ExecutionProvenance::fingerprint(&fingerprint).to_string(),
        tracked: tracked.join("; "),
    }))
}
pub async fn facts(cwd: &Path, before: Option<&str>) -> io::Result<Option<GitFacts>> {
    let Some(before) = before else {
        return Ok(None);
    };
    let after = sample(cwd)
        .await?
        .ok_or_else(|| io::Error::other("git worktree disappeared"))?;
    let range = format!("{before}..{}", after.head);
    let out = git(cwd, &["rev-list", "--count", &range, "--"]).await?;
    if !out.status.success() {
        return Err(io::Error::other(
            "original git baseline is no longer available",
        ));
    }
    let commits = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(io::Error::other)?;
    Ok(Some(GitFacts {
        head_before: before.into(),
        head_after: after.head,
        commits,
        dirty: !after.tracked.is_empty(),
    }))
}
