import json
import os
import time
from pathlib import Path

import pytest

from sluice.errors import InvalidPlan
from sluice.runner import RESTARTED, Runner
from tests.conftest import create, settle, statuses


def d(x):
    return {"default": x}


def src(ref):
    return {"source": ref}


def add(a, b):
    return {"run": "test.add", "in": {"a": a, "b": b}}


def window(seconds, **extra_in):
    return {"run": "test.window", "in": {"seconds": d(seconds), **extra_in}}


def overlap(a, b) -> bool:
    return a["start"] < b["end"] and b["start"] < a["end"]


def outputs(store, project, sid):
    return store.read_state(project)["steps"][sid]["outputs"]


def test_a_chain_runs_in_order_with_run_files(store, runner):
    create(store, "p", {"a": add(d(1), d(2)), "b": add(src("a/sum"), d(10)),
                        "c": {"run": "core.echo", "in": {"value": src("b/sum")}}},
           outputs={"total": src("c/value")})
    steps = settle(runner, store, "p")
    assert {k: e["status"] for k, e in steps.items()} == dict.fromkeys("abc", "succeeded")
    assert outputs(store, "p", "b") == {"sum": 13}
    assert store.status("p")["outputs"] == {"total": 13}
    a = steps["a"]
    assert a["started"] <= a["finished"] and a["error"] is None
    [run_id] = a["run_ids"]
    run_dir = store.runs_dir("p") / run_id
    assert json.loads((run_dir / "input.json").read_text()) == {"a": 1, "b": 2}
    assert json.loads((run_dir / "output.json").read_text()) == {"sum": 3}
    assert "adding 1 + 2" in (run_dir / "stderr.log").read_text()


def test_the_process_contract(store, runner):
    create(store, "p", {"e": {"run": "test.env", "in": {"x": d([1, "two"])}}})
    settle(runner, store, "p")
    out = outputs(store, "p", "e")
    [run_id] = store.read_state("p")["steps"]["e"]["run_ids"]
    run_dir = str(store.runs_dir("p") / run_id)
    assert out["input"] == {"x": [1, "two"]} and out["cwd"] == run_dir and out["step"] == "e"
    env = out["env"]
    assert env["SLUICE_HOME"] == str(store.home)
    assert (env["SLUICE_PROJECT"], env["SLUICE_STEP"], env["SLUICE_RUN_ID"]) == ("p", "e", run_id)
    assert env["SLUICE_RUN_DIR"] == run_dir
    assert env["SLUICE_FN_DIR"].endswith("testpack/test.env")


def test_fan_out_runs_in_parallel(store, runner):
    create(store, "p", {"a": add(d(1), d(1)),
                        **{f"w{i}": window(0.6, tag=src("a/sum")) for i in range(3)}})
    settle(runner, store, "p")
    w = [outputs(store, "p", f"w{i}") for i in range(3)]
    assert overlap(w[0], w[1]) and overlap(w[1], w[2])
    assert all(x["tag"] == 2 for x in w)


def test_fan_in_by_list_source_and_collect(store, runner):
    create(store, "p", {
        "a": add(d(1), d(1)), "b": add(d(2), d(2)),
        "gate": {"run": "core.collect", "in": {"items": src(["a/sum", "b/sum", "n"])}},
        "first": {"run": "core.echo", "in": {"value": src("gate/items.1")}},
    }, inputs={"n": "int?"})
    settle(runner, store, "p")
    assert outputs(store, "p", "gate") == {"items": [2, 4, None]}
    assert outputs(store, "p", "first") == {"value": 4}


def test_scatter_runs_once_per_item_and_collects_in_order(store, runner):
    create(store, "p", {
        "split": {"run": "test.split", "in": {"text": src("text")}},
        "each": {"run": "test.window", "scatter": "tag",
                 "in": {"seconds": d(0.3), "tag": src("split/parts")}},
        "none": {"run": "test.window", "scatter": "tag", "in": {"seconds": d(0), "tag": d([])}},
        "tags": {"run": "core.echo", "in": {"value": src("each/tag")}},
    }, inputs={"text": "string"})
    store.set_input("p", "text", "x y z", "test", "go")
    steps = settle(runner, store, "p")
    assert outputs(store, "p", "tags") == {"value": ["x", "y", "z"]}
    each = outputs(store, "p", "each")
    assert len(steps["each"]["run_ids"]) == 3 and len(each["start"]) == 3
    runs = [{"start": s, "end": e} for s, e in zip(each["start"], each["end"], strict=True)]
    assert overlap(runs[0], runs[1]) and overlap(runs[1], runs[2])
    assert outputs(store, "p", "none") == {"start": [], "end": [], "tag": []}


def test_a_failing_scatter_run_fails_the_step(store, runner):
    create(store, "p", {"b": {"run": "test.boom", "scatter": "ok",
                              "in": {"ok": d([True, False, True])}}})
    steps = settle(runner, store, "p")
    assert steps["b"]["status"] == "failed"
    assert steps["b"]["error"].startswith("run 1: exit code 1")


