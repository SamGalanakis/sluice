# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""check.l0: formatting and whitespace hygiene. A red L0 is data (ok=false), not a failure."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "_lib"))

from lashkit import checks

from sluice.fn import run


def main(inp, ctx):
    s = checks.l0(inp["path"], inp.get("base"))
    return {"ok": s.ok, "summary": s.summary, "log": s.log, "failures": s.failures}


if __name__ == "__main__":
    run(main)
