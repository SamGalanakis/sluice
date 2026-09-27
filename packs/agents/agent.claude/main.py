# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.claude: run `claude -p` on a prompt in a working directory."""

import sys
from pathlib import Path

from sluice.fn import run, with_step_notes

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _agents.engines import claude


def main(inp, ctx):
    if "model" in inp:  # an open fn would otherwise take it as an extra input
        raise ValueError("agent.claude always runs Opus; remove the model input")
    data = claude(
        with_step_notes(inp["prompt"], inp, ctx, inp.get("listen")),
        "opus", inp["cwd"], inp.get("session"))
    return {
        "result": data["result"],
        "session": data["session_id"],
        "cost_usd": data.get("total_cost_usd"),
    }


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
