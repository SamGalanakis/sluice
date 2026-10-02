"""Tests for how the native supervisor keeps a session on track and what it reports about it.
The supervisor is driven by test_native's scripted Model (its pane runs `sleep`, or `sh` where
the pane must start processes)."""

import json
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

from sluice import log as L
from sluice.fn import Transient
from sluice.store import Store

AGENTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(AGENTS))
sys.path.insert(0, str(Path(__file__).parent))

from _agents.native.claude import Claude
from _agents.native.codex import Codex
from _agents.native.processes import cgroup, detached, engine_env, tree_cpu
from _agents.native.supervisor import REMIND, Limits, Snapshot, _Run, lock_session
from test_agents import init_repo, make_claude, make_devin, session_of
from test_native import FakePane, Feed, Model, run


def repo_at(tmp_path):
    """A git repo at tmp_path/work, the cwd `run` gives the model."""
    return init_repo(tmp_path / "work")


def commit(repo, name, text="x\n"):
    (repo.path / name).write_text(text)
    repo.git("add", name)
    repo.git("commit", "-q", "-m", f"add {name}")


# ---- git facts ---------------------------------------------------------------------------------

def test_git_facts_keep_the_first_attempts_head_across_a_transient_retry(tmp_path):
    repo = repo_at(tmp_path)
    start = repo.git("rev-parse", "HEAD")

    def first(m, n, text):
        commit(repo, "a.txt")
        m.error = "rate limit exceeded"

    with pytest.raises(Transient):
        run(Model(first), tmp_path)
    assert json.loads((tmp_path / "run" / "native.json").read_text())["head_before"] == start

    def second(m, n, text):
        commit(repo, "b.txt")
        m.submit(word="w")

    out, _ = run(Model(second), tmp_path, attempt=2)
    assert out["git"] == {"head_before": start, "head_after": repo.git("rev-parse", "HEAD"),
                          "commits": 2, "dirty": False}


def test_outside_a_git_worktree_there_are_no_git_facts(tmp_path):
    out, _ = run(Model(lambda m, n, text: m.submit(word="w")), tmp_path)
    assert out["git"] is None
    assert "head_before" not in json.loads((tmp_path / "run" / "native.json").read_text())


def test_an_agent_fn_reports_git_facts_across_its_own_retry(call_fn, tmp_path):
    repo = init_repo(tmp_path / "repo")
    start = repo.git("rev-parse", "HEAD")
    work = "echo {0} > {0}.txt && git add {0}.txt && git -c user.email=a@b -c user.name=A " \
           "commit -q -m {0}"
    env, _ = make_claude(tmp_path, [{"run": work.format("one"), "error": "API Error: 529"},
                                    {"run": work.format("two"), "reply": "done"}])
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(repo.path), "prompt": "p"},
                             env=env)
    assert code == 0, err
    assert "transient (attempt 1)" in err
    assert out["git"] == {"head_before": start, "head_after": repo.git("rev-parse", "HEAD"),
                          "commits": 2, "dirty": False}


def test_agent_review_counts_commits_across_a_retry(call_fn, tmp_path):
    repo = init_repo(tmp_path / "repo")
    repo.git("branch", "base")
    work = "echo {0} > {0}.txt && git add {0}.txt && git -c user.email=a@b -c user.name=A " \
           "commit -q -m {0}"
    env, _ = make_claude(tmp_path, [{"run": work.format("one"), "error": "API Error: 529"},
                                    {"run": work.format("two"), "reply": "fixed"}])
    code, out, err = call_fn(AGENTS / "agent.review",
                             {"cwd": str(repo.path), "base": "base", "standards": "S.md"},
                             env=env)
    assert code == 0, err
    assert out["commits"] == 2 and out["sha"] == repo.git("rev-parse", "HEAD")
    assert out["git"]["commits"] == 2


# ---- the quiet-worktree note -------------------------------------------------------------------

QUICK = Limits(nudges=2, wall=30, stall=30, settle=0.2, grace=0.2, poll=0.02, ready=10,
               quiet=0.3)


def test_the_quiet_period_reads_its_env_override(monkeypatch):
    monkeypatch.setenv("SLUICE_AGENT_QUIET_MIN", "20")
    assert (Limits.from_env().quiet, Limits().quiet) == (1200, 45 * 60)


