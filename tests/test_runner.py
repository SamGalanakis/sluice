import json
import os
import signal
import time

import pytest

from sluice import lifecycle as L
from sluice import views
from sluice.errors import BadRequest, NotFound
from sluice.runner import Runner
from tests.conftest import create, event_types, output, settle, statuses


def v(x):
    return {"value": x}


def ref(r):
    return {"from": r}


def add(a, b):
    return {"fn": "test.add", "in": {"a": a, "b": b}}


def window(seconds, **extra):
    return {"fn": "test.window", "in": {"seconds": v(seconds)}, **extra}


def sleep(seconds, **extra):
    return {"fn": "test.sleep", "in": {"seconds": v(seconds)}, **extra}


def state_of(store, pid, nid):
    return store.read_state(pid)["nodes"][nid]


def overlap(a, b) -> bool:
    return a["start"] < b["end"] and b["start"] < a["end"]


# ---- basic execution ----------------------------------------------------------------------


def test_linear_chain_runs_in_order_with_outputs_and_run_dir(store, runner):
    create(store, "p", {
        "a": add(v(1), v(2)),
        "b": add(ref("a.sum"), v(10)),
        "c": {"fn": "core.echo", "in": {"value": ref("b.sum")}},
    })
    nodes = settle(runner, store, "p")
    assert {k: e["status"] for k, e in nodes.items()} == dict.fromkeys("abc", "succeeded")
    assert output(store, "p", "b") == {"sum": 13}
    assert output(store, "p", "c") == {"value": 13}
    a = nodes["a"]
    run_dir = store.runs_dir / a["run_id"]
    for f in ("cmd.json", "input.json", "stdout.log", "stderr.log", "output.json", "exit.json"):
        assert (run_dir / f).exists(), f
    assert json.loads((run_dir / "input.json").read_text()) == {"a": 1, "b": 2}
    assert json.loads((run_dir / "exit.json").read_text())["code"] == 0
    assert "adding 1 + 2" in (run_dir / "stderr.log").read_text()
    assert a["output"]["sha"] and a["attempt"] == 1 and a["pid"] is None
    # started before finished, b after a
    ev = [(e["type"], e.get("node")) for e in store.events("p")]
    assert ev.index(("node_succeeded", "a")) < ev.index(("node_started", "b"))
    st = store.read_state("p")
    assert st["plan_rev"] == 1 and st["rev"] >= 1


def test_fn_process_contract_env_and_cwd(store, runner):
    create(store, "p", {"e": {"fn": "test.env", "in": {"x": v([1, "two"])}}})
    settle(runner, store, "p")
    out = output(store, "p", "e")
    e = state_of(store, "p", "e")
    run_dir = str(store.runs_dir / e["run_id"])
    assert out["input"] == {"x": [1, "two"]}
    assert out["cwd"] == run_dir
    env = out["env"]
    assert env["SLUICE_HOME"] == str(store.home)
    assert (env["SLUICE_PLAN"], env["SLUICE_NODE"], env["SLUICE_ATTEMPT"]) == ("p", "e", "1")
    assert env["SLUICE_RUN_ID"] == e["run_id"] and env["SLUICE_RUN_DIR"] == run_dir
    assert env["SLUICE_IDEMPOTENCY_KEY"] == "p/e/1"
    assert env["SLUICE_FN_DIR"].endswith("testpack/test.env")


def test_fan_out_runs_in_parallel_then_fans_in(store, runner):
    create(store, "p", {
        "a": add(v(1), v(1)),
        "w1": {**window(0.6), "after": ["a"]},
        "w2": {**window(0.6), "after": ["a"]},
        "w3": {**window(0.6), "after": ["a"]},
        "join": {"fn": "core.echo", "in": {"value": ref("w1.end")}, "after": ["w2", "w3"]},
    })
    settle(runner, store, "p")
    ws = [output(store, "p", w) for w in ("w1", "w2", "w3")]
    assert overlap(ws[0], ws[1]) and overlap(ws[1], ws[2])
    assert output(store, "p", "join") == {"value": ws[0]["end"]}
    ev = [(e["type"], e.get("node")) for e in store.events("p")]
    assert ev.index(("node_started", "join")) > max(ev.index(("node_succeeded", w))
                                                    for w in ("w1", "w2", "w3"))


