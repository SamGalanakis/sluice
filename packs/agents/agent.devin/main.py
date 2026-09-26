# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.devin: run devin-harness-run on a spec in a working directory."""

import os
import re
from pathlib import Path

from sluice.fn import ShError, Transient, run, sh


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


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(_with_step_thread(inp["spec"], ctx, inp.get("listen")))
    log = Path(inp["log"]) if inp.get("log") else ctx.run_dir / "devin.log"
    argv = [
        os.environ.get("SLUICE_DEVIN_BIN", "devin-harness-run"),
        "--cd", inp["cwd"],
        "--spec", str(spec_file),
        "--log", str(log),
    ]
    if inp.get("resume"):
        argv += ["--resume", inp["resume"]]
    try:
        sh(argv)
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if "capacity issues" in tail:
            raise Transient("devin-harness-run reported capacity issues") from e
        raise
    final_file = Path(str(log) + ".final")
    final = final_file.read_text() if final_file.exists() else ""
    report = None
    report_path = inp.get("report_path")
    if report_path and Path(report_path).exists():
        report = Path(report_path).read_text()
    return {"log": str(log), "final": final, "report": report}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