def test_active_descendants_restart_the_quiet_timer_then_idle_gets_notes(tmp_path, monkeypatch):
    import _agents.native.supervisor as mod

    notes, cpu = [], {(20, 100): 1}
    monkeypatch.setattr(mod.worktree, "sample", lambda cwd: ("abcdefg", "", ""))
    monkeypatch.setattr(mod, "tree_cpu", lambda roots: dict(cpu))
    model = Model()
    monkeypatch.setattr(model, "roots", lambda tmux: [10, 11])
    watcher = _Run(model, FakePane(), tmp_path, [], None, Limits(quiet=300),
                   lambda line: None, dict, record={"cwd": str(tmp_path),
                   "head_before": "abcdefg"}, note=notes.append)
    busy = Snapshot("busy")
    for now in range(30, 631, 30):
        cpu[20, 100] += 1
        watcher.watch(busy, now)
    assert notes == []  # CPU work past two quiet periods, without a worktree change
    watcher.watch(busy, 900)
    assert notes == []
    watcher.watch(busy, 930)
    assert len(notes) == 1
    watcher.watch(busy, 960)
    assert len(notes) == 1
    cpu.clear()
    cpu[20, 200] = 1  # the same pid now belongs to a new command
    watcher.watch(busy, 990)
    watcher.watch(busy, 1260)
    assert len(notes) == 1
    watcher.watch(busy, 1290)
    assert len(notes) == 2
    watcher.watch(Snapshot("idle"), 1320)
    watcher.watch(busy, 1500)
    watcher.watch(busy, 1770)
    assert len(notes) == 2
    watcher.watch(busy, 1800)
    assert len(notes) == 3


def test_tree_cpu_follows_commands_but_ignores_server_cpu(tmp_path, monkeypatch):
    import _agents.native.processes as mod

    proc = tmp_path / "proc"
    proc.mkdir()

    def process(pid, parent, argv, ticks=10, child_ticks=0, state="S"):
        d = proc / str(pid)
        d.mkdir()
        rest = [state, str(parent), *(["0"] * 18)]
        rest[11:15] = [str(ticks), "1", str(child_ticks), "2"]
        rest[19] = str(pid * 100)
        (d / "stat").write_text(f"{pid} (name with ) parentheses) " + " ".join(rest))
        (d / "cmdline").write_bytes(b"\0".join(a.encode() for a in argv) + b"\0")

    process(10, 1, ["codex"])
    process(11, 1, ["codex", "app-server"])
    process(20, 10, ["python", "/tools/mcp_server.py"])
    process(21, 11, ["codex", "app-server"])
    process(22, 10, ["tmux", "new-session"])
    process(23, 11, ["codex-code-mode-host"])
    process(24, 10, ["node", "/tools/figments-mcp/src/index.js"])
    process(25, 10, ["python", "-m", "mcp.server"])
    process(30, 23, ["bash", "-c", "cargo test"], child_ticks=40)
    process(31, 30, ["cargo", "test"], ticks=50)
    process(32, 21, ["python", "driver.py"], ticks=60)
    process(33, 11, ["python", "/work/mcp-refactor/tests/test_mcp.py"], ticks=70)
    process(40, 1, ["pytest"])
    process(41, 10, ["cargo"], state="Z")
    (proc / "42").mkdir()  # exited between listing /proc and reading stat
    (proc / "43").mkdir()
    (proc / "43/stat").write_text("malformed")
    (proc / "self").mkdir()
    monkeypatch.setattr(mod, "Path", lambda p: proc if p == "/proc" else Path(p))
    assert tree_cpu([10, 11, None]) == {
        (30, 3000): 53, (31, 3100): 53, (32, 3200): 63, (33, 3300): 73}
    assert tree_cpu([None]) == {}


def busy_until(model, done):
    """Let the busy model finish (idle, outputs submitted) once `done` is set."""
    def wait():
        done.wait(20)
        model.submit(word="w")
        model.state = None
    threading.Thread(target=wait, daemon=True).start()


