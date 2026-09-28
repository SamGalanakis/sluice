"""Staleness (SPEC §6): a result holds only for the inputs it was computed from."""

import pytest

from sluice import log as L
from sluice.errors import BadRequest, InvalidPlan
from sluice.plan import inputs_hash
from tests.conftest import add, create, d, echo, settle, src, statuses, write_fn

CAT = """from pathlib import Path
from sluice.fn import run

run(lambda inp, ctx: {"text": Path(inp["path"]).read_text()})
"""


def until_status(**want):
    return lambda s: all(s[k]["status"] == v for k, v in want.items())


def transitions(store, project, step):
    recs = L.read(store.home, project, kinds=["step.status"])["records"]
    return [(r["from"], r["to"]) for r in recs if r["step"] == step]


def test_retrying_an_upstream_reruns_it_and_its_result_decides(store, runner, tmp_path):
    source = tmp_path / "source.txt"
    source.write_text("one")
    write_fn(store.project_dir("p") / "fns", "p.cat", {"path": "string"}, {"text": "string"},
             main=CAT)
    create(store, "p", {"a": {"run": "p.cat", "in": {"path": d(str(source))}},
                        "b": echo("a/text"), "c": echo("b/value"), "other": add(d(1), d(1))})
    settle(runner, store, "p")
    # a manual result for a, then a retry: a runs again and produces "one" as before
    store.set_output("p", "a", {"text": "one"}, "test", "known")
    store.retry("p", "a", author="test", reason="run it for real")
    steps = settle(runner, store, "p")
    assert statuses(store, "p") == dict.fromkeys(["a", "b", "c", "other"], "succeeded")
    assert steps["b"]["outputs"] == {"value": "one"}
    assert transitions(store, "p", "b") == [(None, "pending"), ("pending", "succeeded")]

    source.write_text("two")  # the next run of a gives a different result
    store.set_output("p", "a", {"text": "one"}, "test", "known")
    store.retry("p", "a", author="test", reason="run it again")
    steps = settle(runner, store, "p", until=until_status(a="succeeded", b="stale", c="stale"))
    assert statuses(store, "p")["other"] == "succeeded"  # not downstream of a
    assert steps["b"]["outputs"] == {"value": "one"}  # kept for inspection
    assert transitions(store, "p", "c")[-1] == ("succeeded", "stale")
    for _ in range(3):  # stale steps never re-run by themselves
        runner.tick()
    assert statuses(store, "p")["b"] == "stale"


def test_a_changed_plan_input_makes_its_readers_stale(store, runner):
    create(store, "p", {"a": add(src("n"), d(1)), "b": add(src("a/sum"), d(1)),
                        "each": {"run": "core.echo", "scatter": "value",
                                 "in": {"value": src("items")}},
                        "free": add(d(1), d(2))},
           inputs={"n": "int", "items": "int[]"})
    store.set_input("p", "n", 1, "test", "go")
    store.set_input("p", "items", [1, 2], "test", "go")
    settle(runner, store, "p")
    store.set_input("p", "n", 5, "test", "changed my mind")
    store.set_input("p", "items", [1, 2, 3], "test", "one more")
    steps = settle(runner, store, "p", until=until_status(a="stale", b="stale", each="stale"))
    assert steps["free"]["status"] == "succeeded"
    assert steps["b"]["outputs"] == {"sum": 3}
    store.set_input("p", "n", 1, "test", "back to what a was computed from")
    steps = settle(runner, store, "p", until=until_status(a="succeeded", b="succeeded"))
    assert statuses(store, "p")["each"] == "stale"


def test_a_changed_step_input_binding_makes_the_step_stale(store, runner):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(src("a/sum"), d(1))})
    settle(runner, store, "p")
    store.set_step_input("p", "a", "b", 5, "test", "a different literal")
    settle(runner, store, "p", until=until_status(a="stale", b="stale"))


def test_manual_values_need_finished_upstreams_unless_forced(store, runner):
    create(store, "p", {"a": add(src("n"), d(0)), "b": add(src("a/sum"), d(1)),
                        "c": add(src("b/sum"), d(1))},
           inputs={"n": "int"})
    runner.tick()
    with pytest.raises(InvalidPlan) as e:
        store.set_output("p", "b", {"sum": 100}, "test", "known")
    assert e.value.errors == ["step a is pending"]
    assert statuses(store, "p")["b"] == "pending"
    store.set_output("p", "b", {"sum": 100}, "test", "bypass a", force=True)
    assert store.read_state("p")["steps"]["b"]["inputs_hash"] is None
    assert store.history("p")[-1]["force"] is True
    steps = settle(runner, store, "p", until=until_status(c="succeeded"))
    assert steps["c"]["outputs"] == {"sum": 101}
    assert statuses(store, "p") == {"a": "pending", "b": "succeeded", "c": "succeeded"}

    store.set_input("p", "n", 1, "test", "a can run now")
    steps = settle(runner, store, "p", until=until_status(a="succeeded", b="stale", c="stale"))

    # readers of a stale step wait
    store.patch("p", store.get("p")["rev"], [{"op": "add", "path": "/steps/d",
                                              "value": add(src("b/sum"), d(0))}], "test", "d")
    for _ in range(3):
        runner.tick()
    assert statuses(store, "p")["d"] == "pending"

    # retrying the stale step runs it again and clears it; c computed from 100 stays stale
    store.retry("p", "b", author="test", reason="recompute from a")
    steps = settle(runner, store, "p", until=until_status(b="succeeded", d="succeeded"))
    assert steps["b"]["outputs"] == {"sum": 2} and steps["b"].get("manual") is None
    assert steps["d"]["outputs"] == {"sum": 2}
    assert statuses(store, "p")["c"] == "stale"
    store.retry("p", "c", author="test", reason="recompute from b")
    steps = settle(runner, store, "p")
    assert statuses(store, "p") == dict.fromkeys("abcd", "succeeded")
    assert steps["c"]["outputs"] == {"sum": 3}


def test_a_manual_value_with_finished_upstreams_records_their_hash(store, runner):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(src("a/sum"), d(1))})
    settle(runner, store, "p")
    store.set_output("p", "b", {"sum": 7}, "test", "known")
    for _ in range(2):
        runner.tick()
    e = store.read_state("p")["steps"]["b"]
    assert (e["status"], e["outputs"]) == ("succeeded", {"sum": 7})
    assert e["inputs_hash"] == inputs_hash({"a": 2, "b": 1})


def test_only_failed_stale_or_manual_steps_can_be_retried(store, runner):
    create(store, "p", {"a": add(d(1), d(1))})
    settle(runner, store, "p")
    with pytest.raises(BadRequest, match="only a failed, stale or manually set step"):
        store.retry("p", "a", author="test", reason="no")


def test_status_and_views_show_stale_steps(store, runner):
    from sluice import views

    create(store, "p", {"a": add(src("n"), d(1)), "b": add(src("a/sum"), d(1))},
           inputs={"n": "int"})
    store.set_input("p", "n", 1, "test", "go")
    settle(runner, store, "p")
    store.set_input("p", "n", 2, "test", "again")
    settle(runner, store, "p", until=until_status(a="stale", b="stale"))
    assert [r["status"] for r in store.status("p")["steps"]] == ["stale", "stale"]
    assert store.projects()[0]["counts"] == {"stale": 2}
    text = views.render(store, "p", "mermaid")
    assert "  classDef stale " in text and "a / test.add / stale" in text
    assert text.count(" stale\n") >= 2  # both steps carry the stale class
    page = views.render(store, "p", "html")
    assert page.count('class="node card is-stale"') == 2 and "Its inputs changed" in page
