"""Pausing by default, by id, tag and subtree with a reason; ordering edges (`after`); tags;
cancelling a running step."""

import json

import pytest

from sluice import log as L
from sluice import views
from sluice.errors import BadRequest, InvalidPlan
from tests.conftest import add, create, d, echo, settle, src, statuses, window


def status_of(store, project):
    return {s["id"]: s for s in store.status(project)["steps"]}


def test_steps_added_through_the_tools_come_in_paused_unless_started(store, runner):
    store.create_project("p", "", "t", "t")
    store.patch("p", 1, [{"op": "add", "path": "/steps/a", "value": add(d(1), d(2))},
                         {"op": "add", "path": "/steps/b",
                          "value": add(d(1), d(1), paused=False)}], "t", "draft", start=False)
    store.add_step("p", "c", add(d(2), d(2)), "t", "")
    store.add_step("p", "e", add(d(3), d(3)), "t", "", start=True)
    steps = store.get("p")["steps"]
    assert steps["a"]["paused"] is True and steps["c"]["paused"] is True
    assert steps["b"]["paused"] is False and "paused" not in steps["e"]
    [edit] = [h for h in store.history("p") if h["reason"] == "draft"]
    assert {"op": "add", "path": "/steps/a/paused", "value": True} in edit["ops"]
    settle(runner, store, "p", until=lambda s: s["e"]["status"] == "succeeded"
           and s["b"]["status"] == "succeeded")
    assert statuses(store, "p")["a"] == "pending"
    assert status_of(store, "p")["a"]["waiting"] == ["paused"]


def test_pause_selects_by_id_tag_and_subtree_and_keeps_a_reason(store):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(src("a/sum"), d(1)),
                        "c": add(src("b/sum"), d(1)), "e": add(d(1), d(1), after=["a"]),
                        "h": add(d(1), d(1), tags=["heavy", "e2e"]), "x": add(d(1), d(1))})
    out = store.pause_steps("p", ["a"], subtree=True, author="t", reason="host busy")
    assert out["steps"] == ["a", "b", "c", "e"]
    assert store.pause_steps("p", tags=["e2e"], author="t")["steps"] == ["h"]
    st = status_of(store, "p")
    assert st["a"]["paused"] == "host busy" and st["h"]["paused"] is True
    assert st["b"]["waiting"] == ["paused: host busy", "step a is pending"]
    assert "paused" not in st["x"] and st["x"]["waiting"] == []
    store.pause_steps("p", ["b"], author="t")  # already paused: its reason stays
    assert store.get("p")["steps"]["b"]["paused"] == "host busy"
    out = store.pause_steps("p", ["b"], subtree=True, paused=False, author="t")
    assert out["steps"] == ["b", "c"]
    steps = store.get("p")["steps"]
    assert "paused" not in steps["b"] and "paused" not in steps["c"]
    assert steps["a"]["paused"] == "host busy"
    with pytest.raises(BadRequest, match="select steps"):
        store.pause_steps("p", author="t")
    with pytest.raises(Exception, match="no step nope"):
        store.pause_steps("p", ["nope"], author="t")


def test_a_paused_subtree_holds_steps_that_become_ready_later(store, runner):
    create(store, "p", {"a": window(0.5), "b": {"run": "core.echo",
                                                "in": {"value": src("a/end")}}})
    settle(runner, store, "p", until=lambda s: s["a"]["status"] == "running")
    store.pause_steps("p", ["a"], subtree=True, author="t", reason="wait")
    settle(runner, store, "p", until=lambda s: s["a"]["status"] == "succeeded")
    for _ in range(3):
        runner.tick()
    assert statuses(store, "p")["b"] == "pending"
    store.pause_steps("p", ["b"], paused=False, author="t")
    assert settle(runner, store, "p", until=lambda s: s["b"]["status"] == "succeeded")


