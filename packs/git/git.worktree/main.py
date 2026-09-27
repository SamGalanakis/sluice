# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""git.worktree: add a worktree for a branch, creating the branch at base if needed."""

import sys
from pathlib import Path

from sluice.fn import run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _git.refs import ref


def main(inp, ctx):
    repo = Path(inp["repo"]).resolve()
    branch = ref("branch", inp["branch"])
    base = ref("base", inp["base"])
    if inp.get("path"):
        path = Path(inp["path"])
        if not path.is_absolute():
            path = repo / path
        path = path.resolve()
    else:
        path = repo.parent / f"{repo.name}-wt" / branch
    path.parent.mkdir(parents=True, exist_ok=True)
    exists = sh(
        ["git", "-C", str(repo), "show-ref", "--verify", "--quiet",
         f"refs/heads/{branch}"],
        check=False,
    ).returncode == 0
    if exists:
        sh(["git", "-C", str(repo), "worktree", "add", str(path), branch])
    else:
        sh(["git", "-C", str(repo), "worktree", "add",
            "-b", branch, str(path), base])
    sha = sh(["git", "-C", str(path), "rev-parse", "HEAD"]).stdout.strip()
    return {"path": str(path), "branch": branch, "sha": sha}


if __name__ == "__main__":
    run(main)
