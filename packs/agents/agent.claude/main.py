# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.claude: run a prompt in a supervised interactive Claude session (Opus)."""

import sys
from pathlib import Path

from sluice.fn import run

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _agents.native import git_output, run_claude


def main(inp, ctx):
    if "model" in inp:  # an open fn would otherwise take it as an extra input
        raise ValueError("agent.claude always runs Opus; remove the model input")
    out = run_claude(inp["prompt"], inp, ctx, inp["cwd"])
    return {"result": out["final"], "session": out["session"], "cost_usd": out["cost_usd"],
            **git_output(out)}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