def test_a_failure_records_the_exit_code_and_stderr_and_blocks_dependents(store, runner):
    create(store, "p", {"boom": {"run": "test.boom", "in": {}},
                        "after": {"run": "core.echo", "in": {"value": src("boom/done")}}})
    settle(runner, store, "p", until=lambda s: s["boom"]["status"] == "failed")
    runner.tick()
    assert statuses(store, "p") == {"boom": "failed", "after": "pending"}
    err = store.read_state("p")["steps"]["boom"]["error"]
    assert err.startswith("exit code 1\n") and "about to explode" in err
    assert "RuntimeError: boom" in err


def test_outputs_that_do_not_match_fail_the_step(store, runner):
    create(store, "p", {"b": {"run": "test.badout", "in": {}}})
    steps = settle(runner, store, "p")
    assert steps["b"]["error"] == ('outputs do not match the fn: n: expected int, got "seven"; '
                                   'report.outcome: expected one of [done, blocked], got "ok"')


def test_retry_after_a_failure(store, runner, home):
    create(store, "p", {"boom": {"run": "test.boom", "in": {}},
                        "after": {"run": "core.echo", "in": {"value": src("boom/done")}}})
    settle(runner, store, "p", until=lambda s: s["boom"]["status"] == "failed")
    (home / "boom-ok").write_text("")
    store.retry("p", "boom", "test", "fixed")
    steps = settle(runner, store, "p")
    assert steps["boom"]["status"] == "succeeded" and steps["after"]["outputs"] == {"value": True}


def test_plan_inputs_and_manual_outputs_unblock_steps(store, runner):
    create(store, "p", {"a": add(src("n"), d(1)), "b": add(src("a/sum"), d(1))},
           inputs={"n": "int"})
    runner.tick()
    assert statuses(store, "p") == {"a": "pending", "b": "pending"}
    with pytest.raises(InvalidPlan) as e:
        store.set_output("p", "a", {"sum": 41}, "test", "known already")
    assert e.value.errors == ["plan input n has no value"]
    store.set_output("p", "a", {"sum": 41}, "test", "known already", force=True)
    steps = settle(runner, store, "p", until=lambda s: s["b"]["status"] == "succeeded")
    assert steps["b"]["outputs"] == {"sum": 42} and steps["a"]["manual"] is True
    assert statuses(store, "p")["a"] == "succeeded"  # a manual step is never run


def test_every_ready_step_starts_at_once_across_projects(store, runner):
    steps = {f"w{i}": window(0.5) for i in range(10)}
    create(store, "p", steps)
    create(store, "q", {"w": window(0.5)})
    runner.tick()
    assert set(statuses(store, "p").values()) == {"running"}
    assert statuses(store, "q") == {"w": "running"}
    settle(runner, store, "p")
    ws = [outputs(store, "p", sid) for sid in steps]
    assert all(overlap(ws[0], w) for w in ws[1:])


def test_a_paused_step_holds_its_inputs_until_unpaused(store, runner):
    create(store, "p", {"a": add(d(1), d(2)), "b": {**add(src("a/sum"), d(10)), "paused": True},
                        "c": {"run": "core.echo", "in": {"value": src("b/sum")}}})
    settle(runner, store, "p", until=lambda s: s["a"]["status"] == "succeeded")
    for _ in range(3):
        runner.tick()
    assert statuses(store, "p") == {"a": "succeeded", "b": "pending", "c": "pending"}
    assert store.status("p")["steps"][1]["paused"] is True
    store.pause_step("p", "b", False, "test", "")
    assert "paused" not in store.get("p")["steps"]["b"]
    steps = settle(runner, store, "p")
    assert steps["c"]["outputs"] == {"value": 13}


def test_a_paused_project_starts_nothing_and_a_running_step_can_be_paused(store, runner):
    create(store, "p", {"w": window(0.5), "a": add(d(1), d(2))})
    create(store, "q", {"a": add(d(1), d(2))})
    store.update_project("q", paused=True)
    settle(runner, store, "p", until=lambda s: s["w"]["status"] == "running")
    assert store.pause_step("p", "w", True, "test", "") == 3  # a running step takes a pause
    with pytest.raises(InvalidPlan) as e:
        store.update_step("p", "w", {"doc": "no"}, "test", "")
    assert e.value.errors == ["steps.w: cannot change a running step (only pause it)"]
    settle(runner, store, "p")
    for _ in range(3):
        runner.tick()
    assert statuses(store, "q") == {"a": "pending"} and store.status("q")["paused"] is True
    store.update_project("q", paused=False)
    assert settle(runner, store, "q")["a"]["outputs"] == {"sum": 3}


