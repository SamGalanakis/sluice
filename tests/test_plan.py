import pytest

from sluice import plan as P
from sluice.registry import BUILTIN_DIR, Registry, load
from tests.conftest import TESTPACK


@pytest.fixture(scope="module")
def reg() -> Registry:
    return load({"builtin": [BUILTIN_DIR], "global": [TESTPACK]})


def v(doc, reg):
    return P.validate(doc, reg)[0]


def add(a, b):
    return {"run": "test.add", "in": {"a": a, "b": b}}


ONE = {"default": 1}


def test_a_valid_plan(reg):
    doc = {"inputs": {"n": "int", "words": "string[]", "note": "string?"},
           "outputs": {"total": {"source": "b/sum"}, "first": {"source": "split/parts.0"}},
           "steps": {
               "a": add({"source": "n"}, ONE),
               "b": add({"source": "a/sum"}, ONE),
               "split": {"run": "test.split", "in": {"text": {"default": "x y"}}},
               "each": {"run": "test.window", "scatter": "tag",
                        "in": {"seconds": {"default": 0}, "tag": {"source": "words"}}},
               "gate": {"run": "core.collect",
                        "in": {"items": {"source": ["a/sum", "each/end", "note"]}}},
               "first": {"run": "core.echo", "in": {"value": {"source": "each/start.0"}}},
           }}
    errs, plan = P.validate(doc, reg)
    assert errs == []
    assert plan.steps["b"].deps == ["a"]
    assert plan.steps["gate"].deps == ["a", "each"]
    assert str(plan.steps["each"].output_type("end")) == "float[]"


@pytest.mark.parametrize("doc, error", [
    # ids and shape
    ({"id": "p", "steps": {}}, "id: unknown key"),
    ({"label": "x", "steps": {}}, "label: unknown key"),
    ({"steps": {"B!": add(ONE, ONE)}}, "steps.B!: ids match"),
    ({"inputs": {"X": "int"}, "steps": {}}, "inputs.X: ids match"),
    ({"inputs": {"x": "nope"}, "steps": {}}, "inputs.x: unknown type 'nope'"),
    ({"steps": {"a": {**add(ONE, ONE), "when": 1}}}, "steps.a.when: unknown key"),
    ({"rev": 2, "steps": {}}, "rev: maintained by the store"),
    ({"nodes": {}}, "nodes: unknown key"),
    ({}, "steps: required"),
    ({"steps": {"a": add({"default": 1, "source": "x"}, ONE)}},
     'steps.a.in.a: expected {"default": ...} or {"source": ...}'),
    ({"steps": {"a": add({"source": "a.b/c"}, ONE)}}, "steps.a.in.a.source: bad ref 'a.b/c'"),
    # every run exists
    ({"steps": {"a": {"run": "no.such"}}}, "steps.a.run: unknown fn 'no.such'"),
    # inputs bound, none unknown
    ({"steps": {"a": {"run": "test.add", "in": {"a": ONE}}}},
     "steps.a.in.b: required input is not bound"),
    ({"steps": {"a": {"run": "test.add", "in": {"a": ONE, "b": ONE, "c": ONE}}}},
     "steps.a.in.c: fn test.add has no input c (only an open fn takes extra inputs)"),
    # refs name a plan input or a step output
    ({"steps": {"a": add({"source": "missing"}, ONE)}}, "steps.a.in.a: unknown plan input missing"),
    ({"steps": {"a": add({"source": "zz/sum"}, ONE)}}, "steps.a.in.a: unknown step zz"),
    ({"steps": {"a": add(ONE, ONE), "b": add({"source": "a/nope"}, ONE)}},
     "steps.b.in.a: step a (fn test.add) has no output nope"),
    ({"steps": {"a": add(ONE, ONE), "b": add({"source": "a/sum.x"}, ONE)}},
     "steps.b.in.a: a/sum.x: cannot read x of int"),
    ({"outputs": {"o": {"source": "zz/x"}}, "steps": {}}, "outputs.o: unknown step zz"),
    ({"outputs": {"o": {"default": 1}}, "steps": {}}, 'outputs.o: expected {"source": "<ref>"}'),
    # types fit
    ({"inputs": {"s": "string"}, "steps": {"a": add({"source": "s"}, ONE)}},
     "steps.a.in.a: s is string, which does not fit int: string is not int"),
    ({"inputs": {"s": "int?"}, "steps": {"a": add({"source": "s"}, ONE)}},
     "steps.a.in.a: s is int?, which does not fit int: int? is optional"),
    ({"steps": {"a": add({"default": "x"}, ONE)}}, 'steps.a.in.a: expected int, got "x"'),
    # fan-in: the target must be an array (or Any); each element fits the item type
    ({"steps": {"a": add(ONE, ONE), "b": add({"source": ["a/sum"]}, ONE)}},
     "steps.b.in.a: a list source needs an array or Any input, not int"),
    ({"inputs": {"s": "string"}, "steps": {"a": add(ONE, ONE), "t": {
        "run": "test.window", "scatter": "seconds",
        "in": {"seconds": {"source": ["a/sum", "s"]}}}}},
     "steps.t.in.seconds.source[1]: s is string, which does not fit float"),
    # scatter: the input receives an array whose items fit; outputs become arrays
    ({"steps": {"t": {"run": "test.window", "scatter": "seconds",
                      "in": {"seconds": {"default": 1}}}}},
     "steps.t.in.seconds: expected an array, got 1"),
    ({"inputs": {"xs": "string[]"}, "steps": {"t": {
        "run": "test.window", "scatter": "seconds", "in": {"seconds": {"source": "xs"}}}}},
     "steps.t.in.seconds: xs is string[], which does not fit float[]"),
    ({"steps": {"t": {"run": "test.window", "scatter": "tag", "in": {"seconds": ONE}}}},
     "steps.t.scatter: 'tag' is not a bound input of the step"),
    ({"steps": {"t": {"run": "test.window", "scatter": "tag",
                      "in": {"seconds": ONE, "tag": {"default": []}}},
                "u": add({"source": "t/end"}, ONE)}},
     "steps.u.in.a: t/end is float[], which does not fit int"),
    # acyclic
    ({"steps": {"a": add({"source": "b/sum"}, ONE), "b": add({"source": "a/sum"}, ONE)}},
     "steps.a: dependency cycle a -> b -> a"),
])
def test_each_rule_reports_its_path(reg, doc, error):
    errs = v(doc, reg)
    assert any(e.startswith(error) for e in errs), errs


def test_every_error_is_returned_at_once(reg):
    errs = v({"steps": {"a": {"run": "test.add", "in": {"a": {"default": "x"}}},
                        "b": {"run": "no.such"}, "c": add({"source": "zz"}, ONE)}}, reg)
    assert len(errs) == 4, errs


def test_refs_parse():
    assert P.parse_ref("repo") == (P.Ref("repo"), "")
    assert P.parse_ref("gate/items.0.x") == (P.Ref("items", "gate", ("0", "x")), "")
    assert P.parse_ref("a/b/c")[0] is None
    assert P.parse_ref("a..b")[0] is None
