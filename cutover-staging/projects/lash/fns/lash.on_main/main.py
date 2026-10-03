# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.on_main: wait for a commit to land on origin/main."""

import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _lashlib import on_main
from sluice_fn import run, sh


def find(repo, inp):
    if inp.get("sha"):
        return inp["sha"] if on_main(repo, inp["sha"]) else None
    sh(["git", "-C", repo, "fetch", "-q", "origin", "main"])
    out = sh(["git", "-C", repo, "log", "origin/main", "-1", "--format=%H",
              f"--grep={inp['grep']}"]).stdout.strip()
    return out or None


def main(inp, ctx):
    if not (inp.get("sha") or inp.get("grep")):
        raise ValueError("give sha or grep")
    repo, interval = inp["repo"], inp.get("interval") or 60
    deadline = time.time() + inp["timeout"] if inp.get("timeout") else None
    while (sha := find(repo, inp)) is None:
        if deadline is not None and time.time() >= deadline:
            raise TimeoutError(f"not on origin/main after {inp['timeout']}s")
        ctx.log(f"not on origin/main yet; checking again in {interval}s")
        time.sleep(interval)
    full = sh(["git", "-C", repo, "rev-parse", sha]).stdout.strip()
    at = sh(["git", "-C", repo, "show", "-s", "--format=%cI", full]).stdout.strip()
    return {"sha": full, "at": at}


if __name__ == "__main__":
    run(main)
