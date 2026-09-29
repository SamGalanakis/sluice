"""The Claude Code adapter: `claude` in its interactive TUI, on the user's own login.

Channels (see packs/README.md, "Claude", for the signals observed live):
- hooks: a per-run `--settings` file adds SessionStart, Stop, StopFailure and
  UserPromptSubmit hooks that append their JSON payload to `<run_dir>/hooks.jsonl`; the user's
  and the repo's settings stay as they are. SessionStart names the session and its transcript;
  each Stop/StopFailure is a turn end and carries `last_assistant_message`, the live
  `background_tasks` and the `session_crons`. SessionStart with source `compact` (after a
  compaction) also runs reprime.py, whose output re-primes the model with the step's context.
- the status file `<config dir>/sessions/<pane pid>.json` Claude Code keeps: `status` is
  `busy`, `waiting` (a dialog owns the input), `idle`, or `shell` (the turn ended but a
  background shell still runs).
- the transcript JSONL (and each subagent's under `<session>/subagents/`): progress lines,
  API errors, and the model's own ScheduleWakeup / CronCreate results.
- the pane: the composer the task and messages are pasted into."""

import json
import os
import re
import shlex
import sys
import time
from pathlib import Path

from .. import engines
from . import paste
from .processes import engine_env
from .supervisor import Adapter, Snapshot

HOOKS = ("SessionStart", "Stop", "StopFailure", "UserPromptSubmit")
TERMINAL = frozenset({"completed", "failed", "stopped", "killed"})  # background task statuses
WAKE_SLACK = 120.0  # seconds past a wakeup's time before it no longer counts as pending
EXIT_WAIT = 10.0
UNSTOPPED = 5.0  # seconds idle after a prompt with no Stop before the turn counts as ended


def config_dir():
    d = os.environ.get("CLAUDE_CONFIG_DIR")
    return Path(d).expanduser() if d else Path.home() / ".claude"


def _config_file():
    d = os.environ.get("CLAUDE_CONFIG_DIR")
    return (Path(d).expanduser() if d else Path.home()) / ".claude.json"


def _read_json(path):
    try:
        got = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return got if isinstance(got, dict) else None


# ---- the composer ------------------------------------------------------------------------------
# Adapted from Omnigent (https://github.com/omnigent-ai/omnigent),
# omnigent/harnesses/claude_native/bridge.py: _is_box_rule, _composer_row, _occupying_surface,
# _claude_prompt_rendered, _history_search_footer_shown, _draft_in_input_box.
# Copyright (2026) Databricks, Inc. Licensed under the Apache License, Version 2.0
# (http://www.apache.org/licenses/LICENSE-2.0). Changed: condensed, docstrings shortened.

GLYPH = "❯"
MODE_GLYPHS = (GLYPH, "!")
RULE_CHARS = frozenset("─━╭╮╰╯│┃╌╍")
EDGE_GLYPHS = "─━╭╮╰╯╌╍"
MIN_TITLED_RULE = 20
PASTED = "[Pasted text"
HISTORY_FOOTERS = ("search prompts:", "no matching prompt:")


def is_box_rule(line):
    """A row of box-drawing rule glyphs, or such a rule broken by a spaced title."""
    s = line.strip()
    if len(s) < 3:
        return False
    if all(ch in RULE_CHARS for ch in s):
        return True
    lead = len(s) - len(s.lstrip(EDGE_GLYPHS))
    trail = len(s) - len(s.rstrip(EDGE_GLYPHS))
    if lead < 1 or trail < 1 or len(s) < MIN_TITLED_RULE:
        return False
    label = s[lead:len(s) - trail]
    if any(ch in RULE_CHARS for ch in label):
        return False
    return label.startswith(" ") and label.endswith(" ") and bool(label.strip())


