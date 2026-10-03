# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""figments.fork: kiln fork figments <name>, from origin/main (or --branch <base>), on an optional local branch."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from sluice.fn import run, sh

from _figlib import FORKS


def git(path, *args, check=True):
    return sh(["git", "-C", str(path), *args], check=check).stdout.strip()


def main(inp, ctx):
    existing = FORKS / inp["name"]
    if existing.is_dir():
        branch = git(existing, "branch", "--show-current") or None
        return {"path": str(existing), "head": git(existing, "rev-parse", "HEAD"),
                "branch": branch, "reused": True}
    opts = ["--no-build"] if inp.get("review") else []
    if inp.get("base"):
        opts += ["--branch", inp["base"].removeprefix("origin/")]
    res = sh(["kiln", "fork", *opts, "figments", inp["name"]])
    lines = [x.strip() for x in res.stdout.splitlines() if x.strip()]
    if not lines or not Path(lines[-1]).is_dir():
        raise RuntimeError(f"kiln fork printed no fork directory: {res.stdout.strip()!r}")
    path = lines[-1]
    if inp.get("branch"):
        git(path, "checkout", "-q", "-b", inp["branch"])
    return {"path": path, "head": git(path, "rev-parse", "HEAD"),
            "branch": inp.get("branch"), "reused": False}


if __name__ == "__main__":
    run(main)
