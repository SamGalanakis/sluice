import json
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass

import pytest

from sluice import cli
from sluice import log as L
from sluice.store import Store
from tests.conftest import (
    SPAWN_STEPS,
    create,
    d,
    pid_alive,
    spawned_children,
    wait_gone,
    write_config,
)

PLAN = {"inputs": {"n": "int"}, "outputs": {"total": {"source": "b/sum"}},
        "steps": {"a": {"run": "test.add", "in": {"a": {"source": "n"}, "b": {"default": 2}}},
                  "b": {"run": "test.add", "in": {"a": {"source": "a/sum"}, "b": {"default": 1}}},
                  "boom": {"run": "test.boom", "in": {}}}}


@dataclass
class Result:
    returncode: int
    stdout: str
    stderr: str


@pytest.fixture
def sluice(tmp_path, monkeypatch, capsys):
    """`sluice <args>` run in-process (the same main() the console script calls)."""
    home = tmp_path / "home"
    monkeypatch.setenv("SLUICE_HOME", str(home))

    def run(*args, check=True):
        capsys.readouterr()
        try:
            code = cli.main(list(args))
        except SystemExit as e:  # argparse
            code = e.code
        out, err = capsys.readouterr()
        p = Result(code, out, err)
        if check:
            assert p.returncode == 0, p.stderr
        return p

    def tool(tool_name, check=True, **args):
        return run("tool", tool_name, json.dumps(args), check=check)

    run.home, run.tmp, run.tool = home, tmp_path, tool
    return run


def json_out(p):
    return json.loads(p.stdout)


