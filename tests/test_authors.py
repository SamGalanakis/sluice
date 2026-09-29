"""Who did what (SPEC §6b, §8): every write tool names its author — the explicit one, else
SLUICE_AUTHOR, else step:<SLUICE_STEP>, else the MCP client's name, else "mcp" ("cli" from
`sluice tool`) — and every change to a project is a record."""

import json
import sys

import pytest
from mcp import Client
from mcp.types import Implementation

from sluice import drain
from sluice import log as L
from sluice.cli import main
from sluice.mcp_server import author_of, build_server
from tests.conftest import add, create, d, settle, src
from tests.test_inbox_dashboard import post


def records(store, project="p", kinds=None):
    return L.read(store.home, project, kinds=kinds)["records"]


def last(store, kind, project="p"):
    return records(store, project, [kind])[-1]


async def ok(c, tool, **args):
    r = await c.call_tool(tool, args)
    assert not r.is_error, r.content[0].text
    return json.loads(r.content[0].text) if r.content else None


def test_the_author_rule_in_order(monkeypatch):
    assert author_of(None) == "mcp"
    assert author_of(None, fallback="cli") == "cli"
    assert author_of(None, "claude-code") == "claude-code"
    monkeypatch.setenv("SLUICE_STEP", "build")
    assert author_of(None, "claude-code") == "step:build"
    monkeypatch.setenv("SLUICE_AUTHOR", "sam")
    assert author_of(None, "claude-code") == "sam"
    assert author_of("orch", "claude-code") == "orch"
    assert author_of("  ", "claude-code") == "sam"  # a blank author names nobody


async def test_over_mcp_explicit_then_env_then_step_then_client(store, monkeypatch):
    create(store, "p", {"a": add(d(1), d(1))})
    info = Implementation(name="claude-code", version="1")
    async with Client(build_server(store), client_info=info) as c:
        await ok(c, "step_update", project="p", step="a", changes={"doc": "one"})
        assert last(store, "plan.edit")["author"] == "claude-code"
        monkeypatch.setenv("SLUICE_STEP", "build")
        await ok(c, "step_update", project="p", step="a", changes={"doc": "two"})
        assert last(store, "plan.edit")["author"] == "step:build"
        monkeypatch.setenv("SLUICE_AUTHOR", "sam")
        await ok(c, "step_update", project="p", step="a", changes={"doc": "three"})
        assert last(store, "plan.edit")["author"] == "sam"
        await ok(c, "step_update", project="p", step="a", changes={"doc": "four"},
                 author="orch")
        assert last(store, "plan.edit")["author"] == "orch"


async def test_inbox_post_names_its_asker_by_the_author_rule(store, monkeypatch):
    create(store, "p", {"build": add(d(1), d(1))})
    info = Implementation(name="claude-code", version="1")

    def asker(item_id):
        item = next(i for i in store.inbox("p", "all") if i["id"] == item_id)
        return item.get("from"), item.get("run")

    async with Client(build_server(store), client_info=info) as c:
        got = await ok(c, "inbox_post", project="p", title="one")
        assert asker(got["id"]) == ("claude-code", None)
        assert last(store, "inbox.post")["from"] == "claude-code"
        monkeypatch.setenv("SLUICE_STEP", "build")
        monkeypatch.setenv("SLUICE_RUN_ID", "r-7")
        got = await ok(c, "inbox_post", project="p", title="two")
        assert asker(got["id"]) == ("step:build", "r-7")  # inside a step: its run too
        assert last(store, "inbox.post")["run"] == "r-7"
        got = await ok(c, "inbox_post", project="p", title="three", **{"from": "reviewer"})
        assert asker(got["id"]) == ("reviewer", None)  # an explicit from wins
        monkeypatch.setenv("SLUICE_AUTHOR", "sam")
        got = await ok(c, "inbox_post", project="p", title="four")
        assert asker(got["id"]) == ("sam", None)
    r = await build_server(store, author="cli").call_tool(
        "inbox_post", {"project": "p", "title": "five"})
    assert not r.is_error and store.inbox("p")[-1]["from"] == "sam"
    monkeypatch.delenv("SLUICE_AUTHOR")
    monkeypatch.delenv("SLUICE_STEP")
    r = await build_server(store, author="cli").call_tool(
        "inbox_post", {"project": "p", "title": "six"})
    assert not r.is_error and store.inbox("p")[-1]["from"] == "cli"


