# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""gh.run_cancel: cancel a workflow run; already-completed reports cancelled=false."""

import sys
from pathlib import Path

from sluice.fn import ShError, run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _git.refs import ref


def main(inp, ctx):
    argv = ["gh", "run", "cancel", ref("run_id", str(inp["run_id"]))]
    p = sh(argv, cwd=inp["path"], check=False)
    if p.returncode != 0 and "complet" not in (p.stderr + p.stdout).lower():
        raise ShError(argv, p.returncode, p.stdout, p.stderr)
    return {"cancelled": p.returncode == 0}


if __name__ == "__main__":
    run(main)
