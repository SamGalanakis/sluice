"""Tests for the native session runner in packs/agents/_agents/native: the paste routine
against a real tmux server running a fake composer, and the supervisor's state machine driven
by a scripted adapter (the pane runs `sleep`)."""

import json
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from sluice.fn import Transient
from sluice.store import Store

AGENTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(AGENTS))

from _agents.native import paste
from _agents.native.claude import Composer
from _agents.native.supervisor import (
    CONTINUE,
    POINTER,
    Adapter,
    Limits,
    Snapshot,
    ThreadFeed,
    _Run,
    required_outputs,
    supervise,
)
from _agents.native.tmux import Tmux, descendants

FAKE_COMPOSER = Path(__file__).with_name("fake_composer.py")
FAST = Limits(nudges=2, wall=30, stall=30, settle=0.2, grace=0.2, poll=0.02, ready=10)


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return Path(f"/proc/{pid}/stat").read_text().split(") ")[1][0] != "Z"


def server_up(run_dir):
    return subprocess.run(["tmux", "-S", "tmux.sock", "ls"], cwd=run_dir,
                          capture_output=True, check=False).returncode == 0


# ---- the paste routine ------------------------------------------------------------------------

@pytest.fixture
def pane(tmp_path):
    started = []

    def start(boot=0.0, drop_enters=0, wrap=0):
        t = Tmux(tmp_path)
        t.start([sys.executable, str(FAKE_COMPOSER), str(tmp_path / "sent.jsonl"), str(boot),
                 str(drop_enters), str(wrap)], tmp_path, dict(os.environ))
        started.append(t)
        return t

    yield start
    for t in started:
        t.kill()


def sent(tmp_path, n=1, timeout=5.0):
    """The messages the fake composer recorded, once there are `n` of them."""
    f = tmp_path / "sent.jsonl"
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        got = [json.loads(ln)["text"] for ln in f.read_text().splitlines()] \
            if f.exists() else []
        if len(got) >= n:
            return got
        time.sleep(0.05)
    return got


def test_paste_keeps_backticks_dollars_and_a_trailing_backslash(pane, tmp_path):
    t = pane()
    text = 'run `ls $HOME` and "$(date)"\nthen a second line ending in a backslash \\'
    paste.wait_ready(t, Composer, 10)
    paste.deliver(t, text, Composer)
    assert [m.rstrip("\n") for m in sent(tmp_path)] == [text]


def test_without_the_trailing_newline_a_trailing_backslash_eats_the_enter(pane, tmp_path):
    """The red side of the rule above: pasted bare, backslash + Enter is a continuation."""
    t = pane()
    paste.wait_ready(t, Composer, 10)
    t.paste(paste.payload("ends in a backslash \\"))
    time.sleep(0.3)
    t.keys("Enter")
    assert sent(tmp_path, timeout=1.0) == []


def test_a_message_over_16_kb_goes_through_one_paste(pane, tmp_path):
    t = pane()
    text = "\n".join(f"line {i}: `x` $y \\z {'w' * 60}" for i in range(300))
    assert len(text.encode()) > 16 * 1024
    too_long = t.run("send-keys", "-t", "main", "-l", text, check=False)
    assert too_long.returncode != 0 and "too long" in too_long.stderr  # why it is a buffer
    paste.wait_ready(t, Composer, 10)
    paste.deliver(t, text, Composer)
    assert [m.rstrip("\n") for m in sent(tmp_path)] == [text]


def test_the_first_message_waits_for_the_composer_to_boot(pane, tmp_path):
    t = pane(boot=1.5)
    paste.wait_ready(t, Composer, 10)
    paste.deliver(t, "hello", Composer)
    assert [m.rstrip("\n") for m in sent(tmp_path)] == ["hello"]


def test_an_enter_folded_into_the_draft_is_sent_again(pane, tmp_path):
    t = pane(drop_enters=1)
    paste.wait_ready(t, Composer, 10)
    paste.deliver(t, "first try", Composer)
    assert [m.strip("\n") for m in sent(tmp_path)] == ["first try"]


def test_a_draft_the_box_wraps_is_still_recognised(pane, tmp_path):
    """Seen live: a long pointer wrapped under the glyph hid the needle, the Enter went out
    blind mid-paste and was swallowed. With one Enter folded, only a verified submit sends
    it."""
    t = pane(drop_enters=1, wrap=40)
    text = "Your task is in /very/long/" + "path/" * 30 + "task.md; read it fully, then do it."
    paste.wait_ready(t, Composer, 10)
    paste.deliver(t, text, Composer)
    assert [m.strip("\n") for m in sent(tmp_path)] == [text]


