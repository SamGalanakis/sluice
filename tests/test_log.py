"""The log (SPEC §6b): one log.jsonl per project (and one in the home), seq-ordered records of
every kind, capped; read through log_read / log_wait."""

import json
import multiprocessing
import threading
import time

import anyio
import pytest
from mcp import Client

from sluice import log as L
from sluice.mcp_server import build_server
from sluice.store import Store
from tests.conftest import add, create, d, settle, write_config


def kinds(records):
    return [r["kind"] for r in records]


def _post(home: str, n: int, tag: str) -> None:
    s = Store(home)
    for i in range(n):
        s.append("p", {"kind": "message", "thread": "t", "from": tag, "body": str(i)})


def test_one_log_holds_every_kind_in_seq_order(store, runner):
    create(store, "p", {"a": add(d(1), d(1)), "boom": {"run": "test.boom", "in": {}}},
           inputs={"n": "int"})
    store.set_input("p", "n", 1, "me", "go")
    settle(runner, store, "p")
    store.set_output("p", "boom", {"done": True}, "me", "by hand")
    store.retry("p", "boom", author="me", reason="again")
    recs = L.read(store.project_dir("p"))["records"]
    assert [r["seq"] for r in recs] == list(range(1, len(recs) + 1))
    assert all(set(r) >= {"seq", "at", "kind"} for r in recs)
    assert set(kinds(recs)) == {"plan.edit", "plan.input", "step.status", "step.output",
                                "step.retry"}
    status = [(r["step"], r["from"], r["to"]) for r in recs if r["kind"] == "step.status"]
    assert ("a", None, "running") in status and ("a", "running", "succeeded") in status
    assert ("boom", "running", "failed") in status
    assert status[-2:] == [("boom", "failed", "succeeded"), ("boom", "succeeded", "pending")]
    failed = next(r for r in recs if r["kind"] == "step.status" and r["to"] == "failed")
    assert "about to explode" in failed["error"] and failed["run_ids"]
    assert not (store.project_dir("p") / "plan.log.jsonl").exists()


def test_concurrent_writers_in_processes_get_distinct_increasing_seqs(store, home):
    store.create_project("p")
    with multiprocessing.get_context("spawn").Pool(4) as pool:
        pool.starmap(_post, [(str(home), 25, f"w{k}") for k in range(4)])
    recs = L.read(store.project_dir("p"))["records"]
    assert [r["seq"] for r in recs] == list(range(1, 102))  # the creation edit, then 100
    for k in range(4):  # each writer's own messages keep their order
        assert [r["body"] for r in recs if r.get("from") == f"w{k}"] == [str(i)
                                                                          for i in range(25)]


def test_read_filters_by_kind_group_thread_since_and_limit(store):
    store.create_project("p")
    for i in range(6):
        store.append("p", {"kind": "message", "thread": "ab"[i % 2], "from": "x", "body": str(i)})
    store.append("p", {"kind": "step.status", "step": "s", "from": None, "to": "pending"})
    ld = store.project_dir("p")
    assert kinds(L.read(ld, kinds=["step"])["records"]) == ["step.status"]
    assert kinds(L.read(ld, kinds=["plan"])["records"]) == ["plan.edit"]
    a = L.read(ld, threads=["a"])  # threads alone: only messages on them
    assert [r["body"] for r in a["records"]] == ["0", "2", "4"] and a["last_seq"] == 8
    both = L.read(ld, kinds=["message", "step.status"], threads=["b"])["records"]
    assert [r.get("body", r["kind"]) for r in both] == ["1", "3", "5", "step.status"]
    tail = L.read(ld, limit=2)  # without since_seq: the last `limit`
    assert [r["seq"] for r in tail["records"]] == [7, 8] and tail["last_seq"] == 8
    page = L.read(ld, since_seq=2, kinds=["message"], limit=2)  # with it: the next `limit`
    assert [r["seq"] for r in page["records"]] == [3, 4] and page["last_seq"] == 4
    rest = L.read(ld, since_seq=page["last_seq"], kinds=["message"], limit=10)
    assert [r["seq"] for r in rest["records"]] == [5, 6, 7] and rest["last_seq"] == 8
    assert L.read(ld, since_seq=8) == {"records": [], "last_seq": 8}
    assert L.check_kinds(["step", "message", "nope"]) == [
        "unknown kind 'nope'; kinds: " + ", ".join((*L.KINDS, *L.GROUPS))]


def test_a_half_written_last_line_is_not_read_yet(store):
    store.create_project("p")
    path = store.project_dir("p") / L.FILE
    with open(path, "a") as f:
        f.write('{"seq": 2, "at": "x", "kind": "message", "thread": "t", "bo')
    assert [r["seq"] for r in L.read(store.project_dir("p"))["records"]] == [1]
    assert L.read(store.project_dir("p"), since_seq=0)["last_seq"] == 1


