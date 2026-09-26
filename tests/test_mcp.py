import json
import threading
import time

import anyio
import pytest
from mcp import Client

from sluice.mcp_server import build_server
from sluice.runner import Runner
from tests.conftest import create

TOOLS = {"docs", "projects_list", "project_create", "project_update", "fn_list", "fn_get",
         "fn_save", "fn_call", "call_status", "plan_get", "plan_patch", "plan_history",
         "plan_set_input", "step_set_input", "step_set_output", "step_retry", "verify",
         "plan_view", "status", "log_read", "log_wait", "inbox_post", "inbox_list",
         "inbox_answer", "inbox_close", "step_submit"}
UPPER = """from sluice.fn import run

run(lambda inp, ctx: {"text": inp["text"].upper()})
"""


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


async def until(c, project, pred, timeout=20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        s = await ok(c, "status", project=project)
        if pred({x["id"]: x["status"] for x in s["steps"]}):
            return s
        await anyio.sleep(0.1)
    raise AssertionError(f"timed out: {s}")


async def test_the_tool_set(store):
    async with Client(build_server(store)) as c:
        tools = (await c.list_tools()).tools
    assert {t.name for t in tools} == TOOLS and all(t.description for t in tools)


async def test_projects(store):
    async with Client(build_server(store)) as c:
        assert await ok(c, "project_create", name="p", description="first") == {"name": "p"}
        assert await ok(c, "project_create", name="q") == {"name": "q"}
        assert (await fail(c, "project_create", name="p"))["error"] == "bad_request"
        assert (await fail(c, "project_create", name="No"))["error"] == "bad_request"
        assert await ok(c, "project_update", name="q", description="second") == {"name": "q"}
        assert (await fail(c, "project_update", name="zz", description=""))["error"] == \
            "not_found"
        assert await ok(c, "projects_list") == [
            {"name": "p", "description": "first", "rev": 1, "counts": {}},
            {"name": "q", "description": "second", "rev": 1, "counts": {}}]
        assert await ok(c, "plan_get", project="p") == {
            "rev": 1, "plan": {"inputs": {}, "outputs": {}, "steps": {}}}


async def test_fn_tools(store):
    store.create_project("p")
    async with Client(build_server(store)) as c:
        fns = {f["name"]: f for f in await ok(c, "fn_list")}
        assert fns["test.add"] == {"name": "test.add", "doc": "Add two ints.", "scope": "global",
                                   "inputs": {"a": "int", "b": "int"}, "outputs": {"sum": "int"}}
        assert fns["test.boom"]["doc"] == "" and fns["thread.post"]["scope"] == "builtin"
        got = await ok(c, "fn_get", name="core.collect")
        assert got["inputs"] == {"items": "Any[]"} and got["scope"] == "builtin"
        assert got["path"].endswith("fns/core.collect")
        assert (await fail(c, "fn_get", name="no.such"))["error"] == "not_found"

        fn = {"name": "text.upper", "inputs": {"text": "string"}, "outputs": {"text": "string"}}
        saved = await ok(c, "fn_save", fn=fn, main_py=UPPER, project="p")
        assert saved == {"scope": "project",
                         "path": str(store.project_dir("p") / "fns" / "text.upper")}
        assert (await ok(c, "fn_get", name="text.upper", project="p"))["scope"] == "project"
        assert (await fail(c, "fn_get", name="text.upper"))["error"] == "not_found"
        assert "text.upper" in {f["name"] for f in await ok(c, "fn_list", project="p")}
        bad = await fail(c, "fn_save", fn={**fn, "inputs": {"text": "str"}}, main_py=UPPER)
        assert bad["error"] == "invalid" and bad["errors"] == ["inputs.text: unknown type 'str'"]
        clash = await fail(c, "fn_save", fn={**fn, "name": "core.echo"}, main_py=UPPER,
                           project="p")
        assert clash["error"] == "bad_request" and "would collide" in clash["message"]

        bad = await fail(c, "fn_call", name="test.add", inputs={"a": "one", "c": 1})
        assert bad["error"] == "invalid" and bad["errors"] == [
            'inputs.a: expected int, got "one"', "inputs.b: missing required field",
            "inputs.c: fn test.add has no input c"]
        direct = await ok(c, "fn_call", name="text.upper", inputs={"text": "hi"}, project="p",
                          direct=True)
        assert direct == {"call": direct["call"], "status": "succeeded",
                          "outputs": {"text": "HI"}}
        status = await ok(c, "call_status", call=direct["call"], project="p")
        assert status["outputs"] == {"text": "HI"}
        assert (await fail(c, "call_status", call=direct["call"]))["error"] == "not_found"


async def test_fn_call_waits_for_the_runner(live):
    live.create_project("p")
    async with Client(build_server(live)) as c:
        res = await ok(c, "fn_call", name="test.add", inputs={"a": 2, "b": 3}, wait=20)
        assert res == {"call": res["call"], "status": "succeeded", "outputs": {"sum": 5}}
        assert (live.home / "runs" / res["call"] / "output.json").is_file()
        failed = await ok(c, "fn_call", name="test.boom", inputs={}, project="p", wait=20)
        assert failed["status"] == "failed" and "about to explode" in failed["error"]
        assert "about to explode" in (await ok(c, "call_status", call=failed["call"],
                                               project="p"))["stderr_tail"]
        quick = await ok(c, "fn_call", name="test.window", inputs={"seconds": 5})
        assert quick["status"] in ("pending", "running")
        assert (await ok(c, "projects_list"))[0]["counts"] == {}


async def test_plan_editing_and_error_payloads(store):
    store.create_project("p", "t")
    async with Client(build_server(store)) as c:
        assert (await fail(c, "plan_get", project="nope"))["error"] == "not_found"
        plan = {"inputs": {"n": "int"}, "outputs": {},
                "steps": {"a": add({"source": "n"}, d(1))}}
        assert await ok(c, "plan_patch", project="p", rev=1, reason="start", ops=[
            {"op": "replace", "path": "/inputs", "value": plan["inputs"]},
            {"op": "replace", "path": "/steps", "value": plan["steps"]}]) == {"rev": 2}
        assert await ok(c, "plan_get", project="p") == {"rev": 2, "plan": plan}

        assert await ok(c, "plan_patch", project="p", rev=2, reason="grow", author="orch",
                        ops=[{"op": "add", "path": "/steps/b", "value": add(d(3), d(4))}]
                        ) == {"rev": 3}
        conflict = await fail(c, "plan_patch", project="p", rev=2, reason="stale", ops=[])
        assert conflict == {"error": "conflict", "message": "plan is at rev 3", "current_rev": 3}
        invalid = await fail(c, "plan_patch", project="p", rev=3, reason="bad", ops=[
            {"op": "add", "path": "/steps/c", "value": {"run": "test.add",
                                                        "in": {"a": {"source": "zz/sum"}}}}])
        assert invalid["error"] == "invalid" and invalid["errors"] == [
            "steps.c.in.b: required input is not bound", "steps.c.in.a: unknown step zz"]
        args = await fail(c, "plan_patch", project="p")
        assert args["error"] == "bad_request" and "rev: Field required" in args["message"]

        assert await ok(c, "step_set_input", project="p", step="b", input="a", value=10,
                        reason="by hand") == {"rev": 4}
        stale = await fail(c, "step_set_input", project="p", step="b", input="a", value=1,
                           rev=1)
        assert stale["current_rev"] == 4
        assert await ok(c, "plan_set_input", project="p", name="n", value=5, reason="go") == {
            "ok": True}
        typed = await fail(c, "plan_set_input", project="p", name="n", value="x")
        assert typed["errors"] == ['inputs.n: expected int, got "x"']

        hist = await ok(c, "plan_history", project="p")
        assert [(h["rev"], h["author"], h["kind"]) for h in hist] == [
            (1, "", "plan.edit"), (2, "mcp", "plan.edit"), (3, "orch", "plan.edit"),
            (4, "mcp", "plan.edit"), (4, "mcp", "plan.input")]
        assert [h["seq"] for h in hist] == sorted(h["seq"] for h in hist)
        assert [h["rev"] for h in await ok(c, "plan_history", project="p", since_rev=3)] == [4, 4]
        assert await ok(c, "projects_list") == [{"name": "p", "description": "t", "rev": 4,
                                                 "counts": {"pending": 2}}]


async def test_status_manual_outputs_and_retry(live):
    create(live, "p", {"a": add({"source": "n"}, d(1)), "boom": {"run": "test.boom", "in": {}},
                       "c": add({"source": "a/sum"}, d(1))},
           inputs={"n": "int"}, outputs={"total": {"source": "c/sum"}})
    async with Client(build_server(live)) as c:
        await ok(c, "plan_set_input", project="p", name="n", value=1)
        s = await until(c, "p", lambda st: st["c"] == "succeeded" and st["boom"] == "failed")
        assert s["rev"] == 2 and s["inputs"] == {"n": 1} and s["outputs"] == {"total": 3}
        rows = {r["id"]: r for r in s["steps"]}
        assert rows["a"] == {"id": "a", "run": "test.add", "status": "succeeded",
                             "started": rows["a"]["started"], "finished": rows["a"]["finished"],
                             "outputs": {"sum": 2}, "manual": False}
        assert "about to explode" in rows["boom"]["error"]

        running = await fail(c, "step_retry", project="p", step="a")
        assert running["error"] == "bad_request"
        assert await ok(c, "step_set_output", project="p", step="boom", outputs={"done": True},
                        reason="done by hand") == {"ok": True}
        s = await ok(c, "status", project="p")
        boom = next(r for r in s["steps"] if r["id"] == "boom")
        assert (boom["status"], boom["manual"], boom["outputs"]) == ("succeeded", True,
                                                                     {"done": True})
        bad = await fail(c, "step_set_output", project="p", step="boom", outputs={"done": 1})
        assert bad["errors"] == ["outputs.done: expected boolean, got 1"]
        assert await ok(c, "step_retry", project="p", step="boom", reason="really run") == {
            "ok": True}
        await until(c, "p", lambda st: st["boom"] == "failed")
        assert (await fail(c, "status", project="zz"))["error"] == "not_found"


async def test_verify_tool(store):
    store.create_project("p")
    async with Client(build_server(store)) as c:
        assert await ok(c, "verify") == {"ok": True, "problems": []}
        (store.project_dir("p") / ".env").write_text("nope\n")
        assert await ok(c, "verify", project="p") == {
            "ok": False, "problems": [{"where": "projects/p/.env:1",
                                       "message": "not a KEY=value line"}]}
        assert (await fail(c, "verify", project="zz"))["error"] == "not_found"


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
    create(store, "p", {"a": add(d(1), d(1))})
    async with Client(build_server(store)) as c:
        mermaid = await c.call_tool("plan_view", {"project": "p"})
        assert mermaid.content[0].text.startswith("flowchart LR\n")
        page = await c.call_tool("plan_view", {"project": "p", "format": "html"})
        assert page.content[0].text.startswith("<!doctype html>")
        assert (await fail(c, "plan_view", project="p", format="png"))["error"] == "bad_request"