def test_a_message_that_never_leaves_the_box_is_an_error(pane, tmp_path, monkeypatch):
    monkeypatch.setattr(paste, "VERIFY_TIMEOUT", 1.5)
    t = pane(drop_enters=100)
    paste.wait_ready(t, Composer, 10)
    with pytest.raises(paste.NotDelivered, match="never left the input box"):
        paste.deliver(t, "stuck", Composer)


def test_the_composer_is_read_off_the_pane():
    rule = "─" * 40
    assert Composer.ready(f"x\n{rule}\n❯ \n{rule}\n  footer")
    assert Composer.ready(f"{'─' * 20} my session ─\n❯ hi\n{rule}")  # a titled rule
    assert not Composer.ready("booting…")
    assert not Composer.ready(f"{rule}\n! ls\n{rule}")  # shell mode
    assert Composer.occupied(f"{rule}\n! ls\n{rule}") == "shell mode"
    assert Composer.occupied("a dialog\n❯ 1. Yes") == "an overlay"
    assert Composer.occupied("") is None
    placeholder = f"{rule}\n❯ \n  [Pasted text #1 +3 lines]\n{rule}"
    assert Composer.draft_visible(placeholder, "anything")
    assert Composer.draft_visible(f"{rule}\n❯ fix the bug\n{rule}", "fix the bug")
    assert not Composer.draft_visible(f"❯ fix the bug\nout\n{rule}\n❯ \n{rule}", "fix the bug")
    wrapped = f"{rule}\n❯ \n  Your task is in\n  /a/very/long/path\n{rule}"
    assert Composer.draft_visible(wrapped, "Your task is in /a/very/")


# ---- the supervisor ---------------------------------------------------------------------------

class Model(Adapter):
    """A scripted engine: the pane runs `sleep`; each message it gets ends one turn, and
    `on_turn(model, n, text)` (n from 1) may submit outputs, set `waiting` or `error`."""
    name = "fake"
    wait_signal = True
    transient = ("rate limit",)

    def __init__(self, on_turn=None, state=None):
        self.on_turn = on_turn or (lambda m, n, text: None)
        self.sent, self.turns, self.starts, self.waiting, self.error = [], 0, 0, "", ""
        self.state = state  # a fixed state, else idle once a turn ended
        self.progress_marker = 0
        self.resumed = None
        self.cwd_of = {}
        self.exited = False
        self.outputs = {}  # what it has submitted (the run's submission)

    def prepare(self, run_dir, cwd, session):
        self.run_dir, self.resumed = Path(run_dir), session

    def argv(self):
        return ["sleep", "600"]

    def env(self):
        return dict(os.environ)

    def deliver(self, tmux, text):
        self.sent.append(text)
        if getattr(self, "never_start", False):
            return
        self.starts += 1
        self.turns += 1
        self.on_turn(self, self.turns, text)

    def submit(self, **outputs):
        self.outputs = outputs

    def poll(self, tmux):
        state = self.state or ("idle" if self.turns else "starting")
        if state == "busy-moving":
            self.progress_marker += 1
            state = "busy"
        return Snapshot(state, self.turns, waiting=self.waiting, error=self.error,
                        progress=self.progress_marker, starts=self.starts)

    def session_id(self):
        return "s-1"

    def final(self):
        return f"reply {self.turns}"

    def exit(self, tmux):
        self.exited = True

    def session_cwd(self, session):
        return self.cwd_of.get(session)


def run(model, tmp_path, required=("word",), **kw):
    cwd = tmp_path / "work"
    cwd.mkdir(exist_ok=True)
    lines = []
    out = supervise(model, "the task", cwd, tmp_path / "run", required=list(required),
                    sent=lambda: model.outputs,
                    limits=kw.pop("limits", FAST), log=lines.append, **kw)
    return out, lines


def test_done_when_a_turn_ends_with_the_outputs_submitted(tmp_path):
    model = Model(lambda m, n, text: m.submit(word="blue"))
    out, lines = run(model, tmp_path)
    assert out == {"final": "reply 1", "session": "s-1", "git": None}
    assert model.sent == ["the task"] and model.exited
    assert lines[0].startswith("attach: cd ") and lines[0].endswith("&& tmux -S tmux.sock attach")
    assert not server_up(tmp_path / "run")
    assert json.loads((tmp_path / "run" / "native.json").read_text()) == {
        "engine": "fake", "cwd": str((tmp_path / "work").resolve()), "resumed": None,
        "session": "s-1"}


