import json
import threading
import time

import anyio
import pytest
from mcp import Client

from sluice.mcp_server import build_server
from sluice.runner import Runner

TOOLS = {"docs", "fn_list", "fn_get", "fn_call", "plans_list", "plan_create", "plan_get",
         "plan_patch", "plan_history", "plan_set_input", "step_set_input", "step_set_output",
         "step_retry", "plan_view", "status"}


def d(x):
    return {"default": x}


def add(a, b):
    return {"run": "test.add", "in": {"a": a, "b": b}}


@pytest.fixture
def live(store):
    """The store with a runner loop in a thread, woken right after in-process edits."""
    runner = Runner(store)
    store.listeners.append(runner.wake)
    t = threading.Thread(target=runner.run_forever, kwargs={"interval": 0.2}, daemon=True)
    t.start()
    yield store
    runner.stop()
    t.join(10)


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


async def until(c, plan, pred, timeout=20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        s = await ok(c, "status", plan=plan)
        if pred({x["id"]: x["status"] for x in s["steps"]}):
            return s
        await anyio.sleep(0.1)
    raise AssertionError(f"timed out: {s}")


async def test_the_tool_set(store):
    async with Client(build_server(store)) as c:
        tools = (await c.list_tools()).tools
    assert {t.name for t in tools} == TOOLS and all(t.description for t in tools)


async def test_fn_tools(store):
    async with Client(build_server(store)) as c:
        fns = {f["name"]: f for f in await ok(c, "fn_list")}
        assert fns["test.add"] == {"name": "test.add", "doc": "Add two ints.",
                                   "inputs": {"a": "int", "b": "int"}, "outputs": {"sum": "int"}}
        assert fns["test.boom"]["doc"] == "" and "git.head" in fns
        assert (await ok(c, "fn_get", name="core.collect"))["inputs"] == {"items": "Any[]"}
        assert (await fail(c, "fn_get", name="no.such"))["error"] == "not_found"
        bad = await fail(c, "fn_call", name="test.add", inputs={"a": "one", "c": 1})
        assert bad["error"] == "invalid" and bad["errors"] == [
            'inputs.a: expected int, got "one"', "inputs.b: missing required field",
            "inputs.c: fn test.add has no input c"]
        assert store.plan_ids() == []


async def test_fn_call_waits_for_the_result(live):
    async with Client(build_server(live)) as c:
        res = await ok(c, "fn_call", name="test.add", inputs={"a": 2, "b": 3}, wait=20)
        assert res == {"plan": res["plan"], "status": "succeeded", "outputs": {"sum": 5}}
        assert res["plan"].startswith("call-")
        failed = await ok(c, "fn_call", name="test.boom", inputs={}, wait=20)
        assert failed["status"] == "failed" and "about to explode" in failed["error"]
        quick = await ok(c, "fn_call", name="test.window", inputs={"seconds": 5})
        assert quick["status"] in ("pending", "running")
        assert await ok(c, "plans_list") == []
        assert len(await ok(c, "plans_list", include_calls=True)) == 3


async def test_plan_editing_and_error_payloads(store):
    async with Client(build_server(store)) as c:
        doc = {"label": "t", "inputs": {"n": "int"}, "steps": {"a": add({"source": "n"}, d(1))}}
        assert await ok(c, "plan_create", plan="p", doc=doc, reason="start") == {"rev": 1}
        got = await ok(c, "plan_get", plan="p")
        assert got == {"rev": 1, "doc": {"id": "p", **doc}}
        assert (await fail(c, "plan_get", plan="nope"))["error"] == "not_found"
        again = await fail(c, "plan_create", plan="p", doc=doc, reason="again")
        assert again["error"] == "bad_request"

        assert await ok(c, "plan_patch", plan="p", rev=1, reason="grow", author="orch",
                        ops=[{"op": "add", "path": "/steps/b", "value": add(d(3), d(4))}]
                        ) == {"rev": 2}
        conflict = await fail(c, "plan_patch", plan="p", rev=1, reason="stale", ops=[])
        assert conflict == {"error": "conflict", "message": "plan is at rev 2", "current_rev": 2}
        invalid = await fail(c, "plan_patch", plan="p", rev=2, reason="bad", ops=[
            {"op": "add", "path": "/steps/c", "value": {"run": "test.add",
                                                        "in": {"a": {"source": "zz/sum"}}}}])
        assert invalid["error"] == "invalid" and invalid["errors"] == [
            "steps.c.in.b: required input is not bound", "steps.c.in.a: unknown step zz"]
        args = await fail(c, "plan_patch", plan="p")
        assert args["error"] == "bad_request" and "rev: Field required" in args["message"]

        assert await ok(c, "step_set_input", plan="p", step="b", input="a", value=10,
                        reason="by hand") == {"rev": 3}
        stale = await fail(c, "step_set_input", plan="p", step="b", input="a", value=1, rev=1)
        assert stale["current_rev"] == 3
        assert await ok(c, "plan_set_input", plan="p", name="n", value=5, reason="go") == {
            "ok": True}
        typed = await fail(c, "plan_set_input", plan="p", name="n", value="x")
        assert typed["errors"] == ['inputs.n: expected int, got "x"']

        hist = await ok(c, "plan_history", plan="p")
        assert [(h["rev"], h["author"], h.get("action")) for h in hist] == [
            (1, "mcp", None), (2, "orch", None), (3, "mcp", None), (3, "mcp", "plan_set_input")]
        assert [h["rev"] for h in await ok(c, "plan_history", plan="p", since_rev=2)] == [3, 3]
        assert await ok(c, "plans_list") == [{"id": "p", "label": "t", "rev": 3,
                                              "counts": {"pending": 2}}]


async def test_status_manual_outputs_and_retry(live):
    async with Client(build_server(live)) as c:
        await ok(c, "plan_create", plan="p", reason="go", doc={
            "inputs": {"n": "int"}, "outputs": {"total": {"source": "c/sum"}},
            "steps": {"a": add({"source": "n"}, d(1)), "boom": {"run": "test.boom", "in": {}},
                      "c": add({"source": "a/sum"}, d(1))}})
        await ok(c, "plan_set_input", plan="p", name="n", value=1)
        s = await until(c, "p", lambda st: st["c"] == "succeeded" and st["boom"] == "failed")
        assert s["rev"] == 1 and s["inputs"] == {"n": 1} and s["outputs"] == {"total": 3}
        rows = {r["id"]: r for r in s["steps"]}
        assert rows["a"] == {"id": "a", "run": "test.add", "status": "succeeded",
                             "started": rows["a"]["started"], "finished": rows["a"]["finished"],
                             "outputs": {"sum": 2}, "manual": False}
        assert "about to explode" in rows["boom"]["error"]

        running = await fail(c, "step_retry", plan="p", step="a")
        assert running["error"] == "bad_request"
        assert await ok(c, "step_set_output", plan="p", step="boom", outputs={"done": True},
                        reason="done by hand") == {"ok": True}
        s = await ok(c, "status", plan="p")
        boom = next(r for r in s["steps"] if r["id"] == "boom")
        assert (boom["status"], boom["manual"], boom["outputs"]) == ("succeeded", True,
                                                                     {"done": True})
        bad = await fail(c, "step_set_output", plan="p", step="boom", outputs={"done": 1})
        assert bad["errors"] == ["outputs.done: expected boolean, got 1"]
        assert await ok(c, "step_retry", plan="p", step="boom", reason="really run") == {
            "ok": True}
        await until(c, "p", lambda st: st["boom"] == "failed")
        assert (await fail(c, "status", plan="zz"))["error"] == "not_found"


async def test_docs_for_agents(store):
    server = build_server(store)
    async with Client(server) as c:
        index = await ok(c, "docs")
        assert index["plans"] == "Plans" and index["types"] == "Types" and "examples" in index
        page = await c.call_tool("docs", {"topic": "types"})
        assert page.content[0].text.startswith("# Types\n")
        assert (await fail(c, "docs", topic="nope"))["error"] == "not_found"
        uris = {str(r.uri) for r in (await c.list_resources()).resources}
        assert {"sluice://docs/plans", "sluice://docs/types"} <= uris
        res = await c.read_resource("sluice://docs/plans")
        assert res.contents[0].text.startswith("# Plans")
        tools = {t.name: t.description for t in (await c.list_tools()).tools}
        assert "rev: the revision you read" in tools["plan_patch"]
    assert server.instructions.startswith("sluice runs plans")


async def test_plan_view(store):
    async with Client(build_server(store)) as c:
        await ok(c, "plan_create", plan="p", reason="x", doc={"steps": {"a": add(d(1), d(1))}})
        mermaid = await c.call_tool("plan_view", {"plan": "p"})
        assert mermaid.content[0].text.startswith("flowchart LR\n")
        page = await c.call_tool("plan_view", {"plan": "p", "format": "html"})
        assert page.content[0].text.startswith("<!doctype html>")
        assert (await fail(c, "plan_view", plan="p", format="png"))["error"] == "bad_request"