def test_a_busy_session_with_a_quiet_worktree_gets_one_note_per_quiet_period(tmp_path):
    repo = repo_at(tmp_path)
    model, done, notes = Model(state="busy-moving"), threading.Event(), []

    def note(body):
        notes.append((time.monotonic(), body))
        if len(notes) == 2:
            done.set()

    busy_until(model, done)
    _, lines = run(model, tmp_path, limits=QUICK, note=note)
    head = repo.git("rev-parse", "--short=7", "HEAD")
    assert [b for _, b in notes] == [
        f"busy 0 min with no change to the worktree (HEAD {head}, no diff)"] * 2
    # again only after another full period, not at the next sample (every 0.03 s here); the
    # times are when note() ran, which trails the loop's clock by a git sample
    assert notes[1][0] - notes[0][0] >= QUICK.quiet - 0.1
    assert f"quiet: {notes[0][1]}" in lines


def test_a_worktree_that_keeps_changing_gets_no_note(tmp_path):
    repo = repo_at(tmp_path)
    model, done, notes = Model(state="busy-moving"), threading.Event(), []

    def churn():
        for i in range(25):  # 1.25 s of edits, far past the 0.3 s quiet period
            (repo.path / "scratch.txt").write_text("x" * i)
            time.sleep(0.05)
        done.set()

    threading.Thread(target=churn, daemon=True).start()
    busy_until(model, done)
    run(model, tmp_path, limits=QUICK, note=notes.append)
    assert notes == []


def test_the_quiet_note_is_posted_on_the_steps_thread(call_fn, tmp_path):
    repo = init_repo(tmp_path / "repo")
    env, rec = make_claude(tmp_path, [{"reply": "thinking", "busy_s": 1.5}])
    code, _out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(repo.path), "prompt": "p"},
                              env={**env, "SLUICE_AGENT_QUIET_MIN": "0.005"})
    assert code == 0, err
    got = L.read(tmp_path / "sluice-home", "test-project", threads=["step-test-step"])
    notes = got["records"]
    assert notes, err
    head = repo.git("rev-parse", "--short=7", "HEAD")
    for n in notes:
        assert (n["from"], n["to"], n["needs_reply"]) == ("test-step", "orchestrator", False)
        assert n["body"] == (f"test-step: busy 0 min with no change to the worktree "
                             f"(HEAD {head}, no diff)")
    assert len(rec.raw_prompts()) == 1  # the step's own note is not typed back in


# ---- background work and uncommitted changes at the end ---------------------------------------

def test_the_wait_for_background_work_reads_its_env_override(monkeypatch):
    monkeypatch.setenv("SLUICE_AGENT_WORK_MIN", "2")
    assert (Limits.from_env().work, Limits().work) == (120, 10 * 60)


class Shell(Model):
    """A model whose pane runs `sh`; `detach` (seconds) makes its first turn start a sleep in
    the background the way a tool's shell does (`(sleep N &)`: re-parented out of the pane)."""

    def __init__(self, detach, on_turn=None, before=None):
        super().__init__(on_turn or (lambda m, n, text: m.submit(word="w")))
        self.detach, self.before, self.pid = detach, before, None

    def argv(self):
        return ["sh"]

    def wait_ready(self, tmux, timeout):
        if self.before:  # work the session had let go before the task
            tmux.keys("-l", f"(sleep {self.before} &)")
            tmux.keys("Enter")
            self.pid = self.found(tmux)

    def found(self, tmux, known=()):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            new = set(detached([tmux.pane_pid()])) - set(known)
            if new:
                return new.pop()
            time.sleep(0.02)
        raise AssertionError("no detached process showed up")

    def deliver(self, tmux, text):
        if not self.sent and self.detach:
            tmux.keys("-l", f"(sleep {self.detach} &)")
            tmux.keys("Enter")
            self.pid = self.found(tmux, [self.pid] if self.pid else ())
        super().deliver(tmux, text)


def private_scopes(tmp_path):
    """Whether a tmux pane here runs in a cgroup of its own (what processes.detached needs)."""
    from _agents.native.tmux import Tmux
    t = Tmux(tmp_path / "probe")
    (tmp_path / "probe").mkdir()
    t.start(["sleep", "30"], tmp_path, dict(os.environ))
    try:
        group = cgroup(t.pane_pid())
    finally:
        t.kill()
    return group not in (None, "/", cgroup(os.getpid()))


