"""Typed agent blocks (SPEC §5): a step running an open fn binds extra inputs and declares
outputs of its own, which the agent submits with step_submit while the step runs."""

import json
import sys

import pytest

from sluice import plan as P
from sluice.errors import BadRequest, InvalidPlan, NotFound
from sluice.registry import BUILTIN_DIR, load
from tests.conftest import TESTPACK, create, d, settle, src

REG = load({"builtin": [BUILTIN_DIR], "global": [TESTPACK]})


def block(outputs=None, attempts=None, **extra):
    step = {"run": "test.open", "in": {**extra}}
    if attempts is not None:
        step["in"]["attempts"] = d(attempts)
    return {**step, "outputs": outputs} if outputs is not None else step


def validate(steps, **doc):
    return P.validate({"inputs": {}, "outputs": {}, "steps": steps, **doc}, REG)


@pytest.fixture(autouse=True)
def test_python(monkeypatch):
    monkeypatch.setenv("TEST_PYTHON", sys.executable)


# ---- validation --------------------------------------------------------------------------

def test_extra_inputs_take_their_sources_types_and_declared_outputs_are_outputs():
    report = {"type": "record", "fields": {"ok": "boolean", "notes": "string?"}}
    errs, plan = validate({
        "a": block({"word": "string", "report": {"type": report, "doc": "What was found"}},
                   topic=src("topic"), n=d(3)),
        "b": block(word=src("a/word"), ok=src("a/report.ok"), both=src(["a/word", "topic"]),
                   mixed=src(["a/word", "a/report"])),
        "c": {"run": "test.add", "in": {"a": src("count"), "b": src("b/results.0")}},
    }, inputs={"topic": "string", "count": "int"})
    assert errs == []
    a, b = plan.steps["a"], plan.steps["b"]
    assert {k: str(t) for k, t in a.extra.items()} == {"topic": "string", "n": "Any"}
    assert {k: str(t) for k, t in b.extra.items()} == {
        "word": "string", "ok": "boolean", "both": "string[]", "mixed": "Any[]"}
    assert a.output_docs == {"report": "What was found"}
    assert set(a.outputs) == {"ports", "extra", "results", "word", "report"}
    assert a.ports() == {
        "inputs": {"topic": {"type": "string"}, "n": {"type": "Any"}},
        "outputs": {"word": {"type": "string", "doc": ""},
                    "report": {"type": report, "doc": "What was found"}}}


def test_refs_to_declared_outputs_are_type_checked():
    errs, _ = validate({
        "a": block({"word": "string", "n": "int?"}),
        "b": {"run": "test.add", "in": {"a": src("a/word"), "b": src("a/n")}},
        "c": block(x=src("a/nope"), y=src("a/word.len")),
    })
    assert errs == [
        "steps.b.in.a: a/word is string, which does not fit int: string is not int",
        "steps.b.in.b: a/n is int?, which does not fit int: int? is optional",
        "steps.c.in.x: step a (fn test.open) has no output nope",
        "steps.c.in.y: a/word.len: cannot read len of string",
    ]


def test_scattered_open_step_types_the_item_and_its_outputs_are_arrays():
    errs, plan = validate({
        "a": {**block({"word": "string"}, q=src("qs")), "scatter": "q"},
        "b": block(words=src("a/word")),
        "c": {**block(q=src("one")), "scatter": "q"},
    }, inputs={"qs": "string[]", "one": "string"})
    assert errs == ["steps.c.in.q: the scatter input needs an array, not string"]
    assert str(plan.steps["a"].extra["q"]) == "string"
    assert str(plan.steps["b"].extra["words"]) == "string[]"


def test_only_open_fns_take_extra_inputs_and_outputs():
    errs, _ = validate({
        "a": {"run": "test.add", "in": {"a": d(1), "b": d(2), "c": d(3)},
              "outputs": {"x": "string"}},
        "b": block({"results": "string", "Bad": "string", "t": "strng"}, **{"Bad-in": d(1)}),
        "c": block(["not", "an", "object"]),
    })
    assert errs == [
        "steps.a.in.c: fn test.add has no input c (only an open fn takes extra inputs)",
        ("steps.a.outputs: fn test.add is not open; only a step running an open fn declares "
         "outputs"),
        "steps.b.in.Bad-in: extra input names match ^[a-z0-9][a-z0-9_-]*$",
        "steps.b.outputs.Bad: ids match ^[a-z0-9][a-z0-9_-]*$",
        "steps.b.outputs.results: fn test.open already has an output results",
        "steps.b.outputs.t: unknown type 'strng'",
        "steps.c.outputs: expected an object",
    ]


