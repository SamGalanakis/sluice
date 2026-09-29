"""`sluice drain` and `drain --release` (SPEC §9): pause a home for maintenance, wait until
its running work is done, then unpause exactly the projects drain paused."""

import json
import os
import subprocess
import sys
import time

from sluice import calls, drain
from tests.conftest import add, create, d, settle


def drain_spawn(home, *args):
    return subprocess.Popen([sys.executable, "-m", "sluice.cli", "drain", *args],
                            env={**os.environ, "SLUICE_HOME": str(home)},
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def drain_run(home, *args, timeout=30):
    out, err = drain_spawn(home, *args).communicate(timeout=timeout)
    return out, err


def test_drain_pauses_only_the_projects_that_were_not(store):
    create(store, "a", {"x": add(d(1), d(2))})
    create(store, "b", {"x": add(d(1), d(2))})
    store.update_project("a", paused=True)
    out, err = drain_run(store.home, "--no-wait")
    assert err == ""
    assert out.splitlines() == ["paused b"]
    doc = drain._recorded(store)
    assert doc["paused"] == ["b"] and "at" in doc
    assert store.project("a")["paused"] and store.project("b")["paused"]


def test_drain_waits_for_a_running_step(store, runner):
    create(store, "p", {"w": {"run": "test.wait", "in": {"value": d(1)}}})
    steps = settle(runner, store, "p",
                   until=lambda s: s.get("w", {}).get("status") == "running")
    p = drain_spawn(store.home, "-p", "p")
    try:
        deadline = time.time() + 10
        while not store.paused("p"):
            assert time.time() < deadline, "drain did not pause p"
            time.sleep(0.05)
        assert p.poll() is None  # still waiting on w
        rid = steps["w"]["run_ids"][0]
        (store.runs_dir("p") / rid / "go").write_text("")
        settle(runner, store, "p", until=lambda s: s["w"]["status"] == "succeeded")
        out, _ = p.communicate(timeout=30)
    finally:
        p.kill()
    assert out.splitlines()[0] == "paused p"
    assert "running: p 1 (w); calls 0" in out.splitlines()
    assert out.splitlines()[-1] == "drained"


def test_drain_waits_for_a_non_direct_call(store):
    create(store, "p", {"x": add(d(1), d(2))})
    call = calls.create(store, "test.add", {"a": 1, "b": 2}, "p")  # pending, queued
    p = drain_spawn(store.home, "-p", "p")
    try:
        deadline = time.time() + 10
        while not store.paused("p"):
            assert time.time() < deadline, "drain did not pause p"
            time.sleep(0.05)
        assert p.poll() is None  # still waiting on the call
        calls.record(store, "p", {**calls.latest(store, call, "p"), "status": "succeeded",
                                  "outputs": {"sum": 3}})
        out, _ = p.communicate(timeout=30)
    finally:
        p.kill()
    assert "calls 1" in out and out.splitlines()[-1] == "drained"


def test_release_unpauses_only_the_projects_drain_paused(store):
    for name in "abc":
        create(store, name, {"x": add(d(1), d(2))})
    store.update_project("a", paused=True)  # paused on its own, not by drain
    out, _ = drain_run(store.home, "-p", "a", "-p", "b", "--no-wait")
    assert drain._recorded(store)["paused"] == ["b"]
    out, _ = drain_run(store.home, "--release")
    assert out.splitlines() == ["released b"]
    assert not store.project("b")["paused"] and store.project("a")["paused"]
    assert not (store.home / "drain.json").exists()


def test_drain_imports_an_existing_drain_json(store):
    create(store, "a", {"x": add(d(1), d(2))})
    create(store, "b", {"x": add(d(1), d(2))})
    (store.home / "drain.json").write_text(json.dumps({"paused": ["a"], "note": "keep me"}))
    drain_run(store.home, "-p", "b", "--no-wait")
    doc = drain._recorded(store)
    assert doc["paused"] == ["a", "b"] and doc["note"] == "keep me"
    drain_run(store.home, "--release")
    assert not store.project("a")["paused"] and not store.project("b")["paused"]
