"""The runner's process boundary (SPEC §6): a run is reserved in the database before its
process starts, processes are started and stopped outside write transactions, and a failure
at any point never launches a run twice. Also the run-dir GC."""

import fcntl
import json
import os
import shutil
import threading
import time

import pytest

from sluice import calls, db
from sluice import log as L
from sluice import runner as R
from sluice.errors import BadRequest
from sluice.runner import NOT_STARTED, Runner
from sluice.store import Store
from tests.conftest import create, d, settle, write_config
from tests.test_adopt import kills, wait_shim  # noqa: F401 - the fixture

WAIT = {"w": {"run": "test.wait", "in": {"value": d("x")}}}
GATES = {"g": {"run": "test.gate", "scatter": "tag", "in": {"tag": d(["a", "b"])}}}


def entry(store, sid="w", project="p"):
    return store.read_state(project)["steps"].get(sid, {})


def adopted(store, project="p"):
    return [(r["run"], r["outcome"]) for r in L.read(store.home, project,
                                                     kinds=["run.adopt"])["records"]]


class Died(BaseException):
    """The runner stopping at this point (as if killed): nothing catches it."""


def test_a_failure_before_the_reservation_commits_starts_nothing(store, monkeypatch):
    create(store, "p", WAIT)
    real = L.append

    def fail(conn, project, records, cap=L.DEFAULT_MAX):
        if any(r.get("to") == "running" for r in records):
            raise Died()
        return real(conn, project, records, cap)

    monkeypatch.setattr(L, "append", fail)
    spawned = []
    monkeypatch.setattr(R, "spawn", lambda *a: spawned.append(a))
    runner = Runner(store)
    with pytest.raises(Died):
        runner.tick()
    assert entry(store) == {}
    assert spawned == [] and runner.active == {} and not store.runs_dir("p").exists()


def test_a_runner_dying_between_reservation_and_start_leaves_a_run_never_started(
        store, monkeypatch):
    create(store, "p", WAIT)
    monkeypatch.setattr(Runner, "_launch", lambda self, project, launch: None)  # dies here
    Runner(store).tick()
    e = entry(store)
    assert e["status"] == "running" and len(e["run_ids"]) == 1
    monkeypatch.undo()
    spawned = []
    monkeypatch.setattr(R, "spawn", lambda *a: spawned.append(a))
    steps = settle(Runner(store), store, "p")  # the next runner never relaunches it
    assert steps["w"]["status"] == "failed" and steps["w"]["error"] == NOT_STARTED
    assert adopted(store) == [(e["run_ids"][0], "not started")] and spawned == []


def test_a_partial_scatter_launch_fails_only_the_items_never_started(store, kills,  # noqa: F811
                                                                     monkeypatch):
    create(store, "p", GATES)
    real, started = R.spawn, []

    def first_only(*a):  # the runner dies after starting item 0
        if started:
            raise Died()
        started.append(real(*a))
        return started[-1]

    monkeypatch.setattr(R, "spawn", first_only)
    with pytest.raises(Died):
        Runner(store).tick()
    monkeypatch.undo()
    e = entry(store, "g")
    runs = [store.runs_dir("p") / rid for rid in e["run_ids"]]
    kills.append(wait_shim(runs[0]))
    assert not runs[1].exists()
    r2 = Runner(store)
    r2.tick()
    assert adopted(store) == [(runs[0].name, "watching"), (runs[1].name, "not started")]
    (runs[0] / "go").write_text("")
    e = settle(r2, store, "p")["g"]
    assert e["status"] == "failed" and e["error"] == f"run 1: {NOT_STARTED}"
    assert e["results"] == [{"tag": "a"}, None]
    store.retry("p", "g", author="t", reason="again")  # item 1 alone runs again
    e = settle(r2, store, "p", until=lambda s: s["g"]["status"] == "running"
               and s["g"].get("done") == 1)["g"]
    assert e["run_ids"][0] == runs[0].name and e["run_ids"][1] != runs[1].name
    (store.runs_dir("p") / e["run_ids"][1] / "go").write_text("")
    e = settle(r2, store, "p")["g"]
    assert e["status"] == "succeeded" and e["outputs"] == {"tag": ["a", "b"]}