def test_open_shows_in_fn_list(store):
    fns = {f["name"]: f for f in store.registry(None).listing()}
    assert fns["test.open"]["open"] is True and "open" not in fns["test.add"]


# ---- running -----------------------------------------------------------------------------

def records(store, project, kind):
    from sluice import log as L
    return L.read(store.log_dir(project), kinds=[kind])["records"]


def test_submitted_outputs_join_the_steps_outputs(store, runner):
    create(store, "p", {
        "a": block({"word": "string", "n": {"type": "int", "doc": "How many"}},
                   attempts=[{"word": 1}, {"word": "hi", "n": 2}], topic=src("topic")),
        "b": {"run": "core.echo", "in": {"value": src("a/word")}},
    }, inputs={"topic": "string"})
    store.set_input("p", "topic", "cats", "t", "t")
    steps = settle(runner, store, "p")
    assert steps["a"]["status"] == "succeeded", steps["a"].get("error")
    out = steps["a"]["outputs"]
    assert (out["word"], out["n"]) == ("hi", 2)
    assert out["extra"] == {"topic": "cats"}
    assert out["ports"] == {"inputs": {"topic": {"type": "string"}},
                            "outputs": {"word": {"type": "string", "doc": ""},
                                        "n": {"type": "int", "doc": "How many"}}}
    bad, good = out["results"]
    assert bad["code"] == 1 and bad["out"]["error"] == "invalid"
    assert bad["out"]["errors"] == ["outputs.word: expected string, got 1",
                                    "outputs.n: missing required field"]
    [run_id] = steps["a"]["run_ids"]
    assert good == {"code": 0, "out": {"ok": True, "run": run_id}}
    assert steps["b"]["outputs"] == {"value": "hi"}
    [rec] = records(store, "p", "step.submit")
    assert (rec["step"], rec["run"], rec["outputs"]) == ("a", run_id, {"word": "hi", "n": 2})
    run_dir = store.runs_dir("p") / run_id
    assert json.loads((run_dir / "submitted.json").read_text()) == {"word": "hi", "n": 2}


def test_a_resubmit_replaces_the_last(store, runner):
    create(store, "p", {"a": block({"word": "string"},
                                   attempts=[{"word": "one"}, {"word": "two"}])})
    steps = settle(runner, store, "p")
    assert steps["a"]["outputs"]["word"] == "two"
    assert [r["outputs"] for r in records(store, "p", "step.submit")] == [
        {"word": "one"}, {"word": "two"}]


def test_a_step_whose_agent_did_not_submit_fails_naming_what_is_missing(store, runner):
    create(store, "p", {"a": block({"word": "string", "sha": "string", "note": "string?"},
                                   attempts=[])})
    steps = settle(runner, store, "p")
    assert steps["a"]["status"] == "failed"
    assert steps["a"]["error"] == ("declared outputs not submitted: word, sha (the agent must call "
                                   "step_submit with them before it finishes)")


def test_optional_declared_outputs_may_be_left_out(store, runner):
    create(store, "p", {"a": block({"note": "string?"}, attempts=[])})
    steps = settle(runner, store, "p")
    assert steps["a"]["status"] == "succeeded"
    assert steps["a"]["outputs"]["note"] is None


def test_each_scattered_run_submits_its_own(store, runner):
    create(store, "p", {"a": {**block({"word": "string"}, q=src("qs")), "scatter": "q"}},
           inputs={"qs": "string[]"})
    store.set_input("p", "qs", ["x", "y"], "t", "t")
    # every run submits its own item's word; test.open gets the item as its extra input q
    store.patch("p", store.get("p")["rev"], [
        {"op": "add", "path": "/steps/a/in/attempts", "value": d([{"word": "w"}])}], "t", "t")
    steps = settle(runner, store, "p")
    assert steps["a"]["status"] == "succeeded", steps["a"].get("error")
    assert steps["a"]["outputs"]["word"] == ["w", "w"]
    assert [e["q"] for e in steps["a"]["outputs"]["extra"]] == ["x", "y"]
    assert len(records(store, "p", "step.submit")) == 2


