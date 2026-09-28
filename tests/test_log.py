"""The log (SPEC §6b): the database's records, one log per project (and one for the home),
seq-ordered records of every kind, capped; read through log_read / log_wait."""

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
    recs = L.read(store.home, "p")["records"]
    seqs = [r["seq"] for r in recs]
    assert seqs == sorted(set(seqs))
    assert all(set(r) >= {"seq", "at", "kind"} for r in recs)
    assert set(kinds(recs)) == {"plan.edit", "plan.input", "step.status", "step.output",
                                "step.retry"}
    status = [(r["step"], r["from"], r["to"]) for r in recs if r["kind"] == "step.status"]
    assert ("a", None, "running") in status and ("a", "running", "succeeded") in status
    assert ("boom", "running", "failed") in status
    assert status[-2:] == [("boom", "failed", "succeeded"), ("boom", "succeeded", "pending")]
    failed = next(r for r in recs if r["kind"] == "step.status" and r["to"] == "failed")
    assert "about to explode" in failed["error"] and failed["run_ids"]
    assert not list(store.home.rglob("*.jsonl"))


def test_concurrent_writers_in_processes_get_distinct_increasing_seqs(store, home):
    store.create_project("p")
    with multiprocessing.get_context("spawn").Pool(4) as pool:
        pool.starmap(_post, [(str(home), 25, f"w{k}") for k in range(4)])
    recs = L.read(store.home, "p")["records"]
    seqs = [r["seq"] for r in recs]
    assert len(recs) == 101 and seqs == sorted(set(seqs))  # the creation edit, then 100
    for k in range(4):  # each writer's own messages keep their order
        assert [r["body"] for r in recs if r.get("from") == f"w{k}"] == [str(i)
                                                                          for i in range(25)]


def test_read_filters_by_kind_group_thread_since_and_limit(store):
    store.create_project("p")
    store.create_project("other")
    s = [L.last_seq(store.home, "p")]  # s[0]: the creation edit; s[i]: the i-th append
    for i in range(6):
        s += store.append("p", {"kind": "message", "thread": "ab"[i % 2], "from": "x",
                                "body": str(i)})
        store.append("other", {"kind": "message", "thread": "a", "from": "x", "body": "o"})
    s += store.append("p", {"kind": "step.status", "step": "s", "from": None, "to": "pending"})
    h = store.home
    assert kinds(L.read(h, "p", kinds=["step"])["records"]) == ["step.status"]
    assert kinds(L.read(h, "p", kinds=["plan"])["records"]) == ["plan.edit"]
    a = L.read(h, "p", threads=["a"])  # threads alone: only messages on them
    assert [r["body"] for r in a["records"]] == ["0", "2", "4"] and a["last_seq"] == s[7]
    both = L.read(h, "p", kinds=["message", "step.status"], threads=["b"])["records"]
    assert [r.get("body", r["kind"]) for r in both] == ["1", "3", "5", "step.status"]
    tail = L.read(h, "p", limit=2)  # without since_seq: the last `limit`
    assert [r["seq"] for r in tail["records"]] == s[6:8] and tail["last_seq"] == s[7]
    page = L.read(h, "p", since_seq=s[2], kinds=["message"], limit=2)  # the next `limit`
    assert [r["seq"] for r in page["records"]] == s[3:5] and page["last_seq"] == s[4]
    rest = L.read(h, "p", since_seq=page["last_seq"], kinds=["message"], limit=10)
    assert [r["seq"] for r in rest["records"]] == s[5:7] and rest["last_seq"] == s[7]
    assert L.read(h, "p", since_seq=s[7]) == {"records": [], "last_seq": s[7]}
    # a cursor past this log's end (another log's seq) is never moved back
    [later] = store.append("other", {"kind": "message", "thread": "a", "from": "x",
                                     "body": "o"})
    assert later > s[7] and L.read(h, "p", since_seq=later)["last_seq"] == later
    assert L.check_kinds(["step", "message", "nope"]) == [
        "unknown kind 'nope'; kinds: " + ", ".join((*L.KINDS, *L.GROUPS))]
    assert L.check_kinds(["step.cancel"]) == []