def test_when_skips_and_skips_propagate(store, runner):
    create(store, "p", {
        "a": add(v(1), v(2)),
        "yes": {"fn": "core.echo", "in": {"value": ref("a.sum")},
                "when": [{"from": "a.sum", "op": "eq", "value": 3},
                         {"from": "a.sum", "op": "in", "value": [3, 4]},
                         {"from": "a.sum", "op": "truthy"}]},
        "no": {"fn": "core.echo", "in": {"value": v(0)},
               "when": [{"from": "a.sum", "op": "ne", "value": 3}]},
        "no2": {"fn": "core.echo", "in": {"value": v(0)},
                "when": [{"from": "a.sum", "op": "falsy"}]},
        "child": add(ref("no.value"), v(1)),
        "grandchild": {"fn": "core.echo", "in": {"value": ref("child.sum")}},
        "after_yes": {"fn": "core.echo", "in": {"value": ref("yes.value")}},
    })
    nodes = settle(runner, store, "p")
    got = {k: e["status"] for k, e in nodes.items()}
    assert got == {"a": "succeeded", "yes": "succeeded", "no": "skipped", "no2": "skipped",
                   "child": "skipped", "grandchild": "skipped", "after_yes": "succeeded"}
    reasons = {e["node"]: e["data"]["reason"] for e in store.events("p")
               if e["type"] == "node_skipped"}
    assert reasons["no"] == "when is false: a.sum ne 3"
    assert reasons["child"] == "dependency no is skipped"
    assert reasons["grandchild"] == "dependency child is skipped"


def test_failed_dependency_blocks_and_opens_an_inbox_item(store, runner):
    create(store, "p", {"boom": {"fn": "test.boom"},
                        "child": {"fn": "core.echo", "in": {"value": v(1)}, "after": ["boom"]}})
    settle(runner, store, "p", until=lambda n: n["boom"]["status"] == "failed")
    for _ in range(3):
        runner.tick()
    assert statuses(store, "p") == {"boom": "failed", "child": "pending"}
    boom = state_of(store, "p", "boom")
    assert boom["error"] == "RuntimeError: boom (exit 1)"
    assert views.dry_run(store, "p")["blocked"] == [{"id": "child",
                                                     "reason": "dependency boom failed"}]
    [item] = store.inbox_list("p")
    assert item["kind"] == "failure" and item["node"] == "boom" and item["id"] == boom["item"]
    assert "about to explode" in item["stderr_tail"] and "RuntimeError: boom" in item["stderr_tail"]
    assert item["input"] == {"ok": None} and item["run_dir"].endswith(boom["run_id"])


def test_transient_failures_retry_with_backoff_then_succeed(store, runner):
    create(store, "p", {"f": {"fn": "test.flaky", "in": {"succeed_on": v(2)}}})
    t0 = time.time()
    nodes = settle(runner, store, "p")
    f = nodes["f"]
    assert f["status"] == "succeeded" and f["attempt"] == 2 and f["retries"] == 1
    assert output(store, "p", "f") == {"attempt": 2}
    assert time.time() - t0 >= 1.0  # the 1s backoff was honoured
    assert event_types(store, "p", "f") == ["node_started", "node_retrying", "node_started",
                                            "node_succeeded"]
    retry = next(e for e in store.events("p") if e["type"] == "node_retrying")
    assert retry["data"]["attempt"] == 2 and "not yet (attempt 1)" in retry["data"]["reason"]
    assert store.inbox_list("p") == []