def test_a_turn_without_the_outputs_is_nudged_then_fails(tmp_path):
    model = Model()
    with pytest.raises(RuntimeError) as e:
        run(model, tmp_path, required=("word", "why"))
    assert "without submitting word, why (nudged 2 times)" in str(e.value)
    assert "Its last message: reply 3" in str(e.value)
    assert len(model.sent) == 3
    assert model.sent[1].startswith("Your turn ended but these outputs are not submitted: "
                                    "word, why.")
    assert not server_up(tmp_path / "run")


def test_a_nudge_that_gets_the_outputs_ends_the_step(tmp_path):
    model = Model(lambda m, n, text: m.submit(word="late") if n == 2 else None)
    out, lines = run(model, tmp_path)
    assert out["final"] == "reply 2"
    assert "nudge 1/2: not submitted: word" in lines


def test_a_nudge_not_delivered_on_first_try_is_retried(tmp_path):
    class Flaky(Model):
        failed = False
        def deliver(self, tmux, text):
            if text.startswith("Your turn ended") and not self.failed:
                self.failed = True
                raise paste.NotDelivered("composer busy")
            super().deliver(tmux, text)

    model = Flaky(lambda m, n, text: m.submit(word="done") if n == 2 else None)
    run(model, tmp_path)
    assert model.failed and len(model.sent) == 2


def test_a_session_waiting_on_its_background_work_is_not_nudged(tmp_path):
    def on_turn(m, n, text):
        if n == 1:
            m.waiting = "a background shell is running"

            def notified():  # the work ends; the notification's turn submits
                m.waiting = ""
                m.submit(word="built")
                m.turns += 1
            threading.Timer(1.0, notified).start()

    model = Model(on_turn)
    out, lines = run(model, tmp_path)
    assert model.sent == ["the task"]  # never nudged
    assert "waiting: a background shell is running" in lines
    assert out["final"] == "reply 2"


def test_without_required_outputs_an_idle_session_is_done(tmp_path):
    model = Model()
    out, _ = run(model, tmp_path, required=())
    assert model.sent == ["the task"] and out["final"] == "reply 1"


def test_without_required_outputs_background_work_is_still_waited_for(tmp_path):
    def on_turn(m, n, text):
        m.waiting = "a wakeup at 12:00:00"

        def fired():
            m.waiting = ""
            m.turns += 1
        threading.Timer(0.8, fired).start()

    model = Model(on_turn)
    out, _ = run(model, tmp_path, required=())
    assert out["final"] == "reply 2" and model.sent == ["the task"]


def test_an_engine_without_a_waiting_signal_gets_a_grace_period(tmp_path):
    model = Model(lambda m, n, text: threading.Timer(0.6, lambda: m.submit(word="w")).start())
    model.wait_signal = False
    limits = Limits(nudges=2, wall=30, stall=30, settle=0.1, grace=1.5, poll=0.02, ready=10)
    run(model, tmp_path, limits=limits)
    assert model.sent == ["the task"]  # the grace outlasted the late submit


def test_the_wall_clock_cap_ends_the_run(tmp_path):
    limits = Limits(nudges=2, wall=0.5, stall=30, settle=0.2, grace=0.2, poll=0.02, ready=10)
    with pytest.raises(RuntimeError, match="wall-clock cap"):
        run(Model(state="busy-moving"), tmp_path, limits=limits)
    assert not server_up(tmp_path / "run")


def test_no_progress_while_busy_ends_the_run(tmp_path):
    limits = Limits(nudges=2, wall=30, stall=0.4, settle=0.2, grace=0.2, poll=0.02, ready=10)
    with pytest.raises(RuntimeError, match="no progress for"):
        run(Model(state="busy"), tmp_path, limits=limits)


def test_a_message_that_never_starts_a_turn_is_delivered_twice_then_fails(tmp_path):
    model = Model(state="starting")
    model.never_start = True
    limits = Limits(wall=5, stall=3, turn_start=0.15, poll=0.02)
    with pytest.raises(RuntimeError, match="did not start a turn.*delivery and retry"):
        run(model, tmp_path, limits=limits)
    assert model.sent == ["the task", "the task"]


