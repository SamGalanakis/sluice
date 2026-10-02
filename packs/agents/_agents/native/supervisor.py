"""The engine-agnostic supervisor: it runs an engine's interactive session in a private tmux
server, delivers the task, and decides when the step is done; the model's end of turn does not.

An engine plugs in through an adapter (see `Adapter`). Per run:

1. start the server (socket `tmux.sock` in the run dir) running the adapter's command, print
   the `attach:` line, wait for the engine's input, deliver the task (a one-line pointer to
   `<run_dir>/task.md` unless it is one short line: see `hand_over`);
2. poll the adapter's state. When a turn has ended and the session is idle:
   - every required declared output is submitted (the run's submission, step_submit) → done;
   - the session waits on its own background work (a background shell, a pending wakeup) →
     keep waiting, no nudge;
   - the step declares no required outputs → done after `settle` seconds of idle;
   - otherwise, after `settle` seconds of idle → nudge, up to `nudges` times, then fail
     naming the missing outputs and the agent's last message;
   an engine with no waiting signal gets `grace` seconds of idle before the first of these;
   at a done exit, wait (up to `work` seconds) while the session's background work runs:
   what the engine reports (`waiting`) and the processes it let go (processes.detached);
   then, once per run, when tracked files are changed but not committed, send one reminder
   turn (never commit for the agent);
3. type messages addressed to the step on its thread into the session as they arrive, and,
   after an engine reports its context compacted, the step's context (reprime.py);
4. fail on the wall-clock cap, or after `stall` seconds without progress while busy; post one
   note to the orchestrator on the step's thread after `quiet` seconds busy with no change to
   the git worktree or CPU work in the agent's descendants (and again after each further
   quiet period);
5. on done ask the engine to exit, read `final`, `session` and the run's git
   facts; in every case end the tmux server and every process under it (SIGTERM, SIGHUP and
   SIGINT included, so a `step_cancel` leaves nothing behind). A failure's message ends with
   the session to resume.

A run that resumes a session holds `SLUICE_HOME/locks/<engine>-<session>.lock` until it ends,
so a second run resuming the same session fails at once, naming the holder."""

import contextlib
import fcntl
import json
import os
import re
import signal
import sqlite3
import sys
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path

from sluice import db
from sluice import log as L
from sluice import types as T
from sluice.errors import SluiceError
from sluice.fn import Transient, child_env

from . import reprime, worktree
from .paste import NotDelivered, tail
from .processes import detached, engine_env, start_time, tree_cpu
from .tmux import Tmux

NUDGE = ("Your turn ended but these outputs are not submitted: {names}. If you are waiting on "
         "something, wait for it in this turn. Otherwise finish and submit them with the "
         "command from your task, or submit what you have and explain the blocker in "
         "`unresolved`.")
WAIT_NUDGE = ("Your step is still waiting on {work}, and these outputs are not submitted: "
              "{names}. Stop or finish that background work, then submit the outputs. "
              "If it cannot finish, submit what you have and explain the blocker in `unresolved`.")
DIALOG_NUDGE = ("Nobody can answer here. Decide, or post the question with thread.post and "
                "continue the task.")
POINTER = "Your task is in {path}; read it fully, then do it."
MESSAGE = "Message from {frm} on your sluice thread `{thread}`: {body}"
MESSAGE_FILE = "A message from {frm} on your sluice thread `{thread}` is in {path}; read it now."
INLINE_MAX = 500
CONTINUE = ("Your session was interrupted by a rate limit or capacity error. Continue your task "
            "where you left off.")
REMIND = ("You have uncommitted changes: {status}. Commit or discard them (unless your task "
          "says to leave them), then finish.")
