from pathlib import Path

import pytest

from sluice import plan as P
from sluice.fns import Registry
from tests.conftest import TESTPACK
from tests.test_fns import write_fn


@pytest.fixture(scope="module")
def reg(tmp_path_factory) -> Registry:
    pack: Path = tmp_path_factory.mktemp("extra")
    write_fn(pack, "rec.head", {"name": "rec.head", "in": {},
                                "out": {"head": {"branch": "string", "sha": "string?"}}})
    write_fn(pack, "rec.use", {"name": "rec.use", "in": {"base": {"branch": "string",
                                                                  "sha": "string"}},
                               "out": {}})
    write_fn(pack, "rec.self", {"name": "rec.self", "in": {}, "out": {}, "graph": {
        "nodes": {"a": {"fn": "rec.self"}}, "out": {}}}, main=False)
    write_fn(pack, "rec.broken", {"name": "rec.broken", "in": {"x": "int"}, "out": {"y": "int"},
                                  "graph": {"nodes": {
                                      "a": {"fn": "core.echo", "in": {"value": {"from": "zz.v"}}},
                                      "b": {"fn": "test.add", "in": {"a": {"from": "$in.nope"}}},
                                  }, "out": {}}}, main=False)
    for i in range(10):  # deep.l0 wraps deep.l1 ... deep.l9 (a leaf wrapper)
        inner = f"deep.l{i + 1}" if i < 9 else "core.echo"
        binding = {"x": {"from": "$in.x"}} if i < 9 else {"value": {"from": "$in.x"}}
        write_fn(pack, f"deep.l{i}", {"name": f"deep.l{i}", "in": {"x": "int"}, "out": {},
                                      "graph": {"nodes": {"n": {"fn": inner, "in": binding}},
                                                "out": {}}}, main=False)
    return Registry.load([TESTPACK, pack])


def v(doc, reg, **kw):
    doc = {"id": "p", **doc}
    return P.validate(doc, reg, **kw)[0]


def add(a, b):
    return {"fn": "test.add", "in": {"a": a, "b": b}}


ONE = {"value": 1}


def test_a_valid_plan_has_no_errors(reg):
    doc = {"title": "t", "paused": False, "resources": {"db": 1}, "meta": {"k": 1}, "nodes": {
        "a": add(ONE, ONE),
        "b": {**add({"from": "a.sum"}, ONE), "after": ["a"], "claims": ["db"], "hold": False,
              "timeout": "5m", "note": "hi",
              "when": [{"from": "a.sum", "op": "eq", "value": 2},
                       {"from": "a.sum", "op": "in", "value": [1, 2]},
                       {"from": "a.sum", "op": "truthy"}]},
        "q": {"fn": "test.quad", "in": {"x": {"from": "b.sum"}}},
        "e": {"fn": "core.echo", "in": {"value": {"from": "q.y"}}},
        "inner": {"fn": "core.echo", "in": {"value": {"from": "q/t1/a.sum"}}},
        "f": {"fn": "test.echo_log", "in": {"msg": {"file": "notes/msg.txt"}}},
    }}
    assert v(doc, reg) == []


# ---- one failing case per §5.2 rule, with its path ---------------------------------------


