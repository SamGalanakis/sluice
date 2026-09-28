import json
import os
import time
from pathlib import Path

import pytest

from sluice import log as L
from sluice.errors import InvalidPlan
from sluice.runner import Runner, lock_held
from tests.conftest import add, create, d, settle, src, statuses, window


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
    e = steps["b"]
    assert e["status"] == "failed" and e["error"] == "run 1: exit code 1"
    # every run ended; the failed entry keeps what the good items produced
    assert (e["done"], e["total"]) == (3, 3) and len(e["run_ids"]) == 3
    assert e["outputs"] is None
    assert e["results"] == [{"done": True}, None, {"done": True}]


def test_a_failed_scatter_item_does_not_stop_its_siblings(store, runner):
    create(store, "p", {"g": {"run": "test.gate", "scatter": "tag",
                              "in": {"tag": d(["a", "b", "c"])}}})
    settle(runner, store, "p",
           until=lambda s: s["g"]["status"] == "running" and len(s["g"]["run_ids"]) == 3)
    run_dirs = [store.runs_dir("p") / rid
                for rid in store.read_state("p")["steps"]["g"]["run_ids"]]
    (run_dirs[1] / "fail").write_text("")
    (run_dirs[1] / "go").write_text("")  # item 1 fails while items 0 and 2 still wait
    e = settle(runner, store, "p", until=lambda s: s["g"].get("done") == 1)["g"]
    assert e["status"] == "running"  # the step waits for every run to end
    assert lock_held(run_dirs[0] / "shim.lock") and lock_held(run_dirs[2] / "shim.lock")
    for d_ in (run_dirs[0], run_dirs[2]):
        (d_ / "go").write_text("")
    e = settle(runner, store, "p")["g"]
    assert e["status"] == "failed" and e["error"] == "run 1: exit code 1"
    assert e["done"] == 3
    assert e["results"] == [{"tag": "a"}, None, {"tag": "c"}]


def test_two_failed_scatter_items_name_every_run_in_index_order(store, runner):
    create(store, "p", {"g": {"run": "test.gate", "scatter": "tag",
                              "in": {"tag": d(["a", "b", "c"])}}})
    settle(runner, store, "p",
           until=lambda s: s["g"]["status"] == "running" and len(s["g"]["run_ids"]) == 3)
    run_dirs = [store.runs_dir("p") / rid
                for rid in store.read_state("p")["steps"]["g"]["run_ids"]]
    for i in (0, 2):
        (run_dirs[i] / "fail").write_text("")
    for d_ in run_dirs:
        (d_ / "go").write_text("")
    e = settle(runner, store, "p")["g"]
    assert e["status"] == "failed"
    assert e["error"] == "2 of 3 runs failed: run 0: exit code 1; run 2: exit code 1"
    assert e["results"] == [None, {"tag": "b"}, None]


def test_a_spawn_error_fails_the_step_and_kills_the_runs_that_started(store, runner,
                                                                    monkeypatch):
    create(store, "p", {"w": {"run": "test.window", "scatter": "tag",
                              "in": {"seconds": d(30), "tag": d(["a", "b", "c"])}}})
    real = Runner._spawn_run

    def flaky(self, a, i):
        if i == 1:
            raise RuntimeError("spawn blew up")
        return real(self, a, i)

    monkeypatch.setattr(Runner, "_spawn_run", flaky)
    steps = settle(runner, store, "p")
    e = steps["w"]
    assert e["status"] == "failed" and e["error"] == "could not start the fn: spawn blew up"
    assert not runner.active
    [run_id] = e["run_ids"]  # only the run that started is recorded
    [rec] = [r for r in L.read(store.log_dir("p"), kinds=["step.status"])["records"]
             if r["to"] == "failed"]
    assert (rec["step"], rec["error"], rec["run_ids"]) == ("w", e["error"], [run_id])
    run_dir = store.runs_dir("p") / run_id
    deadline = time.time() + 10
    while _procs_in(run_dir) and time.time() < deadline:
        time.sleep(0.05)
    assert _procs_in(run_dir) == []


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
    # a non-scattered step keeps nothing: no results, and the retry is a plain pending
    assert "results" not in store.read_state("p")["steps"]["boom"]
    (home / "boom-ok").write_text("")
    store.retry("p", "boom", author="test", reason="fixed")
    assert store.read_state("p")["steps"]["boom"] == {"status": "pending"}
    steps = settle(runner, store, "p")
    assert steps["boom"]["status"] == "succeeded" and steps["after"]["outputs"] == {"value": True}


