"""The engine code the agents pack's fns share: claude's stream-json reader and runner,
codex's final-message reader and diff fold, and the markers that make a failure Transient.
A fn imports it relative to its own directory, like the jev pack's `_jev` (SPEC §10)."""

import json
import os
import sys
from pathlib import Path

from sluice.fn import ShError, Transient, echo_line, sh_stream

# Substrings that mark a failure worth retrying: in claude's stderr and stream errors
# (lowercased first) and decide.llm's stderr; in the codex harness log's tail.
CLAUDE_TRANSIENT = ("rate limit", "rate_limit", "overloaded", "529")
CODEX_TRANSIENT = ("rate limit", "429", "capacity")

CODEX_BIN = str(Path.home() / ".codex" / "bin" / "codex-harness-run")
CODEX_EFFORT = {"sol": "high", "astra": "high"}  # each model's default effort
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


def _session(log):
    """The session id the harness wrote next to its log ("" when it wrote none)."""
    f = Path(str(log) + ".session")
    return f.read_text().strip() if f.exists() else ""


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
    """Run claude -p with the prompt on stdin (off argv, where it would land in stderr.log and
    ps), echo its progress to stderr as it happens, return its result event."""
    argv = [
        os.environ.get("SLUICE_CLAUDE_BIN", "claude"),
        "-p",
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

    p = sh_stream(argv, on_line, cwd=cwd, check=False, input=prompt)
    failed = p.returncode != 0 or not results or results[-1].get("is_error")
    if failed:
        seen = (p.stderr + "\n" + "\n".join(errors)).lower()
        if any(m in seen for m in CLAUDE_TRANSIENT):
            raise Transient("claude hit a rate limit or capacity error")
        raise ShError(argv, p.returncode, p.stdout, p.stderr + "\n".join(errors)
                      or "claude printed no result")
    return results[-1]
