import json
import os
import signal
import subprocess
import sys
import time

import pytest

from tests.conftest import write_config

PLAN = {"label": "cli", "inputs": {"n": "int"}, "outputs": {"total": {"source": "b/sum"}},
        "steps": {"a": {"run": "test.add", "in": {"a": {"source": "n"}, "b": {"default": 2}}},
                  "b": {"run": "test.add", "in": {"a": {"source": "a/sum"}, "b": {"default": 1}}},
                  "boom": {"run": "test.boom", "in": {}}}}


@pytest.fixture
def sluice(tmp_path):
    home = tmp_path / "home"

    def run(*args, check=True):
        p = subprocess.run([sys.executable, "-m", "sluice.cli", *args], capture_output=True,
                           text=True, timeout=120, check=False, cwd=tmp_path,
                           env={**os.environ, "SLUICE_HOME": str(home)})
        if check:
            assert p.returncode == 0, p.stderr
        return p

    run.home, run.tmp = home, tmp_path
    return run


def json_out(p):
    return json.loads(p.stdout)


def test_init_writes_the_default_config_once(sluice):
    assert "config.json" in sluice("init").stdout
    assert json.loads((sluice.home / "config.json").read_text()) == {
        "fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420}, "max_parallel": 8}
    assert sluice("init", check=False).returncode == 1


def test_plans_manual_values_and_the_loop(sluice):
    write_config(sluice.home)
    (sluice.tmp / "plan.json").write_text(json.dumps(PLAN))
    assert json_out(sluice("plan", "create", "demo", "plan.json")) == {"rev": 1}
    assert json_out(sluice("plan", "show", "demo"))["steps"]["a"]["run"] == "test.add"
    ops = [{"op": "replace", "path": "/label", "value": "renamed"}]
    assert json_out(sluice("plan", "patch", "demo", "--rev", "1", "--reason", "rename",
                           json.dumps(ops))) == {"rev": 2}
    stale = sluice("plan", "patch", "demo", "--rev", "1", "--reason", "x", "[]", check=False)
    assert stale.returncode == 1 and json.loads(stale.stderr)["current_rev"] == 2
    bad = sluice("set-input", "demo", "n", '"x"', check=False)
    assert json.loads(bad.stderr)["errors"] == ['inputs.n: expected int, got "x"']
    assert json_out(sluice("set-input", "demo", "n", "4", "--reason", "go")) == {"ok": True}

    loop = subprocess.Popen([sys.executable, "-m", "sluice.cli", "loop"], cwd=sluice.tmp,
                            env={**os.environ, "SLUICE_HOME": str(sluice.home)})
    try:
        deadline = time.time() + 30
        while time.time() < deadline:
            s = json_out(sluice("status", "demo", "--json"))
            if {x["id"]: x["status"] for x in s["steps"]} == {
                    "a": "succeeded", "b": "succeeded", "boom": "failed"}:
                break
            time.sleep(0.3)
        assert s["outputs"] == {"total": 7}
        assert json_out(sluice("set-output", "demo", "boom", '{"done": true}')) == {"ok": True}
        assert json_out(sluice("retry", "demo", "boom", "--reason", "run it")) == {"ok": True}
    finally:
        loop.send_signal(signal.SIGTERM)
        assert loop.wait(timeout=20) == 0

    table = sluice("status", "demo").stdout
    assert "plan demo  rev 2" in table and "inputs: n=4" in table and "outputs: total=7" in table
    assert "b                test.add         succeeded" in table
    history = sluice("plan", "history", "demo").stdout.splitlines()
    assert history[0].startswith("rev 1") and "1 op(s)" in history[0]
    assert "plan_set_input" in history[2] and "step_set_output" in history[3]
    assert "step_retry" in history[4] and history[4].endswith("run it")
    assert sluice("view", "demo").stdout.startswith("flowchart LR\n")
    sluice("view", "demo", "--html", "demo.html")
    assert "<pre class=\"mermaid\">" in (sluice.tmp / "demo.html").read_text()
    missing = sluice("retry", "demo", "zz", check=False)
    assert json.loads(missing.stderr)["error"] == "not_found"


def test_fn_commands(sluice):
    write_config(sluice.home)
    listing = sluice("fn", "list").stdout
    assert "test.add           (a: int, b: int) -> (sum: int)" in listing
    assert "Add two ints." in listing and "git.head" in listing
    assert json_out(sluice("fn", "show", "core.echo"))["name"] == "core.echo"
    assert sluice("fn", "show", "no.such", check=False).returncode == 1
    called = json_out(sluice("fn", "call", "core.echo", '{"value": 1}'))
    assert called["status"] == "pending" and called["plan"].startswith("call-")
    bad = sluice("fn", "call", "test.add", '{"a": "x", "b": 1}', check=False)
    assert json.loads(bad.stderr)["errors"] == ['inputs.a: expected int, got "x"']


def test_a_broken_fn_dir_is_reported(sluice, tmp_path):
    (tmp_path / "fns" / "x").mkdir(parents=True)
    (tmp_path / "fns" / "x" / "fn.json").write_text("{not json")
    write_config(sluice.home, fn_dirs=[str(tmp_path / "fns")])
    p = sluice("fn", "list", check=False)
    assert p.returncode == 1 and "cannot load fns" in p.stderr and "bad JSON" in p.stderr
