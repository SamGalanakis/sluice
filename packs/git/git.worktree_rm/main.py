# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""git.worktree_rm: remove a worktree; removed=false when the path is not one."""

import os

from sluice.fn import run, sh


def main(inp, ctx):
    repo = inp["repo"]
    target = os.path.realpath(inp["path"])
    listed = sh(["git", "-C", repo, "worktree", "list", "--porcelain"]).stdout
    paths = [
        line.split(" ", 1)[1]
        for line in listed.splitlines()
        if line.startswith("worktree ")
    ]
    if target not in {os.path.realpath(p) for p in paths}:
        return {"removed": False}
    argv = ["git", "-C", repo, "worktree", "remove"]
    if inp.get("force"):
        argv.append("--force")
    argv.append(inp["path"])
    sh(argv)
    return {"removed": True}


if __name__ == "__main__":
    run(main)
