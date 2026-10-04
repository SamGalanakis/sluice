# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.fork: kiln fork lash <name>, optionally moved onto an unmerged base commit (stacking)."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from sluice.fn import run, sh

from _lashlib import net_sh


def main(inp, ctx):
    # A review fork (read-only studies) is cut with --no-build: env.sh is inert, so nothing
    # in it starts a Buck2 daemon or touches the build pool.
    argv = ["kiln", "fork"] + (["--no-build"] if inp.get("review") else []) + ["lash", inp["name"]]
    res = net_sh(argv)
    lines = [x.strip() for x in res.stdout.splitlines() if x.strip()]
    if not lines or not Path(lines[-1]).is_dir():
        raise RuntimeError(f"kiln fork printed no fork directory: {res.stdout.strip()!r}")
    path = lines[-1]
    if inp.get("base"):
        sh(["git", "-C", path, "checkout", "-q", "--detach", inp["base"]])
    head = sh(["git", "-C", path, "rev-parse", "HEAD"]).stdout.strip()
    return {"path": path, "head": head}


if __name__ == "__main__":
    run(main)
