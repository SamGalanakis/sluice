# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.codex: run codex-harness-run on a spec in a working directory."""

import json
import os
import re
from pathlib import Path

from sluice.fn import ShError, Transient, run, sh_stream
from sluice.log import last_seq

DEFAULT_BIN = str(Path.home() / ".codex" / "bin" / "codex-harness-run")
TRANSIENT_MARKERS = ("rate limit", "429", "capacity")
EFFORT = {"sol": "high", "astra": "high", "luna": "max"}  # each model's default effort


def _type(form):
    return form if isinstance(form, str) else json.dumps(form)


def _step_thread(ctx, listen):
    """The step-thread note: where messages for this step arrive and how to ask back."""
    if listen is False:
        return ""
    thread = "step-" + re.sub(r"[^a-z0-9_-]", "-", ctx.step.lower())
    since = last_seq(ctx.home / "projects" / ctx.project)
    read = (f'{{"project": "{ctx.project}", "threads": ["{thread}"], '
            f'"since_seq": {since}}}')
    post = (f'{{"name": "thread.post", "project": "{ctx.project}", "direct": true, '
            f'"inputs": {{"thread": "{thread}", "from": "{ctx.step}", '
            f'"to": "orchestrator", "body": "..."}}}}')
    return (
        f"Messages for you arrive on sluice thread `{thread}` of project "
        f"`{ctx.project}`. At natural pauses (between sub-tasks) check it with "
        f"`sluice tool log_read '{read}'`, and next time pass the `last_seq` it returns "
        f"as `since_seq`. Follow instructions addressed to you; ignore records not on "
        f"your thread. If you hit a question you cannot settle within your task, post "
        f"it with `sluice tool fn_call '{post}'` and continue with anything not blocked "
        f"by it. For a note that needs no answer (a decision you have already made, a "
        f"heads-up), add `\"needs_reply\": false` to the inputs. Post questions and changes "
        f"of scope, not progress."
    )


def _with_step_notes(text, inp, ctx, listen):
    """Append what a plan step adds to the task: its extra inputs with their values, the
    outputs it must submit (and how), and the step-thread note (unless listen is false)."""
    if not (ctx.project and ctx.step):
        return text
    parts = [text]
    if ctx.extra_inputs:
        lines = ["## Inputs"]
        for name, port in ctx.extra_inputs.items():
            value = inp.get(name)
            shown = value if isinstance(value, str) else json.dumps(value, indent=2)
            lines.append(f"`{name}` ({_type(port['type'])}):\n{shown}")
        parts.append("\n\n".join(lines))
    if ctx.outputs:
        lines = ["## Outputs you must submit"]
        for name, port in ctx.outputs.items():
            doc = f": {port['doc']}" if port.get("doc") else ""
            lines.append(f"- `{name}` ({_type(port['type'])}){doc}")
        values = ", ".join(f'"{n}": <{_type(p["type"])}>' for n, p in ctx.outputs.items())
        lines.append(
            "Submit them, as JSON values of those types, before you finish:\n"
            f"`sluice tool step_submit '{{\"project\": \"{ctx.project}\", "
            f"\"step\": \"{ctx.step}\", \"run\": \"{ctx.run_id}\", "
            f"\"outputs\": {{{values}}}}}'`\n"
            "If it returns `invalid`, fix what it lists and submit again (the last "
            "submission counts).")
        parts.append("\n".join(lines))
    parts.append(_step_thread(ctx, listen))
    return "\n\n".join(p for p in parts if p)


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(_with_step_notes(inp["spec"], inp, ctx, inp.get("listen")))
    log = Path(inp["log"]) if inp.get("log") else ctx.run_dir / "codex.log"
    argv = [
        os.environ.get("SLUICE_CODEX_BIN", DEFAULT_BIN),
        "--cd", inp["cwd"],
        "--spec", str(spec_file),
        "--log", str(log),
    ]
    model = inp.get("model") or "sol"
    argv += ["--model", model, "--effort", inp.get("effort") or EFFORT[model]]
    if inp.get("session"):
        argv += ["--resume", inp["session"]]
    try:
        sh_stream(argv, follow=log)  # the harness writes its progress to the log
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if any(m in tail for m in TRANSIENT_MARKERS):
            raise Transient("codex-harness-run hit a rate limit or capacity error") from e
        raise
    # codex writes <log>.session but no <log>.final: the last chunk of the log is the report.
    final = log.read_text()[-4000:] if log.exists() else ""
    report = None
    report_path = inp.get("report_path")
    if report_path and Path(report_path).exists():
        report = Path(report_path).read_text()
    return {"log": str(log), "final": final, "report": report, "session": _session(log)}


def _session(log):
    """The session id the harness wrote next to its log ("" when it wrote none)."""
    f = Path(str(log) + ".session")
    return f.read_text().strip() if f.exists() else ""


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
