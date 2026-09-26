# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""jev.ask: several typed questions about one state in one Jev call."""

import sys
from pathlib import Path

from sluice.fn import run

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _jev.client import ask


def main(inp, ctx):
    resp = ask(inp["state"], inp["questions"], inp.get("model"))
    return {"answers": resp["answers"], "model": resp.get("model", ""),
            "usage": resp.get("usage", {})}


if __name__ == "__main__":
    run(main, retries=3, backoff=5)
