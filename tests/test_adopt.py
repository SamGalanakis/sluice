"""Runs surviving a runner restart (SPEC §4, §6): the shim records the exit, the next
runner adopts what a previous one left, orphans are killed, --kill-runs restores the
stop-everything shutdown."""

import fcntl
import json
import os
import signal
import sqlite3
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

from sluice import calls, db
from sluice import log as L
from sluice import runner as R
from sluice.runner import (
    RESTARTED,
    UNKNOWN,
    Run,
    Runner,
    _probe,
    _proc_identity,
    fn_env,
    kill,
    lock_held,
    read_run,
    spawn,
)
from tests.conftest import create, d, pid_alive, settle, statuses, wait_gone


def wait_step(store, project, sid, timeout=20.0):
    """The step's entry once it is `running` with its run_ids recorded."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        e = store.read_state(project)["steps"].get(sid, {})
        if e.get("status") == "running" and e.get("run_ids"):
            return e
        time.sleep(0.05)
    raise AssertionError(f"{sid} did not start; statuses: {statuses(store, project)}")


def wait_shim(run_dir: Path, timeout=20.0) -> int:
    """The shim's pid once shim.json exists and its lock is held."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        if lock_held(run_dir / "shim.lock"):
            try:
                return json.loads((run_dir / "shim.json").read_text())["pid"]
            except (OSError, ValueError, KeyError):
                pass
        time.sleep(0.05)
    raise AssertionError(f"no live shim in {run_dir}")


def wait_exit(run_dir: Path, timeout=20.0) -> dict:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            return json.loads((run_dir / "exit.json").read_text())
        except (OSError, ValueError):
            time.sleep(0.05)
    raise AssertionError(f"no exit.json in {run_dir}")


def serve(store, **kw) -> tuple[Runner, threading.Thread]:
    """A runner ticking in a thread every 0.1 s."""
    r = Runner(store, **kw)
    t = threading.Thread(target=r.run_forever, kwargs={"interval": 0.1}, daemon=True)
    t.start()
    return r, t


def stop(r: Runner, t: threading.Thread) -> None:
    r.stop()
    t.join(timeout=10)
    assert not t.is_alive()


def run_records(store, project=None):
    return L.read(store.home, project, kinds=["run"])["records"]


def wait_run(store, project, sid, kills):
    """A `test.wait` step once running under its shim; registers its group for cleanup."""
    e = wait_step(store, project, sid)
    run_dir = store.runs_dir(project) / e["run_ids"][0]
    kills.append(wait_shim(run_dir))
    return e, run_dir


@pytest.fixture
def kills():
    """Process groups (by shim pid) to SIGKILL at the end, however the test went."""
    pids = []
    yield pids
    for pid in pids:
        try:
            os.killpg(pid, signal.SIGKILL)
        except OSError:
            pass


def test_the_shim_records_identity_and_exit(store, tmp_path):
    fn = store.fn("test.add")
    run_dir = tmp_path / "run"
    proc = spawn(fn, {"a": 2, "b": 3}, run_dir, fn_env(store, None, fn, "", "r", run_dir))
    assert proc.wait(timeout=30) == 0  # the shim exits with the fn's code
    shim = json.loads((run_dir / "shim.json").read_text())
    assert shim["pid"] == proc.pid and shim["argv"][:3] == ["uv", "run", "--quiet"]
    done = json.loads((run_dir / "exit.json").read_text())
    assert done["code"] == 0 and done["signal"] is None and done["finished"]
    assert json.loads((run_dir / "output.json").read_text()) == {"sum": 5}

    fn = store.fn("test.boom")
    run_dir = tmp_path / "run2"
    proc = spawn(fn, {"ok": False}, run_dir, fn_env(store, None, fn, "", "r2", run_dir))
    assert proc.wait(timeout=30) == 1  # and a failing fn's code the same way
    assert json.loads((run_dir / "exit.json").read_text())["code"] == 1
    assert "about to explode" in (run_dir / "stderr.log").read_text()


