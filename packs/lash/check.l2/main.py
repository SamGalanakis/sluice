# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""check.l2: named gates on an arc branch, judged against the known-red ledger."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "_lib"))

from lashkit import checks

from sluice.fn import run


def main(inp, ctx):
    if not inp["gates"]:
        raise ValueError("check.l2 needs at least one gate")
    return checks.run_gates(inp["path"], inp["gates"], inp.get("known_reds"),
                            allow_known=True, level="L2")


if __name__ == "__main__":
    run(main)
