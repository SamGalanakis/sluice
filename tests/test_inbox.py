"""The inbox (SPEC §2, §8, §10): items waiting on a person, their answers setting plan inputs,
their log records, and `inbox.ask` as a plan step."""

import json
import time

import pytest
from mcp import Client

from sluice import inbox as I
from sluice import log as L
from sluice.errors import BadRequest, InvalidPlan, NotFound, NotOpen
from sluice.mcp_server import build_server
from sluice.store import Store
from tests.conftest import create, settle, statuses, write_config


def src(ref):
    return {"source": ref}


def kinds(store, project):
    return [(r["kind"], r.get("item")) for r in
            L.read(store.project_dir(project), kinds=["inbox"])["records"]]


async def call(c, tool, **args):
    r = await c.call_tool(tool, args)
    return r.is_error, json.loads(r.content[0].text)


async def test_post_list_answer_close_round_trip_over_mcp(store):
    store.create_project("p")
    store.create_project("q")
    async with Client(build_server(store)) as c:
        err, got = await call(c, "inbox_post", project="p", title="Ship it?",
                              body="The **diff** is small.", ui='root = Stack([])',
                              **{"from": "review"})
        assert not err and got == {"id": "i1"}
        _, got = await call(c, "inbox_post", project="q", title="Which colour?")
        assert got == {"id": "i1"}  # ids are per project
        _, got = await call(c, "inbox_post", project="p", title="Close me")
        assert got == {"id": "i2"}

        _, items = await call(c, "inbox_list")
        assert sorted((i["project"], i["id"], i["status"]) for i in items) == [
            ("p", "i1", "open"), ("p", "i2", "open"), ("q", "i1", "open")]
        first = items[0]  # oldest first (by created; ties keep project order)
        assert (first["title"], first["body"], first["ui"], first["from"]) == (
            "Ship it?", "The **diff** is small.", "root = Stack([])", "review")

        answer = {"action": "approve", "params": {"why": "small"}, "values": {"note": "ok"}}
        err, got = await call(c, "inbox_answer", project="p", id="i1", answer=answer)
        assert not err and got["status"] == "answered" and got["answer"] == answer
        assert got["answered"] >= got["created"]
        err, got = await call(c, "inbox_close", project="p", id="i2", reason="not needed")
        assert not err and (got["status"], got["reason"]) == ("closed", "not needed")

        _, open_p = await call(c, "inbox_list", project="p")
        assert open_p == []
        _, answered = await call(c, "inbox_list", project="p", status="answered")
        assert [i["id"] for i in answered] == ["i1"] and answered[0]["answer"] == answer
        _, closed = await call(c, "inbox_list", status="closed")
        assert [(i["project"], i["id"]) for i in closed] == [("p", "i2")]
        _, every = await call(c, "inbox_list", project="p", status="all")
        assert [i["id"] for i in every] == ["i1", "i2"]
        err, got = await call(c, "inbox_list", status="done")
        assert err and got["error"] == "bad_request"
        err, got = await call(c, "inbox_list", project="nope")
        assert err and got["error"] == "not_found"

    assert kinds(store, "p") == [("inbox.post", "i1"), ("inbox.post", "i2"),
                                 ("inbox.answer", "i1"), ("inbox.close", "i2")]
    recs = L.read(store.project_dir("p"), kinds=["inbox"])["records"]
    assert (recs[0]["title"], recs[0]["from"]) == ("Ship it?", "review")
    assert (recs[2]["answer"], recs[2]["by"]) == (answer, "mcp")
    assert recs[3]["reason"] == "not needed"


async def test_stale_answers_and_closes_are_refused(store):
    store.create_project("p")
    a = store.inbox_post("p", "one")["id"]
    b = store.inbox_post("p", "two")["id"]
    store.inbox_answer("p", a, {"action": "answer", "text": "first"}, "me")
    async with Client(build_server(store)) as c:
        err, got = await call(c, "inbox_answer", project="p", id=a,
                              answer={"action": "answer", "text": "second"})
        assert err and got == {"error": "conflict", "status": "answered",
                               "message": f"inbox item {a} is answered, not open"}
        err, got = await call(c, "inbox_close", project="p", id=a)
        assert err and got["error"] == "conflict"
        assert (await call(c, "inbox_close", project="p", id=b))[0] is False
        err, got = await call(c, "inbox_answer", project="p", id=b,
                              answer={"action": "answer", "text": "late"})
        assert err and (got["error"], got["status"]) == ("conflict", "closed")
        err, got = await call(c, "inbox_answer", project="p", id="i9",
                              answer={"action": "answer"})
        assert err and got["error"] == "not_found"
    items = {i["id"]: i for i in store.inbox("p", "all")}
    assert items[a]["answer"] == {"action": "answer", "text": "first"}
    assert "answer" not in items[b]
    assert kinds(store, "p") == [("inbox.post", a), ("inbox.post", b), ("inbox.answer", a),
                                 ("inbox.close", b)]