def test_no_progress_while_starting_ends_at_stall_cap(tmp_path):
    model = Model(state="starting")
    limits = Limits(wall=5, stall=0.2, turn_start=3, poll=0.02)
    with pytest.raises(RuntimeError, match="no progress.*while starting"):
        run(model, tmp_path, limits=limits)


def test_waiting_cap_nudges_with_background_work_named(tmp_path):
    def on_turn(m, n, text):
        m.waiting = "a background shell is running"
        if n == 2:
            m.waiting = ""
            m.submit(word="done")

    model = Model(on_turn)
    limits = Limits(wall=5, stall=3, wait=0.2, settle=0.01, poll=0.02)
    run(model, tmp_path, limits=limits)
    assert "a background shell is running" in model.sent[1]


def test_interactive_dialog_is_dismissed_and_nudged(tmp_path):
    def on_turn(m, n, text):
        if n == 2:
            m.state = "idle"
            m.submit(word="done")

    model = Model(on_turn, state="blocked")
    limits = Limits(wall=5, stall=3, dialog=0.2, poll=0.02)
    run(model, tmp_path, limits=limits)
    assert "Nobody can answer here" in model.sent[1]


def test_submitted_outputs_win_over_a_transient_final_message(tmp_path):
    def on_turn(m, n, text):
        m.submit(word="done")
        m.error = "rate limit exceeded"

    run(Model(on_turn), tmp_path)


def test_a_transient_error_ending_a_turn_raises_transient(tmp_path):
    model = Model(lambda m, n, text: setattr(m, "error", "rate limit exceeded"))
    with pytest.raises(Transient):
        run(model, tmp_path)
    assert not server_up(tmp_path / "run")


def test_an_engine_that_exits_early_fails_the_run(tmp_path):
    with pytest.raises(RuntimeError, match=r"fake exited \(status \?\) before the step was done"):
        run(Model(state="exited"), tmp_path)


def test_resume_from_another_directory_is_refused(tmp_path):
    model = Model()
    model.cwd_of["s-old"] = "/somewhere/else"
    with pytest.raises(ValueError, match="started in /somewhere/else.*another directory"):
        run(model, tmp_path, session="s-old")
    assert model.sent == [] and not (tmp_path / "run" / "tmux.sock").exists()


def test_resume_from_the_same_directory_continues_the_session(tmp_path):
    model = Model(lambda m, n, text: m.submit(word="w"))
    model.cwd_of["s-old"] = str(tmp_path / "work")
    run(model, tmp_path, session="s-old")
    assert model.resumed == "s-old" and model.sent == ["the task"]


def test_a_retry_resumes_the_session_its_first_attempt_started(tmp_path):
    (tmp_path / "work").mkdir()
    (tmp_path / "run").mkdir()
    (tmp_path / "run" / "native.json").write_text(json.dumps(
        {"engine": "fake", "cwd": str((tmp_path / "work").resolve()), "session": "s-first"}))
    model = Model(lambda m, n, text: m.submit(word="w"))
    run(model, tmp_path, attempt=2)
    assert model.resumed == "s-first" and model.sent == [CONTINUE]


@pytest.mark.parametrize("task", ["x" * 501, "two\nlines"])
def test_a_long_or_multiline_task_is_handed_over_as_a_file(tmp_path, task):
    """A TUI collapses such a paste into a placeholder the model reads as pasted content, not
    as the user's request; one short typed line pointing at the file is read as the task."""
    model = Model(lambda m, n, text: m.submit(word="w"))
    cwd = tmp_path / "work"
    cwd.mkdir()
    supervise(model, task, cwd, tmp_path / "run", required=["word"], limits=FAST,
              log=lambda line: None, sent=lambda: model.outputs)
    task_md = (tmp_path / "run" / "task.md").resolve()
    assert model.sent == [POINTER.format(path=task_md)] and task_md.read_text() == task


class Feed:
    thread = "step-s"

    def __init__(self, *items):
        self.items = list(items)

    def poll(self):
        pass

    def peek(self):
        return self.items[0] if self.items else None

    def ack(self):
        self.items.pop(0)