@pytest.mark.parametrize("doc, error", [
    # 1. shape and ids
    ({"id": "Bad", "nodes": {}}, "id: plan ids match"),
    ({"nodes": {"B!": add(ONE, ONE)}}, "nodes.B!: node ids match"),
    ({"nodes": {"a": {**add(ONE, ONE), "colour": 1}}}, "nodes.a.colour: unknown key"),
    ({"nodes": {"a": add({"value": 1, "from": "x.y"}, ONE)}},
     "nodes.a.in.a: a binding is exactly one of"),
    ({"nodes": {"a": add({"from": "nodot"}, ONE)}}, "nodes.a.in.a.from: bad ref 'nodot'"),
    ({"nodes": {"a": {**add(ONE, ONE), "when": [{"from": "a.sum", "op": "gt", "value": 1}]}}},
     "nodes.a.when[0].op: expected one of"),
    ({"nodes": {"a": {**add(ONE, ONE), "timeout": "soon"}}}, "nodes.a.timeout: bad duration"),
    ({"rev": 3, "nodes": {}}, "rev: maintained by the store"),
    ({"resources": {"db": 0}, "nodes": {}}, "resources.db: expected a positive int"),
    ({"nodes": []}, "nodes: required, an object"),
    # 2. fn exists
    ({"nodes": {"a": {"fn": "nope.fn"}}}, "nodes.a.fn: unknown fn nope.fn"),
    # 3. composite expansion
    ({"nodes": {"a": {"fn": "rec.self"}}},
     "nodes.a: fn rec.self: graph.nodes.a: fn rec.self: composite rec.self contains itself"),
    ({"nodes": {"a": {"fn": "rec.broken", "in": {"x": ONE}}}},
     "nodes.a: fn rec.broken: graph.nodes.a.in.value: unknown node zz"),
    ({"nodes": {"a": {"fn": "rec.broken", "in": {"x": ONE}}}},
     "nodes.a: fn rec.broken: graph.nodes.b.in.a: the composite has no input port nope"),
    ({"nodes": {"a": {"fn": "rec.broken", "in": {"x": ONE}}}},
     "nodes.a: fn rec.broken: graph.out.y: required output port is not bound"),
    # 4. refs name an existing node and output port; fields navigate
    ({"nodes": {"b": add({"from": "zz.sum"}, ONE)}}, "nodes.b.in.a: unknown node zz"),
    ({"nodes": {"a": add(ONE, ONE), "b": add({"from": "a.nope"}, ONE)}},
     "nodes.b.in.a: node a (fn test.add) has no output port nope"),
    ({"nodes": {"a": add(ONE, ONE), "b": add({"from": "a.sum.x"}, ONE)}},
     "nodes.b.in.a: a.sum.x: cannot read field x of int"),
    ({"nodes": {"a": add(ONE, ONE), "b": {**add(ONE, ONE), "after": ["zz"]}}},
     "nodes.b.after[0]: unknown node zz"),
    ({"nodes": {"a": add(ONE, ONE),
                "b": {**add(ONE, ONE), "when": [{"from": "zz.sum", "op": "truthy"}]}}},
     "nodes.b.when[0].from: unknown node zz"),
    # 5. required ports bound, no unknown ports
    ({"nodes": {"a": {"fn": "test.add", "in": {"a": ONE}}}},
     "nodes.a.in.b: required input port is not bound"),
    ({"nodes": {"a": {"fn": "test.add", "in": {"a": ONE, "b": ONE, "c": ONE}}}},
     "nodes.a.in.c: fn test.add has no input port c"),
    # 6. types
    ({"nodes": {"a": add(ONE, ONE),
                "b": {"fn": "test.echo_log", "in": {"msg": {"from": "a.sum"}}}}},
     "nodes.b.in.msg: out type int does not fit string: int is not string"),
    ({"nodes": {"h": {"fn": "rec.head"}, "u": {"fn": "rec.use", "in": {"base":
                                                                         {"from": "h.head"}}}}},
     "nodes.u.in.base: out type {branch, sha?} does not fit {branch, sha}: sha is optional"),
    ({"nodes": {"a": add({"value": "x"}, ONE)}}, 'nodes.a.in.a: expected int, got "x"'),
    ({"nodes": {"a": add({"file": "n.txt"}, ONE)}},
     "nodes.a.in.a: a file binding needs a port that accepts string, not int"),
    ({"nodes": {"a": add(ONE, ONE),
                "b": {**add(ONE, ONE), "when": [{"from": "a.sum", "op": "eq", "value": "2"}]}}},
     'nodes.b.when[0].value: expected int, got "2"'),
    # 7. acyclic
    ({"nodes": {"a": add({"from": "b.sum"}, ONE), "b": add({"from": "a.sum"}, ONE)}},
     "nodes.a: dependency cycle a -> b -> a"),
    ({"nodes": {"a": {**add(ONE, ONE), "after": ["a"]}}}, "nodes.a: dependency cycle a -> a"),
    # 8. claims name declared resources
    ({"nodes": {"a": {**add(ONE, ONE), "claims": ["db"]}}},
     "nodes.a.claims[0]: unknown resource db"),
])
def test_each_rule_reports_its_path(reg, doc, error):
    errs = v(doc, reg)
    assert any(e.startswith(error) for e in errs), errs


def test_composites_nest_at_most_8_deep(reg):
    assert v({"nodes": {"a": {"fn": "deep.l2", "in": {"x": ONE}}}}, reg) == []  # 8 levels
    errs = v({"nodes": {"a": {"fn": "deep.l1", "in": {"x": ONE}}}}, reg)  # 9 levels
    assert len(errs) == 1
    assert errs[0].startswith("nodes.a: fn deep.l1: graph.nodes.n: fn deep.l2: graph.nodes.n")
    assert errs[0].endswith("fn deep.l9: composites nest deeper than 8")


def test_composite_inner_claims_are_checked_against_plan_resources(reg, tmp_path):
    write_fn(tmp_path, "rec.claimy", {"name": "rec.claimy", "in": {}, "out": {}, "graph": {
        "nodes": {"a": {"fn": "core.echo", "in": {"value": ONE}, "claims": ["gpu"]}},
        "out": {}}}, main=False)
    reg2 = Registry.load([TESTPACK, tmp_path])
    assert v({"nodes": {"c": {"fn": "rec.claimy"}}}, reg2) == [
        "nodes.c/a.claims[0]: unknown resource gpu"]
    assert v({"resources": {"gpu": 1}, "nodes": {"c": {"fn": "rec.claimy"}}}, reg2) == []