def test_a_runner_dying_after_the_start_is_adopted_not_relaunched(store, kills):  # noqa: F811
    create(store, "p", WAIT)
    Runner(store).tick()  # started, then this runner is gone
    [rid] = entry(store)["run_ids"]
    kills.append(wait_shim(store.runs_dir("p") / rid))
    r2 = Runner(store)
    r2.tick()
    assert adopted(store) == [(rid, "watching")] and entry(store)["run_ids"] == [rid]
    assert [p.name for p in store.runs_dir("p").iterdir()] == [rid]
    (store.runs_dir("p") / rid / "go").write_text("")
    assert settle(r2, store, "p")["w"]["outputs"] == {"value": "x"}


def test_a_result_whose_commit_fails_is_recorded_once_on_the_next_tick(store, monkeypatch):
    create(store, "p", WAIT)
    runner = Runner(store)
    runner.tick()
    [rid] = entry(store)["run_ids"]
    (store.runs_dir("p") / rid / "go").write_text("")
    real = store.write_state
    busy = [True]

    def write_state(project, state):
        if busy and state["steps"]["w"]["status"] == "succeeded":
            busy.clear()
            raise db.Busy()
        real(project, state)

    monkeypatch.setattr(store, "write_state", write_state)
    deadline = time.time() + 30
    while busy and time.time() < deadline:
        runner.tick()  # the Busy is reported, and the tick goes on with other projects
        time.sleep(0.05)
    assert not busy and entry(store)["status"] == "running" and runner.active
    runner.tick()
    assert entry(store)["status"] == "succeeded" and not runner.active
    done = [r for r in L.read(store.home, "p", kinds=["step.status"])["records"]
            if r["to"] == "succeeded"]
    assert len(done) == 1


def test_a_cancel_during_a_reserved_launch_starts_nothing(store, monkeypatch):
    create(store, "p", WAIT)
    real = Runner._launch

    def cancel_first(self, project, launch):
        store.cancel_steps("p", "w", author="me", reason="changed my mind")
        real(self, project, launch)

    monkeypatch.setattr(Runner, "_launch", cancel_first)
    runner = Runner(store)
    runner.tick()
    monkeypatch.undo()
    [rid] = entry(store)["run_ids"]
    assert not (store.runs_dir("p") / rid).exists()
    e = settle(runner, store, "p")["w"]
    assert (e["status"], e["error"]) == ("failed", "cancelled: changed my mind")
    assert not (store.runs_dir("p") / rid).exists() and not runner.active


def cancel_during_first_spawn(store, monkeypatch, step, kills):  # noqa: F811
    """Spawn for real, but commit `step_cancel` of `step` (from another Store) while the
    tick's first spawn is under way; returns the steps' env names of what was spawned."""
    real, spawned = R.spawn, []

    def spawn(fn, inp, run_dir, env):
        if not spawned:
            Store(store.home).cancel_steps("p", step, author="me", reason="stop")
        proc = real(fn, inp, run_dir, env)
        kills.append(proc.pid)
        spawned.append((env["SLUICE_STEP"], run_dir.name))
        return proc

    monkeypatch.setattr(R, "spawn", spawn)
    return spawned


def test_cancel_second_reserved_step_during_first_spawn(store, kills, monkeypatch):  # noqa: F811
    create(store, "p", {"a": {"run": "test.wait", "in": {"value": d("a")}},
                        "b": {"run": "test.wait", "in": {"value": d("b")}}})
    spawned = cancel_during_first_spawn(store, monkeypatch, "b", kills)
    runner = Runner(store)
    runner.tick()
    [rid] = entry(store, "b")["run_ids"]
    assert [s for s, _ in spawned] == ["a"] and not (store.runs_dir("p") / rid).exists()
    e = settle(runner, store, "p", until=lambda s: s["b"]["status"] == "failed")["b"]
    assert e["error"] == "cancelled: stop" and not (store.runs_dir("p") / rid).exists()
    assert entry(store, "a")["status"] == "running" and len(spawned) == 1


