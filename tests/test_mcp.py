import json
import threading
import time

import anyio
import pytest
from mcp import Client

from sluice.mcp_server import build_server
from sluice.runner import Runner

TOOLS = {"plans_list", "plan_create", "plan_get", "plan_patch", "plan_validate", "plan_history",
         "plan_at", "plan_revert", "node_add", "status", "node_get", "node_retry", "node_skip",
         "node_cancel", "dry_run", "events_tail", "inbox_list", "inbox_resolve", "fn_list",
         "fn_get", "fn_call", "fn_result"}


def v(x):
    return {"value": x}


def add(a, b):
    return {"fn": "test.add", "in": {"a": v(a), "b": v(b)}}


@pytest.fixture
def live(store):
    """The store with a runner loop in a background thread, woken by in-process edits."""
    runner = Runner(store)
    store.listeners.append(runner.wake)
    t = threading.Thread(target=runner.run_forever, daemon=True)
    t.start()
    yield store
    runner.stop()
    t.join(10)
    for p in list(runner.procs.values()):
        p.kill()


async def call(c, tool, **args):
    r = await c.call_tool(tool, args)
    return r.is_error, json.loads(r.content[0].text)


async def ok(c, tool, **args):
    err, data = await call(c, tool, **args)
    assert not err, data
    return data


async def fail(c, tool, **args):
    err, data = await call(c, tool, **args)
    assert err, data
    return data


