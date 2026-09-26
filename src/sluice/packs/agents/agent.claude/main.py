# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.claude: run `claude -p` on a prompt in a working directory."""

import json
import os

from sluice.fn import ShError, Transient, run, sh

TRANSIENT_MARKERS = ("rate limit", "overloaded", "529")


def claude(prompt, model, cwd, session=None):
    """Run claude -p and return the parsed JSON object it prints on stdout."""
    argv = [
        os.environ.get("SLUICE_CLAUDE_BIN", "claude"),
        "-p", prompt,
        "--model", model,
        "--output-format", "json",
        "--dangerously-skip-permissions",
    ]
    if session:
        argv += ["--resume", session]
    p = sh(argv, cwd=cwd, check=False)
    if p.returncode != 0:
        if any(m in p.stderr for m in TRANSIENT_MARKERS):
            raise Transient("claude hit a rate limit or capacity error")
        raise ShError(argv, p.returncode, p.stdout, p.stderr)
    return json.loads(p.stdout)


def main(inp, ctx):
    data = claude(
        inp["prompt"], inp.get("model") or "opus", inp["cwd"], inp.get("session"))
    return {
        "result": data["result"],
        "session": data["session_id"],
        "cost_usd": data.get("total_cost_usd"),
    }


if __name__ == "__main__":
    run(main)
