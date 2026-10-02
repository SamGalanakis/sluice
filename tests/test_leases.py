"""Section leases (SPEC §6 "Resources", §7 `ctx.acquire`): a running step's fn holds an amount
of a project resource for part of its work, across processes, granted by the runner in
priority order and let go when the block exits or the run ends."""

import os
import signal
import time

import pytest

from sluice import leases as LS
from sluice import me as M
from sluice import watch
from sluice.runner import Runner
from tests.conftest import d
from tests.test_resources import gate, project, records, release, running, tick_until


def lease(resource="land", amount=None, **extra):
    """A test.lease step: it acquires `amount` (default 1) of `resource` and holds it until
    `go` is in its run dir."""
    ins = {"resource": d(resource), **({"amount": d(amount)} if amount is not None else {})}
    return {"run": "test.lease", "in": ins, **extra}


def run_dir(store, p, sid):
    return store.runs_dir(p) / store.read_state(p)["steps"][sid]["run_ids"][0]


def holding(runner, store, p, sid):
    """Tick until the step's fn holds its lease (its `held` file is there); its run dir."""
    tick_until(runner, store, p, lambda s: s["steps"][sid]["status"] == "running"
               and s["steps"][sid].get("run_ids")
               and (run_dir(store, p, sid) / "held").exists())
    return run_dir(store, p, sid)


def waiting_for(runner, store, p, n, resource="land"):
    """Tick until `n` leases wait for the resource."""
    deadline = time.time() + 30
    while time.time() < deadline:
        runner.tick()
        with store.rx() as conn:
            rows = LS.rows(conn, p)
        if sum(x["granted"] is None and x["resource"] == resource for x in rows) >= n:
            return rows
        time.sleep(0.05)
    raise AssertionError(f"no {n} waiting leases: {rows}")


def holders(store, p, resource="land"):
    return [h["step"] for h in store.status(p)["resources"][resource].get("holders", [])]


def finish(runner, store, p, sid, fail=False):
    d_ = holding(runner, store, p, sid)
    if fail:
        (d_ / "fail").write_text("")
    (d_ / "go").write_text("")
    tick_until(runner, store, p, lambda s: s["steps"][sid]["status"] in ("succeeded", "failed"))


def test_capacity_one_is_mutual_exclusion_across_processes(store, runner):
    project(store, "p", {"a": lease(), "b": lease(), "c": lease()}, {"land": 1})
    order = []
    for _ in range(3):
        st = tick_until(runner, store, "p", lambda s: any(
            e["status"] == "running" and e.get("run_ids")
            and (store.runs_dir("p") / e["run_ids"][0] / "held").exists()
            for e in s["steps"].values()))
        inside = [sid for sid, e in st["steps"].items() if e["status"] == "running"
                  and (store.runs_dir("p") / e["run_ids"][0] / "held").exists()]
        assert len(inside) == 1  # only one fn is inside its section
        [sid] = inside
        assert holders(store, "p") == [sid]
        for _ in range(5):  # the others keep waiting, however often the runner looks
            runner.tick()
            time.sleep(0.05)
        assert holders(store, "p") == [sid]
        order.append(sid)
        finish(runner, store, "p", sid)
    assert sorted(order) == ["a", "b", "c"]
    st = store.read_state("p")["steps"]
    spans = sorted((st[s]["outputs"]["start"], st[s]["outputs"]["end"]) for s in order)
    assert all(spans[i][1] <= spans[i + 1][0] for i in range(2))  # never overlapping
    assert [r["state"] for r in records(store, "p", "step.lease")] == ["held", "released"] * 3


