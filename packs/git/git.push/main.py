# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""git.push: push HEAD:<branch> to a remote (default origin)."""

import sys
from pathlib import Path

from sluice.fn import run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _git.refs import ref


def main(inp, ctx):
    path = inp["path"]
    remote = ref("remote", inp.get("remote") or "origin")
    branch = ref("branch", inp["branch"])
    argv = ["git", "-C", path, "push"]
    if inp.get("force_with_lease"):
        argv.append("--force-with-lease")
    argv += [remote, f"HEAD:{branch}"]
    sh(argv)
    sha = sh(["git", "-C", path, "rev-parse", "HEAD"]).stdout.strip()
    return {"sha": sha}


if __name__ == "__main__":
    run(main)
