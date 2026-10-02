"""Project resources and admission (SPEC §6 "Resources"): a step's `needs` start it only while
every resource it names has room, in priority order; what does not fit stays pending, queued,
with a reason; what running steps hold frees when they leave running."""

import json
import time

import pytest

from sluice import log as L
from sluice import me as M
from sluice import runner as R
from sluice import watch
from sluice.errors import BadRequest, InvalidPlan
from sluice.runner import Runner
from tests.conftest import add, create, d, statuses, write_fn
from tests.test_recipes import write_recipe


def gate(**extra):
    """A test.gate step: it runs until `go` appears in its run dir (`fail` too: it fails)."""
    return {"run": "test.gate", "in": {"tag": d("x")}, **extra}


def project(store, name, steps, resources, **doc):
    store.create_project(name, "", "test", resources=resources)
    plan = {"inputs": {}, "outputs": {}, "steps": steps, **doc}
    ops = [{"op": "replace", "path": f"/{k}", "value": v} for k, v in plan.items()]
    return store.patch(name, 1, ops, "test", "test")


def running(store, p):
    return sorted(s for s, x in statuses(store, p).items() if x == "running")


def tick_until(runner, store, p, until, timeout=30.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        runner.tick()
        if until(store.read_state(p)):
            return store.read_state(p)
        time.sleep(0.05)
    raise AssertionError(f"timed out; statuses: {statuses(store, p)}, "
                         f"resources: {store.read_state(p).get('resources')}")


def started(runner, store, p, sid):
    """Tick until the step runs with its run dir there; returns the run dir."""
    st = tick_until(runner, store, p, lambda s: s["steps"][sid]["status"] == "running"
                    and s["steps"][sid].get("run_ids"))
    return store.runs_dir(p) / st["steps"][sid]["run_ids"][0]


def release(runner, store, p, sid, fail=False):
    """Let a running gate step finish (succeed, or fail), and tick until it has."""
    d_ = started(runner, store, p, sid)
    if fail:
        (d_ / "fail").write_text("")
    (d_ / "go").write_text("")
    tick_until(runner, store, p, lambda s: s["steps"][sid]["status"] in ("succeeded", "failed"))


def waiting(store, p, sid):
    return next(r for r in store.status(p, all=True)["steps"] if r["id"] == sid).get("waiting")


def records(store, p, kind):
    return L.read(store.home, p, kinds=[kind], limit=1000)["records"]


# ---- admission ------------------------------------------------------------------------------


def test_the_cap_holds_under_many_ready_steps(store, runner):
    project(store, "p", {f"s{i}": gate(needs={"lane": 1}) for i in range(6)}, {"lane": 2})
    runner.tick()
    assert running(store, "p") == ["s0", "s1"]
    for _ in range(3):  # more ticks admit nothing more
        runner.tick()
    st = store.read_state("p")["steps"]
    assert running(store, "p") == ["s0", "s1"]
    assert all(st[f"s{i}"] == {"status": "pending", "queued": ["lane"]} for i in range(2, 6))
    assert store.status("p")["resources"] == {"lane": {"capacity": 2, "held": 2, "queued": 4}}
    release(runner, store, "p", "s0")
    tick_until(runner, store, "p", lambda s: s["steps"]["s2"]["status"] == "running")
    assert running(store, "p") == ["s1", "s2"]  # the next waiter, in the same order
    assert store.status("p")["resources"]["lane"] == {"capacity": 2, "held": 2, "queued": 3}


def test_higher_priority_starts_first_and_ties_keep_the_plan_order(store, runner):
    project(store, "p", {"a": gate(needs={"lane": 1}), "b": gate(needs={"lane": 1}, priority=5),
                         "c": gate(needs={"lane": 1}, priority=5),
                         "d": gate(needs={"lane": 1}, priority=-1)}, {"lane": 1})
    order = []
    for _ in range(4):
        tick_until(runner, store, "p", lambda s: any(e["status"] == "running"
                                                     for e in s["steps"].values()))
        [sid] = running(store, "p")
        order.append(sid)
        release(runner, store, "p", sid)
    assert order == ["b", "c", "a", "d"]


def test_priority_orders_admission_within_one_tick(store, runner):
    project(store, "p", {"low": gate(needs={"lane": 1}), "mid": gate(needs={"lane": 1},
                                                                    priority=1),
                         "top": gate(needs={"lane": 1}, priority=9)}, {"lane": 2})
    runner.tick()
    assert running(store, "p") == ["mid", "top"]


def test_leaving_running_frees_what_it_held(store, runner):
    """Success, failure and cancel each admit the next waiter."""
    project(store, "p", {f"s{i}": gate(needs={"lane": 1}) for i in range(4)}, {"lane": 1})
    runner.tick()
    assert running(store, "p") == ["s0"]
    release(runner, store, "p", "s0")
    tick_until(runner, store, "p", lambda s: s["steps"]["s1"]["status"] == "running")
    release(runner, store, "p", "s1", fail=True)
    assert statuses(store, "p")["s1"] == "failed"
    tick_until(runner, store, "p", lambda s: s["steps"]["s2"]["status"] == "running")
    started(runner, store, "p", "s2")
    store.cancel_steps("p", ["s2"], author="test", reason="enough")
    st = tick_until(runner, store, "p", lambda s: s["steps"]["s3"]["status"] == "running")
    assert st["steps"]["s2"]["error"] == "cancelled: enough"
    assert running(store, "p") == ["s3"]


def test_a_step_needing_several_resources_starts_only_when_all_fit(store, runner):
    project(store, "p", {"x": gate(needs={"lane": 1, "codex": 1}),
                         "y": gate(needs={"lane": 1, "codex": 1}),
                         "z": gate(needs={"lane": 1})}, {"lane": 3, "codex": 1})
    runner.tick()
    assert running(store, "p") == ["x", "z"]  # y is short of codex; z, after it, fits
    assert store.read_state("p")["steps"]["y"]["queued"] == ["codex"]
    assert waiting(store, "p", "y") == ["queued: needs codex 1 (1/1 held)"]
    release(runner, store, "p", "x")
    tick_until(runner, store, "p", lambda s: s["steps"]["y"]["status"] == "running")
    assert store.status("p")["resources"] == {"lane": {"capacity": 3, "held": 2, "queued": 0},
                                              "codex": {"capacity": 1, "held": 1, "queued": 0}}


def test_a_step_short_of_two_resources_names_both(store, runner):
    project(store, "p", {"x": gate(needs={"lane": 2, "codex": 1}),
                         "y": gate(needs={"lane": 1, "codex": 1})}, {"lane": 2, "codex": 1})
    runner.tick()
    assert waiting(store, "p", "y") == ["queued: needs lane 1 (2/2 held), codex 1 (1/1 held)"]


def test_steps_without_needs_are_unaffected(store, runner):
    project(store, "p", {"a": gate(), "b": gate(), "c": add(d(1), d(2)),
                         "q": gate(needs={"lane": 1})}, {"lane": 1})
    store.update_project("p", resources={"lane": 0})  # a capacity of 0 admits nothing
    runner.tick()
    assert running(store, "p") == ["a", "b", "c"]
    assert statuses(store, "p")["q"] == "pending"
    assert waiting(store, "p", "q") == ["queued: needs lane 1 (0/0 held)"]
    # a need of 0 always fits, and a project without resources has no `resources`
    store.update_step("p", "q", {"needs": {"lane": 0}}, "test", "")
    runner.tick()
    assert statuses(store, "p")["q"] == "running"
    create(store, "plain", {"a": add(d(1), d(1))})
    assert "resources" not in store.status("plain") and "resources" not in store.project("plain")


def test_a_scattered_step_holds_its_needs_once(store, runner):
    project(store, "p", {"many": {"run": "test.gate", "scatter": "tag",
                                  "in": {"tag": d(["a", "b", "c"])}, "needs": {"lane": 1}},
                         "one": gate(needs={"lane": 1}), "two": gate(needs={"lane": 1})},
            {"lane": 2})
    st = tick_until(runner, store, "p",
                    lambda s: len(s["steps"]["many"].get("run_ids") or []) == 3)
    assert st["steps"]["one"]["status"] == "running"
    assert st["steps"]["two"]["queued"] == ["lane"]
    assert store.status("p")["resources"]["lane"]["held"] == 2


def test_paused_and_not_ready_steps_are_not_queued(store, runner):
    project(store, "p", {"a": gate(needs={"lane": 1}), "b": gate(needs={"lane": 1}),
                         "c": gate(needs={"lane": 1}, after=["a"])}, {"lane": 1})
    runner.tick()
    assert store.read_state("p")["steps"]["b"]["queued"] == ["lane"]
    assert "queued" not in store.read_state("p")["steps"]["c"]  # not ready: not queued
    store.pause_steps("p", ["b"], author="test", reason="hold")
    runner.tick()
    assert "queued" not in store.read_state("p")["steps"]["b"]  # a hold, not a queue
    assert waiting(store, "p", "b") == ["paused: hold"]
    store.update_project("p", paused=True, author="test")
    store.pause_steps("p", ["b"], paused=False, author="test")
    runner.tick()
    assert "queued" not in store.read_state("p")["steps"]["b"]


# ---- what it says -----------------------------------------------------------------------------


def test_the_queued_reason_shows_in_status_units_and_step_context(store, runner):
    project(store, "p", {"a": gate(needs={"lane": 1}, tags=["unit:one"]),
                         "b": gate(needs={"lane": 1}, tags=["unit:two"])}, {"lane": 1})
    runner.tick()
    assert waiting(store, "p", "b") == ["queued: needs lane 1 (1/1 held)"]
    row = next(r for r in store.status("p")["steps"] if r["id"] == "b")
    assert row["needs"] == {"lane": 1} and "priority" not in row
    units = watch.units(store, "p")
    assert units["resources"] == {"lane": {"capacity": 1, "held": 1, "queued": 1}}
    two = next(u for u in units["units"] if u["unit"] == "two")
    assert (two["state"], two["steps"], two["blocked"]) == \
        ("queued", "b≡", "queued: needs lane 1 (1/1 held)")
    assert "queued: needs lane 1 (1/1 held)" in two["line"]
    assert [u["unit"] for u in watch.units(store, "p", state="queued")["units"]] == ["two"]
    ctx = M.context(store, "p", "b")
    assert ctx["queued"] == "queued: needs lane 1 (1/1 held)" and ctx["needs"] == {"lane": 1}
    assert "queued: needs lane 1 (1/1 held)" in M.render(ctx).splitlines()[1]
    assert "queued" not in M.context(store, "p", "a")


def test_queueing_and_admission_are_logged_once_per_change(store, runner):
    project(store, "p", {"a": gate(needs={"lane": 1}), "b": gate(needs={"lane": 1})},
            {"lane": 1})
    for _ in range(5):
        runner.tick()
    [rec] = records(store, "p", "step.queued")
    assert (rec["step"], rec["needs"], rec["resources"], rec["reason"]) == \
        ("b", {"lane": 1}, ["lane"], "needs lane 1 (1/1 held)")
    release(runner, store, "p", "a")
    tick_until(runner, store, "p", lambda s: s["steps"]["b"]["status"] == "running")
    runs = [r for r in records(store, "p", "step.status") if r["to"] == "running"]
    assert [(r["step"], r["needs"]) for r in runs] == [("a", {"lane": 1}), ("b", {"lane": 1})]
    assert len(records(store, "p", "step.queued")) == 1
    assert "queued" not in store.read_state("p")["steps"]["b"]


# ---- a restart --------------------------------------------------------------------------------


def test_a_restarted_runner_counts_what_adopted_runs_hold(store, runner):
    project(store, "p", {"a": gate(needs={"lane": 1}), "b": gate(needs={"lane": 1})},
            {"lane": 1})
    run_dir = started(runner, store, "p", "a")
    r2 = Runner(store)  # a new runner: it knows nothing of `a` but its running entry
    try:
        for _ in range(3):
            r2.tick()
        assert running(store, "p") == ["a"] and "a" in {k[2] for k in r2.active}
        assert store.read_state("p")["steps"]["b"]["queued"] == ["lane"]
        assert waiting(store, "p", "b") == ["queued: needs lane 1 (1/1 held)"]
        (run_dir / "go").write_text("")
        tick_until(r2, store, "p", lambda s: s["steps"]["b"]["status"] == "running")
        assert statuses(store, "p")["a"] == "succeeded"
    finally:
        for a in r2.active.values():
            a.kill()


# ---- capacity fns ------------------------------------------------------------------------------

CAP_MAIN = """# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import time
from pathlib import Path

from sluice.fn import run


def main(inp, ctx):
    text = Path({path!r}).read_text().strip()
    if text == "sleep":
        time.sleep(60)
    if text == "fail":
        raise RuntimeError("no reading")
    return {{"capacity": int(text) if text.lstrip("-").isdigit() else text}}


if __name__ == "__main__":
    run(main)
"""


@pytest.fixture
def cap_fn(store, tmp_path):
    """A project fn `p.cap` in project p returning the capacity written in a file; returns
    the function that sets that file's text."""
    path = tmp_path / "cap.txt"
    path.write_text("2")
    write_fn(store.project_dir("p") / "fns", "p.cap", {}, {"capacity": "Any"},
             main=CAP_MAIN.format(path=str(path)))
    return path.write_text


def capacity(store, p, name="cpu"):
    return (store.read_state(p).get("resources") or {}).get(name)


def call_again(runner, p, name="cpu"):
    runner._caps[(p, name)].due = 0.0  # due now, not CAPACITY_EVERY after the last call


def test_a_capacity_fn_gives_the_capacity_and_its_value_is_cached(store, runner, cap_fn):
    project(store, "p", {f"s{i}": gate(needs={"cpu": 1}) for i in range(4)},
            {"cpu": {"capacity_fn": "p.cap"}})
    runner.tick()  # no value yet: nothing is admitted
    assert running(store, "p") == []
    assert waiting(store, "p", "s0") == ["queued: needs cpu 1 (capacity unknown)"]
    assert store.status("p")["resources"] == {
        "cpu": {"capacity": None, "held": 0, "queued": 4, "capacity_fn": "p.cap"}}
    tick_until(runner, store, "p", lambda s: len(running(store, "p")) == 2)
    assert capacity(store, "p") == {"fn": "p.cap", "capacity": 2}
    assert store.status("p")["resources"]["cpu"] == {"capacity": 2, "held": 2, "queued": 2,
                                                     "capacity_fn": "p.cap"}
    cap_fn("3")
    t = time.time()
    while time.time() - t < 1.5:  # not called again before CAPACITY_EVERY: the value stands
        runner.tick()
        time.sleep(0.05)
    assert capacity(store, "p")["capacity"] == 2 and len(running(store, "p")) == 2
    call_again(runner, "p")
    tick_until(runner, store, "p", lambda s: len(running(store, "p")) == 3)
    assert capacity(store, "p") == {"fn": "p.cap", "capacity": 3}
    recs = records(store, "p", "project.capacity")
    assert [(r["resource"], r["capacity"]) for r in recs] == [("cpu", None), ("cpu", 2),
                                                               ("cpu", 3)]


def test_a_failing_capacity_fn_keeps_the_last_value(store, runner, cap_fn):
    project(store, "p", {f"s{i}": gate(needs={"cpu": 1}) for i in range(3)},
            {"cpu": {"capacity_fn": "p.cap"}})
    tick_until(runner, store, "p", lambda s: len(running(store, "p")) == 2)
    for bad, err in (("fail", "RuntimeError: no reading"),
                     ("-1", "capacity is -1, not an integer >= 0")):
        cap_fn(bad)
        call_again(runner, "p")
        st = tick_until(runner, store, "p", lambda s, err=err: err in (
            (s.get("resources") or {}).get("cpu", {}).get("error") or ""))
        assert st["resources"]["cpu"]["capacity"] == 2  # the last good value stands
        assert store.status("p")["resources"]["cpu"]["capacity"] == 2
        assert err in store.status("p")["resources"]["cpu"]["error"]
    release(runner, store, "p", "s0")
    tick_until(runner, store, "p", lambda s: s["steps"]["s2"]["status"] == "running")
    cap_fn("2")
    call_again(runner, "p")
    tick_until(runner, store, "p", lambda s: "error" not in s["resources"]["cpu"])


def test_a_capacity_fn_with_no_value_admits_nothing_and_blocks_nothing_else(
        store, runner, cap_fn, monkeypatch):
    monkeypatch.setattr(R, "CAPACITY_TIMEOUT", 1.0)
    cap_fn("sleep")
    project(store, "p", {"c": gate(needs={"cpu": 1}), "l": gate(needs={"lane": 1}),
                         "n": gate()}, {"cpu": {"capacity_fn": "p.cap"}, "lane": 1})
    t = time.time()
    runner.tick()
    assert time.time() - t < 1.0  # the tick never waits on the call
    assert running(store, "p") == ["l", "n"]
    st = tick_until(runner, store, "p", lambda s: s["resources"]["cpu"].get("error"))
    assert st["resources"]["cpu"] == {"fn": "p.cap", "capacity": None,
                                      "error": "timed out after 1s"}
    assert statuses(store, "p")["c"] == "pending"
    assert waiting(store, "p", "c") == ["queued: needs cpu 1 (capacity unknown)"]


def test_a_restarted_runner_keeps_the_last_capacity(store, runner, cap_fn):
    project(store, "p", {f"s{i}": gate(needs={"cpu": 1}) for i in range(3)},
            {"cpu": {"capacity_fn": "p.cap"}})
    tick_until(runner, store, "p", lambda s: len(running(store, "p")) == 2)
    cap_fn("fail")
    r2 = Runner(store)
    try:
        r2.tick()
        assert capacity(store, "p") == {"fn": "p.cap", "capacity": 2}
        assert len(running(store, "p")) == 2
    finally:
        for a in r2.active.values():
            a.kill()


def test_dropping_a_capacity_fn_clears_its_value(store, runner, cap_fn):
    project(store, "p", {"s": add(d(1), d(1))}, {"cpu": {"capacity_fn": "p.cap"}})
    tick_until(runner, store, "p", lambda s: (s.get("resources") or {}).get(
        "cpu", {}).get("capacity") == 2)
    store.update_project("p", resources={"cpu": 4}, author="test")
    runner.tick()
    assert "resources" not in store.read_state("p")
    assert store.status("p")["resources"] == {"cpu": {"capacity": 4, "held": 0, "queued": 0}}


# ---- configuration and validation --------------------------------------------------------------


def test_resources_are_set_and_removed_through_the_project(store):
    store.create_project("p", "", "test", resources={"lane": 56, "codex": {"capacity": 12}})
    assert store.project("p")["resources"] == {"lane": {"capacity": 56},
                                               "codex": {"capacity": 12}}
    store.update_project("p", resources={"codex": 8, "gpu": {"capacity": 0}}, author="me",
                         reason="fewer seats")
    store.update_project("p", resources={"lane": None}, author="me")
    assert store.resources("p") == {"codex": {"capacity": 8}, "gpu": {"capacity": 0}}
    recs = records(store, "p", "project.update")
    assert [(r["fields"], r.get("reason")) for r in recs] == [(["resources"], "fewer seats"),
                                                             (["resources"], None)]
    store.update_project("p", resources={"codex": 8}, author="me")  # no change, no record
    assert len(records(store, "p", "project.update")) == 2
    assert [p["resources"] for p in store.projects()] == [store.resources("p")]


@pytest.mark.parametrize(("resources", "message"), [
    ([1], "resources: expected an object of resource name -> capacity"),
    ({"Lane": 1}, "resources.Lane: resource names match"),
    ({"lane": -1}, "resources.lane: expected an integer >= 0"),
    ({"lane": True}, "resources.lane: expected an integer >= 0"),
    ({"lane": {"capacity": 1, "capacity_fn": "x"}}, "resources.lane: expected"),
    ({"lane": None}, "resources.lane: expected"),
    ({"cpu": {"capacity_fn": "nope"}},
     "resources.cpu.capacity_fn: the project sees no fn 'nope'"),
    ({"cpu": {"capacity_fn": "test.add"}},
     "resources.cpu.capacity_fn: fn test.add has no int output `capacity`"),
])
def test_bad_resources_are_refused(store, resources, message):
    with pytest.raises(BadRequest, match=message.replace("(", r"\(").replace(")", r"\)")):
        store.create_project("p", "", "test", resources=resources)


def test_a_needed_resource_cannot_be_removed(store):
    project(store, "p", {"a": gate(needs={"lane": 1})}, {"lane": 1})
    with pytest.raises(BadRequest, match="resources.lane: steps a need it"):
        store.update_project("p", resources={"lane": None})
    store.update_project("p", resources={"lane": 0})  # a lower capacity is fine: it waits


def test_plan_edits_refuse_unknown_resources_and_requests_over_capacity(store):
    project(store, "p", {}, {"lane": 2})
    with pytest.raises(InvalidPlan) as e:
        store.add_step("p", "a", gate(needs={"lane": 3, "gpu": 1}), "test", "")
    assert e.value.errors == [
        "steps.a.needs.lane: asks for 3, more than lane's capacity 2",
        ("steps.a.needs.gpu: the project declares no resource gpu (its resources: lane; "
         "project_update sets them)")]
    for bad, err in (({"needs": {"lane": -1}},
                      "steps.b.needs: expected an object of resource -> amount"),
                     ({"needs": {"lane": "1"}}, "steps.b.needs: expected"),
                     ({"needs": [1]}, "steps.b.needs: expected"),
                     ({"priority": 1.5}, "steps.b.priority: expected an integer"),
                     ({"priority": True}, "steps.b.priority: expected an integer")):
        with pytest.raises(InvalidPlan) as e:
            store.add_step("p", "b", gate(**bad), "test", "")
        assert any(x.startswith(err) for x in e.value.errors), e.value.errors
    store.add_step("p", "c", gate(needs={"lane": 2}, priority=-3), "test", "")
    # a capacity lowered later is not the plan's fault: other edits still go through
    store.update_project("p", resources={"lane": 1})
    store.add_step("p", "d", gate(), "test", "")
    store.update_step("p", "c", {"doc": "big"}, "test", "")
    with pytest.raises(InvalidPlan) as e:
        store.update_step("p", "c", {"needs": {"lane": 2, "x": 0}}, "test", "")
    assert e.value.errors == [
        "steps.c.needs.lane: asks for 2, more than lane's capacity 1",
        ("steps.c.needs.x: the project declares no resource x (its resources: lane; "
         "project_update sets them)")]


def test_a_recipe_sets_needs_and_priority_from_params(store):
    store.create_project("p", "", "test", resources={"lane": 4})
    write_recipe(store.project_dir("p") / "recipes", "job",
                 {"{unit}-work": gate(needs={"{pool}": "{n}"}, priority="{prio}")},
                 {"pool": "string", "n": "int", "prio": "int"})
    store.unit_add("p", "job", {"unit": "u1", "pool": "lane", "n": 2, "prio": 7})
    step = store.get("p")["steps"]["u1-work"]
    assert (step["needs"], step["priority"]) == ({"lane": 2}, 7)
    with pytest.raises(InvalidPlan) as e:
        store.unit_add("p", "job", {"unit": "u2", "pool": "lane", "n": 5, "prio": 0})
    assert e.value.errors == [
        "steps.u2-work.needs.lane: asks for 5, more than lane's capacity 4"]


def test_the_mcp_tools_take_resources(store):
    import anyio

    from sluice.mcp_server import build_server

    server = build_server(store, author="cli")

    def call(name, args):
        res = anyio.run(server.call_tool, name, args)
        return res.is_error, json.loads(res.content[0].text)

    assert call("project_create", {"name": "p", "resources": {"lane": 56}}) == \
        (False, {"name": "p"})
    assert call("project_update", {"name": "p", "resources": {"codex": 12}}) == \
        (False, {"name": "p"})
    assert store.resources("p") == {"lane": {"capacity": 56}, "codex": {"capacity": 12}}
    err, body = call("project_update", {"name": "p", "resources": {"codex": "x"}})
    assert err and "resources.codex: expected" in body["message"]
    err, body = call("status", {"project": "p"})
    assert body["resources"] == {"lane": {"capacity": 56, "held": 0, "queued": 0},
                                 "codex": {"capacity": 12, "held": 0, "queued": 0}}


def test_verify_checks_the_resources(store, cap_fn):
    from sluice.verify import verify

    project(store, "p", {"a": gate(needs={"lane": 2})},
            {"lane": 2, "cpu": {"capacity_fn": "p.cap"}})
    assert verify(store, "p") == {"ok": True, "problems": []}
    store.update_project("p", resources={"lane": 1})
    (store.project_dir("p") / "fns" / "p.cap" / "fn.json").unlink()
    out = verify(store, "p")
    assert out["problems"] == [{"where": "project p: project#resources.cpu.capacity_fn",
                                "message": "the project sees no fn 'p.cap'"}]
    assert out["warnings"] == [{"where": "project p: plan#steps.a.needs.lane",
                                "message": "asks for 2, more than lane's capacity 1; it waits "
                                           "until that changes"}]