def test_waiters_are_granted_in_priority_order(store, runner):
    project(store, "p", {"first": lease(), "low": lease(priority=1), "top": lease(priority=5),
                         "mid": lease(priority=3)}, {"land": 1})
    # `first` takes the lease before the others exist: they are added once it holds it
    store.pause_steps("p", ["low", "top", "mid"], author="test")
    holding(runner, store, "p", "first")
    store.pause_steps("p", ["low", "top", "mid"], paused=False, author="test")
    waiting_for(runner, store, "p", 3)
    res = store.status("p")["resources"]["land"]
    assert [(w["step"], w["priority"]) for w in res["waiting"]] == \
        [("top", 5), ("mid", 3), ("low", 1)]
    assert res["held"] == 1 and [h["step"] for h in res["holders"]] == ["first"]
    finish(runner, store, "p", "first")
    for sid in ("top", "mid", "low"):
        finish(runner, store, "p", sid)
    held = [r["step"] for r in records(store, "p", "step.lease") if r["state"] == "held"]
    assert held == ["first", "top", "mid", "low"]


def test_ties_are_first_come():
    rows = [{"id": i, "step": s} for i, s in ((3, "a"), (1, "b"), (2, "c"), (4, "d"))]
    prio = {"a": 1, "b": 0, "c": 1, "d": 0}
    assert [x["step"] for x in LS.grant_order(rows, prio.get)] == ["c", "a", "b", "d"]


def test_an_exception_inside_the_block_lets_go(store, runner):
    project(store, "p", {"a": lease(), "b": lease()}, {"land": 1})
    tick_until(runner, store, "p", lambda s: holders(store, "p"))
    [first] = holders(store, "p")
    second = "b" if first == "a" else "a"
    finish(runner, store, "p", first, fail=True)
    assert store.read_state("p")["steps"][first]["status"] == "failed"
    holding(runner, store, "p", second)
    [rel] = [r for r in records(store, "p", "step.lease") if r["state"] == "released"]
    assert rel["step"] == first and "reason" not in rel  # the fn's own release


@pytest.mark.parametrize("how", ["cancel", "sigkill"])
def test_a_run_that_ends_without_letting_go_loses_its_lease(store, runner, how):
    project(store, "p", {"a": lease(), "b": lease()}, {"land": 1})
    tick_until(runner, store, "p", lambda s: holders(store, "p"))
    [first] = holders(store, "p")
    second = "b" if first == "a" else "a"
    holding(runner, store, "p", first)
    if how == "cancel":
        store.cancel_steps("p", [first], author="test", reason="stop")
    else:  # the run's whole process group dies at once: no `finally` runs
        rid = store.read_state("p")["steps"][first]["run_ids"][0]
        a = runner.active[("step", "p", first)]
        assert a.runs[0].rid == rid
        os.killpg(a.runs[0].pid, signal.SIGKILL)
    holding(runner, store, "p", second)
    assert store.read_state("p")["steps"][first]["status"] == "failed"
    [rel] = [r for r in records(store, "p", "step.lease") if r["state"] == "released"]
    assert (rel["step"], rel["reason"]) == (first, "its run ended")


def test_leases_and_needs_share_one_total(store, runner):
    project(store, "p", {"x": gate(needs={"lane": 1}), "y": lease("lane")}, {"lane": 2})
    holding(runner, store, "p", "y")
    assert running(store, "p") == ["x", "y"]
    assert store.status("p")["resources"]["lane"]["held"] == 2
    # a lease counts against a step's needs ...
    store.add_step("p", "z", gate(needs={"lane": 1}), "test", "", start=True)
    runner.tick()
    assert store.read_state("p")["steps"]["z"]["queued"] == ["lane"]
    z = next(r for r in store.status("p")["steps"] if r["id"] == "z")
    assert z["waiting"] == ["queued: needs lane 1 (2/2 held)"]
    release(runner, store, "p", "x")  # x's needs free: z is admitted
    tick_until(runner, store, "p", lambda s: s["steps"]["z"]["status"] == "running")
    # ... and a step's needs against a lease
    store.add_step("p", "w", lease("lane"), "test", "", start=True)
    waiting_for(runner, store, "p", 1, "lane")
    for _ in range(3):
        runner.tick()
    lane = store.status("p")["resources"]["lane"]
    assert (lane["held"], [h["step"] for h in lane["holders"]],
            [w["step"] for w in lane["waiting"]]) == (2, ["y"], ["w"])
    finish(runner, store, "p", "y")  # y's lease frees: w is granted
    holding(runner, store, "p", "w")
    assert store.status("p")["resources"]["lane"]["held"] == 2