def test_answers_are_checked(store):
    store.create_project("p")
    i = store.inbox_post("p", "q")["id"]
    for bad, needle in [("yes", "expected an object"), ({"text": "x"}, "answer.action"),
                        ({"action": 1}, "answer.action: expected a string"),
                        ({"action": "a", "values": []}, "answer.values: expected an object"),
                        ({"action": "a", "extra": 1}, "answer.extra: unknown key")]:
        with pytest.raises(InvalidPlan) as e:
            store.inbox_answer("p", i, bad, "me")
        assert any(needle in x for x in e.value.errors), (bad, e.value.errors)
    assert store.inbox("p")[0]["status"] == "open"
    with pytest.raises(BadRequest, match="title"):
        store.inbox_post("p", "  ")
    with pytest.raises(NotFound):
        store.inbox_post("nope", "q")


def test_an_input_item_sets_the_plan_input_and_the_gated_step_starts(store, runner):
    create(store, "p", {"go": {"run": "core.echo", "in": {"value": src("approved")}}},
           inputs={"approved": "boolean"})
    settle(runner, store, "p", until=lambda s: "go" in s)
    assert statuses(store, "p") == {"go": "pending"}
    i = store.inbox_post("p", "Approve the deploy?", input="approved",
                         ui='root = Button("Approve", "approve", {value: true})')["id"]
    item = store.inbox_answer("p", i, {"action": "approve", "params": {"value": True}}, "me")
    assert item["status"] == "answered"
    assert store.read_state("p")["inputs"] == {"approved": True}
    steps = settle(runner, store, "p")
    assert steps["go"]["status"] == "succeeded" and steps["go"]["outputs"] == {"value": True}
    rec = next(r for r in store.history("p") if r["kind"] == "plan.input")
    assert (rec["name"], rec["value"], rec["reason"]) == (
        "approved", True, f"inbox item {i}: Approve the deploy?")
    assert [k for k, _ in kinds(store, "p")] == ["inbox.post", "inbox.answer"]


def test_which_answer_field_sets_the_input(store):
    create(store, "p", {}, inputs={"n": "Any"})
    cases = [({"action": "a", "values": {"value": 1}, "params": {"value": 2}, "text": "3"}, 1),
             ({"action": "a", "params": {"value": 2}, "text": "3"}, 2),
             ({"action": "a", "text": "3"}, "3")]
    for answer, want in cases:
        i = store.inbox_post("p", "n?", input="n")["id"]
        store.inbox_answer("p", i, answer, "me")
        assert store.read_state("p")["inputs"]["n"] == want
    i = store.inbox_post("p", "n?", input="n")["id"]
    with pytest.raises(InvalidPlan, match="valid|fit") as e:
        store.inbox_answer("p", i, {"action": "a"}, "me")
    assert "values.value, params.value or text" in e.value.errors[0]


def test_a_mismatched_answer_is_refused_and_the_item_stays_open(store):
    create(store, "p", {}, inputs={"count": "int"})
    i = store.inbox_post("p", "How many?", input="count")["id"]
    with pytest.raises(InvalidPlan) as e:
        store.inbox_answer("p", i, {"action": "answer", "text": "three"}, "me")
    assert e.value.errors == ['inputs.count: expected int, got "three"']
    assert "does not fit plan input count" in e.value.message
    assert store.inbox("p")[0]["status"] == "open"
    assert store.read_state("p")["inputs"] == {}
    assert kinds(store, "p") == [("inbox.post", i)]
    store.inbox_answer("p", i, {"action": "answer", "values": {"value": 3}}, "me")
    assert store.read_state("p")["inputs"] == {"count": 3}