def test_cancel_scatter_during_partial_launch(store, kills, monkeypatch):  # noqa: F811
    create(store, "p", {"g": {"run": "test.gate", "scatter": "tag",
                              "in": {"tag": d(["a", "b", "c"])}}})
    spawned = cancel_during_first_spawn(store, monkeypatch, "g", kills)
    runner = Runner(store)
    runner.tick()
    run_ids = entry(store, "g")["run_ids"]
    assert [r for _, r in spawned] == run_ids[:1]  # item 0 started; 1 and 2 never do
    first = store.runs_dir("p") / run_ids[0]
    wait_shim(first)
    e = settle(runner, store, "p")["g"]
    assert (e["status"], e["error"]) == ("failed", "cancelled: stop") and not runner.active
    assert not R.lock_held(first / "shim.lock")  # the started item was stopped
    assert [p.name for p in store.runs_dir("p").iterdir()] == run_ids[:1] and len(spawned) == 1


def test_other_projects_write_while_a_cancelled_step_is_stopped(store, kills,  # noqa: F811
                                                                monkeypatch):
    """Stopping a run (SIGTERM, a grace, SIGKILL) happens outside any write transaction."""
    create(store, "p", WAIT)
    store.create_project("q")
    runner = Runner(store)
    runner.tick()
    [rid] = entry(store)["run_ids"]
    kills.append(wait_shim(store.runs_dir("p") / rid))
    store.cancel_steps("p", "w", author="me", reason="stop")
    wrote = []
    real = R.kill

    def slow_kill(*runs, **kw):
        if runs:  # while the stop is under way, another writer gets the database at once
            other = Store(store.home)
            t = threading.Thread(target=lambda: wrote.append(other.append("q", {
                "kind": "message", "thread": "t", "from": "x", "body": "meanwhile"})))
            t.start()
            t.join(5)
        return real(*runs, **kw)

    monkeypatch.setattr(R, "kill", slow_kill)
    monkeypatch.setattr(db, "TIMEOUT", 0.5)
    runner.tick()
    assert wrote and entry(store)["error"] == "cancelled: stop"


def test_a_rolled_back_tick_leaves_the_runner_as_it_was(store, monkeypatch):
    create(store, "p", {"w": {"run": "test.wait", "in": {}}, "e": {"run": "core.echo",
                                                                  "in": {"value": d(1)}}})
    runner = Runner(store)
    real = store.write_state

    def fail(project, state):
        raise Died()

    monkeypatch.setattr(store, "write_state", fail)
    with pytest.raises(Died):
        runner.tick()
    assert runner.active == {} and entry(store) == {} and entry(store, "e") == {}
    monkeypatch.setattr(store, "write_state", real)
    runner.tick()
    assert entry(store)["status"] == "running" and entry(store, "e")["status"] == "succeeded"
    assert list(runner.active) == [("step", "p", "w")]
    for a in runner.active.values():
        a.kill()


def test_a_call_reserved_but_never_started_fails_on_adoption(store, monkeypatch):
    call = calls.create(store, "test.wait", {"value": 1}, None)

    def die(*a):
        raise Died()

    monkeypatch.setattr(R, "spawn", die)
    with pytest.raises(Died):
        Runner(store).tick()  # reserved (running), then the runner is gone
    monkeypatch.undo()
    assert calls.latest(store, call, None)["status"] == "running"
    runner = Runner(store)
    runner.tick()
    res = calls.status(store, call, None)
    assert res["status"] == "failed" and res["error"] == NOT_STARTED
    assert adopted(store, None) == [(call, "not started")]