def test_thread_messages_are_typed_into_the_session(tmp_path):
    def on_turn(m, n, text):
        if n == 3:
            m.submit(word="stopped")

    model = Model(on_turn)
    feed = Feed({"seq": 7, "from": "orchestrator", "body": "stop at step 2"},
                {"seq": 8, "from": "orchestrator", "body": "line one\nline two",
                 "data": {"k": 1}})
    _, lines = run(model, tmp_path, feed=feed)
    message = (tmp_path / "run" / "messages" / "8.md").resolve()
    assert model.sent == [
        "the task",
        "Message from orchestrator on your sluice thread `step-s`: stop at step 2",
        (f"A message from orchestrator on your sluice thread `step-s` is in {message}; "
         "read it now.")]
    assert message.read_text() == ("Message from orchestrator on your sluice thread "
                                   '`step-s`: line one\nline two\n\ndata: {"k": 1}')
    assert "thread message from orchestrator typed into the session" in lines


def test_thread_delivery_rpc_error_is_requeued(tmp_path):
    class Flaky(Model):
        failed = False
        def deliver(self, tmux, text):
            if text.startswith("Message from") and not self.failed:
                self.failed = True
                raise RuntimeError("turn already ended")
            super().deliver(tmux, text)

    model = Flaky(lambda m, n, text: m.submit(word="done") if n == 2 else None)
    feed = Feed({"seq": 7, "from": "orchestrator", "body": "finish"})
    run(model, tmp_path, feed=feed)
    assert model.failed and len(model.sent) == 2


@pytest.mark.parametrize("failed_body", ["first", "second"])
def test_failed_thread_delivery_retains_the_batch_suffix_and_new_arrivals(tmp_path, failed_body):
    class Flaky(Model):
        failures = 2

        def deliver(self, tmux, text):
            if text.endswith(failed_body) and self.failures:
                self.failures -= 1
                raise RuntimeError("turn already ended")
            super().deliver(tmux, text)

    model = Flaky()
    feed = Feed(*({"seq": seq, "from": "o", "body": body}
                  for seq, body in enumerate(("first", "second", "third"), 1)))
    runner = _Run(model, None, tmp_path, [], feed, FAST, lambda _: None, dict)
    runner.forward()
    runner.forward()
    feed.items.append({"seq": 4, "from": "o", "body": "fourth"})
    runner.forward()
    assert [text.rsplit(": ", 1)[1] for text in model.sent] == [
        "first", "second", "third", "fourth"]
    assert feed.items == []


def test_the_thread_feed_reads_messages_for_the_step(tmp_path):
    store = Store(tmp_path)
    store.create_project("p")
    store.append("p", {"kind": "message", "thread": "step-build", "from": "o", "body": "old"})
    feed = ThreadFeed(SimpleNamespace(home=tmp_path, project="p", step="build"))
    store.append("p", *[
        {"kind": "message", "thread": "step-build", "from": "o", "to": "build", "body": "a"},
        {"kind": "message", "thread": "step-build", "from": "build", "body": "mine"},
        {"kind": "message", "thread": "step-build", "from": "o", "to": "other", "body": "b"},
        {"kind": "message", "thread": "step-other", "from": "o", "body": "c"},
        {"kind": "message", "thread": "step-build", "from": "o", "body": "d",
         "data": {"k": 1}},
    ])
    feed.poll()
    assert feed.peek()["body"] == "a"
    feed.poll()
    assert feed.peek()["body"] == "a"
    feed.ack()
    assert feed.peek()["body"] == "d"
    feed.ack()
    feed.poll()
    assert feed.peek() is None


def test_required_outputs_are_the_non_optional_declared_ones():
    ctx = SimpleNamespace(outputs={"a": {"type": "string"}, "b": {"type": "string?"},
                                   "c": {"type": ["null", "int"]}, "d": {"type": "int[]"}})
    assert required_outputs(ctx) == ["a", "d"]


CANCEL_SCRIPT = """
import sys, time
sys.path.insert(0, {agents!r})
sys.path.insert(0, {tests!r})
from test_native import Model, FAST
from _agents.native.supervisor import supervise
supervise(Model(state="busy-moving"), "t", {cwd!r}, {run!r}, required=["x"], limits=FAST)
"""


