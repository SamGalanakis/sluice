import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

from sluice.runner import Runner
from sluice.store import Store
from tests.conftest import TESTPACK


@pytest.fixture
def sluice(tmp_path):
    home = tmp_path / "home"

    def run(*args, check=True, input=None):
        p = subprocess.run([sys.executable, "-m", "sluice.cli", *args], capture_output=True,
                           text=True, input=input, timeout=120, check=False,
                           env={**os.environ, "SLUICE_HOME": str(home)}, cwd=tmp_path)
        if check:
            assert p.returncode == 0, p.stderr
        return p

    run.home = home
    run.tmp = tmp_path
    return run


PLAN = {"title": "cli", "nodes": {
    "a": {"fn": "test.add", "in": {"a": {"value": 1}, "b": {"value": 2}}},
    "b": {"fn": "test.twice", "in": {"x": {"from": "a.sum"}}},
    "c": {"fn": "core.echo", "in": {"value": {"from": "b.y"}},
          "when": [{"from": "a.sum", "op": "eq", "value": 99}]},
}}


def test_init_writes_a_config_once(sluice):
    out = sluice("init", "--pack", str(TESTPACK))
    assert "config.json" in out.stdout
    cfg = json.loads((sluice.home / "config.json").read_text())
    assert cfg["packs"] == [str(TESTPACK)] and cfg["tick"] == "2s"
    assert sluice("init", check=False).returncode == 1
    sluice("init", "--force")
    assert json.loads((sluice.home / "config.json").read_text())["packs"] == []


def test_plan_commands_status_and_loop(sluice):
    sluice("init", "--pack", str(TESTPACK))
    (sluice.tmp / "plan.json").write_text(json.dumps(PLAN))
    assert json.loads(sluice("plan", "create", "demo", "plan.json").stdout) == {"rev": 1}
    shown = sluice("plan", "show", "demo").stdout
    assert "plan demo  rev 1  cli" in shown and "b" in shown and "<- a" in shown
    assert json.loads(sluice("plan", "show", "demo", "--json").stdout)["rev"] == 1

    ops = [{"op": "add", "path": "/nodes/d", "value": {"fn": "core.echo",
                                                       "in": {"value": {"value": "x"}}}}]
    (sluice.tmp / "ops.json").write_text(json.dumps(ops))
    assert json.loads(sluice("plan", "patch", "demo", "--rev", "1", "--reason", "add d",
                             "ops.json").stdout) == {"rev": 2}
    stale = sluice("plan", "patch", "demo", "--rev", "1", "--reason", "again", "ops.json",
                   check=False)
    assert stale.returncode == 1
    assert json.loads(stale.stderr) == {"error": "conflict", "message": "plan is at rev 2",
                                        "current_rev": 2}
    bad = sluice("plan", "patch", "demo", "--rev", "2", "--reason", "bad",
                 json.dumps([{"op": "replace", "path": "/nodes/a/in/a/value", "value": "x"}]),
                 check=False)
    assert json.loads(bad.stderr)["errors"] == ['nodes.a.in.a: expected int, got "x"']
    history = sluice("plan", "history", "demo").stdout.splitlines()
    assert history[0].startswith("rev 1") and "create" in history[0]
    assert history[1].startswith("rev 2") and "add /nodes/d" in history[1]

    dry = sluice("dry-run", "demo").stdout
    assert "start    a" in dry and "blocked  b" in dry

    loop = subprocess.Popen([sys.executable, "-m", "sluice.cli", "loop"], cwd=sluice.tmp,
                            env={**os.environ, "SLUICE_HOME": str(sluice.home)},
                            stderr=subprocess.PIPE)
    try:
        deadline = time.time() + 30
        while time.time() < deadline:
            s = json.loads(sluice("status", "demo", "--json").stdout)
            if all(n["status"] in ("succeeded", "skipped") for n in s["nodes"]):
                break
            time.sleep(0.3)
        second = sluice("loop", check=False)
        assert second.returncode == 1 and "another runner is active" in second.stderr
    finally:
        loop.send_signal(signal.SIGTERM)
        assert loop.wait(timeout=20) == 0
    table = sluice("status", "demo").stdout
    assert "plan demo  rev 2" in table
    assert "c                        core.echo          skipped" in table
    assert "b/a                      test.add           succeeded" in table
    events = sluice("events", "demo").stdout
    assert "runner_started" in events and "runner_stopped" in events
    assert "node_skipped" in events