def test_a_new_runner_marks_leftover_running_steps_failed(store, runner):
    create(store, "p", {"w": window(30), "after": {"run": "core.echo",
                                                   "in": {"value": src("w/end")}}})
    settle(runner, store, "p", until=lambda s: s["w"]["status"] == "running")
    fresh = Runner(store)
    fresh.tick()
    e = store.read_state("p")["steps"]["w"]
    assert (e["status"], e["error"]) == ("failed", RESTARTED)
    assert statuses(store, "p")["after"] == "pending"


def _procs_in(run_dir) -> list[int]:
    """Pids of processes whose working directory is run_dir (Linux /proc)."""
    pids = []
    for proc in Path("/proc").iterdir():
        try:
            if proc.name.isdigit() and Path(os.readlink(proc / "cwd")) == run_dir:
                pids.append(int(proc.name))
        except OSError:
            continue
    return pids


def test_stopping_a_step_kills_the_fn_under_uv_too(store, runner):
    create(store, "p", {"w": window(30)})
    settle(runner, store, "p", until=lambda s: s["w"]["status"] == "running")
    [run_id] = store.read_state("p")["steps"]["w"]["run_ids"]
    run_dir = store.runs_dir("p") / run_id
    deadline = time.time() + 20
    while len(_procs_in(run_dir)) < 2 and time.time() < deadline:  # uv and the fn's python
        time.sleep(0.05)
    assert len(_procs_in(run_dir)) >= 2
    for a in runner.active.values():
        a.kill()
    deadline = time.time() + 5
    while _procs_in(run_dir) and time.time() < deadline:
        time.sleep(0.05)
    assert _procs_in(run_dir) == []


def test_removed_steps_are_dropped_and_new_ones_picked_up(store, runner):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(d(2), d(2))})
    settle(runner, store, "p")
    store.patch("p", 2, [{"op": "remove", "path": "/steps/a"},
                         {"op": "add", "path": "/steps/c", "value": add(src("b/sum"), d(1))}],
                "test", "reshape")
    steps = settle(runner, store, "p")
    assert set(steps) == {"b", "c"} and steps["c"]["outputs"] == {"sum": 5}


def test_built_ins_finish_inline_in_one_tick(store, runner):
    create(store, "p", {"a": {"run": "core.echo", "in": {"value": d(1)}},
                        "b": {"run": "core.echo", "in": {"value": src("a/value")}},
                        "c": {"run": "core.collect", "in": {"items": src(["a/value", "b/value"])}}})
    runner.tick()
    assert statuses(store, "p") == dict.fromkeys("abc", "succeeded")
    assert outputs(store, "p", "c") == {"items": [1, 1]}


def test_core_format_fills_from_an_array_or_a_record(store, runner):
    create(store, "p", {
        "a": add(d(1), d(2)),
        "pos": {"run": "core.format", "in": {"template": d("{0} and {1}"),
                                             "values": src(["a/sum", "a"])}},
        "named": {"run": "core.format", "in": {"template": d("sum={s} all={all}"),
                                               "values": d({"s": "x", "all": [1, "y"]})}},
        "bad": {"run": "core.format", "in": {"template": d("{missing}"), "values": d({})}},
    }, inputs={"a": "Any?"})
    steps = settle(runner, store, "p")
    assert steps["pos"]["outputs"] == {"text": "3 and null"}
    assert steps["named"]["outputs"] == {"text": 'sum=x all=[1, "y"]'}
    assert steps["bad"]["error"] == "core.format: KeyError: 'missing'"


def test_fn_processes_get_the_home_then_the_project_dotenv(store, runner):
    (store.home / ".env").write_text("# secrets\nTEST_TOKEN=abc123\nexport TEST_QUOTED=\"a b\"\n"
                                     "\nnot a line\nSLUICE_STEP=cannot-override\n"
                                     "TEST_SHARED=home\n")
    create(store, "p", {"e": {"run": "test.env", "in": {}}})
    create(store, "q", {"e": {"run": "test.env", "in": {}}})
    (store.project_dir("p") / ".env").write_text("TEST_SHARED=project-p\nTEST_ONLY_P=1\n")
    settle(runner, store, "p")
    settle(runner, store, "q")
    env = outputs(store, "p", "e")["env"]
    assert env["TEST_TOKEN"] == "abc123" and env["TEST_QUOTED"] == "a b"
    assert env["SLUICE_STEP"] == "e"
    assert env["TEST_SHARED"] == "project-p" and env["TEST_ONLY_P"] == "1"
    other = outputs(store, "q", "e")["env"]
    assert other["TEST_SHARED"] == "home" and "TEST_ONLY_P" not in other
    assert other["SLUICE_PROJECT"] == "q"


def test_values_of_removed_plan_inputs_are_dropped(store, runner):
    create(store, "p", {}, inputs={"n": "int", "m": "int"})
    store.set_input("p", "n", 1, "test", "x")
    store.set_input("p", "m", 2, "test", "x")
    store.patch("p", 2, [{"op": "remove", "path": "/inputs/m"}], "test", "drop m")
    runner.tick()
    assert store.read_state("p")["inputs"] == {"n": 1}