def test_reading_backwards_across_blocks(store, monkeypatch):
    monkeypatch.setattr(L, "BLOCK", 64)  # records longer than a block
    store.create_project("p")
    for i in range(5):
        store.append("p", {"kind": "message", "thread": "t", "from": "x",
                           "body": "y" * 150 + str(i)})
    ld = store.project_dir("p")
    res = L.read(ld, since_seq=3)
    assert [r["seq"] for r in res["records"]] == [4, 5, 6] and res["last_seq"] == 6
    assert res["records"][-1]["body"].endswith("4")
    assert L.last_record(ld)["seq"] == 6


def test_the_cap_keeps_run_dirs_that_state_still_uses(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=6)
    store = Store(home)
    from sluice.runner import Runner

    runner = Runner(store)
    create(store, "p", {"a": add(d(1), d(1))})
    settle(runner, store, "p")
    [run_id] = store.read_state("p")["steps"]["a"]["run_ids"]
    for i in range(12):
        store.append("p", {"kind": "message", "thread": "t", "from": "x", "body": str(i)})
    recs = L.read(store.project_dir("p"))["records"]
    assert len(recs) <= 6 and "plan.edit" not in kinds(recs)
    assert (store.runs_dir("p") / run_id).is_dir()  # state.json still refers to it
    assert store.history("p") == []  # history only goes back as far as the log


def test_log_read_and_log_wait_tools(live):
    live.create_project("p")

    async def main():
        async with Client(build_server(live)) as c:
            async def tool(name, **args):
                r = await c.call_tool(name, args)
                return r.is_error, json.loads(r.content[0].text)

            err, res = await tool("log_read", project="p")
            assert not err and kinds(res["records"]) == ["plan.edit"] and res["last_seq"] == 1
            err, bad = await tool("log_read", project="p", kinds=["nope"])
            assert err and bad["error"] == "bad_request"
            err, missing = await tool("log_read", project="zz")
            assert err and missing["error"] == "not_found"

            t0 = time.monotonic()
            err, res = await tool("log_wait", project="p", since_seq=1, timeout=1)
            assert not err and res == {"records": [], "last_seq": 1}
            assert 0.9 <= time.monotonic() - t0 < 5

            def later():
                time.sleep(1.0)
                live.append("p", {"kind": "step.status", "step": "x", "from": None,
                                  "to": "pending"})
                time.sleep(0.5)
                live.append("p", {"kind": "message", "thread": "q", "from": "h", "body": "hi"})

            threading.Thread(target=later, daemon=True).start()
            waited = {}

            async def wait():
                waited["res"] = (await tool("log_wait", project="p", since_seq=1,
                                            threads=["q"], timeout=20))[1]
                waited["at"] = time.monotonic()

            async with anyio.create_task_group() as tg:
                tg.start_soon(wait)
                await anyio.sleep(0.2)
                t1 = time.monotonic()
                err, other = await tool("projects_list")  # the server is not blocked
                assert not err and other[0]["name"] == "p" and time.monotonic() - t1 < 1
                assert "res" not in waited
            assert [r["body"] for r in waited["res"]["records"]] == ["hi"]
            assert waited["res"]["last_seq"] == 3

            err, home = await tool("log_read")  # the home log: calls without a project
            assert not err and home == {"records": [], "last_seq": 0}

    anyio.run(main)


def test_log_wait_on_questions_is_not_woken_by_a_note(live):
    live.create_project("p")
    live.append("p", {"kind": "message", "thread": "q", "from": "w", "body": "fyi",
                      "needs_reply": False})

    async def main():
        async with Client(build_server(live)) as c:
            async def wait(**args):
                r = await c.call_tool("log_wait", {"project": "p", "since_seq": 1,
                                                   "threads": ["q"], **args})
                return json.loads(r.content[0].text)

            res = await wait(timeout=5)  # by default a note wakes it
            assert [r["body"] for r in res["records"]] == ["fyi"]
            t0 = time.monotonic()
            res = await wait(timeout=1, wake="questions")
            assert time.monotonic() - t0 >= 0.9
            assert [r["body"] for r in res["records"]] == ["fyi"]  # returned at the timeout

            def later():
                time.sleep(0.8)
                live.append("p", {"kind": "message", "thread": "q", "from": "w",
                                  "body": "which db?", "needs_reply": True})

            threading.Thread(target=later, daemon=True).start()
            t0 = time.monotonic()
            res = await wait(timeout=20, wake="questions")
            assert time.monotonic() - t0 < 10
            assert [r["body"] for r in res["records"]] == ["fyi", "which db?"]

    anyio.run(main)