def test_cancel_kills_the_tmux_server_and_the_engine(tmp_path):
    cwd, run_dir = tmp_path / "work", tmp_path / "run"
    cwd.mkdir()
    script = CANCEL_SCRIPT.format(agents=str(AGENTS), tests=str(Path(__file__).parent),
                                  cwd=str(cwd), run=str(run_dir))
    p = subprocess.Popen([sys.executable, "-c", script], stderr=subprocess.PIPE, text=True,
                         start_new_session=True)
    t = Tmux(run_dir)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline and not (run_dir.exists() and t.pane_pid()):
        time.sleep(0.05)
    engine = t.pane_pid()
    assert engine and alive(engine)
    os.killpg(p.pid, signal.SIGTERM)
    assert p.wait(timeout=10) == 128 + signal.SIGTERM, p.stderr.read()
    assert not alive(engine) and not server_up(run_dir)
    assert descendants(engine) == []


# ---- the Claude adapter's reading of its signals ----------------------------------------------
# Payload shapes as Claude Code 2.1.283 wrote them in the idle experiments (packs/README.md).

class FakePane:
    def dead(self):
        return None

    def pane_pid(self):
        return 4242


@pytest.fixture
def claude(tmp_path, monkeypatch):
    from _agents.native import claude as C
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "cc"))
    (tmp_path / "cc" / "sessions").mkdir(parents=True)
    transcript = tmp_path / "cc" / "projects" / "-w" / "s-1.jsonl"
    transcript.parent.mkdir(parents=True)
    transcript.write_text("")
    a = C.Claude()
    (tmp_path / "run").mkdir()
    a.prepare(tmp_path / "run", "/w", None)

    def status(value):
        (tmp_path / "cc" / "sessions" / "4242.json").write_text(json.dumps(
            {"pid": 4242, "sessionId": "s-1", "kind": "interactive", "status": value}))

    def hook(event, **payload):
        with open(a.hooks_file, "a") as f:
            f.write(json.dumps({"session_id": "s-1", "transcript_path": str(transcript),
                                "hook_event_name": event, **payload}) + "\n")

    def entry(**rec):
        with open(transcript, "a") as f:
            f.write(json.dumps(rec) + "\n")

    hook("SessionStart", source="startup")
    return SimpleNamespace(a=a, status=status, hook=hook, entry=entry, C=C)


def test_claude_disallows_interactive_questions(claude):
    argv = claude.a.argv()
    assert argv[argv.index("--disallowedTools") + 1] == "AskUserQuestion"


def test_a_background_shell_keeps_an_idle_session_waiting(claude):
    claude.hook("UserPromptSubmit", prompt="p")
    claude.hook("Stop", last_assistant_message="Started.", session_crons=[],
                background_tasks=[{"id": "b", "type": "shell", "status": "running",
                                   "description": "sleep 60 && echo done-sleeping"}])
    claude.status("shell")
    snap = claude.a.poll(FakePane())
    assert (snap.state, snap.turns, snap.waiting) == ("idle", 1,
                                                      "a background shell is running")
    claude.hook("UserPromptSubmit", prompt="<task-notification>…</task-notification>")
    claude.hook("Stop", last_assistant_message="FINISHED", background_tasks=[],
                session_crons=[])
    claude.status("idle")
    snap = claude.a.poll(FakePane())
    assert (snap.state, snap.turns, snap.waiting) == ("idle", 2, "")
    assert claude.a.final() == "FINISHED"


def test_a_background_agent_in_the_turn_end_keeps_it_waiting(claude):
    claude.hook("UserPromptSubmit", prompt="p")
    claude.hook("Stop", last_assistant_message="x", session_crons=[], background_tasks=[
        {"id": "a", "type": "local_agent", "status": "running", "description": "review"},
        {"id": "b", "type": "shell", "status": "completed", "description": "done"}])
    claude.status("idle")
    assert claude.a.poll(FakePane()).waiting == "background task: review"


def test_the_models_wakeup_keeps_it_waiting_until_it_fires(claude):
    claude.hook("UserPromptSubmit", prompt="p")
    claude.entry(type="assistant", message={"content": [
        {"type": "tool_use", "id": "w", "name": "ScheduleWakeup", "input": {}}]})
    claude.entry(type="user", toolUseResult={"scheduledFor": int((time.time() + 90) * 1000)},
                 message={"content": [{"type": "tool_result", "tool_use_id": "w"}]})
    claude.hook("Stop", last_assistant_message="Scheduled.", background_tasks=[],
                session_crons=[{"id": "55e8", "recurring": False, "prompt": "AWAKE"}])
    claude.status("idle")  # a pending wakeup leaves the status file at plain idle
    assert claude.a.poll(FakePane()).waiting.startswith("a wakeup at ")
    claude.entry(type="system", subtype="scheduled_task_fire", content="resuming")
    claude.hook("UserPromptSubmit", prompt="AWAKE")
    claude.hook("Stop", last_assistant_message="AWAKE", background_tasks=[],
                session_crons=[{"id": "80ba", "recurring": False, "prompt": "AWAKE"}])
    snap = claude.a.poll(FakePane())
    # the loop's own re-armed wakeup (no tool call made it) does not count
    assert (snap.turns, snap.waiting) == (2, "")