def test_exit_json_only_lands_once_the_output_is_complete(store, tmp_path):
    fn = store.fn("test.wait")
    run_dir = tmp_path / "run"
    proc = spawn(fn, {"value": "x"}, run_dir, fn_env(store, None, fn, "", "r", run_dir))
    wait_shim(run_dir)
    assert not (run_dir / "exit.json").exists()  # a live run has none yet
    (run_dir / "go").write_text("")
    assert proc.wait(timeout=30) == 0
    # with exit.json there, output.json is already complete — presence is the finish line
    assert json.loads((run_dir / "exit.json").read_text())["code"] == 0
    assert json.loads((run_dir / "output.json").read_text()) == {"value": "x"}


def test_a_left_running_step_is_adopted_and_finishes(store, kills):
    create(store, "p", {"w": {"run": "test.wait", "in": {"value": d(42)}}})
    r1, t1 = serve(store)
    e, run_dir = wait_run(store, "p", "w", kills)
    pid = kills[-1]
    stop(r1, t1)
    assert pid_alive(pid)  # the run survived the runner that started it

    r2 = Runner(store)
    r2.tick()
    assert statuses(store, "p") == {"w": "running"}  # adopted, still running
    [rec] = run_records(store, "p")
    assert (rec["kind"], rec["step"], rec["run"], rec["outcome"]) == \
        ("run.adopt", "w", e["run_ids"][0], "watching")
    (run_dir / "go").write_text("")
    steps = settle(r2, store, "p")
    assert (steps["w"]["status"], steps["w"]["outputs"]) == ("succeeded", {"value": 42})


def test_a_run_finished_while_no_runner_was_up_is_collected(store, kills):
    create(store, "p", {"w": {"run": "test.wait", "in": {"value": d("y")}}})
    r1, t1 = serve(store)
    _, run_dir = wait_run(store, "p", "w", kills)
    stop(r1, t1)
    (run_dir / "go").write_text("")
    assert wait_exit(run_dir)["code"] == 0  # it finished while no runner was up
    steps = settle(Runner(store), store, "p")
    assert (steps["w"]["status"], steps["w"]["outputs"]) == ("succeeded", {"value": "y"})
    [rec] = run_records(store, "p")
    assert (rec["kind"], rec["outcome"]) == ("run.adopt", "finished")


def test_a_run_whose_shim_died_fails_unknown(store, kills):
    create(store, "p", {"w": {"run": "test.wait", "in": {}}})
    r1, t1 = serve(store)
    _, run_dir = wait_run(store, "p", "w", kills)
    pid = kills[-1]
    stop(r1, t1)  # the runner is gone first, so it cannot notice the kill itself
    os.killpg(pid, signal.SIGKILL)  # shim, uv and fn die together: no exit.json ever
    deadline = time.time() + 10
    while lock_held(run_dir / "shim.lock") and time.time() < deadline:
        time.sleep(0.05)
    steps = settle(Runner(store), store, "p")
    assert (steps["w"]["status"], steps["w"]["error"]) == ("failed", UNKNOWN)
    [rec] = run_records(store, "p")
    assert (rec["kind"], rec["outcome"]) == ("run.adopt", "unknown")


