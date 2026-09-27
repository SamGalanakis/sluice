import pytest

from sluice import types as T


def p(form):
    return T.parse(form)


STRING, INT = T.Prim("string"), T.Prim("int")


@pytest.mark.parametrize("form, expected", [
    ("string", STRING),
    ("Any", T.ANY),
    ("string?", T.Optional(STRING)),
    (["null", "int"], T.Optional(INT)),
    (["int", "null"], T.Optional(INT)),
    ("string[]", T.List(STRING)),
    ("string[]?", T.Optional(T.List(STRING))),
    ("string?[]", T.List(T.Optional(STRING))),
    ({"type": "array", "items": "int"}, T.List(INT)),
    ({"type": "enum", "symbols": ["a", "b"]}, T.Enum(("a", "b"))),
    (["null", {"type": "enum", "symbols": ["a"]}], T.Optional(T.Enum(("a",)))),
    ({"type": "record", "fields": {"a": "int", "b": "string?"}},
     T.Record((("a", INT), ("b", T.Optional(STRING))))),
    ("Any?", T.Optional(T.ANY)),
])
def test_parse_cwl_spellings(form, expected):
    assert p(form) == expected


@pytest.mark.parametrize("form, error", [
    ("str", "type: unknown type 'str'"),
    ("bool", "type: unknown type 'bool'"),
    ({"type": "enum", "symbols": []}, "type.symbols: a non-empty list of distinct strings"),
    ({"type": "record", "fields": {"a": "nope"}}, "type.a: unknown type 'nope'"),
    ({"list": "int"}, 'type: not a type: {"list": "int"}'),
    (["int", "string"], 'type: not a type: ["int", "string"]'),
    (3, "type: not a type: 3"),
])
def test_parse_errors_name_the_path(form, error):
    with pytest.raises(T.TypeSyntaxError) as e:
        p(form)
    assert str(e.value) == error


REC = {"type": "record", "fields": {"branch": "string", "sha": "string?"}}
REC_REQ = {"type": "record", "fields": {"branch": "string", "sha": "string"}}


@pytest.mark.parametrize("out, inp", [
    ("int", "int"), ("int", "float"), ("string", "Any"), ("Any", "int"), ("int", "int?"),
    ("int?", "int?"), ("int[]", "float[]"),
    ({"type": "enum", "symbols": ["a"]}, {"type": "enum", "symbols": ["a", "b"]}),
    ({"type": "enum", "symbols": ["a"]}, "string"),
    ({"type": "record", "fields": {"branch": "string", "sha": "string", "x": "int"}}, REC),
    ({"type": "record", "fields": {"branch": "string"}}, REC),
])
def test_fits(out, inp):
    ok, reason = T.fits(p(out), p(inp))
    assert ok, reason


@pytest.mark.parametrize("out, inp, reason", [
    ("float", "int", "float is not int"),
    ("string?", "string", "string? is optional"),
    ({"type": "enum", "symbols": ["a", "c"]}, {"type": "enum", "symbols": ["a", "b"]},
     "c not in [a, b]"),
    ("string", {"type": "enum", "symbols": ["a"]}, "string is not [a]"),
    ("string[]", "int[]", "[]: string is not int"),
    (REC, REC_REQ, "sha is optional"),
    ({"type": "record", "fields": {"branch": "string"}}, REC_REQ, "sha is missing"),
    ("int", "int[]", "int is not int[]"),
])
def test_does_not_fit(out, inp, reason):
    assert T.fits(p(out), p(inp)) == (False, reason)


@pytest.mark.parametrize("form, value", [
    ("int", 3), ("float", 3), ("float", 2.5), ("boolean", False), ("Any", {"x": [1]}),
    ("Any?", None), ("string?", None), ({"type": "enum", "symbols": ["done"]}, "done"),
    ("int[]", [1, 2]), (REC, {"branch": "b"}), (REC, {"branch": "b", "extra": 1}),
])
def test_check_value_accepts(form, value):
    assert T.check_value(p(form), value) == []


@pytest.mark.parametrize("form, value, errors", [
    ("int", True, ["expected int, got true"]),
    ("int", 1.5, ["expected int, got 1.5"]),
    ("boolean", 0, ["expected boolean, got 0"]),
    ("Any", None, []),
    ("string", None, ["expected string, got null"]),
    ({"type": "record", "fields": {"report": {"type": "record", "fields": {
        "outcome": {"type": "enum", "symbols": ["done", "blocked"]}}}}},
     {"report": {"outcome": "ok"}}, ['report.outcome: expected one of [done, blocked], got "ok"']),
    (REC_REQ, {"sha": 2}, ["branch: missing required field", "sha: expected string, got 2"]),
    ("int[]", [1, "2"], ['[1]: expected int, got "2"']),
    ("int[]", "12", ['expected an array, got "12"']),
    (REC, [1], ["expected an object {branch, sha?}, got [1]"]),
])
def test_check_value_rejects_with_paths(form, value, errors):
    assert T.check_value(p(form), value) == errors


def test_check_value_prefixes_a_base_path():
    assert T.check_value(INT, "x", "steps.a.in.n") == ['steps.a.in.n: expected int, got "x"']


def test_navigate_types_and_values():
    t = p({"type": "record", "fields": {"head": REC, "meta": "Any", "tags": "string[]",
                                        "opt": ["null", {"type": "record",
                                                         "fields": {"x": "int"}}]}})
    assert T.navigate(t, ["head", "branch"]) == (STRING, "")
    assert T.navigate(t, ["head", "sha"]) == (T.Optional(STRING), "")
    assert T.navigate(t, ["meta", "deep", "0"]) == (T.ANY, "")
    assert T.navigate(t, ["tags", "0"]) == (STRING, "")
    assert T.navigate(t, ["opt", "x"]) == (T.Optional(INT), "")
    assert T.navigate(t, ["head", "nope"]) == (None, "cannot read nope of {branch, sha?}")
    assert T.navigate(t, ["tags", "x"]) == (None, "cannot read x of string[]")
    v = {"a": {"b": [10, 20]}, "n": None}
    assert T.navigate_value(v, ["a", "b", "1"]) == 20
    assert T.navigate_value(v, ["a", "b", "5"]) is None
    assert T.navigate_value(v, ["n", "x"]) is None


def test_a_digit_that_is_not_decimal_is_not_an_index():
    # "²".isdigit() but not .isdecimal(): int() would reject it
    assert T.navigate(p("string[]"), ["²"]) == (None, "cannot read ² of string[]")
    assert T.navigate_value(["a", "b"], ["²"]) is None


@pytest.mark.parametrize("form", [
    "string", "Any", "int?", "string[]", "float[]?",
    ["null", {"type": "enum", "symbols": ["a", "b"]}],
    {"type": "array", "items": {"type": "record", "fields": {"a": "int", "b": "string?"}}},
    {"type": "array", "items": ["null", {"type": "enum", "symbols": ["x"]}]},
])
def test_form_spells_a_type_back(form):
    assert T.form(T.parse(form)) == form
    assert T.parse(T.form(T.parse({"type": "array", "items": "int"}))) == T.List(T.Prim("int"))