def test_transient_failures_past_the_retry_budget_fail(store, runner):
    create(store, "p", {"f": {"fn": "test.flaky", "in": {"succeed_on": v(9)}}})
    nodes = settle(runner, store, "p", timeout=40)
    f = nodes["f"]
    assert f["status"] == "failed" and f["attempt"] == 3
    assert "(transient; 2 retries used)" in f["error"]
    assert [i["node"] for i in store.inbox_list("p")] == ["f"]


def test_timeout_kills_the_process_group(store, runner):
    create(store, "p", {"s": sleep(30, timeout="1s")})
    settle(runner, store, "p", until=lambda n: n["s"]["status"] == "running")
    pid = state_of(store, "p", "s")["pid"]
    nodes = settle(runner, store, "p", timeout=10)
    assert nodes["s"]["status"] == "failed"
    assert nodes["s"]["error"] == "timed out after 1.0s"
    assert not L.launcher_alive(pid)
    run_dir = store.runs_dir / nodes["s"]["run_id"]
    assert (run_dir / "started").exists() and not (run_dir / "exit.json").exists()


def test_wrong_output_type_fails_with_paths(store, runner):
    create(store, "p", {"b": {"fn": "test.badout"}})
    nodes = settle(runner, store, "p")
    assert nodes["b"]["status"] == "failed"
    assert nodes["b"]["error"] == (
        "output does not match the fn's out type: n: expected int, got \"seven\"; "
        "report.outcome: expected one of [done, blocked], got \"ok\"")


def test_removing_a_running_node_cancels_it(store, runner):
    create(store, "p", {"s": sleep(30), "keep": add(v(1), v(1))})
    nodes = settle(runner, store, "p", until=lambda n: n["s"]["status"] == "running")
    pid = nodes["s"]["pid"]
    store.patch("p", 1, [{"op": "remove", "path": "/nodes/s"}], "test", "drop it")
    runner.tick()
    assert state_of(store, "p", "s")["status"] == "cancelled"
    assert not L.launcher_alive(pid)
    cancel = next(e for e in store.events("p") if e["type"] == "node_cancelled")
    assert cancel["node"] == "s" and cancel["data"]["reason"] == "removed from the plan"
    runner.tick()
    assert "s" not in store.read_state("p")["nodes"]


def test_claims_serialise_nodes(store, runner):
    create(store, "p", {"w1": window(0.5, claims=["db"]), "w2": window(0.5, claims=["db"]),
                        "free": window(0.5)}, resources={"db": 1})
    settle(runner, store, "p")
    w1, w2, free = (output(store, "p", x) for x in ("w1", "w2", "free"))
    assert not overlap(w1, w2)
    assert overlap(w1, free)
    assert all(e["claims_held"] == [] for e in store.read_state("p")["nodes"].values())


def test_a_composite_claim_is_held_until_all_inner_nodes_finish(store, runner):
    create(store, "p", {"pair": {"fn": "test.pair", "in": {"seconds": v(0.4)}, "claims": ["db"]},
                        "other": window(0.4, claims=["db"])}, resources={"db": 1})
    nodes = settle(runner, store, "p", until=lambda n: n["pair/w1"]["status"] == "running")
    assert nodes["pair"]["claims_held"] == ["db"]
    settle(runner, store, "p")
    w1, w2 = output(store, "p", "pair/w1"), output(store, "p", "pair/w2")
    other = output(store, "p", "other")
    assert other["start"] >= w2["end"]  # not squeezed in between w1 and w2
    assert w1["end"] <= w2["start"]


def test_slot_limits(store, runner):
    create(store, "p", {f"h{i}": {"fn": "test.heavy", "in": {"seconds": v(0.4)}}
                        for i in range(3)})
    nodes = settle(runner, store, "p", until=lambda n: any(e["status"] == "running"
                                                          for e in n.values()))
    assert sum(e["status"] == "running" for e in nodes.values()) == 1
    assert "slot heavy is full (1/1)" in [d["reason"] for d in views.dry_run(store, "p")["blocked"]]
    settle(runner, store, "p")
    outs = [output(store, "p", f"h{i}") for i in range(3)]
    for i in range(3):
        for j in range(i + 1, 3):
            assert not overlap(outs[i], outs[j])


