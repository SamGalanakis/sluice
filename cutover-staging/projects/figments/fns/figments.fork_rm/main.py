# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""figments.fork_rm: kiln rm figments <name>, refusing a dirty fork or unpushed commits unless forced."""

from pathlib import Path

from sluice.fn import run, sh


def main(inp, ctx):
    path = Path(inp["path"])
    if not path.exists():
        return {"removed": False}
    if not inp.get("force"):
        dirty = sh(["git", "-C", str(path), "status", "--porcelain"]).stdout.strip()
        if dirty:
            raise RuntimeError(f"fork {inp['name']} has uncommitted changes (force to remove "
                               f"anyway):\n{dirty[:2000]}")
        sh(["git", "-C", str(path), "fetch", "-q", "origin"])
        remotes = sh(["git", "-C", str(path), "branch", "-r", "--contains", "HEAD"]).stdout.strip()
        if not remotes:
            ahead = sh(["git", "-C", str(path), "log", "--oneline", "origin/main..HEAD"]).stdout.strip()
            raise RuntimeError(f"fork {inp['name']} has commits on no remote branch (push them, or "
                               f"force to remove anyway):\n{ahead[:2000]}")
    sh(["kiln", "rm", "figments", inp["name"]])
    return {"removed": True}


if __name__ == "__main__":
    run(main)