def test_every_error_is_returned_at_once(reg):
    errs = v({"nodes": {"a": {"fn": "test.add", "in": {"a": {"value": "x"}}},
                        "b": {"fn": "nope.fn"}, "c": {**add(ONE, ONE), "claims": ["db"]}}}, reg)
    assert len(errs) == 4, errs


def test_rule_9_protects_succeeded_nodes_with_running_dependents(reg):
    old = {"id": "p", "nodes": {"a": add(ONE, ONE), "b": add({"from": "a.sum"}, ONE),
                                "c": add(ONE, ONE)}}
    _, prev = P.validate(old, reg)
    state = {"nodes": {"a": {"status": "succeeded"}, "b": {"status": "running"},
                       "c": {"status": "succeeded"}}}
    deleted = {"id": "p", "nodes": {"b": add(ONE, ONE), "c": add(ONE, ONE)}}
    errs, _ = P.validate(deleted, reg, state=state, previous=prev)
    assert errs == ["nodes.a: cannot delete a succeeded node while its dependent b is running"]
    rewired = {"id": "p", "nodes": {"a": add({"value": 5}, ONE),
                                    "b": add({"from": "a.sum"}, ONE), "c": add(ONE, ONE)}}
    errs, _ = P.validate(rewired, reg, state=state, previous=prev)
    assert errs == ["nodes.a: cannot rewire a succeeded node while its dependent b is running"]
    # c has no running dependent, and a note edit is not a rewire
    ok = {"id": "p", "nodes": {"a": {**add(ONE, ONE), "note": "x"},
                               "b": add({"from": "a.sum"}, ONE)}}
    assert P.validate(ok, reg, state=state, previous=prev)[0] == []
    # once b is done, a may change
    state["nodes"]["b"]["status"] = "succeeded"
    assert P.validate(rewired, reg, state=state, previous=prev)[0] == []


# ---- expansion ----------------------------------------------------------------------------


def test_expansion_ids_substitution_and_deps(reg):
    doc = {"id": "p", "nodes": {
        "a": add(ONE, ONE),
        "gate": add(ONE, ONE),
        "q": {"fn": "test.quad", "in": {"x": {"from": "a.sum"}}, "after": ["gate"],
              "hold": True},
        "t": {"fn": "test.twice", "in": {"x": {"value": 7}}},
        "e": {"fn": "core.echo", "in": {"value": {"from": "q.y"}}},
    }}
    errs, exp = P.validate(doc, reg)
    assert errs == []
    assert list(exp.nodes) == ["a", "gate", "q", "q/t1", "q/t1/a", "q/t1/b", "q/t2", "q/t2/a",
                               "q/t2/b", "t", "t/a", "t/b", "e"]
    inner = exp.nodes["q/t1/a"]
    # $in.x of the nested composite resolves through q's binding to a.sum
    assert inner.bindings["a"] == P.Binding("from", ref=P.Ref("a", "sum"))
    assert exp.nodes["q/t2/a"].bindings["a"] == P.Binding("from", ref=P.Ref("q/t1", "y"))
    # a literal outer binding becomes a literal inner binding
    assert exp.nodes["t/a"].bindings["b"] == P.Binding("value", value=7)
    # inner nodes inherit the composite's dependencies and hold
    assert {"a", "gate"} <= set(exp.nodes["q/t2/b"].deps)
    assert exp.nodes["q/t2/b"].hold and not exp.nodes["t/b"].hold
    assert exp.nodes["q"].children == ["q/t1", "q/t2"]
    assert exp.nodes["q"].outs["y"] == P.Binding("from", ref=P.Ref("q/t2", "y"))
    assert exp.leaves_under("q") == ["q/t1/a", "q/t1/b", "q/t2/a", "q/t2/b"]
    assert exp.ancestors("q/t1/a") == ["q/t1", "q"]
    # topological order: dependencies first
    order = exp.order
    assert order.index("a") < order.index("q/t1/a") < order.index("q/t1/b") < order.index("e")
    assert order.index("q/t2/b") < order.index("q") < order.index("e")


def test_def_sha_tracks_inputs_not_notes(reg):
    def sha(node):
        return P.validate({"id": "p", "nodes": {"a": node}}, reg)[1].nodes["a"].def_sha
    base = add(ONE, ONE)
    assert sha(base) == sha({**base, "note": "n", "hold": True, "timeout": "1m"})
    assert sha(base) != sha(add(ONE, {"value": 2}))
