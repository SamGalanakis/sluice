"""The engine code the agents pack's fns share outside the native sessions (`native/`):
codex's final-message reader and diff fold, and the markers that make a failure Transient.
A fn imports it relative to its own directory, like the jev pack's `_jev` (SPEC §10)."""

import json
import os
from pathlib import Path

from sluice.fn import echo_line

# Substrings that mark a failure worth retrying: in a Claude session's turn-ending error
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
