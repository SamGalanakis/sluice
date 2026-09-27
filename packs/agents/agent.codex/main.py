# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.codex: run codex-harness-run on a spec in a working directory."""

import datetime as dt
import json
import os
import re
from pathlib import Path

from sluice.fn import ShError, Transient, echo_line, run, sh_stream
from sluice.log import last_seq

DEFAULT_BIN = str(Path.home() / ".codex" / "bin" / "codex-harness-run")
TRANSIENT_MARKERS = ("rate limit", "429", "capacity")
EFFORT = {"sol": "high", "astra": "high"}  # each model's default effort


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
    if model not in EFFORT:
        raise ValueError(f"codex models are {', '.join(EFFORT)}, got {model!r}")
    argv += ["--model", model, "--effort", inp.get("effort") or EFFORT[model]]
    if inp.get("session"):
        argv += ["--resume", inp["session"]]
    try:
        since = dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%S")
        fold = DiffFold()
        sh_stream(argv, on_line=fold, follow=log)  # the harness writes its progress to the log
        fold.flush()
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if any(m in tail for m in TRANSIENT_MARKERS):
            raise Transient("codex-harness-run hit a rate limit or capacity error") from e
        raise
    final = _codex_final(log, since)
    report = None
    report_path = inp.get("report_path")
    if report_path and Path(report_path).exists():
        report = Path(report_path).read_text()
    return {"log": str(log), "final": final, "report": report, "session": _session(log)}


def _session(log):
    """The session id the harness wrote next to its log ("" when it wrote none)."""
    f = Path(str(log) + ".session")
    return f.read_text().strip() if f.exists() else ""

CODEX_SESSIONS = Path.home() / ".codex" / "sessions"
TAIL = 4000  # of the log, when codex recorded no final message

DIFF_LINE = ("diff --git ", "index ", "--- ", "+++ ", "@@", "+", "-", " ", "new file mode",
             "deleted file mode", "old mode", "new mode", "similarity index", "rename from",
             "rename to", "Binary files", "\\ No newline")


class DiffFold:
    """sh_stream's on_line for a codex run: echoes each line as echo_line does, except the
    diff codex prints after every patch (the whole turn's diff so far, again and again: most
    of a long run's log), which becomes one line naming how many files and lines it held. The
    log keeps it all."""

    def __init__(self):
        self.files = self.lines = 0

    def __call__(self, line, source):
        if line.startswith("diff --git "):
            self.files += 1
        if self.files and (line.startswith(DIFF_LINE) or not line.strip()):
            self.lines += 1
            return
        self.flush()
        echo_line(line, source)

    def flush(self):
        if self.files:
            echo_line(f"(a diff of {self.files} files, {self.lines} lines: in the log)", "")
            self.files = self.lines = 0


def _codex_final(log, since):
    """The agent's own last message of this run: the `last_agent_message` of the last
    `task_complete` codex recorded (at or after `since`, an ISO time) in its session's
    rollout (~/.codex/sessions/*/*/*/rollout-*-<id>.jsonl; the harness writes the id to
    <log>.session). The log's last TAIL characters when there is none."""
    sid = _session(log)
    root = Path(os.environ.get("SLUICE_CODEX_SESSIONS") or CODEX_SESSIONS)
    final = None
    for rollout in sorted(root.glob(f"*/*/*/rollout-*-{sid}.jsonl")) if sid else []:
        with rollout.open(encoding="utf-8", errors="replace") as f:
            for line in f:
                if '"task_complete"' not in line:
                    continue
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                p = rec.get("payload") if isinstance(rec, dict) else None
                if (isinstance(p, dict) and p.get("type") == "task_complete"
                        and isinstance(p.get("last_agent_message"), str)
                        and str(rec.get("timestamp", "")) >= since):
                    final = p["last_agent_message"]
    if final is not None:
        return final
    return log.read_text()[-TAIL:] if log.exists() else ""


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