def test_submit_is_refused_unless_the_step_runs_and_declares_outputs(store, runner):
    create(store, "p", {"a": block({"word": "string"}, attempts=[]),
                        "b": block(attempts=[])})
    with pytest.raises(BadRequest, match="step a is pending; outputs are submitted while it"):
        store.submit("p", "a", {"word": "x"})
    with pytest.raises(BadRequest, match="step b declares no outputs"):
        store.submit("p", "b", {})
    with pytest.raises(NotFound):
        store.submit("p", "zz", {})
    state = store.read_state("p")
    state["steps"]["a"] = {"status": "running", "run_ids": ["r1", "r2"]}
    store.write_state("p", state)
    with pytest.raises(BadRequest, match="has 2 runs; pass run"):
        store.submit("p", "a", {"word": "x"})
    with pytest.raises(NotFound, match="no run 'r3'"):
        store.submit("p", "a", {"word": "x"}, "r3")
    with pytest.raises(InvalidPlan) as e:
        store.submit("p", "a", {"word": "x", "results": [], "more": 1}, "r1")
    assert e.value.errors == [
        "outputs.results: step a declares no output results (the fn returns that one itself)",
        "outputs.more: step a declares no output more"]
    assert store.submit("p", "a", {"word": "x"}, "r2") == {"ok": True, "run": "r2"}


# ---- outputs an open fn's agent submits on every step (fn.json `submits`) ------------------

def summing_fn(root):
    """test.summed: test.open, but its fn.json says its agent submits a `summary` on every
    step (and an optional `notes`)."""
    main = (TESTPACK / "test.open" / "main.py").read_text()
    spec = json.loads((TESTPACK / "test.open" / "fn.json").read_text())
    spec.update(name="test.summed", submits={
        "summary": {"type": "string", "doc": "What the agent did"}, "notes": "string?"})
    fn_dir = root / "test.summed"
    fn_dir.mkdir(parents=True)
    (fn_dir / "fn.json").write_text(json.dumps(spec))
    (fn_dir / "main.py").write_text(main)
    return fn_dir


def test_an_open_fn_declares_what_its_agent_submits_on_every_step(store, runner):
    store.create_project("p", "", "t", "t")
    summing_fn(store.project_dir("p") / "fns")
    fns = {f["name"]: f for f in store.registry("p").listing()}
    assert fns["test.summed"]["submits"]["summary"] == {"type": "string",
                                                         "doc": "What the agent did"}
    create_steps = {
        "a": {"run": "test.summed", "in": {"attempts": d([{"summary": "done it"}])},
              "outputs": {"word": "string?"}},  # its own outputs add to the fn's
        "b": {"run": "core.echo", "in": {"value": src("a/summary")}},
        "c": {"run": "test.summed", "in": {"attempts": d([])}},
    }
    ops = [{"op": "replace", "path": "/steps", "value": create_steps}]
    store.patch("p", 1, ops, "t", "t")
    step = store.plan("p")[1].steps["a"]
    assert set(step.declared) == {"summary", "notes", "word"}
    assert step.output_docs["summary"] == "What the agent did"
    steps = settle(runner, store, "p")
    assert steps["a"]["status"] == "succeeded", steps["a"].get("error")
    assert steps["a"]["outputs"]["summary"] == "done it"
    assert steps["a"]["outputs"]["ports"]["outputs"]["summary"] == {
        "type": "string", "doc": "What the agent did"}  # the agent is told to submit it
    assert steps["b"]["outputs"] == {"value": "done it"}
    assert steps["c"]["status"] == "failed"  # required on every step running the fn
    assert steps["c"]["error"].startswith("declared outputs not submitted: summary ")


def test_submits_is_for_open_fns_and_names_new_outputs(tmp_path):
    from sluice.registry import parse_fn
    base = {"name": "x.y", "inputs": {}, "outputs": {"final": "string"}}
    _, errs = parse_fn({**base, "submits": {"summary": "string"}}, tmp_path, check_dir=False)
    assert errs == ["submits needs open: true (an open fn's agent submits outputs)"]
    _, errs = parse_fn({**base, "open": True, "submits": {"final": "string", "n": "nope"}},
                       tmp_path, check_dir=False)
    assert errs[0] == "submits.final: already an output of the fn" and "submits.n" in errs[1]
    # a {"doc": ...} with no "type" reads the same for a submit and a plan input
    _, errs = parse_fn({**base, "open": True, "submits": {"summary": {"doc": "x"}}},
                       tmp_path, check_dir=False)
    assert errs == ["submits.summary.type: required"]
    errs, _ = validate({}, inputs={"x": {"doc": "x"}})
    assert errs == ["inputs.x.type: required"]
    fn, errs = parse_fn({**base, "open": True, "submits": {"summary": "string"}}, tmp_path,
                        check_dir=False)
    assert errs == [] and str(fn.submits["summary"]) == "string"
    reg = load({"builtin": [BUILTIN_DIR], "global": [TESTPACK, summing_fn(tmp_path).parent]})
    errs, _ = P.validate({"inputs": {}, "outputs": {}, "steps": {
        "a": {"run": "test.summed", "in": {}, "outputs": {"summary": "string"}}}}, reg)
    assert errs == ["steps.a.outputs.summary: fn test.summed already has an output summary"]