def test_a_retried_scatter_reruns_only_the_failed_items(store, runner):
    create(store, "p", {"g": {"run": "test.gate", "scatter": "tag",
                              "in": {"tag": d(["a", "b", "c"])}}})
    settle(runner, store, "p",
           until=lambda s: s["g"]["status"] == "running" and len(s["g"]["run_ids"]) == 3)
    run_dirs = [store.runs_dir("p") / rid
                for rid in store.read_state("p")["steps"]["g"]["run_ids"]]
    (run_dirs[1] / "fail").write_text("")
    for d_ in run_dirs:
        (d_ / "go").write_text("")
    e = settle(runner, store, "p")["g"]
    assert e["status"] == "failed"
    old_ids = e["run_ids"]
    store.retry("p", "g", author="test", reason="again")
    e = store.read_state("p")["steps"]["g"]
    assert e["status"] == "pending"
    assert e["kept"]["run_ids"] == old_ids
    assert e["kept"]["results"] == [{"tag": "a"}, None, {"tag": "c"}]
    e = settle(runner, store, "p",
               until=lambda s: s["g"]["status"] == "running"
               and len(s["g"]["run_ids"]) == 3)["g"]
    assert "kept" not in e  # dropped once the step starts
    assert (e["run_ids"][0], e["run_ids"][2]) == (old_ids[0], old_ids[2])
    assert e["run_ids"][1] != old_ids[1]  # only the failed item got a new run
    assert len(list(store.runs_dir("p").iterdir())) == 4
    (store.runs_dir("p") / e["run_ids"][1] / "go").write_text("")
    e = settle(runner, store, "p")["g"]
    assert e["status"] == "succeeded" and e["outputs"] == {"tag": ["a", "b", "c"]}


def test_a_retried_scatter_with_changed_inputs_reruns_every_item(store, runner):
    create(store, "p", {"b": {"run": "test.boom", "scatter": "ok",
                              "in": {"ok": src("oks")}}}, inputs={"oks": "boolean[]"})
    store.set_input("p", "oks", [True, False, True], "test", "go")
    steps = settle(runner, store, "p")
    assert steps["b"]["status"] == "failed"
    old_ids = steps["b"]["run_ids"]
    store.retry("p", "b", author="test")
    assert store.read_state("p")["steps"]["b"]["kept"]["run_ids"] == old_ids
    store.set_input("p", "oks", [True, True, True], "test", "all fine now")
    steps = settle(runner, store, "p")
    e = steps["b"]
    # the inputs changed, so nothing kept was valid: all three ran again
    assert e["status"] == "succeeded" and e["outputs"] == {"done": [True, True, True]}
    assert len(e["run_ids"]) == 3 and not set(e["run_ids"]) & set(old_ids)


def test_a_retried_stale_scatter_step_is_unchanged(store, runner):
    create(store, "p", {"w": {"run": "test.window", "scatter": "tag",
                              "in": {"seconds": d(0), "tag": src("tags")}}},
           inputs={"tags": "string[]"})
    store.set_input("p", "tags", ["a", "b"], "test", "go")
    settle(runner, store, "p")
    old_ids = store.read_state("p")["steps"]["w"]["run_ids"]
    store.set_input("p", "tags", ["x", "y"], "test", "changed")
    runner.tick()
    assert statuses(store, "p")["w"] == "stale"
    store.retry("p", "w", author="test")
    # stale, not failed: nothing is kept and the retry is a plain pending entry
    assert store.read_state("p")["steps"]["w"] == {"status": "pending"}
    e = settle(runner, store, "p")["w"]
    assert e["status"] == "succeeded" and e["outputs"]["tag"] == ["x", "y"]
    assert not set(e["run_ids"]) & set(old_ids)


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
    store.pause_steps("p", ["b"], paused=False, author="test")
    assert "paused" not in store.get("p")["steps"]["b"]
    steps = settle(runner, store, "p")
    assert steps["c"]["outputs"] == {"value": 13}


def test_a_paused_project_starts_nothing_and_a_running_step_can_be_paused(store, runner):
    create(store, "p", {"w": window(0.5), "a": add(d(1), d(2))})
    create(store, "q", {"a": add(d(1), d(2))})
    store.update_project("q", paused=True)
    settle(runner, store, "p", until=lambda s: s["w"]["status"] == "running")
    assert store.pause_steps("p", ["w"], author="test") == {"rev": 3, "steps": ["w"]}
    with pytest.raises(InvalidPlan) as e:
        store.update_step("p", "w", {"doc": "no"}, "test", "")
    assert e.value.errors == ["steps.w: cannot change a running step (only pause it)"]
    settle(runner, store, "p")
    for _ in range(3):
        runner.tick()
    assert statuses(store, "q") == {"a": "pending"} and store.status("q")["paused"] is True
    store.update_project("q", paused=False)
    assert settle(runner, store, "q")["a"]["outputs"] == {"sum": 3}


def test_a_new_runner_adopts_leftover_running_steps(store, runner):
    create(store, "p", {"w": {"run": "test.wait", "in": {"value": d("x")}},
                        "after": {"run": "core.echo", "in": {"value": src("w/value")}}})
    settle(runner, store, "p", until=lambda s: s["w"]["status"] == "running"
                                     and s["w"].get("run_ids"))
    [rid] = store.read_state("p")["steps"]["w"]["run_ids"]
    fresh = Runner(store)  # a runner that did not start the run picks it up
    fresh.tick()
    assert statuses(store, "p") == {"w": "running", "after": "pending"}
    (store.runs_dir("p") / rid / "go").write_text("")
    steps = settle(fresh, store, "p")
    assert steps["w"]["outputs"] == {"value": "x"} and steps["after"]["outputs"] == \
        {"value": "x"}
    [rec] = L.read(store.log_dir("p"), kinds=["run.adopt"])["records"]
    assert (rec["step"], rec["run"], rec["outcome"]) == ("w", rid, "watching")


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
