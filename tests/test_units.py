"""Plans that tidy themselves (SPEC §5, §8): units are the steps any edge joins; status and
plan_view leave the done ones out by default, and plan_prune removes them."""

import datetime as dt
import json

import pytest
from mcp import Client

from sluice import plan as P
from sluice.errors import BadRequest
from sluice.mcp_server import build_server
from tests.conftest import add, create, d, src


def ago(**delta) -> str:
    return (dt.datetime.now(dt.UTC) - dt.timedelta(**delta)).strftime("%Y-%m-%dT%H:%M:%SZ")


def ok(sid_outputs=None, finished=None, **extra):
    return {"status": "succeeded", "finished": finished or ago(minutes=1),
            "outputs": sid_outputs or {"sum": 2}, **extra}


# units: a -> b (a handoff); gate -> guarded (a `when`); first -> second (an `after`);
# alone; and n1/n2 which only share a plan input (not an edge)
STEPS = {"a": add(d(1), d(1)), "gate": {"run": "test.boom", "in": {}},
         "first": add(d(1), d(1)), "b": add(src("a/sum"), d(1)),
         "guarded": add(d(1), d(1), when="gate/done"),
         "second": add(d(1), d(1), after=["first"]), "alone": add(d(1), d(1)),
         "n1": add(src("n"), d(1)), "n2": add(src("n"), d(2))}


def test_units_join_steps_over_handoffs_when_and_after(store):
    create(store, "p", STEPS, inputs={"n": "int"})
    _, plan = store.plan("p")
    assert P.units(plan) == [["a", "b"], ["gate", "guarded"], ["first", "second"], ["alone"],
                             ["n1"], ["n2"]]


def test_a_unit_is_done_when_every_step_succeeded_or_was_skipped():
    skipped = {"status": "skipped", "skipped": "gate/done is false", "finished": ago()}
    state = {"inputs": {}, "steps": {"s": ok(), "m": ok(manual=True), "k": skipped,
                                     "k2": skipped, "f": {"status": "failed"},
                                     "st": {"status": "stale"}}}
    assert P.unit_done(["s", "m"], state)  # set by hand counts
    assert P.unit_done(["s", "k"], state)
    assert not P.unit_done(["k", "k2"], state)  # skipped alone is no success
    assert not P.unit_done(["s", "p"], state)  # p has no entry: pending
    assert not P.unit_done(["s", "f"], state)
    assert not P.unit_done(["s", "st"], state)


@pytest.fixture
def tidy(store):
    create(store, "p", STEPS, inputs={"n": "int"}, outputs={"total": src("alone/sum")})
    store.write_state("p", {"inputs": {}, "steps": {
        "a": ok(finished=ago(hours=5)), "b": ok(finished=ago(hours=3)),
        "gate": {"status": "succeeded", "finished": ago(hours=4), "outputs": {"done": False}},
        "guarded": {"status": "skipped", "skipped": "gate/done is false",
                    "finished": ago(hours=4)},
        "first": ok(finished=ago(minutes=5)), "second": {"status": "running"},
        "alone": ok(finished=ago(hours=9)), "n1": ok(), "n2": {"status": "failed",
                                                                "error": "boom"}}})
    return store


def test_status_leaves_out_done_units_unless_all(tidy):
    st = tidy.status("p")
    assert [s["id"] for s in st["steps"]] == ["first", "second", "n2"]
    assert st["done_units"] == {"units": 4, "steps": 6}
    assert st["outputs"] == {"total": 2}  # plan outputs still read their steps
    every = tidy.status("p", all=True)
    assert [s["id"] for s in every["steps"]] == list(STEPS) and "done_units" not in every
    # choosing steps returns what was chosen, done or not
    assert [s["id"] for s in tidy.status("p", steps=["a", "n2"])["steps"]] == ["a", "n2"]
    assert "done_units" not in tidy.status("p", steps=["a"])
    tidy.write_state("p", {"inputs": {}, "steps": {}})
    assert "done_units" not in tidy.status("p")  # nothing done: nothing said


def test_plan_view_leaves_out_done_units_unless_all(tidy):
    from sluice import views

    text = views.render(tidy, "p", "mermaid")
    assert text.startswith("flowchart LR\n  %% 4 done units (6 steps) left out; plan_view "
                           "with all: true shows them\n")
    assert '"first / test.add' in text and '"a / test.add' not in text
    assert "out0" in text and "| out0" not in text  # its step is left out, so is the edge
    full = views.render(tidy, "p", "mermaid", all=True)
    assert "%%" not in full and '"a / test.add' in full and "| out0" in full
    page = views.render(tidy, "p", "html")
    assert "4 done units (6 steps) left out; plan_view with all: true shows them" in page
    assert 'id="step-first"' in page and 'id="step-a"' not in page
    assert "of 9 succeeded" in page  # the summary counts every step
    page = views.render(tidy, "p", "html", all=True)
    assert 'id="step-a"' in page and "left out" not in page


def test_plan_prune_removes_done_units_past_the_age_in_one_edit(tidy):
    rev = tidy.get("p")["rev"]
    out = tidy.prune("p", older_than_hours=3.5, author="orch", reason="tidy")
    # a/b finished last 3 h ago: too recent; alone is read by a plan output: kept
    assert out == {"rev": rev + 1, "units": 1, "steps": ["gate", "guarded"]}
    edit = [h for h in tidy.history("p") if h["kind"] == "plan.edit"][-1]
    assert (edit["rev"], edit["author"], edit["reason"]) == (rev + 1, "orch", "tidy")
    assert edit["ops"] == [{"op": "remove", "path": "/steps/gate"},
                           {"op": "remove", "path": "/steps/guarded"}]
    out = tidy.prune("p", author="orch")
    assert out == {"rev": rev + 2, "units": 2, "steps": ["a", "b", "n1"]}
    assert tidy.history("p")[-1]["reason"] == "prune 2 done units"
    assert list(tidy.get("p")["steps"]) == ["first", "second", "alone", "n2"]
    assert tidy.prune("p") == {"rev": rev + 2, "units": 0, "steps": []}  # no edit
    assert tidy.get("p")["rev"] == rev + 2
    for bad in (-1, True, "3"):
        with pytest.raises(BadRequest):
            tidy.prune("p", bad)


async def test_the_tools(tidy):
    async with Client(build_server(tidy)) as c:
        async def call(tool, **args):
            r = await c.call_tool(tool, args)
            assert not r.is_error, r.content[0].text
            text = r.content[0].text
            return text if tool == "plan_view" else json.loads(text)

        assert (await call("status", project="p"))["done_units"] == {"units": 4, "steps": 6}
        assert len((await call("status", project="p", all=True))["steps"]) == 9
        assert "%% 4 done units" in await call("plan_view", project="p")
        assert "%%" not in await call("plan_view", project="p", all=True)
        pruned = await call("plan_prune", project="p", older_than_hours=3.5)
        assert pruned["steps"] == ["gate", "guarded"] and pruned["units"] == 1