def composer_row(pane):
    """The live input box's row (under the opening rule of the last framed box, led by a
    composer glyph), or None when no input box is on screen."""
    rows = [ln for ln in pane.splitlines() if ln.strip()]
    rules = [i for i, ln in enumerate(rows) if is_box_rule(ln)]
    if not rules:
        return None
    candidates = ([rules[-2] + 1] if len(rules) >= 2 else []) + [rules[-1] + 1]
    for i in candidates:
        if i < len(rows) and rows[i].strip()[:1] in MODE_GLYPHS:
            return rows[i]
    return None


def history_search_shown(pane):
    rows = [ln for ln in pane.splitlines() if ln.strip()]
    last = max((i for i, ln in enumerate(rows) if is_box_rule(ln)), default=None)
    region = rows[last + 1:] if last is not None else rows[-8:]
    fragments = (re.split(r"\s{2,}", ln.strip(), maxsplit=1)[0] for ln in region)
    return " ".join(fragments).lower().startswith(HISTORY_FOOTERS)


class Composer:
    """Reads Claude Code's input box off the pane."""

    @staticmethod
    def ready(pane):
        row = composer_row(pane)
        return (row is not None and row.strip().startswith(GLYPH)
                and not history_search_shown(pane))

    @staticmethod
    def occupied(pane):
        if not pane.strip():
            return None
        if history_search_shown(pane):
            return "the prompt-history search"
        row = composer_row(pane)
        if row is None:
            return "an overlay"
        return None if row.strip().startswith(GLYPH) else "shell mode"

    @staticmethod
    def draft_visible(pane, needle):
        """Whether the draft sits in the input box: the text after the last glyph and the
        rows below it up to the box's closing rule. The box wraps a long draft at its own
        width, sometimes starting on the row under the glyph (Claude Code 2.1.283), so the
        needle is matched with all whitespace removed."""
        rows = [ln for ln in pane.splitlines() if ln.strip()]
        at = max((i for i, ln in enumerate(rows) if GLYPH in ln), default=None)
        if at is None:
            return False
        box = [rows[at].rsplit(GLYPH, 1)[1]]
        for ln in rows[at + 1:at + 8]:
            if is_box_rule(ln):
                break
            box.append(ln)
        text = "".join("".join(box).split())
        return PASTED.replace(" ", "") in text or (bool(needle)
                                                  and "".join(needle.split()) in text)


# ---- progress ----------------------------------------------------------------------------------

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


def summarize(entry):
    """One short line per thing worth seeing in a transcript entry: assistant text, tool
    calls, tool errors, API errors, background-task notifications, wakeups."""
    kind = entry.get("type")
    msg = entry.get("message") if isinstance(entry.get("message"), dict) else {}
    content = msg.get("content")
    lines = []
    if kind == "assistant":
        if entry.get("isApiErrorMessage"):
            return [f"error {_one_line(_text_of(content), 160)}"]
        for block in content if isinstance(content, list) else []:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "text" and block.get("text", "").strip():
                lines.append(_one_line(block["text"], 160))
            elif block.get("type") == "tool_use":
                lines.append(f"tool {block.get('name')} {_tool_arg(block.get('input') or {})}")
    elif kind == "user":
        origin = entry.get("origin") if isinstance(entry.get("origin"), dict) else {}
        if origin.get("kind") == "task-notification":
            m = re.search(r"<summary>(.*?)</summary>", _text_of(content), re.DOTALL)
            lines.append("task notification" + (f": {_one_line(m.group(1), 140)}" if m else ""))
        for block in content if isinstance(content, list) else []:
            if isinstance(block, dict) and block.get("is_error"):
                lines.append("tool error " + _one_line(_text_of(block.get("content")), 160))
    elif kind == "system" and entry.get("subtype") == "scheduled_task_fire":
        lines.append("wakeup: " + _one_line(entry.get("content") or "", 140))
    return lines


