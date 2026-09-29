"""Rewiring and arcs in one call (SPEC §5, §8): edge_add/edge_remove edit a step's `after`
at the current rev, and units carry extra tags (unit_add `tags`, unit_tag)."""

import threading

import pytest

from sluice.errors import BadRequest, InvalidPlan, NotFound
from sluice.store import Store
from tests.conftest import add, create, d
from tests.test_recipes import write_recipe


def last_edit(store, project):
    return [h for h in store.history(project) if h["kind"] == "plan.edit"][-1]


def test_edge_add_and_remove_edit_after_at_the_current_rev(store):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(d(1), d(1)),
                        "c": add(d(1), d(1), after=["a"])})
    assert store.edges("p", "c", "b", author="orch", reason="order") == \
        {"rev": 3, "after": ["a", "b"]}
    edit = last_edit(store, "p")
    assert (edit["rev"], edit["author"], edit["reason"]) == (3, "orch", "order")
    assert edit["ops"] == [{"op": "replace", "path": "/steps/c/after", "value": ["a", "b"]}]
    # edges already there, or a repeat: no edit
    assert store.edges("p", "c", ["b", "a", "b"]) == {"rev": 3, "after": ["a", "b"]}
    assert store.edges("p", "b", ["a", "a"]) == {"rev": 4, "after": ["a"]}
    assert last_edit(store, "p")["ops"] == [
        {"op": "add", "path": "/steps/b/after", "value": ["a"]}]
    assert last_edit(store, "p")["reason"] == "b runs after a"
    assert store.edges("p", "c", ["a"], add=False) == {"rev": 5, "after": ["b"]}
    assert store.edges("p", "c", "b", add=False) == {"rev": 6, "after": []}
    assert last_edit(store, "p")["ops"] == [{"op": "remove", "path": "/steps/c/after"}]
    assert "after" not in store.get("p")["steps"]["c"]
    assert store.edges("p", "c", "b", add=False) == {"rev": 6, "after": []}

    with pytest.raises(InvalidPlan) as e:  # unknown steps, named
        store.edges("p", "zz", ["a", "yy"])
    assert e.value.errors == ["steps.zz: no such step", "steps.yy: no such step"]
    with pytest.raises(InvalidPlan) as e:  # b runs after a already
        store.edges("p", "a", "b")
    assert e.value.errors == ["steps.a: dependency cycle a -> b -> a"]
    with pytest.raises(BadRequest):
        store.edges("p", "c", [])
    assert store.get("p")["rev"] == 6


