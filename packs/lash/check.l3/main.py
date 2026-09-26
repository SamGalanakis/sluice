# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""check.l3: the release gate. `//:workspace_tests` first, then the extra gates."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "_lib"))

from lashkit import checks

from sluice.fn import run

WORKSPACE_TESTS = {"kind": "kiln-test", "target": "//:workspace_tests"}


def main(inp, ctx):
    gates = [WORKSPACE_TESTS, *(inp.get("extra") or [])]
    return checks.run_gates(inp["path"], gates, inp.get("known_reds"),
                            allow_known=bool(inp.get("allow_known_reds")), level="L3")


if __name__ == "__main__":
    run(main)