def test_call_spawn_error_survives_busy_result_write(store, monkeypatch):
    call = calls.create(store, "test.wait", {"value": 1}, None)
    spawned, busy = [], [True]

    def no_spawn(*a):
        spawned.append(a)
        raise OSError("no such shim")

    real = calls.record

    def record(store_, project, rec, was=calls.LIVE):
        if busy and rec["status"] == "failed":
            busy.clear()
            raise db.Busy()
        return real(store_, project, rec, was)

    monkeypatch.setattr(R, "spawn", no_spawn)
    monkeypatch.setattr(calls, "record", record)
    runner = Runner(store)
    runner.tick()  # reserved, the spawn fails, recording that is Busy: retried next tick
    assert not busy and calls.latest(store, call, None)["status"] == "running"
    runner.tick()
    res = calls.status(store, call, None)
    assert (res["status"], res["error"]) == ("failed", "could not start the fn: no such shim")
    ends = [r for r in L.read(store.home, None, kinds=["call"])["records"]
            if r["status"] == "failed"]
    assert len(ends) == 1 and len(spawned) == 1 and not runner.active


# ---- the run-dir GC ------------------------------------------------------------------------


def test_gc_keeps_every_referenced_run_dir_and_removes_the_rest(tmp_path, monkeypatch):
    home = tmp_path / "home"
    write_config(home, log_max=100)
    store = Store(home)
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    runs = store.runs_dir("p")
    names = ["by-record-run", "by-call", "by-status", "by-state", "by-kept", "by-live-call",
             "by-done-call", "by-submission", "live-shim", "nothing", "dead"]
    for n in names:
        (runs / n).mkdir(parents=True)
    store.append("p", {"kind": "run.orphan", "run": "by-record-run"},
                 {"kind": "call", "call": "by-call", "fn": "x", "status": "failed"},
                 {"kind": "step.status", "step": "a", "from": "running", "to": "failed",
                  "run_ids": ["by-status"]})
    with store.tx() as conn:
        store.write_state("p", {"inputs": {}, "steps": {"a": {
            "status": "pending", "run_ids": ["by-state"],
            "kept": {"run_ids": ["by-kept"]}}}})
        for call, status in (("by-live-call", "running"), ("by-done-call", "succeeded")):
            conn.execute("INSERT INTO calls (call, project, fn, status, inputs, created) "
                         "VALUES (?, 'p', 'x', ?, '{}', 'now')", (call, status))
        conn.execute("INSERT INTO submissions (project, run, step, outputs, at) "
                     "VALUES ('p', 'by-submission', 'a', '{}', 'now')")
    lock = os.open(runs / "live-shim" / "shim.lock", os.O_RDWR | os.O_CREAT)
    fcntl.flock(lock, fcntl.LOCK_EX)
    (home / "trash" / "old-1").mkdir(parents=True)
    (home / "runs" / "home-stray").mkdir(parents=True)
    runner = Runner(store)
    fail = [True]
    real = shutil.rmtree

    def flaky(path, ignore_errors=False):  # the first pass cannot remove `dead`
        if fail and os.path.basename(path) == "dead":
            return
        real(path, ignore_errors=ignore_errors)

    monkeypatch.setattr(R.shutil, "rmtree", flaky)
    try:
        runner._gc()
        left = sorted(p.name for p in runs.iterdir())
        assert left == sorted(set(names) - {"nothing"})
        fail.clear()
        runner._gc()  # the retry
        assert sorted(p.name for p in runs.iterdir()) == sorted(set(names) - {"nothing", "dead"})
        assert not (home / "trash" / "old-1").exists()
        assert not (home / "runs" / "home-stray").exists()
    finally:
        os.close(lock)


