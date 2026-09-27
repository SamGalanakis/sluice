# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""git.merge: merge source into target inside a temporary worktree under the run dir."""

import sys
from pathlib import Path

from sluice.fn import ShError, run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _git.refs import ref


def _checked_out(repo, branch):
    """True when refs/heads/<branch> is checked out in any worktree of repo."""
    out = sh(["git", "-C", repo, "worktree", "list", "--porcelain"]).stdout
    for block in out.strip().split("\n\n"):
        if f"branch refs/heads/{branch}" in block.splitlines():
            return True
    return False


def main(inp, ctx):
    repo = inp["repo"]
    source = ref("source", inp["source"])
    target = ref("target", inp["target"])
    wt = ctx.run_dir / "merge-wt"
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    old_sha = sh(["git", "-C", repo, "rev-parse", target]).stdout.strip()
    detached = _checked_out(repo, target)
    if detached:
        # target is checked out in another worktree: merge detached and move
        # the branch ref afterwards, guarded by its previous value.
        sh(["git", "-C", repo, "worktree", "add", "--detach", str(wt), target])
    else:
        sh(["git", "-C", repo, "worktree", "add", str(wt), target])
    try:
        message = inp.get("message") or f"merge {source} into {target}"
        merged = sh(
            ["git", "-C", str(wt), "merge", "--no-ff", "-m", message, source],
            check=False,
        )
        if merged.returncode != 0:
            in_merge = sh(
                ["git", "-C", str(wt), "rev-parse", "-q", "--verify", "MERGE_HEAD"],
                check=False,
            ).returncode == 0
            if not in_merge:
                raise ShError(merged.args, merged.returncode,
                              merged.stdout, merged.stderr)
            conflicts = sh(
                ["git", "-C", str(wt), "diff", "--name-only", "--diff-filter=U"]
            ).stdout.split()
            sh(["git", "-C", str(wt), "merge", "--abort"], check=False)
            return {"merged": False, "sha": None, "conflicts": conflicts}
        sha = sh(["git", "-C", str(wt), "rev-parse", "HEAD"]).stdout.strip()
        if detached:
            sh(["git", "-C", repo, "update-ref", f"refs/heads/{target}", sha, old_sha])
        if inp.get("push"):
            sh(["git", "-C", repo, "push", "origin", target])
        return {"merged": True, "sha": sha, "conflicts": []}
    finally:
        rm = sh(["git", "-C", repo, "worktree", "remove", "--force", str(wt)],
                check=False)
        if rm.returncode != 0:
            ctx.log(f"warning: could not remove temporary worktree {wt}")


if __name__ == "__main__":
    run(main)
