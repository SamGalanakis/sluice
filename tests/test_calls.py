"""fn_call outside the plan (SPEC §8): calls/<id>/ in the home or a project."""

import json
import time

import pytest

from sluice import calls
from sluice.errors import InvalidPlan, NotFound
from sluice.runner import RESTARTED, Runner, run_call_direct
from tests.conftest import create, write_fn

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


def test_a_call_without_a_project_runs_under_the_home(store, runner):
    call = calls.create(store, "test.add", {"a": 2, "b": 3}, None)
    d = store.home / "calls" / call
    assert json.loads((d / "call.json").read_text())["status"] == "pending"
    assert calls.status(store, call, None) == {"call": call, "status": "pending"}
    res = settle_call(runner, store, call)
    assert res["call"] == call and res["status"] == "succeeded" and res["outputs"] == {"sum": 5}
    assert "adding 2 + 3" in res["stderr_tail"]
    assert json.loads((d / "output.json").read_text()) == {"sum": 5}
    assert store.project_names() == []


def test_a_call_in_a_project_uses_its_fns_and_env(store, runner):
    create(store, "p", {})
    (store.home / ".env").write_text("TEST_A=home\nTEST_B=home\n")
    (store.project_dir("p") / ".env").write_text("TEST_B=project\n")
    write_fn(store.project_dir("p") / "fns", "p.env", {}, {"env": "Any"}, main=ENV_MAIN)
    call = calls.create(store, "p.env", {}, "p")
    assert (store.project_dir("p") / "calls" / call / "call.json").is_file()
    with pytest.raises(NotFound):
        calls.create(store, "p.env", {}, None)  # not visible without the project
    res = settle_call(runner, store, call, "p")
    env = res["outputs"]["env"]
    assert (env["TEST_A"], env["TEST_B"], env["SLUICE_PROJECT"]) == ("home", "project", "p")
    assert env["SLUICE_RUN_ID"] == call
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
    assert not (store.home / "calls").exists()


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
    assert json.loads((store.home / "calls" / call / "call.json").read_text())["status"] == \
        "running"
    rec = json.loads((store.home / "calls" / call / "call.json").read_text())
    rec["pid"] = 2 ** 22 + 12345  # a process that is gone
    (store.home / "calls" / call / "call.json").write_text(json.dumps(rec))
    assert calls.status(store, call, None)["status"] == "failed"


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
