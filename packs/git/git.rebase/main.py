# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""git.rebase: `git rebase <onto>` in a worktree; abort and report conflicts on clash."""

import sys
from pathlib import Path

from sluice.fn import ShError, run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _git.refs import ref


def main(inp, ctx):
    path = inp["path"]
    onto = ref("onto", inp["onto"])
    rebased = sh(["git", "-C", path, "rebase", onto], check=False)
    if rebased.returncode != 0:
        in_rebase = sh(
            ["git", "-C", path, "rev-parse", "-q", "--verify", "REBASE_HEAD"],
            check=False,
        ).returncode == 0
        if not in_rebase:
            raise ShError(rebased.args, rebased.returncode,
                          rebased.stdout, rebased.stderr)
        conflicts = sh(
            ["git", "-C", path, "diff", "--name-only", "--diff-filter=U"]
        ).stdout.split()
        sh(["git", "-C", path, "rebase", "--abort"], check=False)
        sha = sh(["git", "-C", path, "rev-parse", "HEAD"]).stdout.strip()
        return {"ok": False, "sha": sha, "conflicts": conflicts}
    sha = sh(["git", "-C", path, "rev-parse", "HEAD"]).stdout.strip()
    return {"ok": True, "sha": sha, "conflicts": []}


if __name__ == "__main__":
    run(main)
