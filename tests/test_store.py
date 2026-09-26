import json
import multiprocessing
import threading

import pytest

from sluice.errors import BadRequest, Conflict, InvalidPlan, NotFound
from sluice.store import Store
from tests.conftest import create


def add(a, b=1):
    return {"fn": "test.add", "in": {"a": {"value": a}, "b": {"value": b}}}


def test_create_writes_snapshot_log_and_event(store):
    assert create(store, "p", {"a": add(1)}) == 1
    d = store.plan_dir("p")
    snap = json.loads((d / "plan.json").read_text())
    assert snap["rev"] == 1 and snap["id"] == "p" and snap["nodes"]["a"]["fn"] == "test.add"
    log = [json.loads(x) for x in (d / "plan.log.jsonl").read_text().splitlines()]
    assert log[0]["rev"] == 1 and log[0]["author"] == "test"
    assert log[0]["ops"] == [{"op": "add", "path": "", "value": {k: v for k, v in snap.items()
                                                                 if k != "rev"}}]
    assert [e["type"] for e in store.events("p")] == ["plan_created"]
    with pytest.raises(BadRequest, match="already exists"):
        create(store, "p", {})


def test_create_rejects_invalid_documents_without_writing(store):
    with pytest.raises(InvalidPlan) as e:
        create(store, "p", {"a": {"fn": "nope.fn"}})
    assert e.value.errors == ["nodes.a.fn: unknown fn nope.fn"]
    assert store.plan_ids() == []
    with pytest.raises(InvalidPlan, match="invalid"):
        store.create("p", {"id": "other", "nodes": {}}, "t", "r")


def test_patch_is_compare_and_swap(store):
    create(store, "p", {"a": add(1)})
    assert store.patch("p", 1, [{"op": "add", "path": "/nodes/b", "value": add(2)}], "me",
                       "more") == 2
    with pytest.raises(Conflict) as e:
        store.patch("p", 1, [{"op": "remove", "path": "/nodes/b"}], "me", "stale")
    assert e.value.current_rev == 2
    assert e.value.payload() == {"error": "conflict", "message": "plan is at rev 2",
                                 "current_rev": 2}
    assert "b" in store.get("p")["nodes"]


def test_patch_rejects_invalid_results_and_bad_ops(store):
    create(store, "p", {"a": add(1)})
    with pytest.raises(InvalidPlan) as e:
        store.patch("p", 1, [{"op": "replace", "path": "/nodes/a/in/a/value", "value": "x"}],
                    "me", "bad type")
    assert e.value.errors == ['nodes.a.in.a: expected int, got "x"']
    with pytest.raises(InvalidPlan) as e:
        store.patch("p", 1, [{"op": "remove", "path": "/nodes/zz"}], "me", "bad op")
    assert e.value.errors[0].startswith("ops[0]:")
    with pytest.raises(InvalidPlan, match="invalid") as e:
        store.patch("p", 1, [{"op": "add", "path": "/rev", "value": 9}], "me", "rev")
    assert e.value.errors == ["rev: maintained by the store; it cannot be set or patched"]
    with pytest.raises(InvalidPlan):
        store.patch("p", 1, [{"op": "replace", "path": "/id", "value": "q"}], "me", "id")
    assert store.get("p")["rev"] == 1
    assert len(store.history("p")) == 1
    with pytest.raises(NotFound):
        store.patch("nope", 1, [], "me", "x")


def test_log_replay_equals_snapshot_and_revert(store):
    create(store, "p", {"a": add(1)})
    store.patch("p", 1, [{"op": "add", "path": "/nodes/b", "value": add(2)}], "me", "b")
    store.patch("p", 2, [{"op": "replace", "path": "/title", "value": "renamed"},
                         {"op": "add", "path": "/nodes/c", "value": add(3)}], "me", "c")
    store.patch("p", 3, [{"op": "remove", "path": "/nodes/a"}], "me", "drop a")
    cur = {k: v for k, v in store.get("p").items() if k != "rev"}
    assert store.plan_at("p", 4) == cur
    assert set(store.plan_at("p", 2)["nodes"]) == {"a", "b"}
    assert store.plan_at("p", 1)["title"] == "p"
    assert [h["rev"] for h in store.history("p", since_rev=2)] == [3, 4]
    assert store.revert("p", 4, 2, "me", "undo") == 5
    assert store.plan_at("p", 5) == store.plan_at("p", 2)
    assert store.history("p")[-1]["author"] == "me"
    with pytest.raises(NotFound):
        store.plan_at("p", 9)