@pytest.fixture
def scopes(tmp_path):
    if not private_scopes(tmp_path):
        pytest.skip("tmux panes here share this process's cgroup")


def reap(pid):
    if pid:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def test_the_end_waits_for_background_work_the_session_let_go(scopes, tmp_path):
    model = Shell(detach=1.0)
    started = time.monotonic()
    try:
        _, lines = run(model, tmp_path)
    finally:
        reap(model.pid)
    assert time.monotonic() - started >= 0.9
    assert f"waiting up to 10 min for background work: sleep (pid {model.pid})" in lines
    assert "background work ended" in lines


def test_submitted_outputs_wait_for_the_engines_own_background_work(tmp_path):
    """The incident (R2): the agent submits, then ends its turn with a background shell
    running; finishing then would kill it."""
    def on_turn(m, n, text):
        m.submit(word="w")
        m.waiting = "a background shell is running"
        threading.Timer(0.8, lambda: setattr(m, "waiting", "")).start()

    model = Model(on_turn)
    started = time.monotonic()
    _, lines = run(model, tmp_path)
    assert time.monotonic() - started >= 0.7 and model.sent == ["the task"]
    assert "waiting up to 10 min for background work: a background shell is running" in lines
    assert "background work ended" in lines


def test_submitted_outputs_with_background_work_still_obey_the_wall_cap(tmp_path):
    def on_turn(m, n, text):
        m.submit(word="w")
        m.waiting = "a background shell is running"
        m.error = "rate limit exceeded"

    limits = Limits(wall=0.1, stall=3, work=0.3, poll=0.01, ready=10)
    with pytest.raises(RuntimeError, match="wall-clock cap"):
        run(Model(on_turn), tmp_path, limits=limits)


def test_thread_messages_interrupt_finalization_and_wait_for_the_new_turn(tmp_path):
    class Receiving(Model):
        busy_polls = 0

        def poll(self, tmux):
            if self.turns == 2 and self.busy_polls < 3:
                self.busy_polls += 1
                return Snapshot("busy", 1, starts=self.starts, progress=self.busy_polls)
            return super().poll(tmux)

    def on_turn(m, n, text):
        m.submit(word="w")
        m.waiting = "a background shell is running" if n == 1 else ""

    feed = Feed({"seq": 1, "from": "o", "body": "finish this too"})
    model = Receiving(on_turn)
    out, _ = run(model, tmp_path, feed=feed,
                 limits=Limits(wall=3, work=0.1, poll=0.01, ready=10))
    assert len(model.sent) == 2 and model.sent[1].endswith("finish this too")
    assert model.busy_polls == 3 and out["final"] == "reply 2"


def test_finalization_retries_an_unacknowledged_thread_message(tmp_path):
    class Flaky(Model):
        failed = False

        def deliver(self, tmux, text):
            if text.startswith("Message from") and not self.failed:
                self.failed = True
                raise RuntimeError("turn already ended")
            super().deliver(tmux, text)

    model = Flaky(lambda m, n, text: m.submit(word="w"))
    feed = Feed({"seq": 1, "from": "o", "body": "finish this too"})
    out, _ = run(model, tmp_path, feed=feed)
    assert model.failed and feed.items == []
    assert len(model.sent) == 2 and out["final"] == "reply 2"


def test_context_compaction_is_reprimed_while_finalization_waits(tmp_path, monkeypatch):
    from _agents.native import reprime
    monkeypatch.setattr(reprime, "context", lambda *_: "restored context")

    class Compacted(Model):
        def poll(self, tmux):
            snap = super().poll(tmux)
            snap.compactions = 1
            return snap

    def on_turn(m, n, text):
        m.submit(word="w")
        m.waiting = "a background shell is running" if n == 1 else ""

    model = Compacted(on_turn)
    _, lines = run(model, tmp_path,
                   limits=Limits(wall=3, work=0.1, poll=0.01, ready=10))
    assert model.sent == ["the task", "restored context"]
    assert "context compacted; the step's context was typed into the session" in lines


