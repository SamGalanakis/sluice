"""Whether anyone still waits for an inbox item (SPEC §8a): the derived `waiting` flag of an
item a step asked, what the tools and the dashboard say once its asker has stopped, and a
retried `inbox.ask` taking up its step's own earlier item (and an answer nobody read)."""

import json
import re

import anyio
from mcp import Client

from sluice import log as L
from sluice import views
from sluice.mcp_server import build_server
from tests.conftest import create, settle
from tests.test_inbox import ask_plan, open_item


def adopts(store, project="p"):
    return [(r["item"], r["status"]) for r in
            L.read(store.home, project, kinds=["inbox.adopt"])["records"]]


def page_text(store, status="open"):
    html = views.inbox_parts(store, None, status)["inbox-items"]
    return re.sub(r"\s+", " ", re.sub(r"<[^>]+>", " ", html))


def stop(runner, store, how):
    """End the ask step's run: cancel it, or kill its process (a failure)."""
    if how == "cancel":
        store.cancel_steps("p", ["ask"], author="test", reason="not now")
    else:
        for a in runner.active.values():
            a.kill()
    return settle(runner, store, "p", until=lambda s: s["ask"]["status"] == "failed")


def retried(runner, store, reason="again"):
    store.retry("p", "ask", author="test", reason=reason)
    return settle(runner, store, "p", until=lambda s: s["ask"]["status"] == "running"
                  and s["ask"].get("run_ids") and store.inbox("p", "all") and all(
                      i.get("waiting") is not False for i in store.inbox("p")))


def rerun(runner, store):
    """Run the succeeded ask step again: a new body makes it stale, then retry it."""
    store.patch("p", store.get("p")["rev"], [{"op": "add", "path": "/steps/ask/in/body",
                                              "value": {"default": "Once more."}}], "test", "b")
    settle(runner, store, "p", until=lambda s: s["ask"]["status"] == "stale")
    store.retry("p", "ask", author="test", reason="ask again")
    return settle(runner, store, "p", until=lambda s: s["ask"]["status"] == "running"
                  and len(store.inbox("p", "all")) == 2)


def test_waiting_while_the_run_waits_and_not_once_it_is_cancelled(store, runner):
    ask_plan(store)
    item = open_item(runner, store)
    run = store.read_state("p")["steps"]["ask"]["run_ids"][-1]
    assert (item["from"], item["run"], item["waiting"]) == ("ask", run, True)
    assert "stopped" not in item and "Nobody is waiting" not in page_text(store)
    stop(runner, store, "cancel")
    [item] = store.inbox("p")
    assert item["status"] == "open"  # never closed for you
    assert (item["waiting"], item["stopped"]) == (False, "ask is cancelled")
    text = page_text(store)
    assert "Nobody is waiting — ask is cancelled" in text
    assert "An answer will not be delivered unless the step is retried." in text


def test_not_waiting_once_the_run_failed_and_after_the_live_ones(store, runner):
    ask_plan(store)
    open_item(runner, store)
    stop(runner, store, "kill")
    later = store.inbox_post("p", "Asked by use", sender="use", run="r-1")["id"]
    items = store.inbox("p")
    assert [(i["id"], i["waiting"], i.get("stopped")) for i in items] == [
        ("i1", False, "ask is failed"), (later, False, "use is pending")]
    other = store.inbox_post("p", "Live?", sender="nobody-step")["id"]  # not a plan step
    assert "waiting" not in store.inbox("p")[-1] and store.inbox("p")[-1]["id"] == other
    # the dashboard lists the items somebody waits for (or nobody can tell) first
    text = page_text(store)
    assert text.index("Live?") < text.index("Pick a colour")


def test_the_tool_result_and_the_steps_own_success(store, runner):
    create(store, "p", {"use": {"run": "core.echo", "in": {"value": {"default": 1}}}})
    settle(runner, store, "p")
    store.inbox_post("p", "Asked on the way out", sender="use", run="r-1")
    store.inbox_post("p", "From a call", sender="call c-gone")
    store.inbox_post("p", "From a removed step", sender="step:gone", run="r-2")

    async def listed():
        async with Client(build_server(store)) as c:
            r = await c.call_tool("inbox_list", {"project": "p"})
            return json.loads(r.content[0].text)

    items = anyio.run(listed)
    assert [(i["title"], i["waiting"], i["stopped"]) for i in items] == [
        ("Asked on the way out", False, "use is succeeded"),
        ("From a call", False, "call c-gone is gone"),
        ("From a removed step", False, "gone is not in the plan")]
    assert items[0]["from"] == "use" and items[0]["run"] == "r-1"
    assert "the call has ended" in page_text(store)
    # answered and closed items carry no flag
    store.inbox_answer("p", items[0]["id"], {"action": "ok"}, "me")
    assert "waiting" not in store.inbox("p", "answered")[0]


