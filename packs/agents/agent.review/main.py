# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.review: review `git diff <base>...HEAD` against the standards file, fix issues."""

import json
import os
import re
import sys

from sluice.fn import ShError, Transient, echo_line, run, sh, sh_stream
from sluice.log import last_seq

TRANSIENT_MARKERS = ("rate limit", "rate_limit", "overloaded", "529")


def _with_step_thread(text, ctx, listen):
    """Append the step-thread instructions when running as a plan step."""
    if listen is False or not (ctx.project and ctx.step):
        return text
    thread = "step-" + re.sub(r"[^a-z0-9_-]", "-", ctx.step.lower())
    since = last_seq(ctx.home / "projects" / ctx.project)
    read = (f'{{"project": "{ctx.project}", "threads": ["{thread}"], '
            f'"since_seq": {since}}}')
    post = (f'{{"name": "thread.post", "project": "{ctx.project}", "direct": true, '
            f'"inputs": {{"thread": "{thread}", "from": "{ctx.step}", '
            f'"to": "orchestrator", "body": "..."}}}}')
    return text + (
        f"\n\nMessages for you arrive on sluice thread `{thread}` of project "
        f"`{ctx.project}`. At natural pauses (between sub-tasks) check it with "
        f"`sluice tool log_read '{read}'`, and next time pass the `last_seq` it returns "
        f"as `since_seq`. Follow instructions addressed to you; ignore records not on "
        f"your thread. If you hit a question you cannot settle within your task, post "
        f"it with `sluice tool fn_call '{post}'` and continue with anything not blocked "
        f"by it."
    )


PROMPT = """\
You are reviewing a branch in the git repository at your working directory.

Review the diff `git diff {base}...HEAD` against the coding standards file at
{standards} (read it first, then the diff).{notes}

Rules:
- Fix every problem you find by editing files and committing. Use
  plain-sentence commit messages. Never add AI attribution of any kind: no
  Co-Authored-By trailers, no "Generated with" lines, no mention of any AI
  tool — not in commits, comments, docs, or tickets.
- Hunt for tautological tests and tests that cannot fail; fix or delete them.
- Do not rewrite history, do not force-push, do not push.
- Your final message is a short prose report of only what you could not fix."""


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
        if any(m in seen for m in TRANSIENT_MARKERS):
            raise Transient("claude hit a rate limit or capacity error")
        raise ShError(argv, p.returncode, p.stdout, p.stderr + "\n".join(errors)
                      or "claude printed no result")
    return results[-1]


def main(inp, ctx):
    cwd = inp["cwd"]
    before = sh(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
    notes = ""
    if inp.get("notes"):
        notes = "\n\nAdditional notes from the caller:\n" + inp["notes"]
    prompt = PROMPT.format(base=inp["base"], standards=inp["standards"], notes=notes)
    data = claude(_with_step_thread(prompt, ctx, inp.get("listen")), "opus", cwd)
    sha = sh(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
    commits = int(
        sh(["git", "rev-list", "--count", f"{before}..{sha}"], cwd=cwd).stdout.strip())
    return {"summary": data["result"], "sha": sha, "commits": commits}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