def test_a_one_shot_job_the_model_made_keeps_it_waiting(claude):
    claude.hook("UserPromptSubmit", prompt="p")
    claude.entry(type="assistant", message={"content": [
        {"type": "tool_use", "id": "c", "name": "CronCreate", "input": {}}]})
    claude.entry(type="user", toolUseResult={"id": "3ea1", "recurring": False},
                 message={"content": [{"type": "tool_result", "tool_use_id": "c"}]})
    claude.hook("Stop", last_assistant_message="Scheduled for 14:22.", background_tasks=[],
                session_crons=[{"id": "3ea1", "recurring": False}])
    claude.status("idle")
    assert claude.a.poll(FakePane()).waiting == "a scheduled job"


def test_idle_before_the_turn_end_arrives_reads_busy(claude, monkeypatch):
    claude.hook("UserPromptSubmit", prompt="p")
    claude.status("idle")
    snap = claude.a.poll(FakePane())
    assert (snap.state, snap.turns) == ("busy", 0)
    monkeypatch.setattr(claude.C, "UNSTOPPED", 0.0)  # an interrupted turn never gets a Stop
    snap = claude.a.poll(FakePane())
    assert (snap.state, snap.turns) == ("idle", 1)


@pytest.mark.parametrize("interruption", ["busy", "waiting", "new-prompt"])
def test_missing_stop_requires_continuous_idle_in_the_current_prompt(
        claude, monkeypatch, interruption):
    clock = [10.0]
    monkeypatch.setattr(claude.C.time, "monotonic", lambda: clock[0])
    monkeypatch.setattr(claude.C, "UNSTOPPED", 5.0)
    claude.hook("UserPromptSubmit", prompt="first")
    claude.status("idle")
    assert claude.a.poll(FakePane()).state == "busy"
    clock[0] = 12.0
    if interruption == "new-prompt":
        claude.hook("UserPromptSubmit", prompt="second")
    else:
        claude.status(interruption)
    assert claude.a.poll(FakePane()).turns == 0
    clock[0] = 16.0
    claude.status("idle")
    assert claude.a.poll(FakePane()).turns == 0
    clock[0] = 22.0
    snap = claude.a.poll(FakePane())
    assert (snap.state, snap.turns) == ("idle", 1)
    assert claude.a.poll(FakePane()).turns == 1


def test_a_turn_ending_in_an_api_error_reports_it(claude):
    claude.hook("UserPromptSubmit", prompt="p")
    claude.hook("StopFailure", error="rate_limit", last_assistant_message="API Error: 429")
    claude.status("idle")
    snap = claude.a.poll(FakePane())
    assert snap.error == "rate_limit API Error: 429" and snap.turns == 1


def test_a_resumed_sessions_history_is_not_replayed(tmp_path, monkeypatch):
    from _agents.native import claude as C
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "cc"))
    transcript = tmp_path / "cc" / "projects" / "-w" / "s-old.jsonl"
    transcript.parent.mkdir(parents=True)
    transcript.write_text(json.dumps({"type": "assistant", "cwd": "/w", "message": {
        "content": [{"type": "text", "text": "old news"}]}}) + "\n")
    a = C.Claude()
    assert a.session_cwd("s-old") == "/w" and a.session_cwd("nope") is None
    (tmp_path / "run").mkdir()
    a.prepare(tmp_path / "run", "/w", "s-old")
    assert a.argv()[-2:] == ["--resume", "s-old"]
    with open(a.hooks_file, "a") as f:
        f.write(json.dumps({"session_id": "s-old", "transcript_path": str(transcript),
                            "hook_event_name": "SessionStart", "source": "resume"}) + "\n")
    with open(transcript, "a") as f:
        f.write(json.dumps({"type": "assistant", "message": {
            "content": [{"type": "text", "text": "new work"}]}}) + "\n")
    a.poll(FakePane())
    assert a.progress() == ["new work"]