def test_after_orders_steps_without_data_or_staleness(store, runner):
    create(store, "p", {"a": window(0.4), "b": add(src("n"), d(1), after=["a"])},
           inputs={"n": "int"})
    store.set_input("p", "n", 1, "t", "t")
    settle(runner, store, "p", until=lambda s: s["a"]["status"] == "running")
    runner.tick()
    assert statuses(store, "p")["b"] == "pending"
    assert status_of(store, "p")["b"]["waiting"] == ["after step a, which is running"]
    steps = settle(runner, store, "p")
    assert steps["b"]["started"] >= steps["a"]["finished"]
    assert status_of(store, "p")["b"]["after"] == ["a"]
    store.update_step("p", "a", {"in": {"seconds": d(0.3)}}, "t", "")
    runner.tick()  # a turns stale; b read nothing from it and stays
    assert statuses(store, "p") == {"a": "stale", "b": "succeeded"}
    for bad, msg in ((["zz"], "steps.b.after: no step zz"),
                     (["b"], "steps.b.after: a step cannot run after itself")):
        with pytest.raises(InvalidPlan) as e:
            store.update_step("p", "b", {"after": bad}, "t", "")
        assert msg in e.value.errors
    with pytest.raises(InvalidPlan) as e:
        store.update_step("p", "a", {"after": ["b"]}, "t", "")
    assert any("dependency cycle" in x for x in e.value.errors)
    with pytest.raises(InvalidPlan) as e:
        store.update_step("p", "b", {"tags": ["Not A Tag"]}, "t", "")
    assert any("steps.b.tags" in x for x in e.value.errors)


def test_cancel_stops_a_running_step(store, runner):
    create(store, "p", {"w": window(30), "x": add(d(1), d(1))})
    settle(runner, store, "p", until=lambda s: s["w"]["status"] == "running"
           and s["x"]["status"] == "succeeded")
    with pytest.raises(BadRequest, match="only a running step can be cancelled: x is succeeded"):
        store.cancel_steps("p", "x", author="t")
    store.cancel_steps("p", "w", author="t", reason="too slow")
    steps = settle(runner, store, "p", timeout=10)
    assert (steps["w"]["status"], steps["w"]["error"]) == ("failed", "cancelled: too slow")
    assert not runner.active
    [rec] = L.read(store.log_dir("p"), kinds=["step.cancel"])["records"]
    assert (rec["step"], rec["reason"]) == ("w", "too slow")


def test_the_board_draws_after_edges_and_the_drawer_shows_tags_and_reason(store):
    create(store, "p", {"a": add(d(1), d(1)),
                        "b": add(d(1), d(1), after=["a"], tags=["heavy"], paused="host busy")})
    page = views.project_page(store, "p", ver="x")
    edges = json.loads(views.html.unescape(page.split(' edges="', 1)[1].split('"', 1)[0]))
    assert ["s:a", "s:b", "after"] in edges
    assert 'aria-description="paused: host busy"' in page and "is-paused" in page
    detail = views.step_detail(store, "p", "b")
    assert "host busy" in detail and "<dt>After</dt>" in detail
    assert '<span class="tag">heavy</span>' in detail  # its tags, in the meta line
    assert "s0 -.->|after| s1" in views.render(store, "p", "mermaid")


def test_retry_and_cancel_take_a_selection(store, runner):
    create(store, "p", {"w1": window(30, tags=["slow"]), "w2": window(30, tags=["slow"]),
                        "x": add(d(1), d(1))})
    settle(runner, store, "p", until=lambda s: s["w1"]["status"] == "running"
           and s["w2"]["status"] == "running" and s["x"]["status"] == "succeeded")
    assert store.cancel_steps("p", tags="slow", author="t") == ["w1", "w2"]
    steps = settle(runner, store, "p", timeout=10)
    assert steps["w1"]["error"] == steps["w2"]["error"] == "cancelled"
    with pytest.raises(BadRequest, match="step x is succeeded"):
        store.retry("p", ["w1", "x"], author="t")  # refused as a whole
    assert statuses(store, "p")["w1"] == "failed"
    assert store.retry("p", tags=["slow"], author="t", reason="again") == ["w1", "w2"]
    assert statuses(store, "p")["w1"] == "pending"