def test_a_page_of_records_and_its_cursor_come_from_one_snapshot(store, monkeypatch):
    """An append between reading the rows and the cursor must not be skipped: the next read
    from the cursor returns it."""
    store.create_project("p")
    store.create_project("q")
    real = L._high

    def racing(conn, project, src="records"):  # another process appends to both logs
        other = Store(store.home)
        t = threading.Thread(target=lambda: (other.append("p", msg("late")),
                                             other.append("q", msg("q"))))
        t.start()
        t.join()
        return real(conn, project, src)

    first = L.read(store.home, "p", since_seq=0)
    monkeypatch.setattr(L, "_high", racing)
    res = L.read(store.home, "p", since_seq=first["last_seq"])
    monkeypatch.setattr(L, "_high", real)
    assert res["records"] == [] and res["last_seq"] == first["last_seq"]
    again = L.read(store.home, "p", since_seq=res["last_seq"])
    assert [r["body"] for r in again["records"]] == ["late"]


def msg(body):
    return {"kind": "message", "thread": "t", "from": "x", "body": body}


def test_last_seq_of_an_empty_log(store):
    assert L.last_seq(store.home, None) == 0
    store.create_project("p")
    assert L.last_seq(store.home, "p") == L.read(store.home, "p")["records"][-1]["seq"]
    assert L.last_seq(store.home, None) == 0


def test_the_cap_drops_the_oldest_records_of_that_log_only(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=10)
    store = Store(home)
    store.create_project("p")
    store.create_project("q")
    for i in range(9):
        store.append("p", msg(str(i)))
        store.append("q", msg(str(i)))
    recs = L.read(home, "p")["records"]
    assert len(recs) == 10 and recs[0]["kind"] == "plan.edit"  # counted per log, not by seqs
    store.append("p", msg("x"))  # past the cap: down to 90% of it
    assert [r["body"] for r in L.read(home, "p")["records"]] == [*map(str, range(1, 9)), "x"]
    assert len(L.read(home, "q")["records"]) == 10


def test_the_cap_keeps_the_plan_history_and_every_run_dir(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=6)
    store = Store(home)
    from sluice.runner import Runner

    runner = Runner(store)
    create(store, "p", {"a": add(d(1), d(1))})
    settle(runner, store, "p")
    [run_id] = store.read_state("p")["steps"]["a"]["run_ids"]
    for i in range(12):
        store.append("p", msg(str(i)))
    recs = L.read(home, "p")["records"]
    assert len(recs) <= 6 and "plan.edit" not in kinds(recs)
    assert (store.runs_dir("p") / run_id).is_dir()  # the trim removes rows only
    runner._gc()
    assert (store.runs_dir("p") / run_id).is_dir()  # the state still refers to it
    edits = store.history("p")  # every edit, though the log no longer has them
    assert [(e["kind"], e["rev"]) for e in edits] == [("plan.edit", 1), ("plan.edit", 2)]
    assert edits[1]["ops"] and all(isinstance(e["seq"], int) for e in edits)


def test_the_cap_drops_finished_calls_and_submissions_nothing_refers_to(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=5)
    store = Store(home)
    store.create_project("p")
    with store.tx() as conn:
        for call, status in (("done", "succeeded"), ("live", "running")):
            conn.execute("INSERT INTO calls (call, project, fn, status, inputs, created) "
                         "VALUES (?, 'p', 'core.echo', ?, '{}', 'now')", (call, status))
        conn.execute("INSERT INTO submissions (project, run, step, outputs, at) "
                     "VALUES ('p', 'r1', 's', '{}', 'now')")
    store.append("p", {"kind": "call", "call": "done", "fn": "core.echo",
                       "status": "succeeded"})
    for i in range(3):
        store.append("p", msg(str(i)))
    with store.rx() as conn:
        assert {r[0] for r in conn.execute("SELECT call FROM calls")} == {"done", "live"}
    for i in range(5):  # the `call` record goes: so does the finished call
        store.append("p", msg(str(i)))
    with store.rx() as conn:
        assert [r[0] for r in conn.execute("SELECT call FROM calls")] == ["live"]
        assert conn.execute("SELECT count(*) FROM submissions").fetchone()[0] == 0


def test_a_trim_that_rolls_back_changes_nothing(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=3)
    store = Store(home)
    store.create_project("p")
    for i in range(2):
        store.append("p", msg(str(i)))
    run_dir = store.runs_dir("p") / "r1"
    run_dir.mkdir(parents=True)
    store.append("p", {"kind": "run.orphan", "run": "r1"})
    before = L.read(home, "p")["records"]
    try:
        with store.tx():
            store.append("p", msg("over the cap"))  # trims inside the transaction
            assert len(L.read(home, "p")["records"]) < len(before) + 1
            raise RuntimeError("the change fails after its trim")
    except RuntimeError:
        pass
    assert L.read(home, "p")["records"] == before and run_dir.is_dir()