def test_an_undeclared_input_is_refused_at_post(store):
    create(store, "p", {}, inputs={"approved": "boolean"})
    with pytest.raises(NotFound, match="no input 'aproved'"):
        store.inbox_post("p", "Approve?", input="aproved")
    assert store.inbox("p", "all") == [] and kinds(store, "p") == []


def test_items_survive_log_trimming(tmp_path):
    home = tmp_path / "home"
    write_config(home, log_max=10)
    store = Store(home)
    store.create_project("p")
    i = store.inbox_post("p", "Still there?")["id"]
    for n in range(30):
        store.append("p", {"kind": "message", "thread": "t", "from": "x", "body": str(n)})
    assert L.read(store.project_dir("p"))["records"][0]["seq"] > 2  # the post record is gone
    assert [x["id"] for x in store.inbox("p")] == [i]
    store.inbox_answer("p", i, {"action": "answer", "text": "yes"}, "me")
    assert store.inbox("p", "answered")[0]["answer"]["text"] == "yes"
    assert I.find(store.project_dir("p"), i)["status"] == "answered"


# ---- inbox.ask as a plan step ----------------------------------------------------------


def ask_plan(store, project="p"):
    create(store, project, {
        "ask": {"run": "inbox.ask", "in": {"title": {"default": "Pick a colour"},
                                          "ui": {"default": 'root = Button("Red", "red")'}}},
        "use": {"run": "core.echo", "in": {"value": src("ask/answer.action")}}})


def open_item(runner, store, project="p"):
    settle(runner, store, project, until=lambda s: s.get("ask", {}).get("status") == "running"
           and store.inbox(project))
    return store.inbox(project)[0]


def test_inbox_ask_blocks_until_answered(store, runner):
    ask_plan(store)
    item = open_item(runner, store)
    assert (item["title"], item["from"], item["ui"]) == (
        "Pick a colour", "ask", 'root = Button("Red", "red")')
    for _ in range(10):  # nobody answers: it keeps waiting
        runner.tick()
        time.sleep(0.1)
    assert statuses(store, "p") == {"ask": "running", "use": "pending"}
    answer = {"action": "red", "params": {}, "values": {}}
    store.inbox_answer("p", item["id"], answer, "me")
    steps = settle(runner, store, "p")
    assert steps["ask"]["status"] == "succeeded", steps["ask"].get("error")
    assert steps["ask"]["outputs"] == {"answer": answer}
    assert steps["use"]["outputs"] == {"value": "red"}
    assert len(store.inbox("p", "all")) == 1


def test_inbox_ask_fails_when_its_item_is_closed(store, runner):
    ask_plan(store)
    item = open_item(runner, store)
    store.inbox_close("p", item["id"], "asked elsewhere", "orchestrator")
    steps = settle(runner, store, "p", until=lambda s: s["ask"]["status"] == "failed")
    assert f"inbox item {item['id']} was closed without an answer: asked elsewhere" in \
        steps["ask"]["error"]
    assert steps["use"]["status"] == "pending"


def test_inbox_ask_after_a_restart_waits_on_the_same_item(store, runner):
    ask_plan(store)
    item = open_item(runner, store)
    for a in runner.active.values():  # the runner stops: its fn processes die with it
        a.kill()
    runner.active.clear()
    settle(runner, store, "p", until=lambda s: s["ask"]["status"] == "failed")
    store.retry("p", "ask", "test", "runner restarted")

    def waiting(steps):  # the new run has found the item (it says so on stderr)
        runs = steps["ask"].get("run_ids") or []
        return steps["ask"]["status"] == "running" and runs and "waiting for an answer to " \
            f"inbox item {item['id']}" in (store.runs_dir("p") / runs[-1] / "stderr.log").read_text()
    settle(runner, store, "p", until=waiting)
    store.inbox_answer("p", item["id"], {"action": "red"}, "me")
    steps = settle(runner, store, "p")
    assert steps["ask"]["outputs"] == {"answer": {"action": "red"}}
    assert [i["id"] for i in store.inbox("p", "all")] == [item["id"]]


def test_opening_a_new_item_rejects_a_closed_one(store):
    store.create_project("p")
    i = store.inbox_post("p", "q")["id"]
    store.inbox_close("p", i, None, "me")
    with pytest.raises(NotOpen):
        store.inbox_close("p", i, None, "me")
    assert store.inbox_post("p", "q")["id"] == "i2"
