"""fn_call outside the plan (SPEC §8): `call` records in the home's or a project's log, run
dirs under runs/<call id>/ next to it."""

import json
import time

import pytest

from sluice import calls
from sluice import log as L
from sluice.errors import InvalidPlan, NotFound
from sluice.runner import RESTARTED, Runner, run_call_direct
from sluice.store import Store
from tests.conftest import create, write_config, write_fn

ENV_MAIN = """import os
from sluice.fn import run

run(lambda inp, ctx: {"env": {k: v for k, v in os.environ.items()
                              if k.startswith(("SLUICE_", "TEST_"))}})
"""


def settle_call(runner, store, call, project=None, timeout=30.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        runner.tick()
        res = calls.status(store, call, project)
        if res["status"] in calls.DONE:
            return res
        time.sleep(0.05)
    raise AssertionError(f"timed out: {res}")


def call_records(directory, call=None):
    return [r for r in L.read(directory)["records"]
            if r["kind"] == "call" and call in (None, r["call"])]


def test_a_call_without_a_project_runs_under_the_home(store, runner):
    call = calls.create(store, "test.add", {"a": 2, "b": 3}, None)
    [rec] = call_records(store.home, call)
    assert (rec["status"], rec["fn"], rec["inputs"]) == ("pending", "test.add", {"a": 2, "b": 3})
    assert calls.status(store, call, None) == {"call": call, "status": "pending"}
    res = settle_call(runner, store, call)
    assert res["call"] == call and res["status"] == "succeeded" and res["outputs"] == {"sum": 5}
    assert "adding 2 + 3" in res["stderr_tail"]
    d = store.home / "runs" / call
    assert json.loads((d / "output.json").read_text()) == {"sum": 5}
    assert json.loads((d / "input.json").read_text()) == {"a": 2, "b": 3}
    assert store.project_names() == []
    recs = call_records(store.home, call)  # one record per status change, seqs increasing
    assert [r["status"] for r in recs] == ["pending", "running", "succeeded"]
    assert [r["seq"] for r in recs] == sorted({r["seq"] for r in recs})
    assert recs[-1]["outputs"] == {"sum": 5} and "inputs" not in recs[-1]


def test_a_call_in_a_project_uses_its_fns_and_env(store, runner):
    create(store, "p", {})
    (store.home / ".env").write_text("TEST_A=home\nTEST_B=home\n")
    (store.project_dir("p") / ".env").write_text("TEST_B=project\n")
    write_fn(store.project_dir("p") / "fns", "p.env", {}, {"env": "Any"}, main=ENV_MAIN)
    call = calls.create(store, "p.env", {}, "p")
    assert [r["status"] for r in call_records(store.project_dir("p"), call)] == ["pending"]
    assert call_records(store.home) == []
    with pytest.raises(NotFound):
        calls.create(store, "p.env", {}, None)  # not visible without the project
    res = settle_call(runner, store, call, "p")
    env = res["outputs"]["env"]
    assert (env["TEST_A"], env["TEST_B"], env["SLUICE_PROJECT"]) == ("home", "project", "p")
    assert env["SLUICE_RUN_ID"] == call
    assert env["SLUICE_RUN_DIR"] == str(store.project_dir("p") / "runs" / call)
    with pytest.raises(NotFound):
        calls.status(store, call, None)  # it lives in the project


def test_call_inputs_are_checked_before_anything_runs(store):
    with pytest.raises(InvalidPlan) as e:
        calls.create(store, "test.add", {"a": "one", "c": 1}, None)
    assert e.value.errors == ['inputs.a: expected int, got "one"',
                              "inputs.b: missing required field",
                              "inputs.c: fn test.add has no input c"]
    with pytest.raises(NotFound):
        calls.create(store, "no.such", {}, None)
    with pytest.raises(NotFound):
        calls.create(store, "test.add", {"a": 1, "b": 1}, "zz")
    with pytest.raises(NotFound):
        calls.status(store, "../../etc", None)
    assert not (store.home / L.FILE).exists()


def test_a_direct_call_runs_in_this_process(store):
    call = calls.create(store, "test.add", {"a": 1, "b": 1}, None, direct=True)
    assert calls.status(store, call, None)["status"] == "running"
    assert run_call_direct(store, call, None) == {"call": call, "status": "succeeded",
                                                  "outputs": {"sum": 2}}
    boom = calls.create(store, "test.boom", {}, None, direct=True)
    res = run_call_direct(store, boom, None)
    assert res["status"] == "failed" and "about to explode" in res["error"]
    fmt = calls.create(store, "core.format", {"template": "{0}!", "values": ["hi"]}, None,
                       direct=True)
    assert run_call_direct(store, fmt, None)["outputs"] == {"text": "hi!"}


def test_the_runner_leaves_direct_calls_alone(store, runner):
    call = calls.create(store, "test.add", {"a": 1, "b": 1}, None, direct=True)
    runner.tick()
    assert [r["status"] for r in call_records(store.home, call)] == ["running"]
    assert calls.status(store, call, None)["status"] == "running"
    rec = calls.latest(store, call, None)
    store.append(None, {k: v for k, v in rec.items() if k not in ("seq", "at")}
                 | {"pid": 2 ** 22 + 12345})  # the latest record names a process that is gone
    assert calls.status(store, call, None)["status"] == "failed"
    runner.tick()  # the runner records that it ended, so the log need not keep it
    assert [r["status"] for r in call_records(store.home, call)] == ["running", "running",
                                                                     "failed"]
    assert calls.status(store, call, None)["error"] == calls.GONE


def test_a_new_runner_fails_calls_left_running(store, runner):
    call = calls.create(store, "test.window", {"seconds": 30}, None)
    runner.tick()
    assert calls.status(store, call, None)["status"] == "running"
    Runner(store).tick()
    res = calls.status(store, call, None)
    assert (res["status"], res["error"]) == ("failed", RESTARTED)


def test_calls_are_refused_in_a_blocked_project(store):
    create(store, "p", {})
    write_fn(store.project_dir("p") / "fns", "core.echo")
    with pytest.raises(InvalidPlan, match="project p"):
        calls.create(store, "test.add", {"a": 1, "b": 1}, "p")
    assert calls.create(store, "test.add", {"a": 1, "b": 1}, None)  # the home is fine


def test_call_status_reads_the_latest_record(store):
    call = calls.create(store, "test.add", {"a": 1, "b": 1}, None)
    other = calls.create(store, "test.add", {"a": 2, "b": 2}, None)
    store.append(None, {"kind": "call", "call": call, "fn": "test.add", "status": "running"})
    store.append(None, {"kind": "call", "call": call, "fn": "test.add", "status": "failed",
                        "error": "first"})
    store.append(None, {"kind": "message", "thread": "t", "from": "x", "body": "noise"})
    assert calls.status(store, call, None) == {"call": call, "status": "failed",
                                               "error": "first"}
    assert calls.status(store, other, None) == {"call": other, "status": "pending"}


def test_the_log_is_capped_trimming_the_oldest_records_and_their_run_dirs(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=10)
    store = Store(home)
    runner = Runner(store)
    done = []
    for i in range(3):  # 3 records each: pending, running, succeeded
        done.append(calls.create(store, "test.add", {"a": i, "b": 0}, None))
        settle_call(runner, store, done[-1])
    assert all((home / "runs" / c).is_dir() for c in done)
    stray = home / "runs" / "not-a-call-of-the-log"
    stray.mkdir()
    for i in range(5):
        store.append(None, {"kind": "message", "thread": "t", "from": "x", "body": str(i)})
    # past 10 records the oldest go, down to 9: at seq 11 (3..11 left), again at 13 (5..13)
    assert [r["seq"] for r in L.read(home)["records"]] == list(range(5, 15))
    with pytest.raises(NotFound):
        calls.status(store, done[0], None)
    assert not (home / "runs" / done[0]).exists()  # no record refers to it any more
    assert (home / "runs" / done[2]).is_dir() and stray.is_dir()
    assert calls.status(store, done[2], None)["outputs"] == {"sum": 2}


def test_trimming_never_drops_a_call_that_is_still_pending_or_running(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=5)
    store = Store(home)
    waiting = calls.create(store, "test.add", {"a": 1, "b": 2}, None)  # no runner: pending
    running = calls.create(store, "test.add", {"a": 1, "b": 1}, None, direct=True)
    for i in range(20):
        store.append(None, {"kind": "message", "thread": "t", "from": "x", "body": str(i)})
    recs = L.read(home)["records"]
    assert len(recs) <= 5
    assert [r.get("call") for r in recs[:2]] == [waiting, running]
    assert calls.status(store, waiting, None)["status"] == "pending"
    runner = Runner(store)
    assert settle_call(runner, store, waiting)["outputs"] == {"sum": 3}  # inputs survived
    assert run_call_direct(store, running, None)["outputs"] == {"sum": 2}