def test_the_wait_for_background_work_is_bounded(scopes, tmp_path):
    model = Shell(detach=60)
    limits = Limits(nudges=2, wall=30, stall=30, settle=0.2, grace=0.2, poll=0.02, ready=10,
                    work=0.3)
    started = time.monotonic()
    try:
        _, lines = run(model, tmp_path, limits=limits)
        assert Path(f"/proc/{model.pid}").exists()  # never killed: only waited for
    finally:
        reap(model.pid)
    assert time.monotonic() - started < 10
    assert (f"background work still running after 0 min (SLUICE_AGENT_WORK_MIN); finishing "
            f"anyway: sleep (pid {model.pid})") in lines


def test_work_let_go_before_the_task_is_not_waited_for(scopes, tmp_path):
    model = Shell(detach=0, before=60)
    try:
        _, lines = run(model, tmp_path)
    finally:
        reap(model.pid)
    assert not any("background work" in ln for ln in lines)


def test_uncommitted_changes_get_one_reminder_and_are_never_committed(tmp_path):
    repo = repo_at(tmp_path)
    start = repo.git("rev-parse", "HEAD")

    def on_turn(m, n, text):
        (repo.path / "seed.txt").write_text("edited\n")
        m.submit(word="w")

    model = Model(on_turn)
    out, lines = run(model, tmp_path)
    assert model.sent == ["the task", REMIND.format(status="M seed.txt")]
    assert "uncommitted changes: reminding the agent once" in lines
    assert repo.git("rev-parse", "HEAD") == start
    assert repo.git("status", "--short") == "M seed.txt"
    assert out["git"] == {"head_before": start, "head_after": start, "commits": 0,
                          "dirty": True}


def test_a_reminded_agent_that_commits_leaves_a_clean_worktree(tmp_path):
    repo = repo_at(tmp_path)

    def on_turn(m, n, text):
        if n == 1:
            (repo.path / "seed.txt").write_text("edited\n")
            m.submit(word="w")
        else:
            repo.git("commit", "-q", "-am", "the edit")

    model = Model(on_turn)
    out, _ = run(model, tmp_path, required=())
    assert len(model.sent) == 2 and model.sent[1].startswith("You have uncommitted changes")
    assert out["git"]["commits"] == 1 and out["git"]["dirty"] is False


def test_untracked_files_alone_get_no_reminder(tmp_path):
    repo = repo_at(tmp_path)
    model = Model(lambda m, n, text: ((repo.path / "new.txt").write_text("n"),
                                      m.submit(word="w")))
    run(model, tmp_path)
    assert model.sent == ["the task"]


# ---- the re-prime after a compaction ----------------------------------------------------------

def compact_hook(settings):
    groups = json.loads(Path(settings).read_text())["hooks"]["SessionStart"]
    [group] = [g for g in groups if g.get("matcher") == "compact"]
    return group["hooks"][0]["command"]


def run_hook(command, payload, **env):
    base = {k: v for k, v in os.environ.items() if not k.startswith("SLUICE_")}
    p = subprocess.run(["sh", "-c", command], input=json.dumps(payload), text=True,
                       capture_output=True, env={**base, **env},
                       check=True, timeout=60)
    return p.stdout


def test_claude_is_reprimed_with_sluice_me_after_a_compaction(tmp_path, monkeypatch):
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "cc"))
    home = tmp_path / "home"
    store = Store(home)
    store.create_project("p", "", "t", "t")
    store.patch("p", 1, [{"op": "replace", "path": "/steps", "value": {
        "fix-x": {"run": "core.echo", "in": {"value": {"default": "v"}},
                  "doc": "Fix the x bug."}}}], "t", "t")
    (tmp_path / "run").mkdir()
    a = Claude()
    a.prepare(tmp_path / "run", str(tmp_path), None)
    command = compact_hook(a.settings)
    task = tmp_path / "run" / "task.md"
    env = {"SLUICE_HOME": str(home), "SLUICE_PROJECT": "p", "SLUICE_STEP": "fix-x"}
    out = json.loads(run_hook(command, {"hook_event_name": "SessionStart",
                                        "source": "compact"}, **env))
    context = out["hookSpecificOutput"]["additionalContext"]
    assert out["hookSpecificOutput"]["hookEventName"] == "SessionStart"
    assert context.startswith(f"Your context was just compacted. This is where your sluice "
                              f"step stands (`sluice me`); your full task is in {task}.")
    assert "step fix-x (core.echo) — pending" in context and "doc: Fix the x bug." in context
    assert run_hook(command, {"source": "startup"}, **env) == ""
    assert run_hook(command, {"source": "resume"}, **env) == ""