async def test_without_a_client_name_it_is_mcp(store):
    create(store, "p", {"a": add(d(1), d(1))})
    r = await build_server(store).call_tool(
        "step_pause", {"project": "p", "steps": ["a"], "reason": "hold"})
    assert not r.is_error
    assert last(store, "plan.edit")["author"] == "mcp"


def test_sluice_tool_is_cli_and_inside_a_step_the_step(store, monkeypatch, capsys):
    create(store, "p", {"a": add(d(1), d(1))})
    monkeypatch.setenv("SLUICE_HOME", str(store.home))
    args = {"project": "p", "step": "a", "changes": {"doc": "x"}}
    assert main(["tool", "step_update", json.dumps(args)]) == 0
    assert last(store, "plan.edit")["author"] == "cli"
    monkeypatch.setenv("SLUICE_STEP", "fix-x")
    assert main(["tool", "step_update", json.dumps({**args, "changes": {"doc": "y"}})]) == 0
    assert last(store, "plan.edit")["author"] == "step:fix-x"
    capsys.readouterr()


def test_an_agent_inside_a_step_submits_as_that_step(store, runner, monkeypatch):
    monkeypatch.setenv("TEST_PYTHON", sys.executable)
    create(store, "p", {"agent": {"run": "test.open", "in": {"attempts": d([{"word": "hi"}])},
                                  "outputs": {"word": "string"}}})
    settle(runner, store, "p")
    assert last(store, "step.submit")["author"] == "step:agent"


async def test_every_manual_record_names_its_author(store, monkeypatch):
    monkeypatch.setenv("SLUICE_AUTHOR", "sam")
    create(store, "p", {"a": add(d(1), d(1)), "b": add(src("a/sum"), d(1)),
                        "w": {"run": "core.external", "in": {}}},
           inputs={"n": "int?"})
    async with Client(build_server(store)) as c:
        start = L.last_seq(store.home, "p")
        await ok(c, "plan_set_input", project="p", name="n", value=3)
        await ok(c, "step_set_output", project="p", step="a", outputs={"sum": 2})
        await ok(c, "step_retry", project="p", steps=["a"])
        await ok(c, "step_cancel", project="p", steps=["w"], reason="not needed")
        await ok(c, "step_set_input", project="p", step="b", input="b", value=5)
        await ok(c, "step_add", project="p", step="c", spec=add(d(1), d(1)))
        await ok(c, "step_pause", project="p", steps=["c"], paused=False)
        await ok(c, "fn_call", name="core.echo", inputs={"value": 1}, project="p", direct=True)
        item = (await ok(c, "inbox_post", project="p", title="ok?"))["id"]
        await ok(c, "inbox_answer", project="p", id=item, answer={"action": "yes"})
        item = (await ok(c, "inbox_post", project="p", title="still?"))["id"]
        await ok(c, "inbox_close", project="p", id=item)
        await ok(c, "step_remove", project="p", steps=["c"])
        await ok(c, "project_update", name="p", paused=True)
        await ok(c, "project_create", name="q")
    got = [r for r in records(store) if r["seq"] > start]
    who = {}
    for r in got:
        name = r.get("author", r.get("by"))
        if r["kind"] not in ("step.status", "inbox.post") and \
                not (r["kind"] == "call" and r["status"] != "running"):
            who.setdefault(r["kind"], set()).add(name)
    assert who == {k: {"sam"} for k in (
        "plan.input", "step.output", "step.retry", "step.cancel", "plan.edit", "call",
        "inbox.answer", "inbox.close", "project.pause")}
    assert records(store, "q")[0]["author"] == "sam"  # its creation, rev 1


# ---- project records ----------------------------------------------------------------------