REMIND_FILE = "A note about your uncommitted changes is in {path}; read it now."
COMPACT_FILE = "Your context was compacted; where your step stands is in {path}; read it now."
QUIET = "busy {min} min with no change to the worktree (HEAD {head}, {diff})"
RESUME = ("\nsession: {sid}. To resume it, bind the step's session input to it and retry: "
          'step_set_input(project, step, "session", "{sid}"), then step_retry.')


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
    turn_start: float = 60.0
    wait: float = 90 * 60.0
    dialog: float = 60.0
    quiet: float = 45 * 60.0
    work: float = 10 * 60.0

    @classmethod
    def from_env(cls):
        return cls(
            nudges=int(_env_float("SLUICE_AGENT_NUDGES", 3)),
            wall=_env_float("SLUICE_AGENT_MAX_MIN", 600) * 60,
            stall=_env_float("SLUICE_AGENT_STALL_MIN", 30) * 60,
            settle=_env_float("SLUICE_AGENT_SETTLE_S", 10),
            grace=_env_float("SLUICE_AGENT_GRACE_MIN", 10) * 60,
            poll=_env_float("SLUICE_AGENT_POLL_S", 0.5),
            turn_start=_env_float("SLUICE_AGENT_TURN_START_S", 60),
            wait=_env_float("SLUICE_AGENT_WAIT_MIN", 90) * 60,
            quiet=_env_float("SLUICE_AGENT_QUIET_MIN", 45) * 60,
            work=_env_float("SLUICE_AGENT_WORK_MIN", 10) * 60,
        )


@dataclass
class Snapshot:
    """One read of an engine's state.

    state: "starting" (no turn yet), "busy", "idle" or "exited".
    turns: turn ends so far this run (a turn end the supervisor has not answered is new).
    waiting: why an idle session is not finished: its own background work ("" when none).
    progress: anything that changes whenever the session does something (transcript sizes).
    error: the error that ended the last turn, if one did (checked for transient markers).
    exit_status: the engine's exit status once exited.
    compactions: the times the engine reported its context compacted (0 for an engine that
    re-primes itself, as Claude does through its SessionStart hook)."""
    state: str
    turns: int = 0
    waiting: str = ""
    progress: object = None
    error: str = ""
    exit_status: str = ""
    starts: int = 0
    compactions: int = 0


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
        return engine_env()

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

    def exit(self, tmux):
        """Ask the engine to exit cleanly; return once it has (or give up quietly)."""

    def close(self):
        """Stop anything the adapter started outside the tmux server."""

    def session_cwd(self, session):
        """The directory `session` was started in, or None when unknown."""

    def session_key(self, session):
        """The engine's own id of `session` (what its lock is named by)."""
        return session

    def roots(self, tmux):
        """The processes the engine runs as, whose cgroups hold the work it starts."""
        return [tmux.pane_pid()]


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


def submitted():
    """What the run's agent has submitted so far (step_submit): its submission in the home's
    database, found from the environment the runner sets ({} outside a project's step)."""
    home, project = os.environ.get("SLUICE_HOME"), os.environ.get("SLUICE_PROJECT")
    run = os.environ.get("SLUICE_RUN_ID")
    if not (home and project and run):
        return {}
    try:
        return db.submission(home, project, run) or {}
    except (OSError, sqlite3.Error, SluiceError):
        return {}


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