async def until_status(c, plan, pred, timeout=20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        s = await ok(c, "status", plan=plan)
        by = {n["id"]: n["status"] for n in s["nodes"]}
        if pred(by):
            return s
        await anyio.sleep(0.1)
    raise AssertionError(f"timed out: {by}")


async def test_every_tool_is_listed(store):
    async with Client(build_server(store)) as c:
        tools = await c.list_tools()
    assert {t.name for t in tools.tools} == TOOLS
    assert all(t.description for t in tools.tools)


async def test_plan_editing_tools_and_error_payloads(store):
    async with Client(build_server(store)) as c:
        assert await ok(c, "plan_create", plan="p", doc={"title": "t", "nodes": {"a": add(1, 2)}},
                        reason="start") == {"rev": 1}
        got = await ok(c, "plan_get", plan="p")
        assert got["rev"] == 1 and got["doc"]["nodes"]["a"]["fn"] == "test.add"
        assert "rev" not in got["doc"]
        assert (await ok(c, "plan_get", plan="p", path="/nodes/a/in/b")) == {"rev": 1,
                                                                             "doc": v(2)}
        assert (await fail(c, "plan_get", plan="p", path="/nodes/zz"))["error"] == "not_found"
        assert (await fail(c, "plan_get", plan="nope"))["error"] == "not_found"
        dup = await fail(c, "plan_create", plan="p", doc={"nodes": {}}, reason="again")
        assert dup["error"] == "bad_request"

        assert await ok(c, "plan_patch", plan="p", rev=1, reason="grow", author="orch",
                        ops=[{"op": "add", "path": "/nodes/b", "value": add(3, 4)}]) == {"rev": 2}
        conflict = await fail(c, "plan_patch", plan="p", rev=1, reason="stale",
                              ops=[{"op": "remove", "path": "/nodes/b"}])
        assert conflict == {"error": "conflict", "message": "plan is at rev 2", "current_rev": 2}
        invalid = await fail(c, "plan_patch", plan="p", rev=2, reason="bad",
                             ops=[{"op": "add", "path": "/nodes/c",
                                   "value": {"fn": "test.add", "in": {"a": {"from": "zz.sum"}}}}])
        assert invalid["error"] == "invalid"
        assert invalid["errors"] == ["nodes.c.in.a: unknown node zz",
                                     "nodes.c.in.b: required input port is not bound"]

        good = await ok(c, "plan_validate", plan="p", doc={"nodes": {"x": add(1, 1)}})
        assert good == {"ok": True, "errors": []}
        bad = await ok(c, "plan_validate", plan="p", doc={"nodes": {"x": {"fn": "no.such"}}})
        assert bad == {"ok": False, "errors": ["nodes.x.fn: unknown fn no.such"]}
        assert (await ok(c, "plan_get", plan="p"))["rev"] == 2

        assert await ok(c, "node_add", plan="p", rev=2, id="c", node=add(5, 6),
                        reason="one more") == {"rev": 3}
        exists = await fail(c, "node_add", plan="p", rev=3, id="c", node=add(5, 6), reason="x")
        assert exists["errors"] == ["nodes.c: a node with this id already exists"]

        hist = await ok(c, "plan_history", plan="p")
        assert [(h["rev"], h["author"]) for h in hist] == [(1, "mcp"), (2, "orch"), (3, "mcp")]
        assert [h["rev"] for h in await ok(c, "plan_history", plan="p", since_rev=2)] == [3]
        at1 = await ok(c, "plan_at", plan="p", rev=1)
        assert set(at1["doc"]["nodes"]) == {"a"}
        assert await ok(c, "plan_revert", plan="p", rev=3, to_rev=1, reason="undo") == {"rev": 4}
        assert set((await ok(c, "plan_get", plan="p"))["doc"]["nodes"]) == {"a"}

        plans = await ok(c, "plans_list")
        assert plans == [{"id": "p", "title": "t", "rev": 4, "paused": False,
                          "counts": {"pending": 1}}]
        bad_args = await fail(c, "plan_patch", plan="p")
        assert bad_args["error"] == "bad_request" and "rev: Field required" in bad_args["message"]


async def test_runtime_tools(live):
    store = live
    async with Client(build_server(store)) as c:
        await ok(c, "plan_create", plan="p", reason="go", doc={"nodes": {
            "a": add(1, 2),
            "boom": {"fn": "test.boom"},
            "ask": {"fn": "core.ask", "in": {"question": v("which?"), "to": v("human")}},
            "use": {"fn": "core.echo", "in": {"value": {"from": "ask.answer"}}},
            "nap": {"fn": "test.sleep", "in": {"seconds": v(30)}},
            "later": {"fn": "core.echo", "in": {"value": v(1)}, "after": ["boom"]},
        }})
        s = await until_status(c, "p", lambda b: b["a"] == "succeeded" and b["boom"] == "failed"
                               and b["ask"] == "waiting" and b["nap"] == "running")
        assert s["rev"] == 1 and s["state_rev"] >= 1
        row = next(n for n in s["nodes"] if n["id"] == "boom")
        assert row["fn"] == "test.boom" and row["attempt"] == 1 and "boom" in row["error"]
        assert set(s["counts"]) >= {"succeeded", "failed", "waiting", "running", "pending"}
        assert s["ready"] == []

        dry = await ok(c, "dry_run", plan="p")
        assert {"id": "later", "reason": "dependency boom failed"} in dry["blocked"]
        assert {"id": "use", "reason": "waiting for ask (waiting)"} in dry["blocked"]

        node = await ok(c, "node_get", plan="p", node="a")
        assert node["definition"] == add(1, 2) and node["expanded_ids"] == ["a"]
        assert node["state"]["status"] == "succeeded" and node["output"] == {"sum": 3}
        assert "adding 1 + 2" in node["stderr_tail"]
        assert (await fail(c, "node_get", plan="p", node="zz"))["error"] == "not_found"

        evs = await ok(c, "events_tail", plan="p")
        assert evs[0]["type"] == "plan_created"
        tail = await ok(c, "events_tail", plan="p", since_seq=1, limit=2)
        assert [e["seq"] for e in tail] == [2, 3]

        items = await ok(c, "inbox_list", plan="p")
        by_kind = {i["kind"]: i for i in items}
        assert by_kind["ask"]["to"] == "human" and by_kind["failure"]["node"] == "boom"
        wrong = await fail(c, "inbox_resolve", item=by_kind["ask"]["id"],
                           resolution={"action": "ack"})
        assert wrong["error"] == "bad_request"
        assert await ok(c, "inbox_resolve", item=by_kind["ask"]["id"],
                        resolution={"answer": "left"}, author="sam") == {"ok": True}
        await until_status(c, "p", lambda b: b["use"] == "succeeded")
        assert (await ok(c, "node_get", plan="p", node="use"))["output"] == {"value": "left"}

        assert (await ok(c, "node_skip", plan="p", node="boom", reason="later"))["ok"] is True
        await until_status(c, "p", lambda b: b["later"] == "skipped")
        assert (await ok(c, "inbox_list", plan="p")) == []
        assert len(await ok(c, "inbox_list", plan="p", open_only=False)) == 2

        (store.home / "boom-ok").write_text("")
        assert (await ok(c, "node_retry", plan="p", node="boom", reason="fixed"))["ok"] is True
        await until_status(c, "p", lambda b: b["boom"] == "succeeded" and b["later"] == "succeeded")

        assert (await ok(c, "node_cancel", plan="p", node="nap", reason="enough"))["ok"] is True
        await until_status(c, "p", lambda b: b["nap"] == "cancelled")
        again = await fail(c, "node_cancel", plan="p", node="nap", reason="twice")
        assert again["error"] == "bad_request"


async def test_fn_list_and_get(store):
    async with Client(build_server(store)) as c:
        fns = {f["name"]: f for f in await ok(c, "fn_list")}
        assert fns["test.add"] == {"name": "test.add", "version": 1,
                                   "description": "Add two ints. Effect-free, so cached.",
                                   "in": {"a": "int", "b": "int"}, "out": {"sum": "int"},
                                   "composite": False, "effects": False}
        assert fns["test.echo_log"]["description"] == ""  # declared without one
        assert fns["test.twice"]["composite"] is True and fns["core.ask"]["effects"] is True
        raw = await ok(c, "fn_get", name="test.twice")
        assert raw["graph"]["out"] == {"y": {"from": "b.value"}}
        assert (await fail(c, "fn_get", name="no.such"))["error"] == "not_found"


async def test_fn_call_with_wait_returns_the_result(live):
    async with Client(build_server(live)) as c:
        res = await ok(c, "fn_call", name="test.add", input={"a": 2, "b": 3}, wait=20)
        assert res["status"] == "succeeded" and res["output"] == {"sum": 5}
        assert res["call"].startswith("call-")
        assert live.get(res["call"])["meta"] == {"adhoc": True, "fn": "test.add"}
        assert await ok(c, "plans_list") == []
        listed = await ok(c, "plans_list", include_adhoc=True)
        assert [p["id"] for p in listed] == [res["call"]]


async def test_fn_call_without_wait_then_poll_fn_result(live):
    async with Client(build_server(live)) as c:
        res = await ok(c, "fn_call", name="test.twice", input={"x": 21})
        assert res == {"call": res["call"], "status": "pending"}
        deadline = time.time() + 20
        while time.time() < deadline:
            got = await ok(c, "fn_result", call=res["call"])
            if got["status"] == "succeeded":
                break
            await anyio.sleep(0.1)
        assert got == {"call": res["call"], "status": "succeeded", "output": {"y": 42},
                       "stderr_tail": got["stderr_tail"]}
        assert "adding 21 + 21" in got["stderr_tail"]


async def test_fn_call_with_invalid_input_is_rejected(store):
    async with Client(build_server(store)) as c:
        bad = await fail(c, "fn_call", name="test.add", input={"a": "one", "c": 1})
        assert bad["error"] == "invalid"
        assert bad["errors"] == ['input.a: expected int, got "one"',
                                 "input.b: missing required field",
                                 "input.c: fn test.add has no input port c"]
        assert (await fail(c, "fn_call", name="no.such", input={}))["error"] == "not_found"
    assert store.plan_ids() == []


async def test_fn_call_of_a_failing_fn_fails_and_opens_an_inbox_item(live):
    async with Client(build_server(live)) as c:
        res = await ok(c, "fn_call", name="test.boom", input={}, wait=20)
        assert res["status"] == "failed" and res["error"] == "RuntimeError: boom (exit 1)"
        [item] = await ok(c, "inbox_list", plan=res["call"])
        assert item["kind"] == "failure" and item["node"] == "call"
        got = await ok(c, "fn_result", call=res["call"])
        assert "about to explode" in got["stderr_tail"]