def test_concurrent_edge_adds_keep_both_edges(home):
    main = Store(home)
    steps = {f"s{i}": add(d(1), d(1)) for i in range(8)}
    create(main, "p", {**steps, "t": add(d(1), d(1))})
    ready = threading.Barrier(8)
    errors = []

    def one(i):
        try:
            ready.wait()
            Store(home).edges("p", "t", f"s{i}", author=f"w{i}")
        except Exception as e:  # noqa: BLE001 - reported below
            errors.append(e)

    threads = [threading.Thread(target=one, args=(i,)) for i in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert errors == []
    assert sorted(main.get("p")["steps"]["t"]["after"]) == sorted(steps)
    assert main.get("p")["rev"] == 2 + 8


PAIR = {"{unit}-a": {"run": "test.add", "in": {"a": {"default": 1}, "b": {"default": 1}},
                     "tags": ["heavy"]},
        "{unit}-b": {"run": "test.add", "in": {"a": {"source": "{unit}-a/sum"},
                                               "b": {"default": 1}}}}


def test_unit_tags_select_an_arc_everywhere(store):
    create(store, "p", {})
    write_recipe(store.home / "recipes", "pair", PAIR)
    store.unit_add("p", "pair", {"unit": "one"}, tags=["arc:tsvm", "heavy"])
    store.unit_add("p", "pair", {"unit": "two"}, tags="arc:tsvm")
    store.unit_add("p", "pair", {"unit": "three"})
    steps = store.get("p")["steps"]
    assert steps["one-a"]["tags"] == ["unit:one", "heavy", "arc:tsvm"]
    assert steps["one-b"]["tags"] == ["unit:one", "arc:tsvm", "heavy"]
    arc = ["one-a", "one-b", "two-a", "two-b"]
    assert [s["id"] for s in store.status("p", tags=["arc:tsvm"])["steps"]] == arc
    store.pause_steps("p", steps="three-a", paused=False)
    assert store.pause_steps("p", tags=["arc:tsvm"], paused=False)["steps"] == arc
    steps = store.get("p")["steps"]
    assert [s for s in steps if "paused" not in steps[s]] == [*arc, "three-a"]
    assert store.pause_steps("p", tags="arc:tsvm", reason="hold the arc")["steps"] == arc
    assert {s: x.get("paused") for s, x in store.get("p")["steps"].items()} == {
        **dict.fromkeys(arc, "hold the arc"), "three-a": None, "three-b": True}

    rev = store.get("p")["rev"]
    with pytest.raises(InvalidPlan) as e:  # unit: tags are the store's
        store.unit_add("p", "pair", {"unit": "four"}, tags=["unit:other"])
    assert e.value.errors == ["tags: unit:other is reserved (a unit's steps carry unit:<unit>)"]
    assert store.get("p")["rev"] == rev


def test_unit_tag_retags_a_unit_in_one_edit(store):
    create(store, "p", {})
    write_recipe(store.home / "recipes", "pair", PAIR)
    store.unit_add("p", "pair", {"unit": "one"})
    rev = store.get("p")["rev"]
    with store.tx():  # a running step takes a tag change too
        store.write_state("p", {"inputs": {}, "steps": {"one-a": {"status": "running"}}})
    out = store.unit_tag("p", "one", add=["arc:tsvm"], remove=["heavy"], author="orch")
    assert out == {"rev": rev + 1, "steps": ["one-a", "one-b"]}
    assert last_edit(store, "p")["ops"] == [
        {"op": "replace", "path": "/steps/one-a/tags", "value": ["unit:one", "arc:tsvm"]},
        {"op": "replace", "path": "/steps/one-b/tags", "value": ["unit:one", "arc:tsvm"]}]
    assert last_edit(store, "p")["reason"] == "tag unit one +arc:tsvm -heavy"
    assert store.unit_tag("p", "one", add="arc:tsvm", remove="heavy") == out  # no edit
    assert store.select_steps("p", tags="arc:tsvm") == ["one-a", "one-b"]
    with pytest.raises(InvalidPlan) as e:
        store.unit_tag("p", "one", add=["unit:two"], remove=["unit:one"])
    assert e.value.errors == [
        "add: unit:two is reserved (a unit's steps carry unit:<unit>)",
        "remove: unit:one is reserved (a unit's steps carry unit:<unit>)"]
    with pytest.raises(InvalidPlan):
        store.unit_tag("p", "one", add=["x"], remove=["x"])
    with pytest.raises(NotFound):
        store.unit_tag("p", "nope", add=["x"])
    with pytest.raises(InvalidPlan) as e:  # the edit validates the tags
        store.unit_tag("p", "one", add=["Not A Tag"])
    assert e.value.errors[0].startswith("steps.one-a.tags: expected an array of tags")
    assert store.get("p")["rev"] == rev + 1


def test_plan_prune_by_tag_takes_only_that_arcs_done_units(store):
    create(store, "p", {})
    write_recipe(store.home / "recipes", "pair", PAIR)
    store.unit_add("p", "pair", {"unit": "one"}, tags=["arc:tsvm"])
    store.unit_add("p", "pair", {"unit": "two"})
    done = {"status": "succeeded", "finished": "2026-01-01T00:00:00Z", "outputs": {"sum": 2}}
    with store.tx():
        store.write_state("p", {"inputs": {}, "steps": dict.fromkeys(
            ["one-a", "one-b", "two-a", "two-b"], done)})
    out = store.prune("p", tags=["arc:tsvm"])
    assert (out["units"], out["steps"]) == (1, ["one-a", "one-b"])
    assert list(store.get("p")["steps"]) == ["two-a", "two-b"]