def test_inbox_and_node_actions(sluice):
    sluice("init", "--pack", str(TESTPACK))
    plan = {"nodes": {"boom": {"fn": "test.boom"},
                      "q": {"fn": "core.ask", "in": {"question": {"value": "go?"}}}}}
    sluice("plan", "create", "p", json.dumps(plan))
    store = Store(sluice.home)
    runner = Runner(store)
    deadline = time.time() + 20
    while time.time() < deadline and len(store.inbox_list()) < 2:
        runner.tick()
        time.sleep(0.05)
    listing = sluice("inbox").stdout
    assert "p.0001" in listing and "p.0002" in listing and "go?" in listing
    items = json.loads(sluice("inbox", "--plan", "p", "--json").stdout)
    ask = next(i for i in items if i["kind"] == "ask")
    fail = next(i for i in items if i["kind"] == "failure")
    assert json.loads(sluice("inbox", "resolve", ask["id"], '{"answer": 42}').stdout) == {
        "ok": True}
    bad = sluice("inbox", "resolve", fail["id"], '{"action": "explode"}', check=False)
    assert json.loads(bad.stderr)["error"] == "bad_request"
    assert json.loads(sluice("node", "skip", "p", "boom", "--reason", "meh").stdout)["ok"]
    assert "inbox empty" in sluice("inbox").stdout
    assert len(json.loads(sluice("inbox", "--all", "--json").stdout)) == 2
    assert json.loads(sluice("node", "retry", "p", "boom").stdout)["nodes"] == ["boom"]
    missing = sluice("node", "cancel", "p", "zz", check=False)
    assert json.loads(missing.stderr)["error"] == "not_found"
    assert store.read_state("p")["nodes"]["q"]["status"] == "succeeded"


def test_fn_commands(sluice):
    sluice("init", "--pack", str(TESTPACK))
    listing = sluice("fn", "list").stdout
    assert "test.add               (a: int, b: int) -> (sum: int)" in listing
    assert "Add two ints." in listing
    assert json.loads(sluice("fn", "show", "test.twice").stdout)["name"] == "test.twice"
    (sluice.tmp / "in.json").write_text('{"a": 40, "b": 2}')
    assert json.loads(sluice("fn", "test", "test.add", "in.json").stdout) == {"sum": 42}
    assert json.loads(sluice("fn", "test", "test.quad", '{"x": 1}').stdout) == {"y": 4, "half": 2}
    boom = sluice("fn", "test", "test.boom", "{}", check=False)
    assert boom.returncode == 1 and "about to explode" in boom.stderr
    assert "test.boom: failed: RuntimeError: boom" in boom.stderr
    assert not (sluice.home / "plans").exists()  # fn test leaves this home untouched

    called = json.loads(sluice("fn", "call", "test.add", '{"a": 1, "b": 1}').stdout)
    assert called["status"] == "pending"
    store = Store(sluice.home)
    runner = Runner(store)
    deadline = time.time() + 20
    while time.time() < deadline:
        runner.tick()
        res = json.loads(sluice("fn", "result", called["call"]).stdout)
        if res["status"] == "succeeded":
            break
        time.sleep(0.1)
    assert res["output"] == {"sum": 2}
    bad = sluice("fn", "call", "test.add", '{"a": "x", "b": 1}', check=False)
    assert json.loads(bad.stderr)["errors"] == ['input.a: expected int, got "x"']
    assert sluice("fn", "show", "no.such", check=False).returncode == 1


def test_a_broken_pack_is_reported(sluice, tmp_path):
    bad = tmp_path / "badpack" / "x"
    bad.mkdir(parents=True)
    (bad / "fn.json").write_text("{not json")
    sluice("init", "--pack", str(Path(bad).parent))
    p = sluice("fn", "list", check=False)
    assert p.returncode == 1 and "cannot load fns" in p.stderr and "bad JSON" in p.stderr