def thread_note(ctx, delivery="pasted"):
    """The step-thread note for a session whose thread messages arrive automatically."""
    thread = thread_name(ctx.step)
    post = (f'{{"name": "thread.post", "project": "{ctx.project}", "direct": true, '
            f'"inputs": {{"thread": "{thread}", "from": "{ctx.step}", '
            f'"to": "orchestrator", "body": "..."}}}}')
    return (
        f"Messages for you on sluice thread `{thread}` of project `{ctx.project}` are "
        f"{delivery} into this session as they arrive when they are addressed to this step "
        f"(or to nobody); you need not poll for them. Follow "
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
        self.home, self.project = Path(ctx.home), ctx.project
        self.thread = thread_name(ctx.step)
        self.me = ctx.step
        self.since = L.last_seq(self.home, self.project)
        self.pending = deque()

    def poll(self):
        try:
            got = L.read(self.home, self.project, self.since, threads=[self.thread])
        except (OSError, sqlite3.Error, SluiceError):
            return
        self.since = got["last_seq"]
        self.pending.extend(rec for rec in got["records"] if rec.get("from") != self.me
                            and rec.get("to") in (None, "", self.me))

    def peek(self):
        return self.pending[0] if self.pending else None

    def ack(self):
        self.pending.popleft()


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
    sent: object  # () -> what the agent has submitted so far
    record: dict = field(default_factory=dict)
    note: object = None  # (body) -> posts a note to the orchestrator on the step's thread
    pending_text: str = ""
    await_base: int = 0
    await_at: float | None = None
    redelivered: bool = False
    helpers: set = field(default_factory=set)  # (pid, start) let go before the task
    reminded: bool = False
    work_since: float | None = None  # waiting for background work at a done exit since
    compacted: int = 0
    mark: object = None  # the worktree's last sample
    sampled: float = 0.0
    changed: float | None = None  # busy without worktree changes or descendant CPU since then
    noted: float | None = None
    cpu: dict = field(default_factory=dict)  # (pid, start time) -> descendant CPU ticks

    @property
    def cwd(self):
        return self.record["cwd"]

    def expect_start(self, text, starts):
        self.pending_text = text
        self.await_base = starts
        self.await_at = time.monotonic()
        self.redelivered = False

    def deliver(self, text, starts):
        self.expect_start(text, starts)
        try:
            self.adapter.deliver(self.tmux, text)
        except NotDelivered:
            self.log("message not delivered; retrying once")
            self.adapter.deliver(self.tmux, text)
            self.await_at = time.monotonic()

    def note_session(self):
        sid = self.adapter.session_id()
        if sid and self.record.get("session") != sid:
            self.record["session"] = sid
            _write_json(self.run_dir / "native.json", self.record)

    def transient(self, text):
        low = text.lower()
        return (any(m in low for m in self.adapter.transient)
                or (self.adapter.name == "devin" and bool(re.search(
                    r"\b(?:http|status(?:_code)?)\s*[:=]?\s*529\b", low))))

    def say(self, text, snap):
        """Type `text` into the session, busy or not (a turn it starts is then expected)."""
        self.adapter.deliver(self.tmux, text)
        if snap is not None and snap.state != "busy":
            self.expect_start(text, snap.starts)

    def forward(self, snap=None):
        """Type the step's new thread messages into the session; returns whether one was
        delivered."""
        sent = False
        if not self.feed:
            return sent
        self.feed.poll()
        while (rec := self.feed.peek()) is not None:
            frm, thread = rec.get("from"), self.feed.thread
            body = str(rec.get("body", ""))
            if rec.get("data") is not None:
                body += "\n\ndata: " + json.dumps(rec["data"])
            text = hand_over(MESSAGE.format(frm=frm, thread=thread, body=body),
                             self.run_dir / "messages" / f"{rec.get('seq')}.md", MESSAGE_FILE,
                             frm=frm, thread=thread)
            try:
                self.say(text, snap)
            except (NotDelivered, RuntimeError, TimeoutError) as e:
                self.log(f"thread message from {frm} not delivered yet: {e}")
                break
            self.feed.ack()
            verb = "delivered to" if self.adapter.name == "codex" else "typed into"
            self.log(f"thread message from {frm} {verb} the session")
            sent = True
        return sent

    def reprime(self, snap):
        """After the engine reports its context compacted, give the session the step's
        context again; returns whether it was delivered."""
        if snap.compactions <= self.compacted:
            return False
        text = hand_over(reprime.context(self.run_dir / "task.md", child_env()),
                         self.run_dir / "messages" / f"compact-{snap.compactions}.md",
                         COMPACT_FILE)
        try:
            self.say(text, snap)
        except (NotDelivered, RuntimeError, TimeoutError) as e:
            self.log(f"step context after compaction not delivered yet: {e}")
            return False
        self.compacted = snap.compactions
        self.log("context compacted; the step's context was typed into the session")
        return True

    def watch(self, snap, now):
        """Sample the worktree and descendant CPU; after `quiet` seconds busy with neither
        changing, post one note, and again after each further quiet period. Detection only."""
        lim = self.limits
        if not (self.note and self.record.get("head_before")) or snap.state != "busy":
            self.changed = self.noted = None
            self.cpu = {}
            return
        if self.changed is not None and now - self.sampled < min(lim.quiet / 10, 180):
            return
        self.sampled = now
        mark = worktree.sample(self.cwd)
        cpu = tree_cpu(self.adapter.roots(self.tmux))
        active = any(ticks > self.cpu.get(pid, 0) for pid, ticks in cpu.items())
        self.cpu = cpu
        if self.changed is None or mark != self.mark or active:
            self.mark, self.changed, self.noted = mark, now, None
            return
        if mark is None or now - (self.noted or self.changed) < lim.quiet:
            return
        self.noted = now
        body = QUIET.format(min=round((now - self.changed) / 60), head=mark[0][:7],
                            diff="uncommitted diff unchanged" if mark[2] else "no diff")
        self.log(f"quiet: {body}")
        try:
            self.note(body)
        except Exception as e:  # noqa: BLE001 - a note that cannot be posted is only logged
            self.log(f"quiet note not posted: {e}")

    def work(self, snap):
        """What the session still runs: its own background work as the engine reports it
        (`snap.waiting`), and the processes it let go (processes.detached) after the task."""
        let_go = {p: n for p, n in detached(self.adapter.roots(self.tmux)).items()
                  if (p, start_time(p)) not in self.helpers}
        names = ", ".join(f"{n} (pid {p})" for p, n in sorted(let_go.items())[:5])
        return "; ".join(w for w in (snap.waiting, names) if w)

    def finish(self, snap, now):
        """At a done exit: "wait" while the session's background work runs (up to `work`
        seconds), then, once per run, "remind" the agent (one turn) of tracked changes it has
        not committed; else "done"."""
        lim, work = self.limits, self.work(snap)
        if work:
            if self.work_since is None:
                self.work_since = now
                self.log(f"waiting up to {lim.work / 60:.0f} min for background work: {work}")
            if now - self.work_since < lim.work:
                return "wait"
            self.log(f"background work still running after {lim.work / 60:.0f} min "
                     f"(SLUICE_AGENT_WORK_MIN); finishing anyway: {work}")
        elif self.work_since is not None:
            self.log("background work ended")
        self.work_since = None
        if self.reminded:
            return "done"
        self.reminded = True
        status = worktree.changes(self.cwd)
        if not (status and status.strip()):
            return "done"
        lines = worktree.cut(status).splitlines()
        text = hand_over(REMIND.format(status="; ".join(ln.strip() for ln in lines)),
                         self.run_dir / "messages" / "uncommitted.md", REMIND_FILE)
        self.log("uncommitted changes: reminding the agent once")
        try:
            self.deliver(text, snap.starts)
        except (NotDelivered, RuntimeError, TimeoutError) as e:
            self.log(f"reminder not delivered: {e}")
            return "done"
        return "remind"

    def loop(self):
        a, lim = self.adapter, self.limits
        start = time.monotonic()
        base, nudges = 0, 0  # base: the turn ends seen when we last spoke
        marker, moved = None, start
        idle_since, said = None, ""
        wait_since = dialog_since = None
        while True:
            snap = a.poll(self.tmux)
            for line in a.progress():
                self.log(line)
            self.note_session()
            now = time.monotonic()
            submitted = self.sent()
            missing = [n for n in self.required if n not in submitted]
            complete = self.required and not missing and snap.state == "idle" \
                and snap.turns > base
            if not complete and snap.error and snap.turns > base and self.transient(snap.error):
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
            if snap.state != "idle" and now - moved > lim.stall:
                raise RuntimeError(f"{a.name} made no progress for {lim.stall / 60:.0f} min "
                                   f"while {snap.state} (SLUICE_AGENT_STALL_MIN)")
            if self.await_at is not None:
                if snap.starts > self.await_base:
                    self.await_at = None
                elif now - self.await_at > lim.turn_start:
                    if self.redelivered:
                        raise RuntimeError(f"{a.name} did not start a turn within "
                                           f"{lim.turn_start:.0f} s after delivery and retry")
                    self.log(f"{a.name} did not start a turn; delivering the message once more")
                    a.deliver(self.tmux, self.pending_text)
                    self.redelivered = True
                    self.await_at = time.monotonic()
                    self.await_base = snap.starts
            self.watch(snap, now)
            if self.forward(snap) | self.reprime(snap):
                base, idle_since = snap.turns, None
                self.work_since = None
            if snap.state == "blocked":
                self.work_since = None
                dialog_since = dialog_since or now
                if now - dialog_since > lim.dialog:
                    self.tmux.keys("Escape")
                    self.log("interactive dialog stayed open; asking the agent to decide")
                    self.deliver(DIALOG_NUDGE, snap.starts)
                    base, dialog_since = snap.turns, None
                time.sleep(lim.poll)
                continue
            dialog_since = None
            if not (snap.state == "idle" and snap.turns > base):
                idle_since = None
                self.work_since = None
                time.sleep(lim.poll)
                continue
            if self.feed and self.feed.peek() is not None:
                idle_since = self.work_since = None
                time.sleep(lim.poll)
                continue
            if not complete and snap.waiting:
                if snap.waiting != said:
                    self.log(f"waiting: {snap.waiting}")
                    said = snap.waiting
                    wait_since = now
                if wait_since is not None and now - wait_since > lim.wait:
                    if nudges >= lim.nudges:
                        raise RuntimeError(f"{a.name} waited on {snap.waiting} for "
                                           f"{lim.wait / 60:.0f} min without submitting "
                                           f"{', '.join(missing)}")
                    nudges += 1
                    self.log(f"nudge {nudges}/{lim.nudges}: waiting on {snap.waiting}")
                    self.deliver(WAIT_NUDGE.format(work=snap.waiting,
                                                   names=", ".join(missing)), snap.starts)
                    base, wait_since, said = snap.turns, None, ""
                idle_since = None
                time.sleep(lim.poll)
                continue
            said = ""
            wait_since = None
            if not complete:
                idle_since = idle_since or now
                pause = lim.grace if not a.wait_signal and nudges == 0 else lim.settle
                if now - idle_since < pause:
                    time.sleep(lim.poll)
                    continue
            if complete or not self.required:
                end = self.finish(snap, now)
                if end == "done":
                    return
                if end == "remind":
                    base, idle_since = snap.turns, None
                time.sleep(lim.poll)
                continue
            if nudges >= lim.nudges:
                last = " ".join(a.final().split())[:1500]
                raise RuntimeError(
                    f"the agent ended its turn without submitting {', '.join(missing)} "
                    f"(nudged {nudges} times). Its last message: {last or '(none)'}")
            nudges += 1
            self.log(f"nudge {nudges}/{lim.nudges}: not submitted: {', '.join(missing)}")
            self.deliver(NUDGE.format(names=", ".join(missing)), snap.starts)
            base, idle_since = snap.turns, None
            time.sleep(lim.poll)


def _log(line):
    print(line, file=sys.stderr, flush=True)


def lock_session(engine, key):
    """Hold SLUICE_HOME/locks/<engine>-<session>.lock (flock) for this run, with the holder's
    project, step and run written in it; released when the returned file is closed or the
    process ends. Raises when another run holds it: two writers corrupt a session."""
    home = Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")
    (home / "locks").mkdir(parents=True, exist_ok=True)
    path = home / "locks" / f"{engine}-{re.sub(r'[^A-Za-z0-9._-]', '_', key)}.lock"
    f = open(path, "a+")  # noqa: SIM115 - held until the run ends
    try:
        fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        f.seek(0)
        try:
            holder = json.loads(f.read())
        except ValueError:
            holder = {}
        f.close()
        raise RuntimeError(
            f"{engine} session {key} is in use by step {holder.get('step') or '?'} of project "
            f"{holder.get('project') or '?'} (run {holder.get('run') or '?'}): two runs cannot "
            "resume one session at once. Wait for that step to end, or start a new "
            "session") from None
    f.seek(0)
    f.truncate()
    env = os.environ
    f.write(json.dumps({"project": env.get("SLUICE_PROJECT", ""),
                        "step": env.get("SLUICE_STEP", ""), "run": env.get("SLUICE_RUN_ID", ""),
                        "pid": os.getpid()}))
    f.flush()
    return f


def supervise(adapter, task, cwd, run_dir, *, required=(), session=None, feed=None,
              limits=None, attempt=1, log=_log, sent=None, note=None):
    """Run `task` in a live session of the adapter's engine until the step is done. Returns
    {"final", "session", "git"} (git: worktree.facts, None outside a git
    worktree; its `head_before` is read once per run and kept in native.json across retries).
    `session` resumes that session (refused when it was started in another directory, or
    while another run resumes it); on a retry (`attempt` > 1) a session an earlier attempt of
    this run started is resumed and told to continue. `sent()` says what the agent has
    submitted so far (default: `submitted`, the run's submission); `note(body)` posts to the
    orchestrator (the quiet-worktree note; none without it)."""
    limits = limits or Limits.from_env()
    run_dir = Path(run_dir)
    run_dir.mkdir(parents=True, exist_ok=True)
    cwd = str(Path(cwd).resolve())
    rec_path = run_dir / "native.json"
    message, before = task, {}
    if attempt > 1:
        with contextlib.suppress(OSError, ValueError):
            before = json.loads(rec_path.read_text())
        if before.get("cwd") != cwd:
            before = {}
    if not session and before.get("session"):
        session, message = before["session"], CONTINUE
    if session:
        was = adapter.session_cwd(session)
        if was and str(Path(was).resolve()) != cwd:
            raise ValueError(f"session {session} was started in {was}, not {cwd}: {adapter.name} "
                             "cannot resume a session from another directory; run the "
                             "step in the session's own directory")
    head_before = before.get("head_before") or worktree.head(cwd)
    run = _Run(adapter, Tmux(run_dir), run_dir, list(required), feed, limits, log,
               sent or submitted,
               {"engine": adapter.name, "cwd": cwd, "resumed": session or None,
                **({"head_before": head_before} if head_before else {})}, note)
    _write_json(rec_path, run.record)
    if message is task:
        (run_dir / "task.md").write_text(task)  # re-read after a compaction
        message = hand_over(task, run_dir / "task.md", POINTER)
    lock = lock_session(adapter.name, adapter.session_key(session)) if session else None
    handlers = _exit_on_signals()
    try:
        run.tmux.kill()  # a server an earlier attempt of this run left
        adapter.prepare(run_dir, cwd, session)
        run.tmux.start(adapter.argv(), cwd, adapter.env())
        log(f"attach: {run.tmux.attach}")
        adapter.wait_ready(run.tmux, limits.ready)
        run.helpers = {(p, start_time(p)) for p in detached(adapter.roots(run.tmux))}
        run.deliver(message, getattr(adapter, "starts", 0))
        log(f"task delivered ({len(message)} chars)")
        run.loop()
        final, sid = adapter.final(), adapter.session_id()
        adapter.exit(run.tmux)
        run.note_session()
        return {"final": final, "session": sid, "git": worktree.facts(cwd, head_before)}
    except Exception as e:
        sid = adapter.session_id() or run.record.get("session") or session
        if sid and len(e.args) == 1 and isinstance(e.args[0], str):
            e.args = (e.args[0] + RESUME.format(sid=sid),)
        raise
    finally:
        for s in handlers:
            signal.signal(s, signal.SIG_IGN)  # a second signal must not cut the cleanup short
        try:
            run.tmux.kill()
            adapter.close()
        finally:
            if lock:
                lock.close()
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
