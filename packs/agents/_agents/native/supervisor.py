"""The engine-agnostic supervisor: it runs an engine's interactive session in a private tmux
server, delivers the task, and decides when the step is done; the model's end of turn does not.

An engine plugs in through an adapter (see `Adapter`). Per run:

1. start the server (socket `tmux.sock` in the run dir) running the adapter's command, print
   the `attach:` line, wait for the engine's input, deliver the task (a one-line pointer to
   `<run_dir>/task.md` unless it is one short line: see `hand_over`);
2. poll the adapter's state. When a turn has ended and the session is idle:
   - every required declared output is in `<run_dir>/submitted.json` → done;
   - the session waits on its own background work (a background shell, a pending wakeup) →
     keep waiting, no nudge;
   - the step declares no required outputs → done after `settle` seconds of idle;
   - otherwise, after `settle` seconds of idle → nudge, up to `nudges` times, then fail
     naming the missing outputs and the agent's last message;
   an engine with no waiting signal gets `grace` seconds of idle before the first of these;
3. type messages addressed to the step on its thread into the session as they arrive;
4. fail on the wall-clock cap, or after `stall` seconds without progress while busy;
5. on done ask the engine to exit, read `final`, `session` and `cost_usd`; in every case end
   the tmux server and every process under it (SIGTERM, SIGHUP and SIGINT included, so a
   `step_cancel` leaves nothing behind)."""

import contextlib
import json
import os
import re
import signal
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

from sluice import log as L
from sluice import types as T
from sluice.fn import Transient

from .paste import NotDelivered, tail
from .tmux import Tmux

NUDGE = ("Your turn ended but these outputs are not submitted: {names}. If you are waiting on "
         "something, wait for it in this turn. Otherwise finish and submit them with the "
         "command from your task, or submit what you have and explain the blocker in "
         "`unresolved`.")
POINTER = "Your task is in {path}; read it fully, then do it."
MESSAGE = "Message from {frm} on your sluice thread `{thread}`: {body}"
MESSAGE_FILE = "A message from {frm} on your sluice thread `{thread}` is in {path}; read it now."
INLINE_MAX = 500
CONTINUE = ("Your session was interrupted by a rate limit or capacity error. Continue your task "
            "where you left off.")


def _env_float(name, default):
    try:
        return float(os.environ.get(name, default))
    except ValueError:
        return float(default)


@dataclass
class Limits:
    """Every cap and pause of the loop, in seconds (nudges: a count). `from_env` reads the
    SLUICE_AGENT_* overrides."""
    nudges: int = 3
    wall: float = 600 * 60.0
    stall: float = 30 * 60.0
    settle: float = 10.0
    grace: float = 10 * 60.0
    poll: float = 0.5
    ready: float = 180.0

    @classmethod
    def from_env(cls):
        return cls(
            nudges=int(_env_float("SLUICE_AGENT_NUDGES", 3)),
            wall=_env_float("SLUICE_AGENT_MAX_MIN", 600) * 60,
            stall=_env_float("SLUICE_AGENT_STALL_MIN", 30) * 60,
            settle=_env_float("SLUICE_AGENT_SETTLE_S", 10),
            grace=_env_float("SLUICE_AGENT_GRACE_MIN", 10) * 60,
            poll=_env_float("SLUICE_AGENT_POLL_S", 0.5),
        )


@dataclass
class Snapshot:
    """One read of an engine's state.

    state: "starting" (no turn yet), "busy", "idle" or "exited".
    turns: turn ends so far this run (a turn end the supervisor has not answered is new).
    waiting: why an idle session is not finished: its own background work ("" when none).
    progress: anything that changes whenever the session does something (transcript sizes).
    error: the error that ended the last turn, if one did (checked for transient markers).
    exit_status: the engine's exit status once exited."""
    state: str
    turns: int = 0
    waiting: str = ""
    progress: object = None
    error: str = ""
    exit_status: str = ""


class Adapter:
    """What an engine provides. `name` names it in messages; `transient` holds lowercase
    markers that make an error retryable; `wait_signal` says whether `poll` can tell an idle
    session waiting on its own background work (without one the first nudge waits `grace`)."""
    name = "engine"
    transient: tuple = ()
    wait_signal = False

    def prepare(self, run_dir, cwd, session):
        """Write per-run files before launch (settings, hook scripts)."""

    def argv(self):
        """The command the pane runs."""
        raise NotImplementedError

    def env(self):
        """The environment the engine runs in."""
        return dict(os.environ)

    def wait_ready(self, tmux, timeout):
        """Block until the engine takes input, answering startup dialogs it knows."""

    def deliver(self, tmux, text):
        """Send one user message; raise paste.NotDelivered when it does not arrive."""
        raise NotImplementedError

    def poll(self, tmux):
        """The current Snapshot."""
        raise NotImplementedError

    def progress(self):
        """New one-line progress summaries since the last call."""
        return []

    def session_id(self):
        return ""

    def final(self):
        """The agent's last message."""
        return ""

    def cost_usd(self):
        return None

    def exit(self, tmux):
        """Ask the engine to exit cleanly; return once it has (or give up quietly)."""

    def close(self):
        """Stop anything the adapter started outside the tmux server."""

    def session_cwd(self, session):
        """The directory `session` was started in, or None when unknown."""