@pytest.mark.parametrize("when", ["before", "after"])
def test_run_gc_and_orphan_sweep_race_reference_then_directory(store, kills,  # noqa: F811
                                                               monkeypatch, when):
    """Direct calls launched (reference committed, then dir made, then process) while the GC
    and the orphan sweep have listed the run dirs, `when` their references are read: a pass
    either never lists the dir or finds it referenced. The GC's run is held with its dir made
    and no process yet (no shim lock to protect it); the sweep's run lives. Neither is removed
    nor killed."""
    stale = store.runs_dir(None) / "stale"
    stale.mkdir(parents=True)  # nothing refers to it: the GC's pass does remove it
    gates = {k: (threading.Event(), threading.Event()) for k in ("gc", "sweep", "dir")}

    def pause(gate):
        reached, go = gates[gate]
        reached.set()
        assert go.wait(20)

    def between(real, gate):  # the pass has listed the dirs
        def f(*a):
            if a[-1] is None and when == "before":
                pause(gate)
            refs = real(*a)
            if a[-1] is None and when == "after":
                pause(gate)
            return refs
        return f

    real_spawn = R.spawn

    def spawn(fn, inp, run_dir, env):  # the first launch: its dir, then (later) its process
        if not gates["dir"][0].is_set():
            run_dir.mkdir(parents=True)
            pause("dir")
        proc = real_spawn(fn, inp, run_dir, env)
        kills.append(proc.pid)  # stopped however the test goes
        return proc

    runner = Runner(store)
    monkeypatch.setattr(L, "refs", between(L.refs, "gc"))
    monkeypatch.setattr(runner, "_referenced_runs", between(runner._referenced_runs, "sweep"))
    monkeypatch.setattr(R, "spawn", spawn)

    def reserve():
        return calls.create(store, "test.wait", {"value": 1}, None, direct=True)

    def launch(call):
        launches.append(threading.Thread(target=lambda: results.append(
            R.run_call_direct(Store(store.home), call, None)), daemon=True))
        launches[-1].start()
        runs.append(store.runs_dir(None) / call)

    runs, launches, results = [], [], []
    passes = threading.Thread(target=lambda: (runner._gc(), runner._orphans()))
    passes.start()
    try:
        assert gates["gc"][0].wait(20)
        launch(reserve())
        assert gates["dir"][0].wait(20)  # its dir exists; no process yet
        gates["gc"][1].set()
        assert gates["sweep"][0].wait(20)  # the GC's pass is over
        assert runs[0].is_dir() and not stale.exists()
        gates["dir"][1].set()
        wait_shim(runs[0])
        launch(reserve())
        wait_shim(runs[1])
        gates["sweep"][1].set()
        passes.join(20)
    finally:
        for _, go in gates.values():
            go.set()
    assert not passes.is_alive()
    for run in runs:  # neither removed nor killed
        assert run.is_dir() and R.lock_held(run / "shim.lock")
        (run / "go").write_text("")
    for t in launches:
        t.join(20)
    assert [r["status"] for r in results] == ["succeeded", "succeeded"]


def test_a_finished_run_dir_goes_once_nothing_retained_refers_to_it(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=5)
    store = Store(home)
    create(store, "p", {"a": {"run": "test.boom", "in": {}}})
    runner = Runner(store)
    [rid] = settle(runner, store, "p")["a"]["run_ids"]
    store.retry("p", "a", author="t", reason="again")  # the state no longer names it
    runner._gc()
    assert (store.runs_dir("p") / rid).is_dir()  # its step.status record still does
    for i in range(10):
        store.append("p", {"kind": "message", "thread": "t", "from": "x", "body": str(i)})
    assert json.loads((store.runs_dir("p") / rid / "exit.json").read_text())
    runner._gc()
    assert not (store.runs_dir("p") / rid).exists()