def test_effect_free_results_are_cached(store, runner):
    create(store, "p", {"a": add(v(20), v(22)), "b": {**add(v(20), v(22)), "after": ["a"]}})
    nodes = settle(runner, store, "p")
    assert nodes["a"]["cache_hit"] is False and nodes["a"]["run_id"]
    assert nodes["b"]["cache_hit"] is True and nodes["b"]["run_id"] is None
    assert output(store, "p", "b") == {"sum": 42}
    assert len(list(store.runs_dir.iterdir())) == 1
    hits = [e["data"]["cache_hit"] for e in store.events("p") if e["type"] == "node_succeeded"]
    assert hits == [False, True]


# ---- spawn and forward --------------------------------------------------------------------


def test_spawn_adds_nodes_that_then_run(store, runner):
    create(store, "p", {"sp": {"fn": "test.spawner",
                               "in": {"count": v(3), "prefix": v("s")}}})
    nodes = settle(runner, store, "p", until=lambda n: len(n) == 4 and all(
        e["status"] == "succeeded" for e in n.values()))
    assert set(nodes) == {"sp", "s-0", "s-1", "s-2"}
    assert [output(store, "p", f"s-{i}")["sum"] for i in range(3)] == [100, 101, 102]
    assert output(store, "p", "sp") == {"spawned": ["s-0", "s-1", "s-2"]}
    last = store.history("p")[-1]
    assert last["rev"] == 2 and last["author"] == "node:sp"
    assert last["reason"].startswith("fan out 3 (run ")
    spawned = next(e for e in store.events("p") if e["type"] == "spawn_applied")
    assert spawned["data"] == {"rev": 2, "nodes": ["s-0", "s-1", "s-2"]}


def test_an_invalid_spawn_fails_the_node(store, runner):
    create(store, "p", {"sp": {"fn": "test.spawner",
                               "in": {"count": v(1), "prefix": v("s"), "bad": v(True)}}})
    nodes = settle(runner, store, "p")
    assert nodes["sp"]["status"] == "failed"
    assert nodes["sp"]["error"].startswith(
        "invalid spawn: nodes.s-0.in.b: out type list<string> does not fit int")
    assert store.get("p")["rev"] == 1
    assert [i["node"] for i in store.inbox_list("p")] == ["sp"]


def test_forward_chain_answers_for_the_original_node(store, runner):
    gate = {"fn": "test.gate", "claims": ["db"], "in": {
        "round": v(1), "green_at": v(3), "prefix": v("gate"), "claims": v(["db"])}}
    create(store, "p", {"gate": gate,
                        "report": {"fn": "core.echo", "in": {"value": ref("gate.round")}},
                        "rival": window(0.2, claims=["db"])},
           resources={"db": 1})
    nodes = settle(runner, store, "p", until=lambda n: n["gate"]["status"] == "forwarded")
    assert nodes["gate"]["forward"] == "gate-2" and nodes["gate"]["claims_held"] == ["db"]
    assert nodes["report"]["status"] == "pending"
    nodes = settle(runner, store, "p", until=lambda n: len(n) == 5 and all(
        e["status"] in L.TERMINAL for e in n.values()))
    assert {k: e["status"] for k, e in nodes.items()} == dict.fromkeys(
        ["gate", "gate-2", "gate-3", "report", "rival"], "succeeded")
    assert nodes["gate-2"]["forward"] == "gate-3"
    # the dependent reads the final target's output through the original node
    assert output(store, "p", "report") == {"value": 3}
    assert output(store, "p", "gate")["round"] == 3
    # the claim was held across both retries: the rival only ran after the final gate
    assert output(store, "p", "rival")["start"] >= output(store, "p", "gate-3")["end"]
    fwd = [e for e in store.events("p") if e["type"] == "node_forwarded"]
    assert [(e["node"], e["data"]["to"]) for e in fwd] == [("gate", "gate-2"),
                                                          ("gate-2", "gate-3")]


