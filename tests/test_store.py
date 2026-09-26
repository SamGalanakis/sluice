import copy
import json
import multiprocessing
import threading

import jsonpatch
import pytest

from sluice.errors import BadRequest, Conflict, InvalidPlan, NotFound
from sluice.store import Store
from tests.conftest import create


def add(a, b=1):
    return {"run": "test.add", "in": {"a": {"default": a}, "b": {"default": b}}}


def replay(store, pid):
    """Rebuild the plan from its log alone (manual-value entries carry no ops)."""
    doc = None
    for e in store.history(pid):
        if e["rev"] == 1 and "ops" in e:
            doc = copy.deepcopy(e["ops"][0]["value"])
        elif "ops" in e:
            doc = jsonpatch.apply_patch(doc, e["ops"])
    return doc


def test_create_writes_snapshot_and_log(store):
    assert create(store, "p", {"a": add(1)}) == 1
    snap = json.loads((store.plan_dir("p") / "plan.json").read_text())
    assert snap == {"id": "p", "label": "p", "steps": {"a": add(1)}, "rev": 1}
    [entry] = store.history("p")
    assert (entry["rev"], entry["author"], entry["reason"]) == (1, "test", "test")
    assert entry["ops"] == [{"op": "add", "path": "", "value": replay(store, "p")}]
    with pytest.raises(BadRequest, match="already exists"):
        create(store, "p", {})


def test_create_rejects_an_invalid_plan_without_writing(store):
    with pytest.raises(InvalidPlan) as e:
        create(store, "p", {"a": {"run": "no.such"}})
    assert e.value.errors == ["steps.a.run: unknown fn 'no.such'"]
    assert store.plan_ids() == []


def test_patch_is_compare_and_swap(store):
    create(store, "p", {"a": add(1)})
    assert store.patch("p", 1, [{"op": "add", "path": "/steps/b", "value": add(2)}], "me",
                       "more") == 2
    with pytest.raises(Conflict) as e:
        store.patch("p", 1, [{"op": "remove", "path": "/steps/b"}], "me", "stale")
    assert e.value.payload() == {"error": "conflict", "message": "plan is at rev 2",
                                 "current_rev": 2}
    assert "b" in store.get("p")["steps"]


def test_patch_rejects_invalid_results_and_bad_ops(store):
    create(store, "p", {"a": add(1)})
    with pytest.raises(InvalidPlan) as e:
        store.patch("p", 1, [{"op": "replace", "path": "/steps/a/in/a/default", "value": "x"}],
                    "me", "bad type")
    assert e.value.errors == ['steps.a.in.a: expected int, got "x"']
    with pytest.raises(InvalidPlan) as e:
        store.patch("p", 1, [{"op": "remove", "path": "/steps/zz"}], "me", "bad op")
    assert e.value.errors[0].startswith("ops[0]:")
    with pytest.raises(InvalidPlan) as e:
        store.patch("p", 1, [{"op": "replace", "path": "/id", "value": "q"}], "me", "id")
    assert e.value.errors == ["id: the plan id cannot change"]
    assert store.get("p")["rev"] == 1 and len(store.history("p")) == 1
    with pytest.raises(NotFound):
        store.patch("nope", 1, [], "me", "x")


def test_running_steps_cannot_be_removed_or_changed(store):
    create(store, "p", {"a": add(1), "b": add(2)})
    with store.lock("p"):
        store.write_state("p", {"inputs": {}, "steps": {"a": {"status": "running"}}})
    with pytest.raises(InvalidPlan) as e:
        store.patch("p", 1, [{"op": "remove", "path": "/steps/a"}], "me", "drop")
    assert e.value.errors == ["steps.a: cannot remove a running step"]
    with pytest.raises(InvalidPlan) as e:
        store.patch("p", 1, [{"op": "replace", "path": "/steps/a/in/b/default", "value": 3}],
                    "me", "change")
    assert e.value.errors == ["steps.a: cannot change a running step"]
    assert store.patch("p", 1, [{"op": "remove", "path": "/steps/b"}], "me", "ok") == 2


def test_the_log_replays_to_the_snapshot(store):
    create(store, "p", {"a": add(1)}, inputs={"n": "int"})
    store.patch("p", 1, [{"op": "add", "path": "/steps/b", "value": add(2)}], "me", "b")
    store.set_input("p", "n", 4, "me", "a manual value between edits")
    store.patch("p", 2, [{"op": "replace", "path": "/label", "value": "renamed"},
                         {"op": "remove", "path": "/steps/a"}], "me", "c")
    assert replay(store, "p") == {k: v for k, v in store.get("p").items() if k != "rev"}
    assert [e["rev"] for e in store.history("p", since_rev=1)] == [2, 2, 3]