def required_outputs(ctx):
    """The step's declared outputs that must be submitted (the non-optional ones)."""
    names = []
    for name, port in (ctx.outputs or {}).items():
        try:
            t = T.parse(port.get("type") if isinstance(port, dict) else port)
        except T.TypeSyntaxError:
            t = None
        if not isinstance(t, T.Optional):
            names.append(name)
    return names


def submitted(run_dir):
    try:
        got = json.loads((Path(run_dir) / "submitted.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return got if isinstance(got, dict) else {}


def hand_over(text, path, pointer, **fmt):
    """What to type into the session for `text`: the text itself when it is one line of at
    most INLINE_MAX characters, else `pointer` (formatted with `path` and `fmt`) to the file
    `path`, where the text is written. A TUI collapses a longer or multi-line paste into a
    placeholder, and the model then reads it as pasted content, not as the user's own request
    (Claude Code 2.1.283: more than 3 lines or about 800 characters)."""
    if "\n" not in text.strip() and len(text) <= INLINE_MAX:
        return text.strip()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return pointer.format(path=path.resolve(), **fmt)


def thread_name(step):
    return "step-" + re.sub(r"[^a-z0-9_-]", "-", step.lower())


def thread_note(ctx):
    """The step-thread note for a session whose thread messages are pasted in."""
    thread = thread_name(ctx.step)
    post = (f'{{"name": "thread.post", "project": "{ctx.project}", "direct": true, '
            f'"inputs": {{"thread": "{thread}", "from": "{ctx.step}", '
            f'"to": "orchestrator", "body": "..."}}}}')
    return (
        f"Messages for you on sluice thread `{thread}` of project `{ctx.project}` are "
        f"pasted into this session as they arrive; you need not poll for them. Follow "
        f"instructions addressed to you. If you hit a question you cannot settle within your "
        f"task, post it with `sluice tool fn_call '{post}'` and continue with anything not "
        f"blocked by it. For a note that needs no answer (a decision you have already made, a "
        f"heads-up), add `\"needs_reply\": false` to the inputs. Post questions and changes "
        f"of scope, not progress."
    )


class ThreadFeed:
    """Messages for the step on its thread, from the project's log: those not posted by the
    step itself and addressed to it or to nobody, after the log's end at construction."""

    def __init__(self, ctx):
        self.dir = Path(ctx.home) / "projects" / ctx.project
        self.thread = thread_name(ctx.step)
        self.me = ctx.step
        self.since = L.last_seq(self.dir)
        self.pending = []

    def poll(self):
        try:
            got = L.read(self.dir, self.since, threads=[self.thread])
        except OSError:
            return []
        self.since = got["last_seq"]
        self.pending += [rec for rec in got["records"] if rec.get("from") != self.me
                         and rec.get("to") in (None, "", self.me)]
        out, self.pending = self.pending, []
        return out

    def unread(self, item):
        """Put a message back (its delivery failed) to try again on the next poll."""
        self.pending.insert(0, item)


def _write_json(path, obj):
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(obj))
    os.replace(tmp, path)


@dataclass
class _Run:
    adapter: Adapter
    tmux: Tmux
    run_dir: Path
    required: list
    feed: object
    limits: Limits
    log: object
    record: dict = field(default_factory=dict)

    def note_session(self):
        sid = self.adapter.session_id()
        if sid and self.record.get("session") != sid:
            self.record["session"] = sid
            _write_json(self.run_dir / "native.json", self.record)

    def transient(self, text):
        low = text.lower()
        return any(m in low for m in self.adapter.transient)

    def forward(self):
        """Type the step's new thread messages into the session; returns whether one was
        delivered."""
        sent = False
        for rec in self.feed.poll() if self.feed else []:
            frm, thread = rec.get("from"), self.feed.thread
            body = str(rec.get("body", ""))
            if rec.get("data") is not None:
                body += "\n\ndata: " + json.dumps(rec["data"])
            text = hand_over(MESSAGE.format(frm=frm, thread=thread, body=body),
                             self.run_dir / "messages" / f"{rec.get('seq')}.md", MESSAGE_FILE,
                             frm=frm, thread=thread)
            try:
                self.adapter.deliver(self.tmux, text)
            except NotDelivered as e:
                self.log(f"thread message from {frm} not delivered yet: {e}")
                self.feed.unread(rec)
                break
            self.log(f"thread message from {frm} typed into the session")
            sent = True
        return sent

    def loop(self):
        a, lim = self.adapter, self.limits
        start = time.monotonic()
        base, nudges = 0, 0  # base: the turn ends seen when we last spoke
        marker, moved = None, start
        idle_since, said = None, ""
        while True:
            snap = a.poll(self.tmux)
            for line in a.progress():
                self.log(line)
            self.note_session()
            now = time.monotonic()
            if snap.error and snap.turns > base and self.transient(snap.error):
                raise Transient(f"{a.name} hit a rate limit or capacity error: "
                                f"{snap.error[:300]}")
            if snap.state == "exited":
                pane = tail(self.tmux.capture(history=200))
                if self.transient(pane + " " + snap.error):
                    raise Transient(f"{a.name} exited on a rate limit or capacity error")
                raise RuntimeError(f"{a.name} exited (status {snap.exit_status or '?'}) before "
                                   f"the step was done. Last output:\n{pane}")
            if snap.progress != marker:
                marker, moved = snap.progress, now
            if now - start > lim.wall:
                raise RuntimeError(f"{a.name} ran past the wall-clock cap of "
                                   f"{lim.wall / 60:.0f} min (SLUICE_AGENT_MAX_MIN)")
            if snap.state == "busy" and now - moved > lim.stall:
                raise RuntimeError(f"{a.name} made no progress for {lim.stall / 60:.0f} min "
                                   "while busy (SLUICE_AGENT_STALL_MIN)")
            if self.forward():
                base, idle_since = snap.turns, None
            if not (snap.state == "idle" and snap.turns > base):
                idle_since = None
                time.sleep(lim.poll)
                continue
            missing = [n for n in self.required if n not in submitted(self.run_dir)]
            if self.required and not missing:
                return
            if snap.waiting:
                if snap.waiting != said:
                    self.log(f"waiting: {snap.waiting}")
                    said = snap.waiting
                idle_since = None
                time.sleep(lim.poll)
                continue
            said = ""
            idle_since = idle_since or now
            pause = lim.grace if not a.wait_signal and nudges == 0 else lim.settle
            if now - idle_since < pause:
                time.sleep(lim.poll)
                continue
            if not self.required:
                return
            if nudges >= lim.nudges:
                last = " ".join(a.final().split())[:1500]
                raise RuntimeError(
                    f"the agent ended its turn without submitting {', '.join(missing)} "
                    f"(nudged {nudges} times). Its last message: {last or '(none)'}")
            nudges += 1
            self.log(f"nudge {nudges}/{lim.nudges}: not submitted: {', '.join(missing)}")
            a.deliver(self.tmux, NUDGE.format(names=", ".join(missing)))
            base, idle_since = snap.turns, None
            time.sleep(lim.poll)


def _log(line):
    print(line, file=sys.stderr, flush=True)


def supervise(adapter, task, cwd, run_dir, *, required=(), session=None, feed=None,
              limits=None, attempt=1, log=_log):
    """Run `task` in a live session of the adapter's engine until the step is done. Returns
    {"final", "session", "cost_usd"}. `session` resumes that session (refused when it was
    started in another directory); on a retry (`attempt` > 1) a session an earlier attempt of
    this run started is resumed and told to continue."""
    limits = limits or Limits.from_env()
    run_dir = Path(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    cwd = str(Path(cwd).resolve())
    rec_path = run_dir / "native.json"
    message = task
    if not session and attempt > 1:
        with contextlib.suppress(OSError, ValueError):
            before = json.loads(rec_path.read_text())
            if before.get("session") and before.get("cwd") == cwd:
                session, message = before["session"], CONTINUE
    if session:
        was = adapter.session_cwd(session)
        if was and str(Path(was).resolve()) != cwd:
            raise ValueError(f"session {session} was started in {was}, not {cwd}: {adapter.name} "
                             "cannot resume a session from another directory; run the "
                             "step in the session's own directory")
    run = _Run(adapter, Tmux(run_dir), run_dir, list(required), feed, limits, log,
               {"engine": adapter.name, "cwd": cwd, "resumed": session or None})
    _write_json(rec_path, run.record)
    if message is task:
        message = hand_over(task, run_dir / "task.md", POINTER)
    run.tmux.kill()  # a server an earlier attempt of this run left
    handlers = _exit_on_signals()
    try:
        adapter.prepare(run_dir, cwd, session)
        run.tmux.start(adapter.argv(), cwd, adapter.env())
        log(f"attach: {run.tmux.attach}")
        adapter.wait_ready(run.tmux, limits.ready)
        adapter.deliver(run.tmux, message)
        log(f"task delivered ({len(message)} chars)")
        run.loop()
        final, sid = adapter.final(), adapter.session_id()
        adapter.exit(run.tmux)
        run.note_session()
        return {"final": final, "session": sid, "cost_usd": adapter.cost_usd()}
    finally:
        for s in handlers:
            signal.signal(s, signal.SIG_IGN)  # a second signal must not cut the cleanup short
        try:
            run.tmux.kill()
            adapter.close()
        finally:
            for s, h in handlers.items():
                signal.signal(s, h)


def _exit_on_signals():
    """SIGTERM, SIGHUP and SIGINT raise SystemExit so the cleanup runs. Returns the previous
    handlers (none off the main thread, where signals cannot be caught)."""
    if threading.current_thread() is not threading.main_thread():
        return {}

    def handler(sig, _frame):
        raise SystemExit(128 + sig)

    return {s: signal.signal(s, handler) for s in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)}