async def test_project_update_logs_each_change_with_author_and_reason(store, tmp_path):
    store.create_project("p")
    icon = tmp_path / "i.svg"
    icon.write_text('<svg xmlns="http://www.w3.org/2000/svg"/>')
    async with Client(build_server(store)) as c:
        start = L.last_seq(store.home, "p")
        await ok(c, "project_update", name="p", paused=True, reason="deploy", author="orch")
        await ok(c, "project_update", name="p", paused=True, reason="again")  # no change
        await ok(c, "project_update", name="p", paused=False)
        await ok(c, "project_update", name="p", archived=True, author="orch")
        await ok(c, "project_update", name="p", description="new", icon=str(icon),
                 author="orch")
        await ok(c, "project_update", name="p", description="new", icon=str(icon))
        await ok(c, "project_update", name="p", archived=False, paused=True,
                 description="newer", author="orch")
    got = [{k: v for k, v in r.items() if k not in ("seq", "at")}
           for r in records(store) if r["seq"] > start]
    assert got == [
        {"kind": "project.pause", "paused": True, "reason": "deploy", "author": "orch"},
        {"kind": "project.pause", "paused": False, "author": "mcp"},
        {"kind": "project.archive", "archived": True, "author": "orch"},
        {"kind": "project.update", "fields": ["description", "icon"], "author": "orch"},
        {"kind": "project.pause", "paused": True, "author": "orch"},
        {"kind": "project.archive", "archived": False, "author": "orch"},
        {"kind": "project.update", "fields": ["description"], "author": "orch"}]
    assert [r["kind"] for r in L.read(store.home, "p", kinds=["project"])["records"]] == \
        [g["kind"] for g in got]


def test_drain_and_release_log_as_drain(store, monkeypatch, capsys):
    for name in ("a", "b"):
        store.create_project(name)
    store.update_project("b", paused=True, author="orch", reason="mine")
    monkeypatch.setenv("SLUICE_HOME", str(store.home))
    assert main(["drain", "--no-wait"]) == 0
    rec = last(store, "project.pause", "a")
    assert (rec["paused"], rec["author"], rec["reason"]) == \
        (True, "drain", "drain: paused for maintenance")
    assert last(store, "project.pause", "b")["author"] == "orch"  # drain left b alone
    assert main(["drain", "--release"]) == 0
    rec = last(store, "project.pause", "a")
    assert (rec["paused"], rec["author"], rec["reason"]) == (False, "drain", "drain released")
    assert len(records(store, "b", ["project.pause"])) == 1
    capsys.readouterr()


async def test_the_drain_tool_names_its_caller(store):
    store.create_project("a")
    async with Client(build_server(store)) as c:
        await ok(c, "drain", author="orch")
        await ok(c, "release")
    assert [(r["paused"], r["author"]) for r in records(store, "a", ["project.pause"])] == \
        [(True, "orch"), (False, "mcp")]
    assert drain.release(store) == []  # nothing listed any more


def test_the_dashboard_pauses_as_dashboard(store, port):
    create(store, "p", {"a": add(d(1), d(1))})
    assert post(port, "/projects/p/pause", {"paused": "1"}, json_body=False)[0] == 303
    assert post(port, "/projects/p/archive", {"archived": "1"}, json_body=False)[0] == 303
    assert [(r["kind"], r["author"]) for r in records(store, "p", ["project"])] == \
        [("project.pause", "dashboard"), ("project.archive", "dashboard")]


@pytest.mark.parametrize("tool", ["project_delete", "fn_save", "inbox_post"])
def test_tools_that_leave_no_authored_record_take_no_author(store, tool):
    assert "author" not in build_server(store).params[tool]


async def test_the_client_name_comes_over_http_too(store, port):
    create(store, "p", {"a": add(d(1), d(1))})
    info = Implementation(name="codex", version="1")
    async with Client(f"http://127.0.0.1:{port}/mcp", client_info=info) as c:
        await ok(c, "step_update", project="p", step="a", changes={"doc": "x"})
    assert last(store, "plan.edit")["author"] == "codex"