def _patcher(home: str, n: int, tag: str) -> None:
    s = Store(home)
    for i in range(n):
        while True:
            try:
                s.patch("p", s.get("p")["rev"],
                        [{"op": "add", "path": f"/steps/{tag}-{i}", "value": add(i)}], tag, "go")
                break
            except Conflict:
                pass


def _check_consistent(store, expected: int) -> None:
    doc = store.get("p")
    assert len(doc["steps"]) == expected
    assert [h["rev"] for h in store.history("p")] == list(range(1, doc["rev"] + 1))
    assert replay(store, "p") == {k: v for k, v in doc.items() if k != "rev"}


def test_concurrent_patchers_in_threads(store, home):
    create(store, "p", {})
    threads = [threading.Thread(target=_patcher, args=(str(home), 10, f"t{k}")) for k in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    _check_consistent(store, 40)


def test_concurrent_patchers_in_processes(store, home):
    create(store, "p", {})
    with multiprocessing.get_context("spawn").Pool(4) as pool:
        pool.starmap(_patcher, [(str(home), 8, f"p{k}") for k in range(4)])
    _check_consistent(store, 32)


def test_set_input_is_typed_logged_and_frozen_once_read(store):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": {"source": "n"}, "b": {"default": 1}}}},
           inputs={"n": "int", "unused": "string?"})
    with pytest.raises(InvalidPlan) as e:
        store.set_input("p", "n", "x", "me", "wrong")
    assert e.value.errors == ['inputs.n: expected int, got "x"']
    with pytest.raises(NotFound):
        store.set_input("p", "zz", 1, "me", "unknown")
    store.set_input("p", "n", 1, "me", "first")
    store.set_input("p", "n", 2, "me", "not read yet, so it may change")
    assert store.read_state("p")["inputs"] == {"n": 2}
    last = store.history("p")[-1]
    assert (last["action"], last["input"], last["value"], last["author"]) == (
        "plan_set_input", "n", 2, "me")
    assert "ops" not in last
    with store.lock("p"):
        st = store.read_state("p")
        st["steps"]["a"] = {"status": "running"}
        store.write_state("p", st)
    with pytest.raises(BadRequest, match="input n was already read by step a"):
        store.set_input("p", "n", 3, "me", "too late")
    store.set_input("p", "n", 2, "me", "the same value is fine")


def test_step_set_input_is_an_edit(store):
    create(store, "p", {"a": add(1)})
    assert store.set_step_input("p", "a", "b", 7, "me", "by hand") == 2
    assert store.get("p")["steps"]["a"]["in"]["b"] == {"default": 7}
    assert store.history("p")[-1]["ops"] == [
        {"op": "add", "path": "/steps/a/in/b", "value": {"default": 7}}]
    with pytest.raises(Conflict):
        store.set_step_input("p", "a", "b", 8, "me", "stale", rev=1)
    with pytest.raises(InvalidPlan):
        store.set_step_input("p", "a", "b", "x", "me", "bad type")


def test_set_output_and_retry(store):
    create(store, "p", {"a": add(1),
                        "t": {"run": "test.window", "scatter": "tag",
                              "in": {"seconds": {"default": 0}, "tag": {"default": [1, 2]}}}})
    with pytest.raises(InvalidPlan) as e:
        store.set_output("p", "a", {"sum": "x", "extra": 1}, "me", "bad")
    assert e.value.errors == ['outputs.sum: expected int, got "x"',
                              "outputs.extra: fn test.add has no output extra"]
    with pytest.raises(InvalidPlan):  # a scattered step's outputs are arrays
        store.set_output("p", "t", {"start": 1.0, "end": 2.0, "tag": 1}, "me", "bad")
    store.set_output("p", "t", {"start": [1.0], "end": [2.0], "tag": [1]}, "me", "ok")
    store.set_output("p", "a", {"sum": 5}, "me", "done by hand")
    e = store.read_state("p")["steps"]["a"]
    assert (e["status"], e["outputs"], e["manual"]) == ("succeeded", {"sum": 5}, True)
    assert store.history("p")[-1]["action"] == "step_set_output"
    store.retry("p", "a", "me", "run it for real")
    assert store.read_state("p")["steps"]["a"] == {"status": "pending"}
    assert store.history("p")[-1]["action"] == "step_retry"
    with pytest.raises(BadRequest, match="step a is pending"):
        store.retry("p", "a", "me", "again")