def _patcher(home: str, n: int, tag: str) -> int:
    """Add n nodes, retrying on conflict. Returns how many conflicts it saw."""
    s = Store(home)
    conflicts = 0
    for i in range(n):
        while True:
            rev = s.get("p")["rev"]
            try:
                s.patch("p", rev, [{"op": "add", "path": f"/nodes/{tag}-{i}", "value": add(i)}],
                        tag, "concurrent")
                break
            except Conflict:
                conflicts += 1
    return conflicts


def _check_consistent(store, expected_nodes: int) -> None:
    doc = store.get("p")
    assert len(doc["nodes"]) == expected_nodes
    hist = store.history("p")
    assert [h["rev"] for h in hist] == list(range(1, doc["rev"] + 1))
    assert store.plan_at("p", doc["rev"]) == {k: v for k, v in doc.items() if k != "rev"}
    seqs = [e["seq"] for e in store.events("p")]
    assert seqs == list(range(1, len(seqs) + 1))


def test_concurrent_patchers_in_threads(store, home):
    create(store, "p", {})
    threads = [threading.Thread(target=_patcher, args=(str(home), 10, f"t{k}"))
               for k in range(4)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    _check_consistent(store, 40)


def test_concurrent_patchers_in_processes(store, home):
    create(store, "p", {})
    ctx = multiprocessing.get_context("spawn")
    with ctx.Pool(4) as pool:
        pool.starmap(_patcher, [(str(home), 8, f"p{k}") for k in range(4)])
    _check_consistent(store, 32)


def test_state_has_its_own_rev(store):
    create(store, "p", {})
    st = store.read_state("p")
    assert st == {"rev": 0, "plan_rev": 0, "nodes": {}}
    with store.lock("p"):
        assert store.write_state("p", st) == 1
        assert store.write_state("p", st) == 2
    assert store.read_state("p")["rev"] == 2


def test_events_tail(store):
    create(store, "p", {})
    for i in range(5):
        store.append_event("p", "node_started", f"n{i}", {"i": i})
    evs = store.events("p")
    assert [e["seq"] for e in evs] == [1, 2, 3, 4, 5, 6]
    assert [e["seq"] for e in store.events("p", since_seq=3)] == [4, 5, 6]
    assert [e["seq"] for e in store.events("p", since_seq=1, limit=2)] == [2, 3]
    assert [e["seq"] for e in store.events("p", limit=2)] == [5, 6]
    assert evs[1]["node"] == "n0" and evs[1]["data"] == {"i": 0}


def test_inbox_open_list_close(store):
    create(store, "p", {})
    a = store.inbox_open("p", "failure", "n1", {"error": "boom"})
    b = store.inbox_open("p", "ask", "n2", {"question": "?"})
    assert (a["id"], b["id"]) == ("p.0001", "p.0002")
    assert [i["id"] for i in store.inbox_list()] == ["p.0001", "p.0002"]
    store.inbox_close("p.0001", {"action": "ack"}, "me")
    assert [i["id"] for i in store.inbox_list("p")] == ["p.0002"]
    closed = store.inbox_get("p.0001")
    assert closed["status"] == "resolved" and closed["resolved_by"] == "me"
    assert len(store.inbox_list(open_only=False)) == 2
    with pytest.raises(BadRequest, match="already resolved"):
        store.inbox_close("p.0001", {"action": "ack"}, "me")
    with pytest.raises(NotFound):
        store.inbox_get("p.0009")
    assert [e["type"] for e in store.events("p")][-1] == "inbox_resolved"