def test_log_read_and_log_wait_tools(live):
    live.create_project("p")

    async def main():
        async with Client(build_server(live)) as c:
            async def tool(name, **args):
                r = await c.call_tool(name, args)
                return r.is_error, json.loads(r.content[0].text)

            err, res = await tool("log_read", project="p")
            first = res["records"][0]["seq"]
            assert not err and kinds(res["records"]) == ["plan.edit"] and res["last_seq"] == first
            err, bad = await tool("log_read", project="p", kinds=["nope"])
            assert err and bad["error"] == "bad_request"
            err, missing = await tool("log_read", project="zz")
            assert err and missing["error"] == "not_found"

            t0 = time.monotonic()
            err, res = await tool("log_wait", project="p", since_seq=first, timeout=1)
            assert not err and res == {"records": [], "last_seq": first}
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
                waited["res"] = (await tool("log_wait", project="p", since_seq=first,
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
            assert waited["res"]["last_seq"] == L.last_seq(live.home, "p")

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
    h, s0 = store.home, L.last_seq(store.home, "p")
    note = {"kind": "message", "thread": "t", "from": "w", "body": "fyi",
            "needs_reply": False}
    [s1] = store.append("p", note)
    t0 = time.monotonic()
    res = L.wait(h, "p", s0, ["message"], wake="questions", timeout=0.2, interval=0.02)
    assert time.monotonic() - t0 >= 0.19  # a note alone does not wake it
    assert res["records"] == [] and res["last_seq"] == s1
    assert [r["body"] for r in res["held"]] == ["fyi"]  # it comes back at the timeout

    def later():
        time.sleep(0.1)
        store.append("p", {"kind": "message", "thread": "t", "from": "w",
                           "body": "which db?", "needs_reply": True},
                     {**note, "body": "meanwhile"})

    threading.Thread(target=later, daemon=True).start()
    res = L.wait(h, "p", s0, ["message"], wake="questions", timeout=10, interval=0.02)
    assert [r["body"] for r in res["records"]] == ["fyi", "which db?"]
    assert [r["body"] for r in res["held"]] == ["meanwhile"]  # after the waking record
    last = L.last_seq(h, "p")
    assert res["last_seq"] == last
    res = L.wait(h, "p", res["last_seq"], ["message"], timeout=0.1, interval=0.02)
    assert res == {"records": [], "held": [], "last_seq": last}
    res = L.wait(h, "p", s0, ["message"], timeout=5, interval=0.02, limit=2)
    assert [r["body"] for r in res["records"]] == ["fyi", "which db?"]
    assert res["held"] == [] and res["last_seq"] == res["records"][-1]["seq"]  # the limit


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
    h = store.home
    store.create_project("noise")
    s = [0]  # s[i]: the i-th record: messages on a (odd) and b (even), a step change last
    for i in range(1, 13):
        rec = ({"kind": "step.status", "step": "s", "from": None, "to": "pending"} if i == 12
               else {"kind": "message", "thread": "a" if i % 2 else "b", "from": "t",
                     "body": str(i)})
        s += store.append(None, rec)
        store.append("noise", msg("gap"))  # another log's records: gaps in this one's seqs

    def seqs(res):
        return [s.index(r["seq"]) for r in res["records"]]

    newest = L.page(h, None, size=5)
    assert seqs(newest) == [12, 11, 10, 9, 8] and not newest["newer"] and newest["older"]
    assert newest["last_seq"] == s[12]
    older = L.page(h, None, before=s[8], size=5)
    assert seqs(older) == [7, 6, 5, 4, 3] and older["newer"] and older["older"]
    last = L.page(h, None, before=s[3], size=5)
    assert seqs(last) == [2, 1] and last["newer"] and not last["older"]
    assert seqs(L.page(h, None, before=s[1], size=5)) == []
    exact = L.page(h, None, before=s[6], size=5)
    assert seqs(exact) == [5, 4, 3, 2, 1] and not exact["older"]
    newer = L.page(h, None, after=s[2], size=5)
    assert seqs(newer) == [7, 6, 5, 4, 3] and newer["newer"] and newer["older"]
    top = L.page(h, None, after=s[7], size=5)
    assert seqs(top) == [12, 11, 10, 9, 8] and not top["newer"] and top["older"]
    assert seqs(L.page(h, None, after=s[12], size=5)) == []
    a = L.page(h, None, threads=["a"], size=3)
    assert seqs(a) == [11, 9, 7] and a["older"] and not a["newer"]
    assert seqs(L.page(h, None, threads=["a"], before=s[7], size=3)) == [5, 3, 1]
    assert seqs(L.page(h, None, kinds=["step"])) == [12]
    both = L.page(h, None, kinds=["step", "message"], threads=["b"], size=3)
    assert seqs(both) == [12, 10, 8] and both["older"]