def test_forward_inside_a_composite_keeps_the_composite_claim(store, runner):
    create(store, "p", {"c": {"fn": "test.gated", "claims": ["db"],
                              "in": {"green_at": v(2), "prefix": v("cg")}},
                        "rival": window(0.2, claims=["db"])}, resources={"db": 1})
    nodes = settle(runner, store, "p", until=lambda n: len(n) == 4 and all(
        e["status"] in L.TERMINAL for e in n.values()))
    assert nodes["c/g"]["status"] == "succeeded" and nodes["c/g"]["forward"] == "cg-2"
    assert nodes["c"]["status"] == "succeeded"
    assert output(store, "p", "c") == {"round": 2, "end": output(store, "p", "cg-2")["end"]}
    assert output(store, "p", "rival")["start"] >= output(store, "p", "cg-2")["end"]


def test_retrying_a_forwarded_node_retries_its_failed_target(store, runner, home):
    create(store, "p", {"gate": {"fn": "test.gate", "in": {
        "round": v(1), "green_at": v(2), "prefix": v("g"), "claims": v([])}}})
    settle(runner, store, "p", until=lambda n: "g-2" in n and n["g-2"]["status"] == "running")
    L.node_action(store, "p", "gate", "cancel", "stop", "test")
    assert state_of(store, "p", "g-2")["status"] == "cancelled"
    nodes = settle(runner, store, "p", until=lambda n: n["gate"]["status"] == "cancelled")
    L.node_action(store, "p", "gate", "retry", "again", "test")
    assert state_of(store, "p", "g-2")["status"] == "pending"
    nodes = settle(runner, store, "p")
    assert nodes["gate"]["status"] == "succeeded" and nodes["g-2"]["attempt"] == 2


def test_a_forward_to_a_target_that_does_not_fit_is_rejected(store, runner):
    create(store, "p", {"f": {"fn": "test.env", "in": {}, "hold": True}})
    runner.tick()
    _, exp = store.expanded("p")
    state = store.read_state("p")
    errs = runner._apply_spawn("p", exp.nodes["f"], state["nodes"]["f"], {
        "reason": "r", "forward": "t", "nodes": {"t": add(v(1), v(1))}}, state)
    assert errs == [("_spawn.forward: out type {sum} of t (fn test.add) does not fit "
                     "{env, cwd, input} of fn test.env: env is missing")]
    errs = runner._apply_spawn("p", exp.nodes["f"], state["nodes"]["f"], {
        "reason": "r", "forward": "zz", "nodes": {"t": add(v(1), v(1))}}, state)
    assert errs == ["_spawn.forward: 'zz' is not one of the spawned nodes"]
    assert store.get("p")["rev"] == 1


# ---- ask, inbox, operator actions ----------------------------------------------------------


def test_core_ask_waits_until_the_item_is_resolved(store, runner):
    create(store, "p", {"q": {"fn": "core.ask", "in": {"question": v("ship it?"),
                                                       "context": v({"pr": 7})}},
                        "then": {"fn": "core.echo", "in": {"value": ref("q.answer.go")}}})
    for _ in range(3):
        runner.tick()
    assert statuses(store, "p") == {"q": "waiting", "then": "pending"}
    [item] = store.inbox_list("p")
    assert (item["kind"], item["question"], item["context"], item["to"]) == (
        "ask", "ship it?", {"pr": 7}, "orchestrator")
    L.inbox_resolve(store, item["id"], {"answer": {"go": True}}, "me")
    assert state_of(store, "p", "q")["status"] == "succeeded"
    settle(runner, store, "p")
    assert output(store, "p", "then") == {"value": True}
    assert store.inbox_get(item["id"])["resolved_by"] == "me"


