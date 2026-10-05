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

/// How long the work-tree summary may take before the launch goes on without it.
pub const WORKTREE_TIMEOUT: Duration = Duration::from_secs(3);

/// `cwd` as `git status --porcelain` sees it now, within `timeout`: the count of entries and
/// the first `sluice_model::attempt::PATHS` of them. None when `cwd` is not a directory or not
/// a git work tree. Never an error: git absent, failing or too slow is `Unavailable`.
pub async fn worktree(
    git: &Path,
    cwd: &Path,
    timeout: Duration,
) -> Option<sluice_model::attempt::WorkTreeState> {
    use sluice_model::attempt::{PATHS, WorkTreeState};
    if !cwd.is_dir() {
        return None;
    }
    let mut cmd = tokio::process::Command::new(git);
    cmd.current_dir(cwd)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=normal"])
        .stdin(Stdio::null())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => {
            return Some(WorkTreeState::Unavailable {
                reason: format!("git status took longer than {}s", timeout.as_secs_f64()),
            });
        }
        Ok(Err(e)) if e.kind() == io::ErrorKind::NotFound => {
            return Some(WorkTreeState::Unavailable {
                reason: "git is not installed or not on PATH".into(),
            });
        }
        Ok(Err(e)) => {
            return Some(WorkTreeState::Unavailable {
                reason: format!("git could not run: {e}"),
            });
        }
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("not a git repository") {
            return None;
        }
        let line = stderr.lines().next().unwrap_or("").trim();
        return Some(WorkTreeState::Unavailable {
            reason: if line.is_empty() {
                format!("git status failed ({})", out.status)
            } else {
                line.chars().take(200).collect()
            },
        });
    }
    let mut entries = Vec::new();
    let mut parts = out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let Some(part) = parts.next() {
        let entry = String::from_utf8_lossy(part);
        let (status, path) = entry.split_at(entry.len().min(3));
        entries.push(format!("{} {path}", status.trim()));
        // A rename or copy is followed by its source path.
        if part.first().is_some_and(|c| *c == b'R' || *c == b'C') {
            let _ = parts.next();
        }
    }
    Some(if entries.is_empty() {
        WorkTreeState::Clean
    } else {
        WorkTreeState::Dirty {
            count: entries.len(),
            paths: entries.into_iter().take(PATHS).collect(),
        }
    })
}

/// The note's work tree for a step whose `cwd` input is `cwd`: `git` from PATH, bounded by
/// WORKTREE_TIMEOUT. None when there is no cwd or it is not a git work tree.
pub async fn worktree_of(cwd: Option<&str>) -> Option<sluice_model::attempt::WorkTree> {
    let cwd = cwd.filter(|c| !c.trim().is_empty())?;
    let state = worktree(Path::new("git"), Path::new(cwd), WORKTREE_TIMEOUT).await?;
    Some(sluice_model::attempt::WorkTree {
        cwd: cwd.to_owned(),
        state,
    })
}