def test_a_retry_takes_up_its_open_item(store, runner):
    ask_plan(store)
    item = open_item(runner, store)
    first = item["run"]
    stop(runner, store, "cancel")
    steps = retried(runner, store)
    [again] = store.inbox("p", "all")  # no duplicate
    assert again["id"] == item["id"] and again["run"] == steps["ask"]["run_ids"][-1] != first
    assert again["waiting"] is True
    [rec] = L.read(store.home, "p", kinds=["inbox.adopt"])["records"]
    assert (rec["item"], rec["from"], rec["run"], rec["was"], rec["status"]) == (
        item["id"], "ask", again["run"], first, "open")
    store.inbox_answer("p", item["id"], {"action": "red"}, "me")
    steps = settle(runner, store, "p")
    assert steps["ask"]["outputs"] == {"answer": {"action": "red"}}


def test_a_retry_takes_up_an_answer_nobody_read(store, runner):
    ask_plan(store)
    item = open_item(runner, store)
    stop(runner, store, "kill")
    store.inbox_answer("p", item["id"], {"action": "blue"}, "me")  # nobody is waiting
    [rec] = L.read(store.home, "p", kinds=["inbox.answer"])["records"]
    assert rec["waiting"] is False
    assert "while nobody was waiting" in views.log_summary(rec)
    store.retry("p", "ask", author="test", reason="deliver it")
    steps = settle(runner, store, "p")
    assert steps["ask"]["outputs"] == {"answer": {"action": "blue"}}
    assert steps["use"]["outputs"] == {"value": "blue"}
    assert [i["id"] for i in store.inbox("p", "all")] == [item["id"]]
    assert adopts(store) == [(item["id"], "answered")]
    # delivered now: the next retry asks again
    rerun(runner, store)
    assert [(i["id"], i["status"]) for i in store.inbox("p", "all")] == [
        (item["id"], "answered"), ("i2", "open")]


def test_an_answer_the_run_read_is_not_taken_up_again(store, runner):
    ask_plan(store)
    item = open_item(runner, store)
    store.inbox_answer("p", item["id"], {"action": "red"}, "me")
    assert "waiting" not in L.read(store.home, "p", kinds=["inbox.answer"])["records"][0]
    settle(runner, store, "p")
    rerun(runner, store)
    assert adopts(store) == []


def test_a_new_title_or_another_step_posts_a_new_item(store, runner):
    create(store, "p", {
        "ask": {"run": "inbox.ask", "in": {"title": {"default": "Pick a colour"}}},
        "other": {"run": "inbox.ask", "in": {"title": {"default": "Pick a colour"}},
                  "after": ["ask"]}})
    item = open_item(runner, store)
    stop(runner, store, "cancel")
    rev = store.get("p")["rev"]
    store.patch("p", rev, [{"op": "replace", "path": "/steps/ask/in/title",
                            "value": {"default": "Pick a shade"}}], "test", "reword")
    store.retry("p", "ask", author="test", reason="reworded")
    settle(runner, store, "p", until=lambda s: s["ask"]["status"] == "running"
           and len(store.inbox("p")) == 2)
    old, new = store.inbox("p")
    assert (old["id"], old["waiting"]) == (item["id"], False)
    assert (new["title"], new["waiting"]) == ("Pick a shade", True)
    store.inbox_answer("p", new["id"], {"action": "teal"}, "me")
    settle(runner, store, "p", until=lambda s: s.get("other", {}).get("status") == "running"
           and len(store.inbox("p", "all")) == 3)
    third = store.inbox("p", "all")[-1]  # the same title, but another step's: its own item
    assert (third["from"], third["title"], third["waiting"]) == ("other", "Pick a colour", True)
    assert adopts(store) == []