def test_failure_item_retry_recovers_the_node(store, runner, home):
    create(store, "p", {"boom": {"fn": "test.boom"},
                        "child": {"fn": "core.echo", "in": {"value": ref("boom.done")}}})
    settle(runner, store, "p", until=lambda n: n["boom"]["status"] == "failed")
    [item] = store.inbox_list("p")
    (home / "boom-ok").write_text("")
    L.inbox_resolve(store, item["id"], {"action": "retry"}, "me")
    e = state_of(store, "p", "boom")
    assert e["status"] == "pending" and e["attempt"] == 2
    assert store.inbox_get(item["id"])["resolution"] == {"action": "retry"}
    nodes = settle(runner, store, "p")
    assert nodes["boom"]["status"] == "succeeded" and nodes["child"]["status"] == "succeeded"
    assert store.inbox_list("p") == []


def test_node_retry_reopens_dependents_skipped_because_of_it(store, runner):
    create(store, "p", {"a": add(v(1), v(1)), "b": add(ref("a.sum"), v(1)),
                        "c": {"fn": "core.echo", "in": {"value": ref("b.sum")}}})
    L.node_action(store, "p", "a", "skip", "not now", "me")
    nodes = settle(runner, store, "p")
    assert {k: e["status"] for k, e in nodes.items()} == dict.fromkeys("abc", "skipped")
    assert nodes["a"]["skipped_by"] == "operator" and nodes["b"]["skipped_by"] == "dep"
    L.node_action(store, "p", "a", "retry", "now", "me")
    assert statuses(store, "p") == dict.fromkeys("abc", "pending")
    settle(runner, store, "p")
    assert output(store, "p", "c") == {"value": 3}


def test_node_cancel_kills_a_running_node(store, runner):
    create(store, "p", {"s": sleep(30)})
    nodes = settle(runner, store, "p", until=lambda n: n["s"]["status"] == "running")
    L.node_action(store, "p", "s", "cancel", "enough", "me")
    assert state_of(store, "p", "s")["status"] == "cancelled"
    assert not L.launcher_alive(nodes["s"]["pid"])
    runner.tick()
    assert state_of(store, "p", "s")["status"] == "cancelled"
    assert event_types(store, "p", "s") == ["node_started", "node_cancelled"]


def test_operator_actions_refuse_wrong_states(store, runner):
    create(store, "p", {"a": add(v(1), v(1))})
    settle(runner, store, "p")
    with pytest.raises(BadRequest, match="node a is succeeded; retry applies to"):
        L.node_action(store, "p", "a", "retry", "x", "me")
    with pytest.raises(NotFound):
        L.node_action(store, "p", "zz", "skip", "x", "me")


def test_changing_a_finished_node_reruns_it(store, runner):
    create(store, "p", {"a": add(v(1), v(1))})
    settle(runner, store, "p")
    store.patch("p", 1, [{"op": "replace", "path": "/nodes/a/in/b/value", "value": 5}], "me",
                "new input")
    nodes = settle(runner, store, "p")
    assert nodes["a"]["attempt"] == 2 and output(store, "p", "a") == {"sum": 6}


def test_hold_and_pause_start_nothing(store, runner):
    create(store, "p", {"a": {**add(v(1), v(1)), "hold": True}, "b": add(v(2), v(2))},
           paused=True)
    runner.tick()
    assert statuses(store, "p") == {"a": "pending", "b": "pending"}
    assert views.dry_run(store, "p")["blocked"] == [{"id": "a", "reason": "plan is paused"},
                                                    {"id": "b", "reason": "plan is paused"}]
    store.patch("p", 1, [{"op": "replace", "path": "/paused", "value": False}], "me", "go")
    settle(runner, store, "p", until=lambda n: n["b"]["status"] == "succeeded")
    assert state_of(store, "p", "a")["status"] == "pending"
    assert views.dry_run(store, "p")["blocked"] == [{"id": "a", "reason": "held"}]


def test_file_bindings_read_relative_to_the_plan_dir(store, runner):
    create(store, "p", {"m": {"fn": "test.echo_log", "in": {"msg": {"file": "brief.md"}}}})
    (store.plan_dir("p") / "brief.md").write_text("hello from a file")
    settle(runner, store, "p")
    assert output(store, "p", "m") == {"msg": "hello from a file", "attempt": 1}