def test_a_failing_sluice_me_reprimes_with_the_task_path(tmp_path, monkeypatch):
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "cc"))
    (tmp_path / "run").mkdir()
    a = Claude()
    a.prepare(tmp_path / "run", str(tmp_path), None)
    out = json.loads(run_hook(compact_hook(a.settings), {"source": "compact"},
                              SLUICE_HOME=str(tmp_path / "home")))
    task = tmp_path / "run" / "task.md"
    assert out["hookSpecificOutput"]["additionalContext"] == (
        f"Your context was just compacted. Your full task is in {task}; read it again before "
        "you continue (`sluice me` failed: sluice me: not inside a step (SLUICE_PROJECT and "
        "SLUICE_STEP are not set); pass --project and --step).")


def test_devin_is_reprimed_after_its_post_compaction_hook(call_fn, tmp_path):
    env, rec = make_devin(tmp_path, [{"compact": True, "busy_s": 0.3, "reply": "working"},
                                     {"reply": "back on track"}])
    code, out, err = call_fn(AGENTS / "agent.devin", {"cwd": str(tmp_path), "spec": "s"},
                             env=env)
    assert code == 0, err
    task = call_fn.run_dirs[-1] / "task.md"
    prompts = rec.prompts()
    assert len(prompts) == 2 and out["final"] == "back on track"
    assert prompts[1].startswith(f"Your context was just compacted. Your full task is in "
                                 f"{task}; read it again")
    assert "has no step 'test-step'" in prompts[1]  # the fn's `sluice me` ran; no plan here
    assert "context compacted" in err
    assert task.read_text().startswith(prompts[0][:40].split("\n")[0])


def test_codex_counts_its_context_compactions():
    codex = Codex()
    codex.thread, codex.resuming = "t", True

    class Rpc:
        def drain(self):
            return [{"method": "item/completed", "params": {
                "threadId": "t", "item": {"type": "contextCompaction", "id": "c"}}}]

    class Server:
        def poll(self):
            return None

    codex.rpc, codex.server = Rpc(), Server()
    snap = codex.poll(FakePane())
    assert snap.compactions == 1 and codex.lines == ["codex: context compacted"]


def test_an_engine_reporting_a_compaction_gets_the_steps_context(tmp_path, monkeypatch):
    from _agents.native import reprime
    monkeypatch.setattr(reprime, "context",
                        lambda task, env=None: f"step x — running\ntask {task}")

    class Compacting(Model):
        def poll(self, tmux):
            snap = super().poll(tmux)
            snap.compactions = 1 if self.turns else 0
            return snap

    model = Compacting(lambda m, n, text: m.submit(word="w") if n == 2 else None)
    _, lines = run(model, tmp_path)
    message = (tmp_path / "run" / "messages" / "compact-1.md").resolve()
    assert model.sent[1] == (f"Your context was compacted; where your step stands is in "
                             f"{message}; read it now.")
    assert message.read_text() == f"step x — running\ntask {tmp_path / 'run' / 'task.md'}"
    assert model.sent.count(model.sent[1]) == 1  # once per compaction
    assert "context compacted; the step's context was typed into the session" in lines


# ---- one writer per session --------------------------------------------------------------------

def test_a_second_run_resuming_a_held_session_fails_at_once(tmp_path, monkeypatch):
    monkeypatch.setenv("SLUICE_PROJECT", "p")
    monkeypatch.setenv("SLUICE_STEP", "other")
    monkeypatch.setenv("SLUICE_RUN_ID", "r-1")
    held = lock_session("fake", "s-old")
    model = Model(lambda m, n, text: m.submit(word="w"))
    with pytest.raises(RuntimeError, match=r"fake session s-old is in use by step other of "
                                           r"project p \(run r-1\): two runs cannot resume"):
        run(model, tmp_path, session="s-old")
    assert model.sent == []
    held.close()
    run(model, tmp_path, session="s-old")
    assert model.resumed == "s-old"
    lock_session("fake", "s-old").close()  # released when the run ended