class Tail:
    """The complete JSON lines appended to a file since the last read."""

    def __init__(self, path, pos=0):
        self.path, self.pos, self.buf = Path(path), pos, b""

    def read(self):
        try:
            size = self.path.stat().st_size
        except OSError:
            return []
        if size < self.pos:
            self.pos, self.buf = 0, b""
        if size == self.pos:
            return []
        with open(self.path, "rb") as f:
            f.seek(self.pos)
            data = f.read(size - self.pos)
        self.pos += len(data)
        *lines, self.buf = (self.buf + data).split(b"\n")
        out = []
        for line in lines:
            try:
                rec = json.loads(line)
            except ValueError:
                continue
            if isinstance(rec, dict):
                out.append(rec)
        return out


# ---- the adapter -------------------------------------------------------------------------------

class Claude(Adapter):
    name = "claude"
    wait_signal = True
    transient = engines.CLAUDE_TRANSIENT + ("usage limit", "hit your limit")

    def __init__(self, model="opus"):
        self.model = model
        self.lines = []
        self.turns = 0
        self.starts = 0
        self.last_event = ""
        self.turn_end = None  # the payload of the latest Stop or StopFailure
        self.sid = ""
        self.transcript = None
        self.tails = {}  # transcript path -> Tail
        self.tool_names = {}  # tool_use id -> tool name
        self.wakeup = None  # epoch seconds of the model's pending ScheduleWakeup
        self.my_crons = set()  # ids of one-shot jobs the model made with CronCreate
        self.api_error = ""
        self.last_text = ""
        self.status_path = None
        self.cwd = None
        self.trust_at = 0.0
        self.seen = {}  # transcript path -> its size before this run (a resumed session's)
        self.unstopped = None  # since when the status is idle with the prompt's Stop missing

    # launch
    def prepare(self, run_dir, cwd, session):
        self.run_dir, self.cwd, self.resume = Path(run_dir).resolve(), cwd, session
        self.hooks_file = self.run_dir / "hooks.jsonl"
        self.hooks_file.write_text("")
        self.hooks = Tail(self.hooks_file)
        append = shlex.quote(str(self.hooks_file))
        command = ("p=$(cat | tr -d '\\r\\n'); "
                   f"[ -n \"$p\" ] && printf '%s\\n' \"$p\" >> {append}; :")
        hook = {"hooks": [{"type": "command", "command": command}]}
        hooks = {h: [hook] for h in HOOKS}
        hooks["SessionStart"].append({"matcher": "compact", "hooks": [
            {"type": "command", "command": self._reprime_command()}]})
        self.settings = self.run_dir / "claude-settings.json"
        self.settings.write_text(json.dumps({"hooks": hooks}, indent=2))
        for path in (config_dir() / "projects").glob(f"*/{session}.jsonl") if session else []:
            sub = path.with_suffix("") / "subagents"
            for f in [path, *(sorted(sub.glob("agent-*.jsonl")) if sub.is_dir() else [])]:
                self.seen[f] = f.stat().st_size  # history: not this run's progress

    def _reprime_command(self):
        """reprime.py, run by this fn's own interpreter (it needs only the standard library)."""
        script = Path(__file__).with_name("reprime.py")
        return " ".join(shlex.quote(str(a)) for a in (sys.executable, script,
                                                       self.run_dir / "task.md"))

    def argv(self):
        argv = [os.environ.get("SLUICE_CLAUDE_BIN", "claude"), "--model", self.model,
                "--dangerously-skip-permissions", "--disallowedTools", "AskUserQuestion",
                "--settings", str(self.settings)]
        return argv + (["--resume", self.resume] if self.resume else [])

    def env(self):
        return engine_env()

    def wait_ready(self, tmux, timeout):
        paste.wait_ready(tmux, Composer, timeout, on_pane=lambda pane: self._trust(tmux, pane))

    def _trust(self, tmux, pane):
        """Answer the workspace-trust dialog of a new directory with yes (its default is
        "No, exit"): the step's cwd is the plan's own choice, as it was for `claude -p`."""
        if "Yes, I trust this folder" not in pane or time.monotonic() - self.trust_at < 0.5:
            return
        self.trust_at = time.monotonic()
        chosen = next((ln for ln in pane.splitlines() if GLYPH in ln), "")
        tmux.keys("Enter" if "Yes, I trust" in chosen else "Down")

    def deliver(self, tmux, text):
        paste.deliver(tmux, text, Composer)

    # state
    def _read_hooks(self):
        for rec in self.hooks.read():
            event = rec.get("hook_event_name", "")
            if rec.get("session_id"):
                self.sid = rec["session_id"]
            path = rec.get("transcript_path")
            if isinstance(path, str) and path and "/subagents/" not in path:
                self.transcript = Path(path)
            if event in ("Stop", "StopFailure"):
                self.turns += 1
                self.turn_end = rec
            if event == "UserPromptSubmit":
                self.starts += 1
            if event in HOOKS:
                self.last_event = event

    def _read_transcripts(self):
        if self.transcript is None:
            return
        paths = [self.transcript]
        sub = self.transcript.with_suffix("") / "subagents"
        if sub.is_dir():
            paths += sorted(sub.glob("agent-*.jsonl"))
        for path in paths:
            tail = self.tails.setdefault(path, Tail(path, self.seen.get(path, 0)))
            main = path == self.transcript
            for entry in tail.read():
                indent = "" if main and not entry.get("isSidechain") else "  "
                self.lines += [indent + ln for ln in summarize(entry)]
                if main:
                    self._track(entry)

    def _track(self, entry):
        """Follow the model's own wakeups and scheduled jobs, API errors and last text."""
        kind = entry.get("type")
        msg = entry.get("message") if isinstance(entry.get("message"), dict) else {}
        content = msg.get("content") if isinstance(msg.get("content"), list) else []
        if kind == "assistant":
            if entry.get("isApiErrorMessage"):
                self.api_error = _text_of(msg.get("content"))
                return
            self.api_error = ""
            for b in content:
                if not isinstance(b, dict):
                    continue
                if b.get("type") == "tool_use":
                    self.tool_names[b.get("id")] = b.get("name")
                elif b.get("type") == "text" and b.get("text", "").strip():
                    self.last_text = b["text"]
        elif kind == "user" and isinstance(entry.get("toolUseResult"), dict):
            result = entry["toolUseResult"]
            ids = [b.get("tool_use_id") for b in content if isinstance(b, dict)]
            name = next((self.tool_names.get(i) for i in ids if i in self.tool_names), None)
            if name == "ScheduleWakeup" and isinstance(result.get("scheduledFor"), (int, float)):
                self.wakeup = result["scheduledFor"] / 1000
            elif name == "CronCreate" and result.get("id") and not result.get("recurring"):
                self.my_crons.add(result["id"])
        elif kind == "system" and entry.get("subtype") == "scheduled_task_fire":
            self.wakeup = None

    def _status(self, tmux):
        """The status file's `status` ("" when there is no readable file)."""
        if self.status_path is None:
            pid = tmux.pane_pid()
            d = config_dir() / "sessions"
            rec = _read_json(d / f"{pid}.json") if pid else None
            if rec and rec.get("kind") == "interactive" and (
                    not self.sid or rec.get("sessionId") == self.sid):
                self.status_path = d / f"{pid}.json"
        rec = _read_json(self.status_path) if self.status_path else None
        return rec.get("status", "") if rec else ""

    def _waiting(self, status):
        if status == "shell":
            return "a background shell is running"
        end = self.turn_end or {}
        live = [t for t in end.get("background_tasks") or []
                if not (isinstance(t, dict) and t.get("status") in TERMINAL)]
        # A background shell shows in the status file; trust the turn end's list for the other
        # kinds (background agents), or for all when there is no status file.
        unseen = [t for t in live if not status or not isinstance(t, dict)
                  or t.get("type") != "shell"]
        if unseen:
            t = unseen[0] if isinstance(unseen[0], dict) else {}
            return f"background task: {_one_line(t.get('description') or t.get('type'), 80)}"
        if self.wakeup and time.time() < self.wakeup + WAKE_SLACK:
            at = time.strftime("%H:%M:%S", time.localtime(self.wakeup))
            return f"a wakeup at {at}"
        crons = {c.get("id") for c in end.get("session_crons") or [] if isinstance(c, dict)}
        if crons & self.my_crons:
            return "a scheduled job"
        return ""

    def poll(self, tmux):
        # The status first, the hooks after: a Stop that lands in between is then seen with
        # the idle it precedes, never an idle read with the previous turn's Stop.
        dead = tmux.dead()
        status = self._status(tmux) if dead is None else ""
        self._read_hooks()
        self._read_transcripts()
        progress = (self.hooks.pos, tuple(t.pos for t in self.tails.values()))
        error = ""
        if self.turn_end and self.turn_end.get("hook_event_name") == "StopFailure":
            error = f"{self.turn_end.get('error', '')} " \
                    f"{self.turn_end.get('last_assistant_message') or ''}"
        if self.api_error and self.api_error not in error:
            error = f"{error} {self.api_error}".strip()
        if dead is not None:
            return Snapshot("exited", self.turns, progress=progress, error=error,
                            exit_status=dead)
        if status == "waiting":
            state = "blocked"
        elif status == "busy":
            state = "busy"
        elif status in ("idle", "shell") or self.last_event in ("Stop", "StopFailure"):
            state = "idle"
        else:
            state = "busy" if self.last_event == "UserPromptSubmit" else "starting"
        if state == "idle" and self.last_event == "UserPromptSubmit":
            # Idle, but the prompt's turn end is not in yet. An interrupted turn (Escape in
            # an attached terminal) ends without a Stop: past UNSTOPPED seconds it counts.
            self.unstopped = self.unstopped or time.monotonic()
            if time.monotonic() - self.unstopped < UNSTOPPED:
                state = "busy"
            else:
                self.turns, self.turn_end, self.last_event = self.turns + 1, None, "Stop"
        if self.last_event != "UserPromptSubmit":
            self.unstopped = None
        waiting = self._waiting(status) if state == "idle" else ""
        return Snapshot(state, self.turns, waiting=waiting, progress=progress, error=error,
                        starts=self.starts)

    def progress(self):
        out, self.lines = self.lines, []
        return out

    def session_id(self):
        return self.sid

    def final(self):
        end = self.turn_end or {}
        return end.get("last_assistant_message") or self.last_text

    def exit(self, tmux):
        tmux.keys("-l", "/exit")
        time.sleep(0.3)
        tmux.keys("Enter")
        deadline = time.monotonic() + EXIT_WAIT
        while time.monotonic() < deadline and tmux.dead() is None:
            time.sleep(0.2)

    def cost_usd(self):
        """The session's cost as Claude Code records it at exit (`lastCost` of the project in
        its config, when `lastSessionId` is this session), else None."""
        cfg = _read_json(_config_file()) or {}
        projects = cfg.get("projects") if isinstance(cfg.get("projects"), dict) else {}
        p = projects.get(self.cwd) or {}
        cost = p.get("lastCost")
        if p.get("lastSessionId") == self.sid and isinstance(cost, (int, float)):
            return float(cost)
        return None

    def session_cwd(self, session):
        """Where `session` was started: its transcript sits under the project dir Claude
        keys by cwd, and its entries record that cwd."""
        for path in (config_dir() / "projects").glob(f"*/{session}.jsonl"):
            with open(path, encoding="utf-8", errors="replace") as f:
                for line in f:
                    try:
                        rec = json.loads(line)
                    except ValueError:
                        continue
                    if isinstance(rec, dict) and isinstance(rec.get("cwd"), str):
                        return rec["cwd"]
        return None
