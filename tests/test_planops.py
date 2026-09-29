"""Rewiring in one call (SPEC §5, §8): edge_add/edge_remove edit a step's `after` at the
current rev, without a rev round trip and without dropping anyone's edges."""

import threading

import pytest

from sluice.errors import BadRequest, InvalidPlan
from sluice.store import Store
from tests.conftest import add, create, d


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