def test_when_runs_or_skips_a_step_and_skipping_follows_data_not_order(store, runner):
    create(store, "p", {
        "check": {"run": "core.echo", "in": {"value": src("ok")}},
        "land": {**add(d(1), d(1)), "when": "check/value"},
        "close": echo("land/sum"),  # reads land: skipped with it
        "cleanup": {**add(d(2), d(2)), "after": ["land"]},  # ordered only: still runs
        "other": {**add(d(3), d(3)), "when": "ok"},
    }, inputs={"ok": "boolean"})
    store.set_input("p", "ok", False, "t", "t")
    steps = settle(runner, store, "p", until=lambda s: all(
        e["status"] in ("succeeded", "failed", "skipped") for e in s.values()))
    assert {k: e["status"] for k, e in steps.items()} == {
        "check": "succeeded", "land": "skipped", "close": "skipped", "cleanup": "succeeded",
        "other": "skipped"}
    assert steps["land"]["skipped"] == "check/value is false"
    assert steps["close"]["skipped"] == "step land was skipped"
    st = status_of(store, "p")
    assert (st["land"]["when"], st["land"]["skipped"]) == ("check/value", "check/value is false")
    recs = [r for r in L.read(store.log_dir("p"), kinds=["step.status"])["records"]
            if r["to"] == "skipped"]
    assert {r["step"]: r["reason"] for r in recs}["land"] == "check/value is false"
    # the condition changes: skipped steps are decided afresh and run
    store.set_input("p", "ok", True, "t", "t")
    runner.tick()  # other reads ok itself; check read it too, so check is stale
    assert statuses(store, "p")["check"] == "stale"
    store.retry("p", "check", author="t", reason="recheck")
    steps = settle(runner, store, "p", until=lambda s: all(
        e["status"] == "succeeded" for e in s.values()))
    assert steps["close"]["outputs"] == {"value": 2}


def test_when_null_skips_and_a_paused_step_is_held_not_skipped(store, runner):
    create(store, "p", {"a": {**add(d(1), d(1)), "when": "maybe"},
                        "b": {**add(d(1), d(1)), "when": "no", "paused": True}},
           inputs={"maybe": "boolean?", "no": "boolean"})
    store.set_input("p", "no", False, "t", "t")
    for _ in range(3):
        runner.tick()
    assert statuses(store, "p") == {"a": "skipped", "b": "pending"}
    assert store.read_state("p")["steps"]["a"]["skipped"] == "maybe is null"
    store.pause_steps("p", "b", paused=False, author="t")
    runner.tick()
    assert statuses(store, "p")["b"] == "skipped"


def test_the_board_shows_a_skipped_step_and_a_finished_plan(store, runner):
    create(store, "p", {"a": {**add(d(1), d(1)), "when": "go"}, "b": add(d(1), d(1))},
           inputs={"go": "boolean"})
    store.set_input("p", "go", False, "t", "t")
    settle(runner, store, "p", until=lambda s: s["a"]["status"] == "skipped"
           and s["b"]["status"] == "succeeded")
    page = views.project_page(store, "p", ver="x")
    assert "is-skipped" in page and 'class="g g-skipped"' in page
    assert 'aria-description="Skipped: go is false"' in page
    assert "Finished." in views.index(store)
    assert "<dt>When</dt>" in views.step_detail(store, "p", "a")


def test_a_when_of_type_any_is_checked_when_it_runs(store, runner):
    create(store, "p", {"v": {"run": "core.echo", "in": {"value": d(3)}},
                        "a": {**add(d(1), d(1)), "when": "v/value"}})
    steps = settle(runner, store, "p")
    assert (steps["a"]["status"], steps["a"]["error"]) == (
        "failed", "when: v/value is 3, not a boolean")


def test_a_brief_status_cuts_long_strings(store):
    create(store, "p", {"a": add(d(1), d(1))}, inputs={"spec": "string"},
           outputs={"total": {"source": "a/sum"}})
    long = "x" * 450
    store.set_input("p", "spec", long, "test", "")
    with store.lock("p"):
        state = store.read_state("p")
        state["steps"]["a"] = {"status": "succeeded",
                               "outputs": {"sum": 2, "final": long, "notes": ["short", long]}}
        store.write_state("p", state)
    cut = "x" * 200 + "… [250 more characters]"
    brief = store.status("p", brief=True)
    assert brief["inputs"]["spec"] == cut
    assert brief["steps"][0]["outputs"] == {"sum": 2, "final": cut, "notes": ["short", cut]}
    assert store.status("p")["steps"][0]["outputs"]["final"] == long  # whole without brief