def test_deleting_a_project_removes_its_files_and_nothing_brings_them_back(store):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    settle(Runner(store), store, "p")
    direct = calls.create(store, "test.add", {"a": 1, "b": 1}, "p", direct=True)
    rec = calls.latest(store, direct, "p")
    store.update_project("p", archived=True)
    store.delete_project("p")
    assert not store.project_dir("p").exists() and not list((store.home / "trash").iterdir())
    # a direct call that ends after its project went records nothing, recreates nothing
    assert calls.record(store, "p", {**rec, "status": "succeeded", "outputs": {"sum": 2}}) \
        is False
    assert store.project_names() == [] and not store.project_dir("p").exists()
    store.create_project("p")  # a new project of the name starts clean
    assert L.read(store.home, "p")["records"][0]["kind"] == "plan.edit"
    assert len(L.read(store.home, "p")["records"]) == 1 and store.read_state("p")["steps"] == {}


def deleted_with_a_run(store):
    """Project p archived with one finished run; returns the run's dir."""
    create(store, "p", {"a": {"run": "test.add", "in": {"a": d(1), "b": d(1)}}})
    [rid] = settle(Runner(store), store, "p")["a"]["run_ids"]
    store.update_project("p", archived=True)
    return store.runs_dir("p") / rid


def pending_deletions(store):
    with store.rx() as conn:
        return [tuple(r) for r in db.all_rows(conn, "SELECT name, token FROM deletions")]


def test_delete_recreate_before_cleanup_preserves_replacement_files(store, monkeypatch):
    """From the deletion's commit until its directory is gone the name is fenced: a new
    project of the name is refused meanwhile, and the cleanup (or a GC pass that read the
    deletion before it finished) never touches the replacement's files."""
    run = deleted_with_a_run(store)
    gates = {k: (threading.Event(), threading.Event()) for k in ("committed", "renamed")}

    def pause(k):
        reached, go = gates[k]
        reached.set()
        assert go.wait(10)

    real_remove, real_rmtree = store._remove_dir, shutil.rmtree

    def remove_dir(name, token):  # after the commit, before any file is touched
        pause("committed")
        return real_remove(name, token)

    def rmtree(path, *a, **kw):  # moved to trash, not yet removed
        if threading.current_thread() is deleter:
            pause("renamed")
        return real_rmtree(path, *a, **kw)

    monkeypatch.setattr(store, "_remove_dir", remove_dir)
    monkeypatch.setattr(shutil, "rmtree", rmtree)
    other = Store(store.home)
    deleter = threading.Thread(target=store.delete_project, args=("p",))
    deleter.start()
    try:
        assert gates["committed"][0].wait(10)
        [(_, token)] = pending_deletions(other)
        assert other.project_names() == [] and run.is_dir()
        with pytest.raises(BadRequest, match="'p' is still being removed; try again shortly"):
            other.create_project("p")
        gates["committed"][1].set()
        assert gates["renamed"][0].wait(10)
        assert not store.project_dir("p").exists()
        with pytest.raises(BadRequest, match="still being removed"):
            other.create_project("p")
    finally:
        for _, go in gates.values():
            go.set()
        deleter.join(10)
    monkeypatch.undo()
    assert pending_deletions(other) == [] and not list((store.home / "trash").iterdir())
    other.create_project("p")
    mine = store.project_dir("p") / "fns" / "mine.txt"
    mine.parent.mkdir(parents=True)
    mine.write_text("the replacement's")
    assert other._remove_dir("p", token) is True  # a stale GC pass: the row is gone
    Runner(other)._gc()
    assert mine.read_text() == "the replacement's" and other.project_names() == ["p"]


def test_delete_inside_rolled_back_outer_write_preserves_artifacts(store):
    run = deleted_with_a_run(store)
    with pytest.raises(ValueError), store.tx():
        store.delete_project("p")
        assert run.is_dir()  # nothing is removed before the outermost commit
        raise ValueError("the outer change fails")
    assert store.project_names() == ["p"] and (run / "output.json").is_file()
    assert pending_deletions(store) == [] and store.read_state("p")["steps"]["a"]
    with store.tx():
        store.delete_project("p")
        assert run.is_dir() and pending_deletions(store) != []
    assert store.project_names() == [] and not store.project_dir("p").exists()
    assert pending_deletions(store) == []


