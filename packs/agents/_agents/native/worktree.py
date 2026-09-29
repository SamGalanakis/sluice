"""What a session did to its git worktree, read with a few cheap `git` calls that never take a
lock the agent's own git commands could trip over (GIT_OPTIONAL_LOCKS=0) and never prompt."""

import os
import subprocess

from .processes import GIT_ENV

TIMEOUT = 20.0


def git(cwd, *args):
    """The command's stdout, or None when it fails (not a git dir, git missing, a timeout)."""
    try:
        p = subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True,
                           timeout=TIMEOUT, stdin=subprocess.DEVNULL, check=False,
                           env={**os.environ, **GIT_ENV, "GIT_OPTIONAL_LOCKS": "0"})
    except (OSError, subprocess.SubprocessError):
        return None
    return p.stdout if p.returncode == 0 else None


def head(cwd):
    """HEAD's commit, or None outside a git worktree (or before its first commit)."""
    return (git(cwd, "rev-parse", "--verify", "-q", "HEAD") or "").strip() or None


def changes(cwd):
    """`git status --short` of the tracked files changed but not committed ("" when clean,
    None outside a git worktree)."""
    return git(cwd, "status", "--short", "--untracked-files=no")


def facts(cwd, before):
    """The git facts of a run that started at commit `before`: {head_before, head_after,
    commits (in before..after: HEAD moved by that many, whoever made them), dirty (tracked
    changes left uncommitted)}. None when either end is unknown."""
    after = head(cwd)
    if not (before and after):
        return None
    count = git(cwd, "rev-list", "--count", f"{before}..{after}")
    status = changes(cwd)
    return {"head_before": before, "head_after": after,
            "commits": int(count) if count and count.strip().isdigit() else 0,
            "dirty": bool(status and status.strip())}