def test_status_and_step_context_show_holders_and_waiters(store, runner):
    project(store, "p", {"a": lease(tags=["unit:one"]), "b": lease(tags=["unit:two"])},
            {"land": 1})
    tick_until(runner, store, "p", lambda s: holders(store, "p"))
    [first] = holders(store, "p")
    second = "b" if first == "a" else "a"
    waiting_for(runner, store, "p", 1)
    res = store.status("p")["resources"]["land"]
    assert set(res) == {"capacity", "held", "queued", "holders", "waiting"}
    assert (res["capacity"], res["held"], res["queued"]) == (1, 1, 0)
    [h], [w] = res["holders"], res["waiting"]
    assert (h["step"], h["amount"], w["step"], w["amount"], w["priority"]) == \
        (first, 1, second, 1, 0)
    assert h["run"] == store.read_state("p")["steps"][first]["run_ids"][0] and h["since"]
    rows = {r["id"]: r for r in store.status("p")["steps"]}
    assert rows[first]["leases"] == [{"resource": "land", "amount": 1, "held": True}]
    assert rows[second]["leases"] == [{"resource": "land", "amount": 1, "held": False}]
    assert watch.units(store, "p")["resources"]["land"]["waiting"][0]["step"] == second
    ctx = M.context(store, "p", second)
    assert ctx["leases"] == [{"resource": "land", "amount": 1, "held": False}]
    assert "lease: land 1 (waiting)" in M.render(ctx)


def test_a_bad_lease_raises_at_once(store, runner):
    project(store, "p", {"none": lease("gpu"), "big": lease(amount=2),
                         "zero": lease(amount=0)}, {"land": 1})
    st = tick_until(runner, store, "p", lambda s: all(
        e["status"] in ("failed", "succeeded") or (
            e["status"] == "running" and e.get("run_ids")
            and (store.runs_dir("p") / e["run_ids"][0] / "held").exists())
        for e in s["steps"].values()))
    assert "declares no resource 'gpu' (its resources: land)" in st["steps"]["none"]["error"]
    assert "2 of land is more than its capacity 1" in st["steps"]["big"]["error"]
    assert st["steps"]["zero"]["status"] == "running"  # 0 always fits
    with pytest.raises(RuntimeError, match="only inside a plan step's run"):
        LS.request(store.home, "p", "", "", "land", 1)
    with pytest.raises(RuntimeError, match="is not a running run of step none"):
        LS.request(store.home, "p", "none", "r1", "land", 1)


def test_a_resource_with_leases_cannot_be_removed(store, runner):
    from sluice.errors import BadRequest

    project(store, "p", {"a": lease()}, {"land": 1})
    holding(runner, store, "p", "a")
    with pytest.raises(BadRequest, match="resources.land: step a holds or waits for a lease"):
        store.update_project("p", resources={"land": None})


def test_a_restarted_runner_keeps_granted_leases(store, runner):
    project(store, "p", {"a": lease(), "b": lease()}, {"land": 1})
    tick_until(runner, store, "p", lambda s: holders(store, "p"))
    [first] = holders(store, "p")
    holding(runner, store, "p", first)
    r2 = Runner(store)
    try:
        for _ in range(3):
            r2.tick()
        assert holders(store, "p") == [first]  # adopted: its run still lives, so its lease
        finish(r2, store, "p", first)
        second = "b" if first == "a" else "a"
        holding(r2, store, "p", second)
    finally:
        for a in r2.active.values():
            a.kill()