@pytest.mark.parametrize("adopt", [False, True])
def test_runner_reaps_native_processes_after_fn_is_gone(tmp_path, adopt):
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    command = [sys.executable, "-c", ("import subprocess,time; "
                                           "subprocess.Popen(['sleep','60']); time.sleep(60)")]
    tmux = subprocess.run(["tmux", "-S", "tmux.sock", "new-session", "-d", "--", *command],
                          cwd=run_dir, capture_output=True, text=True, check=False)
    assert tmux.returncode == 0, tmux.stderr
    server = int(subprocess.run(["tmux", "-S", "tmux.sock", "display-message", "-p", "#{pid}"],
                                cwd=run_dir, capture_output=True, text=True,
                                check=True).stdout)
    engine = int(subprocess.run(["tmux", "-S", "tmux.sock", "display-message", "-p",
                                 "#{pane_pid}"], cwd=run_dir, capture_output=True, text=True,
                                check=True).stdout)
    app = subprocess.Popen(command, start_new_session=True)
    try:
        deadline = time.time() + 3
        children = []
        while time.time() < deadline:
            children = [int(d.name) for d in Path("/proc").iterdir() if d.name.isdigit()
                        and (ident := _proc_identity(int(d.name))) and ident[1] in (engine, app.pid)]
            if len(children) >= 2:
                break
            time.sleep(0.05)
        assert len(children) >= 2
        (run_dir / "native-processes.json").write_text(json.dumps({
            "tmux_server": {"pid": server, "start_time": _proc_identity(server)[0]},
            "engine": {"pid": engine, "start_time": _proc_identity(engine)[0]},
            "app_server": {"pid": app.pid, "start_time": _proc_identity(app.pid)[0]}}))
        if adopt:
            assert _probe(run_dir)[0] == "restarted"
        else:
            kill(Run({}, run_dir=run_dir), grace=0)
        assert wait_gone([server, engine, app.pid, *children]) == []
        assert not (run_dir / "tmux.sock").exists()
    finally:
        subprocess.run(["tmux", "-S", "tmux.sock", "kill-server"], cwd=run_dir,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
        for pid in [server, engine, app.pid, *children]:
            if pid_alive(pid):
                os.kill(pid, signal.SIGKILL)
        app.wait(timeout=5)


def test_runner_does_not_signal_a_reused_native_pid(tmp_path):
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    app = subprocess.Popen(["sleep", "60"], start_new_session=True)
    try:
        started = _proc_identity(app.pid)[0]
        (run_dir / "native-processes.json").write_text(json.dumps({
            "app_server": {"pid": app.pid, "start_time": started + 1}}))
        kill(Run({}, run_dir=run_dir), grace=0)
        assert pid_alive(app.pid)
    finally:
        app.kill()
        app.wait()


def test_an_unreferenced_live_run_is_killed_at_startup(store, kills):
    fn = store.fn("test.wait")
    run_dir = store.runs_dir(None) / "orphan-run"  # no step or call names it
    spawn(fn, {}, run_dir, fn_env(store, None, fn, "", "orphan-run", run_dir))
    kills.append(wait_shim(run_dir))
    Runner(store).tick()  # the startup pass sweeps it
    assert wait_gone(kills) == []
    [rec] = [r for r in run_records(store) if r["kind"] == "run.orphan"]
    assert rec["run"] == "orphan-run"


def test_step_cancel_kills_an_adopted_run(store, kills):
    create(store, "p", {"w": {"run": "test.wait", "in": {}}})
    r1, t1 = serve(store)
    wait_run(store, "p", "w", kills)
    stop(r1, t1)
    store.cancel_steps("p", "w", author="test", reason="not any more")
    steps = settle(Runner(store), store, "p",
                   until=lambda s: s["w"]["status"] == "failed")
    assert steps["w"]["error"] == "cancelled: not any more"
    assert wait_gone(kills) == []  # the adopted run's whole process group went


def test_a_running_entry_with_no_run_ids_fails_restarted(store):
    create(store, "p", {"w": {"run": "test.wait", "in": {}}})
    state = store.read_state("p")  # pre-change state: running, nothing recorded
    state["steps"]["w"] = {"status": "running", "started": "2020-01-01T00:00:00Z",
                           "inputs_hash": "x"}
    store.write_state("p", state)
    steps = settle(Runner(store), store, "p",
                   until=lambda s: s["w"]["status"] == "failed")
    assert steps["w"]["error"] == RESTARTED  # never "succeeds" off no evidence


def test_step_cancel_kills_a_run_adoption_never_took(store, kills):
    create(store, "p", {"w": {"run": "test.wait", "in": {}}})
    rid = "20200101T000000-w-0-abcd"
    fn = store.fn("test.wait")
    run_dir = store.runs_dir("p") / rid
    spawn(fn, {}, run_dir, fn_env(store, "p", fn, "w", rid, run_dir))
    kills.append(wait_shim(run_dir))
    state = store.read_state("p")
    state["steps"]["w"] = {"status": "running", "started": "2020-01-01T00:00:00Z",
                           "run_ids": [rid], "inputs_hash": "x", "cancel": "stop it"}
    store.write_state("p", state)
    Runner(store)._tick_project("p")  # no startup adoption: the entry has no Active
    e = store.read_state("p")["steps"]["w"]
    assert (e["status"], e["error"]) == ("failed", "cancelled: stop it")
    assert wait_gone(kills) == []  # its recorded shim pid still got the group


def test_a_fn_that_outlives_its_shim_is_killed(store, kills):
    create(store, "p", {"w": {"run": "test.wait", "in": {}}})
    r1, t1 = serve(store)
    _, run_dir = wait_run(store, "p", "w", kills)
    shim_pid = kills[-1]
    deadline = time.time() + 10
    while not (run_dir / "child.json").exists() and time.time() < deadline:
        time.sleep(0.05)
    child = json.loads((run_dir / "child.json").read_text())
    stop(r1, t1)
    os.kill(shim_pid, signal.SIGKILL)  # the shim alone dies; uv and the fn keep running
    assert pid_alive(child["pid"])
    steps = settle(Runner(store), store, "p",
                   until=lambda s: s["w"]["status"] == "failed")
    assert steps["w"]["error"] == UNKNOWN
    assert wait_gone([child["pid"]]) == []  # the survivor's group went with it


def test_a_second_shim_waits_out_probes_then_refuses(store, tmp_path, kills):
    fn = store.fn("test.wait")
    run_dir = tmp_path / "run"
    p1 = spawn(fn, {"value": 1}, run_dir, fn_env(store, None, fn, "", "r", run_dir))
    kills.append(wait_shim(run_dir))
    # another shim on the same dir retries briefly, then refuses — and the first,
    # probed by lock_held the whole time, runs on
    p2 = subprocess.Popen([sys.executable, "-m", "sluice.exec", str(run_dir), "--",
                           "uv", "run", "--quiet", "--script", str(fn.dir / "main.py")],
                          cwd=run_dir, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL)
    assert p2.wait(timeout=30) == 2
    assert pid_alive(kills[-1])
    (run_dir / "go").write_text("")
    assert p1.wait(timeout=30) == 0


def test_a_fn_that_cannot_start_records_exit_127(store, tmp_path):
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    (run_dir / "input.json").write_text("{}")
    with open(run_dir / "stderr.log", "wb") as err:
        proc = subprocess.Popen([sys.executable, "-m", "sluice.exec", str(run_dir), "--",
                                 "no-such-binary-9f8d7c"], cwd=run_dir, stderr=err)
    assert proc.wait(timeout=30) == 127
    done = json.loads((run_dir / "exit.json").read_text())
    assert done["code"] == 127 and done["error"]
    fn = store.fn("test.wait")
    assert read_run(fn, run_dir, 127)[1].startswith("could not start the fn:")


def test_spawn_clears_a_reused_run_dirs_old_evidence(store, tmp_path):
    fn = store.fn("test.add")
    run_dir = tmp_path / "run"
    spawn(fn, {"a": 1, "b": 1}, run_dir, fn_env(store, None, fn, "", "r", run_dir)
          ).wait(timeout=30)
    assert json.loads((run_dir / "exit.json").read_text())["code"] == 0
    # reuse the dir for a slower fn: the old exit.json must not survive the respawn
    fn = store.fn("test.wait")
    proc = spawn(fn, {"value": "x"}, run_dir, fn_env(store, None, fn, "", "r", run_dir))
    wait_shim(run_dir)
    try:
        assert not (run_dir / "exit.json").exists()
    finally:
        (run_dir / "go").write_text("")
        proc.wait(timeout=30)


def test_an_orphan_is_only_logged_when_signalled(store, tmp_path):
    d0 = store.runs_dir(None) / "ghost-run"  # a lock held with no shim.json at all
    d0.mkdir(parents=True)
    fd = os.open(d0 / "shim.lock", os.O_RDWR | os.O_CREAT, 0o644)
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    try:
        Runner(store).tick()  # the sweep finds it but can signal nothing
        assert [r for r in run_records(store) if r["kind"] == "run.orphan"] == []
    finally:
        os.close(fd)


def test_adoption_persists_done_for_a_scatter(store, kills):
    create(store, "p", {"w": {"run": "test.wait", "scatter": "value",
                              "in": {"value": d(["a", "b"])}}})
    r1, t1 = serve(store)
    e = wait_step(store, "p", "w")
    for rid in e["run_ids"]:
        kills.append(wait_shim(store.runs_dir("p") / rid))
    stop(r1, t1)
    (store.runs_dir("p") / e["run_ids"][0] / "go").write_text("")  # run 0 finishes alone
    deadline = time.time() + 10
    while not (store.runs_dir("p") / e["run_ids"][0] / "exit.json").exists() \
            and time.time() < deadline:
        time.sleep(0.05)
    state = store.read_state("p")  # the old runner never saw it finish
    state["steps"]["w"]["done"] = 0
    store.write_state("p", state)
    r2 = Runner(store)
    r2.tick()
    assert store.read_state("p")["steps"]["w"]["done"] == 1  # adoption wrote it back
    (store.runs_dir("p") / e["run_ids"][1] / "go").write_text("")
    assert settle(r2, store, "p")["w"]["status"] == "succeeded"


def test_a_retried_scatter_is_adopted_with_its_kept_items(store, kills):
    create(store, "p", {"g": {"run": "test.gate", "scatter": "tag",
                              "in": {"tag": d(["a", "b", "c"])}}})
    r1, t1 = serve(store)
    e = wait_step(store, "p", "g")
    run_dirs = [store.runs_dir("p") / rid for rid in e["run_ids"]]
    (run_dirs[1] / "fail").write_text("")
    for rd in run_dirs:
        (rd / "go").write_text("")
    e = settle(r1, store, "p")["g"]
    assert e["status"] == "failed" and e["results"][1] is None
    store.retry("p", "g", author="test", reason="again")
    e = wait_step(store, "p", "g")  # item 1 alone re-runs; it waits on its `go`
    assert e["run_ids"][1] != run_dirs[1].name
    kills.append(wait_shim(store.runs_dir("p") / e["run_ids"][1]))
    stop(r1, t1)  # the runner dies mid-retry, leaving item 1's run alive

    r2 = Runner(store)
    r2.tick()
    e = store.read_state("p")["steps"]["g"]
    assert (e["status"], e["done"]) == ("running", 2)
    recs = run_records(store, "p")
    assert [r["outcome"] for r in recs] == ["finished", "watching", "finished"]
    (store.runs_dir("p") / e["run_ids"][1] / "go").write_text("")
    e = settle(r2, store, "p")["g"]
    assert e["status"] == "succeeded" and e["outputs"] == {"tag": ["a", "b", "c"]}


def outside(store, project, step):
    """The step's entry and its step.status records as another connection sees them: only
    what is committed."""
    conn = sqlite3.connect(store.home / db.FILE)
    try:
        state = json.loads(conn.execute("SELECT doc FROM states WHERE project = ?",
                                        (project,)).fetchone()[0])
        tos = [r[0] for r in conn.execute(
            "SELECT json_extract(data, '$.to') FROM records WHERE project = ? AND "
            "kind = 'step.status' AND step = ? ORDER BY seq", (project, step))]
    finally:
        conn.close()
    return state["steps"].get(step, {}), tos


def test_a_run_is_committed_with_its_record_before_its_process_starts(store, kills,
                                                                       monkeypatch):
    """The reservation (running, the run id, the pending→running record) commits as one
    transaction, before the run's dir or process exists; nothing of it shows before."""
    create(store, "p", {"w": {"run": "test.wait", "in": {}}})
    at_append, at_spawn = [], []
    real_append, real_spawn = L.append, R.spawn

    def append(conn, project, records, cap=L.DEFAULT_MAX):
        if any(r.get("to") == "running" for r in records):
            at_append.append(outside(store, "p", "w"))
        return real_append(conn, project, records, cap)

    def spawn(fn, inp, run_dir, env):
        at_spawn.append((outside(store, "p", "w"), run_dir.exists()))
        return real_spawn(fn, inp, run_dir, env)

    monkeypatch.setattr(L, "append", append)
    monkeypatch.setattr(R, "spawn", spawn)
    Runner(store).tick()
    assert at_append == [({}, [])]  # inside the transaction: nothing visible yet
    [((entry, tos), existed)] = at_spawn
    [run_id] = store.read_state("p")["steps"]["w"]["run_ids"]
    assert entry["status"] == "running" and entry["run_ids"] == [run_id]
    assert tos == ["running"] and not existed
    kills.append(wait_shim(store.runs_dir("p") / run_id))


def test_a_run_dir_from_before_the_shim_still_fails_restarted(store):
    create(store, "p", {"w": {"run": "test.wait", "in": {}}})
    rid = "20200101T000000-w-0-abcd"
    (store.runs_dir("p") / rid).mkdir(parents=True)  # a pre-change run dir: no shim.json
    state = store.read_state("p")
    state["steps"]["w"] = {"status": "running", "started": "2020-01-01T00:00:00Z",
                           "run_ids": [rid], "inputs_hash": "x"}
    store.write_state("p", state)
    steps = settle(Runner(store), store, "p",
                   until=lambda s: s["w"]["status"] == "failed")
    assert steps["w"]["error"] == RESTARTED
    [rec] = run_records(store, "p")
    assert (rec["run"], rec["outcome"]) == (rid, "restarted")


def test_the_runner_writes_its_heartbeat(store):
    r, t = serve(store)
    try:
        deadline = time.time() + 10
        while not (store.home / "runner.json").exists() and time.time() < deadline:
            time.sleep(0.05)
        beat = json.loads((store.home / "runner.json").read_text())
        assert beat["pid"] == os.getpid() and beat["started"] and beat["beat"]
    finally:
        stop(r, t)


def test_kill_runs_stops_runs_on_exit(store, kills):
    create(store, "p", {"w": {"run": "test.wait", "in": {}}})
    r, t = serve(store, kill_runs=True)
    wait_run(store, "p", "w", kills)
    stop(r, t)
    assert wait_gone(kills) == []  # --kill-runs: stopped, not left for adoption


def test_a_broken_state_does_not_stop_other_projects(store, capsys):
    create(store, "a", {})
    create(store, "z", {"w": {"run": "test.add", "in": {"a": d(1), "b": d(2)}}})
    with store.tx() as conn:
        conn.execute("""UPDATE states SET doc = '{"inputs": {}}' WHERE project = 'a'""")
    steps = settle(Runner(store), store, "z")
    assert steps["w"]["status"] == "succeeded" and steps["w"]["outputs"] == {"sum": 3}
    assert "sluice runner: a:" in capsys.readouterr().err


def test_alive_knows_a_reused_pid_is_a_different_process():
    me = os.getpid()
    if calls.pid_start(me) is None:
        pytest.skip("no /proc")
    assert calls.alive(me, calls.pid_start(me)) is True
    assert calls.alive(me, "1") is False  # same pid, a different start time
    assert calls.alive(me) is True        # nothing recorded: the pid alone, as before
    assert calls.alive(2 ** 22 + 12345) is False


def test_a_direct_call_record_with_a_wrong_start_time_is_gone(store, runner):
    call = calls.create(store, "test.add", {"a": 1, "b": 1}, None, direct=True)
    rec = calls.latest(store, call, None)
    if rec.get("pid_start") is None:
        pytest.skip("no /proc")
    with store.tx() as conn:  # a reused pid: its start time is not the recorded one
        conn.execute("UPDATE calls SET pid_start = '1' WHERE call = ?", (call,))
    assert calls.status(store, call, None)["status"] == "failed"
    runner.tick()
    assert calls.status(store, call, None)["error"] == calls.GONE


def test_runner_reaps_native_session_of_a_gone_direct_call(store, runner):
    call = calls.create(store, "test.add", {"a": 1, "b": 1}, None, direct=True)
    rec = calls.latest(store, call, None)
    if rec.get("pid_start") is None:
        pytest.skip("no /proc")
    run_dir = store.runs_dir(None) / call
    run_dir.mkdir(parents=True)
    app = subprocess.Popen(["sleep", "60"], start_new_session=True)
    try:
        (run_dir / "native-processes.json").write_text(json.dumps({
            "app_server": {"pid": app.pid, "start_time": _proc_identity(app.pid)[0]}}))
        with store.tx() as conn:
            conn.execute("UPDATE calls SET pid_start = '1' WHERE call = ?", (call,))
        runner.tick()
        assert calls.status(store, call, None)["error"] == calls.GONE
        assert wait_gone([app.pid]) == []
    finally:
        if pid_alive(app.pid):
            app.kill()
        app.wait()
