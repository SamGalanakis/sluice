import pytest

from sluice import types as T
from sluice.util import parse_duration


def p(form):
    return T.parse(form)


# ---- parse ------------------------------------------------------------------------------


def test_parse_forms():
    assert p("string?") == T.Optional(T.Prim("string"))
    assert p(["a", "b"]) == T.Enum(("a", "b"))
    assert p({"list": "int"}) == T.List(T.Prim("int"))
    assert p({"optional": {"optional": "int"}}) == T.Optional(T.Prim("int"))
    # a record whose single field is named like a special form needs {"record": ...}
    assert p({"record": {"list": "int"}}) == T.Record((("list", T.Prim("int")),))
    assert p({"list": "int", "n": "int"}) == T.Record((("list", T.Prim("int")),
                                                       ("n", T.Prim("int"))))
    u = p({"union": {"ok": {"sha": "string"}, "err": {"msg": "string"}}})
    assert isinstance(u, T.Union) and u.variant("err") == T.Record((("msg", T.STRING),))


@pytest.mark.parametrize("form, where", [
    ("str", "type: unknown type 'str'"),
    ([], "type: an enum is a non-empty list of strings"),
    (["a", "a"], "type: enum values repeat"),
    ({"union": {"x": "int"}}, "type.union.x: a union variant must be a record"),
    ({"f": {"list": "nope"}}, "type.f.list: unknown type 'nope'"),
    (3, "type: not a type"),
])
def test_parse_errors_name_the_path(form, where):
    with pytest.raises(T.TypeSyntaxError) as e:
        T.parse(form)
    assert str(e.value).startswith(where)


# ---- fits -------------------------------------------------------------------------------


@pytest.mark.parametrize("out, inp", [
    ("int", "int"),
    ("int", "float"),
    ("string", "any"),
    ("any", "int"),
    ("int", "int?"),
    ("int?", "int?"),
    (["a"], ["a", "b"]),
    (["a", "b"], "string"),
    ({"list": "int"}, {"list": "float"}),
    ({"map": ["x"]}, {"map": "string"}),
    ({"branch": "string", "sha": "string", "extra": "int"}, {"branch": "string", "sha": "string?"}),
    ({"branch": "string"}, {"branch": "string", "sha": "string?"}),
    ({"union": {"a": {"x": "int"}}}, {"union": {"a": {"x": "float"}, "b": {}}}),
])
def test_fits(out, inp):
    ok, reason = T.fits(p(out), p(inp))
    assert ok, reason


@pytest.mark.parametrize("out, inp, reason", [
    ("float", "int", "float is not int"),
    ("string?", "string", "string? is optional"),
    (["a", "c"], ["a", "b"], "c not in [a, b]"),
    ("string", ["a"], "string is not [a]"),
    ({"list": "string"}, {"list": "int"}, "[]: string is not int"),
    ({"branch": "string", "sha": "string?"}, {"branch": "string", "sha": "string"},
     "sha is optional"),
    ({"branch": "string"}, {"branch": "string", "sha": "string"}, "sha is missing"),
    ({"a": {"b": "int"}}, {"a": {"b": "string"}}, "a.b: int is not string"),
    ({"union": {"a": {}, "c": {}}}, {"union": {"a": {}}}, "tag c is not accepted"),
    ({"x": "int"}, {"list": "int"}, "{x} is not list<int>"),
])
def test_does_not_fit(out, inp, reason):
    ok, why = T.fits(p(out), p(inp))
    assert not ok
    assert why == reason


# ---- check_value ------------------------------------------------------------------------


@pytest.mark.parametrize("form, value", [
    ("int", 3),
    ("float", 3),
    ("float", 2.5),
    ("bool", False),
    ("any", {"x": [1]}),
    ("string?", None),
    (["done", "blocked"], "done"),
    ({"list": "int"}, [1, 2]),
    ({"map": "bool"}, {"a": True}),
    ({"a": "int", "b": "string?"}, {"a": 1}),
    ({"a": "int"}, {"a": 1, "extra": "fine"}),
    ({"union": {"ok": {"sha": "string"}, "err": {}}}, {"kind": "ok", "sha": "abc"}),
])
def test_check_value_accepts(form, value):
    assert T.check_value(p(form), value) == []


@pytest.mark.parametrize("form, value, errors", [
    ("int", True, ["expected int, got true"]),
    ("int", 1.5, ["expected int, got 1.5"]),
    ("bool", 0, ["expected bool, got 0"]),
    ("string", None, ["expected string, got null"]),
    ({"report": {"outcome": ["done", "blocked"]}}, {"report": {"outcome": "ok"}},
     ['report.outcome: expected one of [done, blocked], got "ok"']),
    ({"a": "int", "b": "string"}, {"b": 2}, ["a: missing required field",
                                             "b: expected string, got 2"]),
    ({"xs": {"list": "int"}}, {"xs": [1, "2"]}, ['xs[1]: expected int, got "2"']),
    ({"m": {"map": "int"}}, {"m": {"k": "v"}}, ['m.k: expected int, got "v"']),
    ({"union": {"ok": {"sha": "string"}}}, {"kind": "nope"},
     ['kind: expected one of [ok], got "nope"']),
    ({"union": {"ok": {"sha": "string"}}}, {"kind": "ok"}, ["sha: missing required field"]),
    ({"a": "int"}, [1], ["expected an object {a}, got [1]"]),
])
def test_check_value_rejects_with_paths(form, value, errors):
    assert T.check_value(p(form), value) == errors


def test_check_value_prefixes_a_base_path():
    assert T.check_value(p("int"), "x", "nodes.a.in.n") == ['nodes.a.in.n: expected int, got "x"']


# ---- navigation -------------------------------------------------------------------------


def test_navigate_records_optionals_maps_and_any():
    t = p({"head": {"branch": "string", "sha": "string?"}, "meta": "any",
           "opt": {"optional": {"x": "int"}}, "m": {"map": "int"},
           "u": {"union": {"a": {}, "b": {}}}})
    assert T.navigate(t, ["head", "branch"]) == (T.STRING, "")
    assert T.navigate(t, ["head", "sha"]) == (T.Optional(T.STRING), "")
    assert T.navigate(t, ["meta", "deep", "er"]) == (T.ANY, "")
    assert T.navigate(t, ["opt", "x"]) == (T.Optional(T.Prim("int")), "")
    assert T.navigate(t, ["m", "anything"]) == (T.Optional(T.Prim("int")), "")
    assert T.navigate(t, ["u", "kind"]) == (T.Enum(("a", "b")), "")
    assert T.navigate(t, ["head", "nope"]) == (None, "no field nope in {branch, sha?}")
    assert T.navigate(t, ["head", "branch", "x"]) == (None, "cannot read field x of string")


def test_navigate_value():
    v = {"a": {"b": 1}, "n": None}
    assert T.navigate_value(v, ["a", "b"]) == 1
    assert T.navigate_value(v, ["n", "x"]) is None
    assert T.navigate_value(v, ["missing"]) is None


# ---- durations --------------------------------------------------------------------------


def test_durations():
    assert parse_duration("90s") == 90
    assert parse_duration("30m") == 1800
    assert parse_duration("3h") == 10800
    for bad in ("1d", "1.5h", "h", "", 5, None, "10 s"):
        with pytest.raises(ValueError):
            parse_duration(bad)