def test_log_wait_sees_an_append_from_another_process(live, home):
    live.create_project("p")

    async def main():
        async with Client(build_server(live)) as c:
            proc = multiprocessing.get_context("spawn").Process(target=_post,
                                                                 args=(str(home), 1, "other"))
            t0 = time.monotonic()
            proc.start()
            r = await c.call_tool("log_wait", {"project": "p", "since_seq": 1,
                                               "kinds": ["message"], "timeout": 30})
            res = json.loads(r.content[0].text)
            proc.join()
            assert [m["from"] for m in res["records"]] == ["other"]
            assert time.monotonic() - t0 < 25

    anyio.run(main)


def test_wait_accumulates_and_holds_notes_until_a_waking_record(store):
    store.create_project("p")
    ld = store.project_dir("p")
    note = {"kind": "message", "thread": "t", "from": "w", "body": "fyi",
            "needs_reply": False}
    store.append("p", note)
    t0 = time.monotonic()
    res = L.wait(ld, 1, ["message"], wake="questions", timeout=0.2, interval=0.02)
    assert time.monotonic() - t0 >= 0.19  # a note alone does not wake it
    assert res["records"] == [] and res["last_seq"] == 2
    assert [r["body"] for r in res["held"]] == ["fyi"]  # it comes back at the timeout

    def later():
        time.sleep(0.1)
        store.append("p", {"kind": "message", "thread": "t", "from": "w",
                           "body": "which db?", "needs_reply": True},
                     {**note, "body": "meanwhile"})

    threading.Thread(target=later, daemon=True).start()
    res = L.wait(ld, 1, ["message"], wake="questions", timeout=10, interval=0.02)
    assert [r["body"] for r in res["records"]] == ["fyi", "which db?"]
    assert [r["body"] for r in res["held"]] == ["meanwhile"]  # after the waking record
    assert res["last_seq"] == 4
    res = L.wait(ld, res["last_seq"], ["message"], timeout=0.1, interval=0.02)
    assert res == {"records": [], "held": [], "last_seq": 4}
    res = L.wait(ld, 1, ["message"], timeout=5, interval=0.02, limit=2)
    assert [r["body"] for r in res["records"]] == ["fyi", "which db?"]
    assert res["held"] == [] and res["last_seq"] == 3  # the limit ends the wait early


@pytest.fixture
def live(store):
    from sluice.runner import Runner

    runner = Runner(store, kill_runs=True)  # leave no runs running past teardown
    store.listeners.append(runner.wake)
    t = threading.Thread(target=runner.run_forever, kwargs={"interval": 0.2}, daemon=True)
    t.start()
    yield store
    runner.stop()
    t.join(10)


def test_page_pages_newest_first_by_seq_with_the_filter(store):
    ld = store.home
    for i in range(1, 13):  # seqs 1..12: messages on a (odd) and b (even), a step change at 12
        rec = ({"kind": "step.status", "step": "s", "from": None, "to": "pending"} if i == 12
               else {"kind": "message", "thread": "a" if i % 2 else "b", "from": "t",
                     "body": str(i)})
        store.append(None, rec)

    def seqs(res):
        return [r["seq"] for r in res["records"]]

    newest = L.page(ld, size=5)
    assert seqs(newest) == [12, 11, 10, 9, 8] and not newest["newer"] and newest["older"]
    assert newest["last_seq"] == 12
    older = L.page(ld, before=8, size=5)
    assert seqs(older) == [7, 6, 5, 4, 3] and older["newer"] and older["older"]
    last = L.page(ld, before=3, size=5)
    assert seqs(last) == [2, 1] and last["newer"] and not last["older"]
    assert seqs(L.page(ld, before=1, size=5)) == []
    exact = L.page(ld, before=6, size=5)
    assert seqs(exact) == [5, 4, 3, 2, 1] and not exact["older"]
    newer = L.page(ld, after=2, size=5)
    assert seqs(newer) == [7, 6, 5, 4, 3] and newer["newer"] and newer["older"]
    top = L.page(ld, after=7, size=5)
    assert seqs(top) == [12, 11, 10, 9, 8] and not top["newer"] and top["older"]
    assert seqs(L.page(ld, after=12, size=5)) == []
    a = L.page(ld, threads=["a"], size=3)
    assert seqs(a) == [11, 9, 7] and a["older"] and not a["newer"]
    assert seqs(L.page(ld, threads=["a"], before=7, size=3)) == [5, 3, 1]
    assert seqs(L.page(ld, kinds=["step"])) == [12]
    both = L.page(ld, kinds=["step", "message"], threads=["b"], size=3)
    assert seqs(both) == [12, 10, 8] and both["older"]