def test_a_failed_run_releases_its_session_lock(tmp_path):
    with pytest.raises(RuntimeError):
        run(Model(), tmp_path, session="s-old")
    lock_session("fake", "s-old").close()


def test_the_lock_is_taken_per_engine_session(tmp_path):
    held = lock_session("fake", "s-a")
    try:
        run(Model(lambda m, n, text: m.submit(word="w")), tmp_path, session="s-b")
    finally:
        held.close()
    lines = (Path(os.environ["SLUICE_HOME"]) / "locks" / "fake-s-a.lock").read_text()
    assert json.loads(lines)["pid"] == os.getpid()


# ---- the git environment -----------------------------------------------------------------------

GIT_QUIET = {"GIT_TERMINAL_PROMPT": "0", "GIT_EDITOR": "true", "GIT_MERGE_AUTOEDIT": "no"}


def test_git_never_prompts_in_either_env_builder(monkeypatch):
    monkeypatch.setenv("CLAUDECODE", "1")
    for env in (engine_env(), Claude().env()):
        assert GIT_QUIET.items() <= env.items() and "CLAUDECODE" not in env


def test_claude_env_is_the_engine_env():
    assert Claude().env() == engine_env()


# ---- the session of a failed run ---------------------------------------------------------------

def test_a_failed_run_ends_its_error_with_its_session(tmp_path):
    with pytest.raises(RuntimeError) as e:
        run(Model(), tmp_path)
    assert str(e.value).splitlines()[-1] == (
        "session: s-1. To resume it, bind the step's session input to it and retry: "
        'step_set_input(project, step, "session", "s-1"), then step_retry.')


def test_a_failed_agent_fn_names_its_session_last(call_fn, tmp_path):
    env, _ = make_claude(tmp_path, [{"reply": "not done"}] * 3)
    code, _out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                              env={**env, "SLUICE_STEP_OUTPUTS": json.dumps(
                                  {"word": {"type": "string"}})})
    assert code == 1
    sid = session_of(call_fn)
    assert err.strip().splitlines()[-1].startswith(f"session: {sid}. To resume it")
    error = json.loads((call_fn.run_dirs[-1] / "error.json").read_text())["message"]
    assert error.endswith(f'step_set_input(project, step, "session", "{sid}"), then '
                          "step_retry.")


# ---- the context fill --------------------------------------------------------------------------

def usage(inp, read, created, out=300):
    return {"input_tokens": inp, "cache_read_input_tokens": read,
            "cache_creation_input_tokens": created, "output_tokens": out}


@pytest.fixture
def transcript(tmp_path, monkeypatch):
    monkeypatch.setenv("CLAUDE_CONFIG_DIR", str(tmp_path / "cc"))
    (tmp_path / "cc" / "sessions").mkdir(parents=True)
    path = tmp_path / "cc" / "projects" / "-w" / "s-1.jsonl"
    path.parent.mkdir(parents=True)
    path.write_text("")
    a = Claude()
    (tmp_path / "run").mkdir()
    a.prepare(tmp_path / "run", "/w", None)
    with open(a.hooks_file, "a") as f:
        f.write(json.dumps({"session_id": "s-1", "transcript_path": str(path),
                            "hook_event_name": "SessionStart", "source": "startup"}) + "\n")

    def write(*entries):
        with open(path, "a") as f:
            f.writelines(json.dumps(e) + "\n" for e in entries)
        a.poll(FakePane())
        return a.progress()

    return write


def said(text, use=None, **extra):
    msg = {"role": "assistant", "content": [{"type": "text", "text": text}]}
    return {"type": "assistant", "message": {**msg, **({"usage": use} if use else {})}, **extra}


def test_the_context_fill_leads_the_last_progress_line(transcript):
    assert transcript(said("no usage yet")) == ["no usage yet"]
    assert transcript(said("reading", usage(5, 140_000, 2_300)),
                      said("editing", usage(8, 141_000, 900))) == [
        "reading", "ctx 142k · editing"]
    # a subagent's entry in the main transcript is not the session's own context
    assert transcript(said("aside", usage(1, 9_000, 0), isSidechain=True)) == [
        "  ctx 142k · aside"]
    assert transcript(said("after the compaction", usage(3, 20_000, 1_000))) == [
        "ctx 21k · after the compaction"]
