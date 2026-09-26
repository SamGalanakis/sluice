# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.run: dispatch a spec to the engine named in the input (devin/codex/claude)."""

import json
import os
import re
import sys
from pathlib import Path

from sluice.fn import ShError, Transient, echo_line, run, sh_stream
from sluice.log import last_seq

CODEX_BIN = str(Path.home() / ".codex" / "bin" / "codex-harness-run")
CLAUDE_TRANSIENT = ("rate limit", "rate_limit", "overloaded", "529")
CODEX_TRANSIENT = ("rate limit", "429", "capacity")


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
        f"by it."
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


def _devin(inp, ctx):
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(inp["spec"])
    log = ctx.run_dir / "devin.log"
    argv = [
        os.environ.get("SLUICE_DEVIN_BIN", "devin-harness-run"),
        "--cd", inp["cwd"],
        "--spec", str(spec_file),
        "--log", str(log),
    ]
    if inp.get("model"):
        argv += ["--model", inp["model"]]
    if inp.get("session"):
        argv += ["--resume", inp["session"]]
    try:
        sh_stream(argv, follow=log)  # the harness writes its progress to the log
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if "capacity issues" in tail:
            raise Transient("devin-harness-run reported capacity issues") from e
        raise
    final_file = Path(str(log) + ".final")
    return {"final": final_file.read_text() if final_file.exists() else "",
            "session": _session(log)}


def _codex(inp, ctx):
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(inp["spec"])
    log = ctx.run_dir / "codex.log"
    argv = [
        os.environ.get("SLUICE_CODEX_BIN", CODEX_BIN),
        "--cd", inp["cwd"],
        "--spec", str(spec_file),
        "--log", str(log),
    ]
    if inp.get("model"):
        argv += ["--model", inp["model"]]
    if inp.get("session"):
        argv += ["--resume", inp["session"]]
    try:
        sh_stream(argv, follow=log)  # the harness writes its progress to the log
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if any(m in tail for m in CODEX_TRANSIENT):
            raise Transient("codex-harness-run hit a rate limit or capacity error") from e
        raise
    # codex writes <log>.session but no <log>.final: the last chunk of the log is the report.
    return {"final": log.read_text()[-4000:] if log.exists() else "", "session": _session(log)}


def _session(log):
    """The session id the harness wrote next to its log ("" when it wrote none)."""
    f = Path(str(log) + ".session")
    return f.read_text().strip() if f.exists() else ""


def _one_line(text, n):
    return " ".join(str(text).split())[:n]


def _text_of(content):
    if isinstance(content, list):
        return " ".join(b.get("text", "") for b in content if isinstance(b, dict))
    return str(content or "")


def _tool_arg(inp):
    """A short summary of a tool call's input: its path, command, pattern or url."""
    for key in ("file_path", "path", "notebook_path", "command", "pattern", "url", "query",
                "description", "prompt"):
        if isinstance(inp.get(key), str):
            return _one_line(inp[key], 100)
    return _one_line(json.dumps(inp), 100)


def _progress(ev):
    """One short line per thing worth seeing in a stream-json event: text, tool calls,
    errors, the end."""
    kind, lines = ev.get("type"), []
    if kind == "assistant":
        for block in (ev.get("message") or {}).get("content") or []:
            if block.get("type") == "text" and block.get("text", "").strip():
                lines.append(_one_line(block["text"], 160))
            elif block.get("type") == "tool_use":
                lines.append(f"tool {block.get('name')} {_tool_arg(block.get('input') or {})}")
        if ev.get("error"):
            lines.append(f"error {ev['error']}")
    elif kind == "user":
        for block in (ev.get("message") or {}).get("content") or []:
            if isinstance(block, dict) and block.get("is_error"):
                lines.append("tool error " + _one_line(_text_of(block.get("content")), 160))
    elif kind == "result":
        if ev.get("is_error"):
            lines.append(f"error {ev.get('subtype')}: {_one_line(ev.get('result') or '', 160)}")
        else:
            cost = ev.get("total_cost_usd")
            lines.append(f"done: {ev.get('num_turns')} turns"
                         + (f", ${cost:.4f}" if isinstance(cost, (int, float)) else ""))
    elif kind == "system" and ev.get("subtype") == "init":
        lines.append(f"session {ev.get('session_id')} model {ev.get('model')}")
    elif kind == "rate_limit_event":
        status = (ev.get("rate_limit_info") or {}).get("status", "")
        if not status.startswith("allowed"):
            lines.append(f"error rate limit {status}")
    indent = "  " if ev.get("parent_tool_use_id") else ""  # a subagent's events
    return [indent + line.rstrip() for line in lines]


def _error_text(ev):
    """The error an event reports, if any (for spotting rate limits and capacity errors)."""
    if ev.get("type") == "result" and ev.get("is_error"):
        return f"{ev.get('api_error_status') or ''} {ev.get('result') or ''}"
    if ev.get("type") == "assistant" and ev.get("error"):
        return f"{ev['error']} {_text_of((ev.get('message') or {}).get('content'))}"
    if ev.get("type") == "rate_limit_event":
        status = (ev.get("rate_limit_info") or {}).get("status", "")
        return "" if status.startswith("allowed") else f"rate limit {status}"
    return ""


def claude(prompt, model, cwd, session=None):
    """Run claude -p, echo its progress to stderr as it happens, return its result event."""
    argv = [
        os.environ.get("SLUICE_CLAUDE_BIN", "claude"),
        "-p", prompt,
        "--model", model,
        "--output-format", "stream-json",
        "--verbose",
        "--dangerously-skip-permissions",
    ]
    if session:
        argv += ["--resume", session]
    results, errors = [], []

    def on_line(line, source):
        try:
            ev = json.loads(line) if source == "stdout" else None
        except ValueError:
            ev = None
        if not isinstance(ev, dict):
            return echo_line(line, source)
        if ev.get("type") == "result":
            results.append(ev)
        if err := _error_text(ev):
            errors.append(err)
        for text in _progress(ev):
            print(text, file=sys.stderr, flush=True)

    p = sh_stream(argv, on_line, cwd=cwd, check=False)
    failed = p.returncode != 0 or not results or results[-1].get("is_error")
    if failed:
        seen = (p.stderr + "\n" + "\n".join(errors)).lower()
        if any(m in seen for m in CLAUDE_TRANSIENT):
            raise Transient("claude hit a rate limit or capacity error")
        raise ShError(argv, p.returncode, p.stdout, p.stderr + "\n".join(errors)
                      or "claude printed no result")
    return results[-1]


def _claude(inp):
    if inp.get("model"):
        raise ValueError("the claude engine always runs Opus; model is for codex and devin")
    data = claude(inp["spec"], "opus", inp["cwd"], inp.get("session"))
    return {"final": data["result"], "session": data["session_id"]}


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    inp = {**inp, "spec": _with_step_notes(inp["spec"], inp, ctx, inp.get("listen"))}
    engine = inp["engine"]
    if engine == "claude":
        out = _claude(inp)
    elif engine == "codex":
        out = _codex(inp, ctx)
    elif engine == "devin":
        out = _devin(inp, ctx)
    else:
        raise ValueError(f"unknown engine {engine!r}")
    report_path = inp.get("report_path")
    out["report"] = (
        Path(report_path).read_text()
        if report_path and Path(report_path).exists()
        else None
    )
    return out


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