def test_the_first_run_writes_the_default_config_and_lists_the_tools(sluice):
    listing = sluice("tool").stdout
    assert json.loads((sluice.home / "config.json").read_text()) == {
        "fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420}, "log_max": 10000}
    names = [line.split()[0] for line in listing.splitlines()]
    assert {"projects_list", "plan_patch", "fn_call", "verify", "status"} <= set(names)
    assert any(line.startswith("verify ") and "Check functions" in line
               for line in listing.splitlines())
    for gone in ("init", "plan", "status", "fn"):
        assert sluice(gone, check=False).returncode == 2


def test_tool_errors_and_bad_arguments_exit_1(sluice):
    missing = sluice.tool("status", project="zz", check=False)
    assert missing.returncode == 1 and json.loads(missing.stderr)["error"] == "not_found"
    unknown = sluice.tool("no_such_tool", check=False)
    assert unknown.returncode == 1 and json.loads(unknown.stderr)["error"] == "bad_request"
    bad = sluice("tool", "status", "{not json", check=False)
    assert bad.returncode == 1 and "not JSON" in json.loads(bad.stderr)["message"]
    wrong = sluice("tool", "status", "[1]", check=False)
    assert wrong.returncode == 1 and "JSON object" in json.loads(wrong.stderr)["message"]
    args = sluice.tool("plan_patch", project="p", check=False)
    assert args.returncode == 1 and "Field required" in json.loads(args.stderr)["message"]


def test_verify_exits_non_zero_on_problems(sluice):
    write_config(sluice.home)
    assert json_out(sluice.tool("project_create", name="p")) == {"name": "p"}
    assert json_out(sluice.tool("verify")) == {"ok": True, "problems": []}
    (sluice.home / "projects" / "p").mkdir(parents=True, exist_ok=True)
    (sluice.home / "projects" / "p" / ".env").write_text("oops\n")
    p = sluice.tool("verify", project="p", check=False)
    assert p.returncode == 1 and json_out(p)["problems"] == [
        {"where": "projects/p/.env:1", "message": "not a KEY=value line"}]


def test_query_prints_a_table_binds_params_and_lists_the_schema(sluice):
    write_config(sluice.home)
    sluice.tool("project_create", name="p", description="a  long\ndescription")
    sluice.tool("project_create", name="q")
    out = sluice("query", "SELECT name, description, archived FROM projects "
                 "WHERE name = ? OR archived = ? ORDER BY name", "p", "1", "--table").stdout
    assert out.splitlines() == ["name  description         archived",
                                "----  ------------------  --------",
                                "p     a long description  0",
                                "(1 row)"]
    cut = sluice("query", "SELECT name, description FROM projects ORDER BY name",
                 "--table", "--width", "5", "--limit", "1").stdout.splitlines()
    assert cut[2] == "p     a lo…" and cut[-1] == "(1 row, truncated)"
    two = sluice("query", "SELECT ? AS n, ? AS s UNION ALL SELECT 1, 'z'", "null", "x y")
    assert json_out(two) == {"columns": ["n", "s"], "rows": [[None, "x y"], [1, "z"]],
                             "truncated": False}
    assert two.stdout.splitlines()[-2:] == ['  [null, "x y"],', '  [1, "z"]]}']
    assert json_out(sluice("query", "SELECT 1 WHERE 0"))["rows"] == []
    listing = sluice("query").stdout.splitlines()
    assert "view  log(" in "\n".join(listing) and listing[0].startswith("table projects(name")
    bad = sluice("query", "DELETE FROM projects", check=False)
    assert bad.returncode == 1 and json.loads(bad.stderr)["error"] == "bad_request"


def test_a_direct_fn_call_needs_no_runner(sluice):
    write_config(sluice.home)
    res = json_out(sluice.tool("fn_call", name="test.add", inputs={"a": 1, "b": 2},
                               direct=True))
    assert res["status"] == "succeeded" and res["outputs"] == {"sum": 3}
    queued = json_out(sluice.tool("fn_call", name="core.echo", inputs={"value": 1}))
    assert queued["status"] == "pending"  # no runner is up
    bad = sluice.tool("fn_call", name="test.add", inputs={"a": "x", "b": 1}, check=False)
    assert json.loads(bad.stderr)["errors"] == ['inputs.a: expected int, got "x"']


def test_a_project_through_the_tools_and_the_loop(sluice):
    write_config(sluice.home)
    sluice.tool("project_create", name="demo", description="cli")
    ops = [{"op": "replace", "path": f"/{k}", "value": v} for k, v in PLAN.items()]
    assert json_out(sluice.tool("plan_patch", project="demo", rev=1, reason="plan",
                                ops=ops, start=True)) == {"rev": 2}
    assert json_out(sluice.tool("plan_get", project="demo"))["plan"] == PLAN
    stale = sluice.tool("plan_patch", project="demo", rev=1, reason="x", ops=[], check=False)
    assert stale.returncode == 1 and json.loads(stale.stderr)["current_rev"] == 2
    bad = sluice.tool("plan_set_input", project="demo", name="n", value="x", check=False)
    assert json.loads(bad.stderr)["errors"] == ['inputs.n: expected int, got "x"']
    assert json_out(sluice.tool("plan_set_input", project="demo", name="n", value=4,
                                reason="go")) == {"ok": True}

    loop = subprocess.Popen([sys.executable, "-m", "sluice.cli", "loop"], cwd=sluice.tmp,
                            env={**os.environ, "SLUICE_HOME": str(sluice.home)})
    try:
        deadline = time.time() + 30
        while time.time() < deadline:
            s = json_out(sluice.tool("status", project="demo", all=True))
            if {x["id"]: x["status"] for x in s["steps"]} == {
                    "a": "succeeded", "b": "succeeded", "boom": "failed"}:
                break
            time.sleep(0.3)
        assert s["outputs"] == {"total": 7}
        queued = json_out(sluice.tool("fn_call", name="test.add", inputs={"a": 1, "b": 1},
                                      project="demo", wait=20))
        assert queued["status"] == "succeeded" and queued["outputs"] == {"sum": 2}
        assert json_out(sluice.tool("step_set_output", project="demo", step="boom",
                                    outputs={"done": True})) == {"ok": True}
        assert json_out(sluice.tool("step_retry", project="demo", steps="boom",
                                    reason="run it")) == {"steps": ["boom"]}
    finally:
        loop.send_signal(signal.SIGTERM)
        assert loop.wait(timeout=20) == 0

    history = json_out(sluice.tool("plan_history", project="demo"))
    assert [h["kind"] for h in history] == [
        "plan.edit", "plan.edit", "plan.input", "step.output", "step.retry"]
    assert history[-1]["reason"] == "run it"
    view = sluice.tool("plan_view", project="demo")
    assert view.stdout.startswith("flowchart LR\n")
    page = sluice.tool("plan_view", project="demo", format="html")
    assert '<div class="plane"' in page.stdout and "<nav" not in page.stdout
    missing = sluice.tool("step_retry", project="demo", steps="zz", check=False)
    assert json.loads(missing.stderr)["error"] == "not_found"


def test_sighup_stops_a_kill_runs_loop_and_every_process_its_fns_started(home):
    """`sluice loop --kill-runs`: closing the terminal (`tmux kill-session`) sends SIGHUP: the
    loop stops its fns like on SIGTERM, including a child in a session of its own that only
    the fn's SIGTERM handling reaches, and exits 0."""
    store = Store(home)
    create(store, "p", SPAWN_STEPS)
    loop = subprocess.Popen([sys.executable, "-m", "sluice.cli", "loop", "--kill-runs"],
                            env={**os.environ, "SLUICE_HOME": str(home)},
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        pids = spawned_children(store, "p", list(SPAWN_STEPS))
        assert all(map(pid_alive, pids))
        loop.send_signal(signal.SIGHUP)
        assert loop.wait(timeout=20) == 0, loop.stderr.read().decode()
        assert wait_gone(pids) == []
    finally:
        if loop.poll() is None:
            loop.kill()
            loop.wait()


def test_sighup_leaves_a_default_loops_runs_for_the_next_loop_to_adopt(home):
    """`sluice loop` with no --kill-runs: SIGHUP (a closed terminal) exits 0 but leaves a
    running fn alive; the next loop adopts it and the step still succeeds."""
    store = Store(home)
    create(store, "p", {"w": {"run": "test.wait", "in": {"value": d("x")}}})
    env = {**os.environ, "SLUICE_HOME": str(home)}
    runs = home / "projects" / "p" / "runs"

    def shim_of():  # the run dir and its shim's pid once the first loop has spawned it
        deadline = time.time() + 30
        while time.time() < deadline:
            for rd in sorted(runs.glob("*")):
                if (rd / "shim.json").exists():
                    return rd, json.loads((rd / "shim.json").read_text())["pid"]
            time.sleep(0.1)
        raise AssertionError("the step's run never started")

    def adopted():  # the next loop's run.adopt record in the project log
        deadline = time.time() + 30
        while time.time() < deadline:
            if L.read(home, "p", kinds=["run.adopt"])["records"]:
                return
            time.sleep(0.1)
        raise AssertionError("the run was never adopted")

    def step_status():
        return store.read_state("p")["steps"]["w"]["status"]

    loop = subprocess.Popen([sys.executable, "-m", "sluice.cli", "loop"], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        run_dir, shim_pid = shim_of()
        loop.send_signal(signal.SIGHUP)
        assert loop.wait(timeout=20) == 0, loop.stderr.read().decode()
        assert pid_alive(shim_pid)  # left running for the next runner

        loop = subprocess.Popen([sys.executable, "-m", "sluice.cli", "loop"], env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        adopted()
        (run_dir / "go").write_text("")
        deadline = time.time() + 30
        while step_status() != "succeeded" and time.time() < deadline:
            time.sleep(0.1)
        assert step_status() == "succeeded"
    finally:
        if loop.poll() is None:
            loop.kill()
            loop.wait()
