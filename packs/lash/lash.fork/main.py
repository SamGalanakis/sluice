# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.fork: `kiln fork lash <name>`, then a branch from the base in that fork."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "_lib"))

from lashkit import kiln

from sluice.fn import run, sh


def fork_path(stdout: str) -> str:
    """kiln prints the fork path (AGENTS.md: `F=$(kiln fork lash <name>)`); take the last
    line that is a directory, so progress chatter above it does not matter."""
    for line in reversed(stdout.strip().splitlines()):
        if line.strip() and Path(line.strip()).is_dir():
            return str(Path(line.strip()).resolve())
    raise RuntimeError(f"kiln fork printed no fork directory: {stdout.strip()[-500:]!r}")


def resolve_base(path: str, base: str) -> str:
    def verifies(ref: str) -> bool:
        p = sh(["git", "rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"], cwd=path,
               check=False)
        return p.returncode == 0

    # Prefer the freshly fetched remote ref over a possibly stale local branch of the same name.
    if not base.startswith("origin/") and verifies(f"refs/remotes/origin/{base}"):
        return f"origin/{base}"
    if verifies(base):
        return base
    raise RuntimeError(f"base {base!r} does not resolve to a commit in the fork")


def main(inp, ctx):
    name = inp["name"]
    p = sh([kiln(), "fork", "lash", name])
    path = fork_path(p.stdout)
    base = resolve_base(path, inp.get("base") or "origin/main")
    if inp.get("detach"):
        sh(["git", "checkout", "--detach", base], cwd=path)
        branch = ""
    else:
        branch = inp.get("branch") or name
        sh(["git", "switch", "-c", branch, base], cwd=path)
    sha = sh(["git", "rev-parse", "HEAD"], cwd=path).stdout.strip()
    return {"path": path, "branch": branch, "sha": sha, "base": base}


if __name__ == "__main__":
    run(main)