@pytest.mark.parametrize("fails", ["rename", "rmtree", "crash"])
def test_delete_cleanup_recovers_before_trash_rename(store, monkeypatch, fails):
    """A deletion whose directory removal failed (the move to trash, the removal) or never
    ran (the process died after the commit) is finished by the runner's GC — only that
    one: a directory prepared for a project not created yet stays."""
    run = deleted_with_a_run(store)
    prepared = store.project_dir("q") / "fns"
    prepared.mkdir(parents=True)

    def fail(*a, **kw):
        raise OSError(f"{fails} is unavailable")

    if fails == "crash":
        monkeypatch.setattr(store, "_remove_dir", lambda name, token: None)
    else:
        monkeypatch.setattr({"rename": os, "rmtree": shutil}[fails],
                            {"rename": "replace", "rmtree": "rmtree"}[fails], fail)
    assert store.delete_project("p") == {"deleted": "p"}
    monkeypatch.undo()
    assert store.project_names() == [] and [n for n, _ in pending_deletions(store)] == ["p"]
    assert run.is_dir() or (store.home / "trash").is_dir()
    with pytest.raises(BadRequest, match="still being removed"):
        store.create_project("p")
    Runner(store)._gc()
    assert pending_deletions(store) == [] and not store.project_dir("p").exists()
    assert not (store.home / "trash").exists() or not list((store.home / "trash").iterdir())
    assert prepared.is_dir()
    store.create_project("p")  # clean
    assert not store.project_dir("p").exists() and store.read_state("p")["steps"] == {}


def test_a_leftover_project_dir_refuses_the_name_unless_it_only_holds_prepared_files(store):
    d_ = store.project_dir("p")
    (d_ / "fns").mkdir(parents=True)
    (d_ / ".env").write_text("A=1\n")
    store.create_project("p")  # fns/ and .env may be prepared before
    store.project_dir("q").joinpath("runs").mkdir(parents=True)
    with pytest.raises(Exception, match="left over from an earlier project"):
        store.create_project("q")
    assert store.project_names() == ["p"]


def test_a_home_holds_only_its_database_config_and_run_files(store):
    """Plans, state, calls, threads, the inbox and the dashboard leave nothing on disk but
    the database, config.json and what the runs themselves write under runs/."""
    import re

    from sluice import views

    create(store, "p", {
        "fan": {"run": "test.add", "scatter": "a", "in": {"a": d([1, 2]), "b": d(1)}},
        "say": {"run": "thread.post", "in": {"thread": d("t"), "body": d("hi"),
                                              "from": d("say"), "needs_reply": d(False)}},
        "ask": {"run": "inbox.ask", "in": {"title": d("ok?")}}})
    runner = Runner(store)
    settle(runner, store, "p", until=lambda s: bool(store.inbox("p")))
    [item] = store.inbox("p")
    store.inbox_answer("p", item["id"], {"action": "answer", "text": "yes"}, "me")
    queued = calls.create(store, "test.add", {"a": 1, "b": 1}, None)
    direct = calls.create(store, "thread.wait", {"thread": "t", "since_seq": 0, "timeout": 5},
                          "p", direct=True)
    R.run_call_direct(store, direct, "p")
    steps = settle(runner, store, "p", until=lambda s: all(
        e["status"] in ("succeeded", "failed") for e in s.values())
        and calls.latest(store, queued, None)["status"] == "succeeded")
    assert {e["status"] for e in steps.values()} == {"succeeded"}
    for page in (views.index(store), views.project_page(store, "p", "v"),
                 views.log_page(store, "p", views.LogQuery.parse({})),
                 views.inbox_page(store, None, "all", "v"), views.step_detail(store, "p", "ask")):
        assert page
    runner._gc()
    allowed = re.compile(r"^(config\.json|sluice\.db(-wal|-shm)?|(projects/p/)?runs/[^/]+/.+)$")
    files = [str(f.relative_to(store.home)) for f in store.home.rglob("*") if f.is_file()]
    assert [f for f in files if not allowed.match(f)] == []
