"""Resident Devin TUI adapter, using lifecycle hooks and a per-run config."""

import json
import os
import shlex
import shutil
import sqlite3
import time
from pathlib import Path

from . import paste
from .claude import Tail
from .processes import engine_env
from .supervisor import Adapter, Snapshot

HOOKS = ("SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "Stop",
         "PostCompaction", "SessionEnd")
READY = ("Ask Devin to build features, fix bugs, or work on your code",
         "Guide Devin while it works")
TRANSIENT = ("capacity issues", "rate limit", "rate_limit", "overloaded")
GUARDRAIL = ("You are running as a delegated worker. Nobody can answer questions, so make "
             "reasonable choices and record unresolved questions in your report. Follow "
             "AGENTS.md and CLAUDE.md. Work only in the requested directory. Never add "
             "co-author trailers or tool attribution. Never merge a PR; push only when the task "
             "says so.")
FUSION = "fusion-claude-opus-5-5-high-sidekick-swe-2-medium"
MODELS = {None: "swe-2-high", "swe-2-high": "swe-2-high", "high": "swe-2-high",
          "fusion": FUSION, FUSION: FUSION}


def _config_path():
    return Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "devin" / "config.json"


# Adapted from Omnigent (https://github.com/omnigent-ai/omnigent),
# omnigent/harnesses/devin_native/bridge.py: _strip_jsonc_comments.
# Copyright (2026) Databricks, Inc. Licensed under the Apache License, Version 2.0
# (http://www.apache.org/licenses/LICENSE-2.0). Changed: used only for the per-run config copy.
def _strip_jsonc_comments(raw):
    """Remove JSONC comments while leaving string contents unchanged."""
    out = []
    index = 0
    in_string = False
    while index < len(raw):
        char = raw[index]
        if in_string:
            out.append(char)
            if char == "\\" and index + 1 < len(raw):
                out.append(raw[index + 1])
                index += 2
                continue
            if char == '"':
                in_string = False
            index += 1
            continue
        if char == '"':
            in_string = True
        elif char == "/" and index + 1 < len(raw) and raw[index + 1] == "/":
            while index < len(raw) and raw[index] != "\n":
                index += 1
            continue
        elif char == "/" and index + 1 < len(raw) and raw[index + 1] == "*":
            index += 2
            while index + 1 < len(raw) and raw[index:index + 2] != "*/":
                index += 1
            index += 2
            continue
        out.append(char)
        index += 1
    return "".join(out)


def _read_config():
    path = _config_path()
    if not path.exists():
        return {}
    try:
        raw = path.read_text(encoding="utf-8")
        try:
            cfg = json.loads(raw)
        except ValueError:
            cfg = json.loads(_strip_jsonc_comments(raw))
        if not isinstance(cfg, dict):
            raise TypeError(f"Devin user config {path} must be an object")
        return cfg
    except ValueError as e:
        raise ValueError(f"cannot parse Devin user config {path}: {e}") from e


def _hook_config(command):
    return {event: [{"hooks": [{"type": "command", "command": command, "timeout": 30}],
                     **({"matcher": ""} if event in ("PreToolUse", "PostToolUse") else {})}]
            for event in HOOKS}


def _region(pane):
    lines = pane.splitlines()
    rules = [i for i, line in enumerate(lines) if "────" in line]
    if len(rules) >= 2:
        return "\n".join(lines[rules[-2] + 1:rules[-1]])
    return "\n".join(lines[-8:])


class Composer:
    """Read Devin's composer between its last two horizontal rules."""

    @staticmethod
    def ready(pane):
        return any(mark in " ".join(pane.split()) for mark in READY)

    @staticmethod
    def occupied(pane):
        return None if Composer.ready(pane) else "a dialog or menu"

    @staticmethod
    def draft_visible(pane, needle):
        region = _region(pane)
        plain = "".join(region.split()).lstrip("❭❯>")
        return bool(needle) and "".join(needle.split()) in plain


