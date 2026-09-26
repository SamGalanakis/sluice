# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.claude: run `claude -p` on a prompt in a working directory."""

import json
import os
import re

from sluice.fn import ShError, Transient, run, sh

TRANSIENT_MARKERS = ("rate limit", "overloaded", "529")


def _with_step_thread(text, ctx, listen):
    """Append the step-thread instructions when running as a plan step."""
    if listen is False or not (ctx.project and ctx.step):
        return text
    thread = "step-" + re.sub(r"[^a-z0-9_-]", "-", ctx.step.lower())
    read = (f'{{"project": "{ctx.project}", "threads": ["{thread}"], '
            f'"since_seq": <last>}}')
    post = (f'{{"name": "thread.post", "project": "{ctx.project}", "direct": true, '
            f'"inputs": {{"thread": "{thread}", "from": "{ctx.step}", '
            f'"to": "orchestrator", "body": "..."}}}}')
    return text + (
        f"\n\nMessages for you arrive on sluice thread `{thread}` of project "
        f"`{ctx.project}`. At each natural checkpoint run `sluice tool log_read "
        f"'{read}'` and follow instructions addressed to you. If you hit a question "
        f"you cannot settle within your task, post it with `sluice tool fn_call "
        f"'{post}'` and continue with anything not blocked by it."
    )


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
        _with_step_thread(inp["prompt"], ctx, inp.get("listen")),
        inp.get("model") or "opus", inp["cwd"], inp.get("session"))
    return {
        "result": data["result"],
        "session": data["session_id"],
        "cost_usd": data.get("total_cost_usd"),
    }


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