# ---- restart ------------------------------------------------------------------------------


def test_a_new_runner_reattaches_to_a_running_node(store, runner):
    create(store, "p", {"s": sleep(1.5), "after": {"fn": "core.echo", "in": {
        "value": ref("s.slept")}}})
    settle(runner, store, "p", until=lambda n: n["s"]["status"] == "running")
    first = state_of(store, "p", "s")
    fresh = Runner(store)  # a restarted runner: no process handles, no memory
    nodes = settle(fresh, store, "p")
    assert nodes["s"]["status"] == "succeeded"
    assert (nodes["s"]["run_id"], nodes["s"]["attempt"]) == (first["run_id"], 1)
    assert output(store, "p", "after") == {"value": 1.5}
    assert event_types(store, "p", "s").count("node_started") == 1


def test_a_run_lost_without_exit_json_follows_the_retry_policy(store, runner):
    create(store, "p", {"s": sleep(30)})
    nodes = settle(runner, store, "p", until=lambda n: n["s"]["status"] == "running")
    os.killpg(nodes["s"]["pid"], signal.SIGKILL)
    runner.procs[nodes["s"]["pid"]].wait()
    nodes = settle(Runner(store), store, "p", timeout=10)
    assert nodes["s"]["status"] == "failed"
    assert nodes["s"]["error"].startswith("lost: the launcher exited without exit.json")


# ---- composites ---------------------------------------------------------------------------


def test_composite_status_and_outputs_are_derived(store, runner):
    create(store, "p", {"q": {"fn": "test.quad", "in": {"x": v(3)}},
                        "use": {"fn": "core.echo", "in": {"value": ref("q.half")}}})
    runner.tick()
    assert state_of(store, "p", "q")["status"] == "running"
    nodes = settle(runner, store, "p")
    assert nodes["q"]["composite"] and nodes["q"]["status"] == "succeeded"
    assert nodes["q/t1"]["status"] == "succeeded"
    assert output(store, "p", "q") == {"y": 12, "half": 6}
    assert output(store, "p", "use") == {"value": 6}
    got = views.node_get(store, "p", "q")
    assert got["expanded_ids"][:3] == ["q", "q/t1", "q/t1/a"]
    assert got["output"] == {"y": 12, "half": 6}


def test_a_failing_inner_node_fails_the_composite(store, runner):
    create(store, "p", {"f": {"fn": "test.fragile"}})
    nodes = settle(runner, store, "p", until=lambda n: n["f/b"]["status"] == "failed")
    runner.tick()
    nodes = store.read_state("p")["nodes"]
    assert nodes["f"]["status"] == "failed" and nodes["f/e"]["status"] == "pending"


def test_a_composite_whose_out_cannot_resolve_is_skipped(store, runner):
    create(store, "p", {"m": {"fn": "test.maybe", "in": {"go": v(False)}},
                        "use": {"fn": "core.echo", "in": {"value": ref("m.v")}},
                        "yes": {"fn": "test.maybe", "in": {"go": v(True)}}})
    nodes = settle(runner, store, "p")
    assert nodes["m/a"]["status"] == "skipped" and nodes["m/b"]["status"] == "succeeded"
    assert nodes["m"]["status"] == "skipped"
    assert nodes["use"]["status"] == "skipped"
    assert output(store, "p", "yes") == {"v": 2, "go": True}


def test_a_when_on_a_composite_applies_to_its_inner_nodes(store, runner):
    create(store, "p", {"a": add(v(1), v(1)),
                        "t": {"fn": "test.twice", "in": {"x": v(1)},
                              "when": [{"from": "a.sum", "op": "eq", "value": 99}]}})
    nodes = settle(runner, store, "p")
    assert nodes["t/a"]["status"] == "skipped" and nodes["t"]["status"] == "skipped"