class Devin(Adapter):
    name = "devin"
    transient = TRANSIENT
    wait_signal = False

    def __init__(self, model=None, log=None):
        if model not in MODELS:
            raise ValueError(f"unknown Devin model {model!r}; accepted values: "
                             "swe-2-high (or high, the default), fusion (or "
                             f"{FUSION})")
        self.model = MODELS[model]
        self.log = Path(log) if log else None
        self.sid = ""
        self.turns = 0
        self.starts = 0
        self.active = False
        self.last = ""
        self.lines = []
        self.error = ""
        self.ended = False
        self.compactions = 0

    def session_key(self, session):
        """The session id `session` names: itself, or the id in `<session>.session` (a
        run's log path)."""
        path = Path(str(session) + ".session")
        return path.read_text().strip() if path.exists() else session

    def prepare(self, run_dir, cwd, session):
        self.run_dir = Path(run_dir).resolve()
        self.cwd = cwd
        self.resume = self.session_key(session) if session else None
        self.hooks_file = self.run_dir / "hooks.jsonl"
        self.hooks_file.write_text("")
        self.hooks = Tail(self.hooks_file)
        self.export_file = self.run_dir / "devin.json"
        self.export_file.unlink(missing_ok=True)
        self.log = self.log or self.run_dir / "devin.log"
        self.log.parent.mkdir(parents=True, exist_ok=True)
        self.log.write_text("")
        target = shlex.quote(str(self.hooks_file))
        cmd = "/bin/sh -c " + shlex.quote(f"cat >> {target}; printf '\n' >> {target}")
        cfg = _read_config()
        hooks = cfg.get("hooks")
        if hooks is not None and not isinstance(hooks, dict):
            raise ValueError("Devin user config hooks must be an object")
        hooks = dict(hooks or {})
        for event, entries in _hook_config(cmd).items():
            hooks[event] = [*(hooks.get(event) or []), *entries]
        cfg["hooks"] = hooks
        agent = cfg.get("agent")
        if agent is not None and not isinstance(agent, dict):
            raise ValueError("Devin user config agent must be an object")
        cfg["agent"] = {**(agent or {}), "model": self.model}
        self.config_file = self.run_dir / "devin-config.json"
        fd = os.open(self.config_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(cfg, f, indent=2)

    def env(self):
        return engine_env()

    def argv(self):
        args = [os.environ.get("SLUICE_DEVIN_BIN", "devin"), "--config", str(self.config_file),
                "--export", str(self.export_file), "--model", self.model,
                "--permission-mode", "dangerous", "--respect-workspace-trust", "false"]
        return args + (["--resume", self.resume] if self.resume else [])

    def wait_ready(self, tmux, timeout):
        paste.wait_ready(tmux, Composer, timeout)

    def deliver(self, tmux, text):
        paste.deliver(tmux, text, Composer)
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            pane = " ".join(tmux.capture().split()).lower()
            if "queued" in pane and "send now" in pane:
                tmux.keys("Enter")
                time.sleep(0.3)
            else:
                return

    def _read_hooks(self):
        for rec in self.hooks.read():
            event = rec.get("hook_event_name")
            if rec.get("session_id"):
                self.sid = rec["session_id"]
            if event == "UserPromptSubmit":
                self.starts += 1
                self.active = True
                self.error = ""
            elif event == "Stop":
                self.turns += 1
                self.active = False
                self.last = str(rec.get("last_assistant_message") or self.last)
                self.lines.append(" ".join(self.last.split())[:160])
                failure = str(rec.get("error") or "")
                if failure:
                    self.error = failure
            elif event == "SessionEnd":
                self.ended = True
            elif event == "PostCompaction":
                self.compactions += 1
                self.lines.append("context compacted")
            elif event == "PreToolUse":
                tool = str(rec.get("tool_name") or "tool")
                inp = rec.get("tool_input") or {}
                detail = next((str(inp[k]) for k in ("command", "path", "file_path", "prompt")
                               if isinstance(inp, dict) and k in inp), "")
                self.lines.append(f"tool {tool} {' '.join(detail.split())[:100]}".strip())
            elif event == "PostToolUse":
                response = rec.get("tool_response") or {}
                if isinstance(response, dict) and response.get("error"):
                    self.lines.append("tool error " + str(response["error"])[:160])

    def poll(self, tmux):
        self._read_hooks()
        if not self.sid and self.export_file.exists():
            try:
                self.sid = json.loads(self.export_file.read_text()).get("session_id") or ""
            except ValueError:
                pass
        dead = tmux.dead()
        if dead is not None or self.ended:
            state = "exited"
        elif self.active:
            state = "busy"
        elif self.turns:
            state = "idle"
        else:
            state = "starting"
        sizes = tuple(p.stat().st_size if p.exists() else 0
                      for p in (self.hooks_file, self.export_file))
        return Snapshot(state, self.turns, progress=sizes, error=self.error, starts=self.starts,
                        exit_status=str(dead or ""), compactions=self.compactions)

    def progress(self):
        out, self.lines = self.lines, []
        if out:
            with self.log.open("a") as f:
                f.write("\n".join(out) + "\n")
        return out

    def session_id(self):
        return self.sid

    def final(self):
        if self.last:
            return self.last
        try:
            steps = json.loads(self.export_file.read_text()).get("steps") or []
        except (OSError, ValueError):
            return ""
        for step in reversed(steps):
            if step.get("source") in ("agent", "assistant") and step.get("message"):
                msg = step["message"]
                return msg if isinstance(msg, str) else json.dumps(msg)
        return ""

    def exit(self, tmux):
        tmux.keys("-l", "/exit")
        tmux.keys("Enter")
        deadline = time.monotonic() + 5
        while tmux.dead() is None and time.monotonic() < deadline:
            time.sleep(0.2)

    def close(self):
        """Keep transcript and session artifacts even when the step fails."""
        if not hasattr(self, "export_file"):
            return
        if self.export_file.exists():
            shutil.copyfile(self.export_file, Path(str(self.log) + ".json"))
        Path(str(self.log) + ".final").write_text(self.final())
        Path(str(self.log) + ".session").write_text(self.sid + "\n")

    def session_cwd(self, session):
        session = self.session_key(session)
        db = Path.home() / ".local/share/devin/cli/sessions.db"
        if not db.exists():
            return None
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as con:
            row = con.execute("SELECT working_directory FROM sessions WHERE id = ?",
                              (session,)).fetchone()
        return row[0] if row else None
