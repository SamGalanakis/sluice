"""Recipes and unit_add (SPEC §5): a named step shape in `recipes/<name>.json`, expanded with
tiny `{param}` substitution into one unit of steps, added in one plan edit."""

import json
import re
from pathlib import Path

import pytest

from sluice import log as L
from sluice import recipe as RC
from sluice.errors import BadRequest, InvalidPlan, NotFound
from tests.conftest import create, statuses, write_config

ROOT = Path(__file__).resolve().parents[1]
PACKS = ROOT / "packs"


def write_recipe(d: Path, name: str, steps: dict, params: dict | None = None,
                 doc: str = "", **extra) -> Path:
    d.mkdir(parents=True, exist_ok=True)
    path = d / f"{name}.json"
    path.write_text(json.dumps({"name": name, "doc": doc, "params": params or {},
                                "steps": steps, **extra}))
    return path


def recipe(steps: dict, params: dict | None = None) -> RC.Recipe:
    r = RC.Recipe("r", "global", Path("r.json"), params={"unit": RC.T.Prim("string")},
                  raw={"name": "r", "params": params or {}, "steps": steps})
    for name, form in (params or {}).items():
        r.params[name] = RC.T.parse(form)
    return r


def test_substitution_in_ids_and_nested_strings():
    r = recipe({"{unit}-work": {"run": "x.y", "doc": "Work on {unit} at {n} ({flag})",
                                "in": {"spec": {"default": ["see {unit}", {"deep": "{unit}!"}]},
                                       "{key}": {"default": "k"}}}},
               {"n": "int", "flag": "boolean", "key": "string"})
    steps, errs = RC.expand(r, {"unit": "u1", "n": 3, "flag": False, "key": "extra"})
    assert errs == []
    assert steps == {"u1-work": {
        "run": "x.y", "doc": "Work on u1 at 3 (false)",
        "in": {"spec": {"default": ["see u1", {"deep": "u1!"}]}, "extra": {"default": "k"}}}}


def test_a_whole_string_placeholder_keeps_its_type():
    r = recipe({"a": {"in": {"n": {"default": "{n}"}, "tags": "{tags}", "o": "{opt}",
                             "text": "{n}{n}"}}},
               {"n": "int", "tags": "string[]", "opt": "string?"})
    steps, errs = RC.expand(r, {"unit": "u", "n": 7, "tags": ["x", "y"]})
    assert errs == []
    assert steps["a"]["in"] == {"n": {"default": 7}, "tags": ["x", "y"], "o": None,
                                "text": "77"}


def test_doubled_braces_are_literal():
    r = recipe({"a": {"doc": "{{unit}} is {unit}; {{not a param}} and }}{{"}})
    steps, errs = RC.expand(r, {"unit": "u"})
    assert errs == [] and steps["a"]["doc"] == "{unit} is u; {not a param} and }{"


def test_an_unknown_param_or_a_lone_brace_is_an_error_naming_it():
    r = recipe({"a-{unit}": {"doc": "{nope} and {unit}", "in": {"x": "a { b"}}})
    _, errs = RC.expand(r, {"unit": "u"})
    assert errs == ["steps.a-u.doc: unknown param {nope}",
                    "steps.a-u.in.x: a lone '{'; write {{ for a literal brace"]


def test_params_are_checked_against_their_types():
    r = recipe({"{unit}": {"doc": "{n}"}}, {"n": "int", "kind": {"type": "enum",
                                                                  "symbols": ["a", "b"]}})
    _, errs = RC.expand(r, {"unit": "U 1", "n": "three", "kind": "c", "extra": 1})
    assert errs == [
        "params.extra: recipe r has no param extra (its params: unit, n, kind)",
        "params.unit: the unit's name, a string matching ^[a-z0-9][a-z0-9_-]*$; got 'U 1'",
        'params.n: expected int, got "three"',
        'params.kind: expected one of [a, b], got "c"']
    _, errs = RC.expand(r, {"n": 1, "kind": "a"})
    assert errs == [("params.unit: the unit's name, a string matching ^[a-z0-9][a-z0-9_-]*$; "
                     "got None")]
    _, errs = RC.expand(r, {"unit": "u", "kind": "a"})
    assert errs == ["params.n: required (int)"]


def test_a_project_recipe_shadows_a_global_one(store):
    create(store, "p", {})
    create(store, "q", {})
    write_recipe(store.home / "recipes", "lane", {"{unit}": {"run": "core.echo",
                                                             "in": {"value": {"default": 1}}}},
                 doc="global lane")
    write_recipe(store.home / "recipes", "other", {"{unit}": {"run": "core.echo",
                                                              "in": {"value": {"default": 2}}}},
                 params={"size": "int?"})
    write_recipe(store.project_dir("p") / "recipes", "lane",
                 {"{unit}-mine": {"run": "core.echo", "in": {"value": {"default": 3}}}},
                 doc="p's lane")
    assert store.recipes("p") == [
        {"name": "lane", "doc": "p's lane", "params": {"unit": "string"}, "scope": "project"},
        {"name": "other", "doc": "", "params": {"unit": "string", "size": "int?"},
         "scope": "global"}]
    assert store.recipes("q")[0] == {"name": "lane", "doc": "global lane",
                                     "params": {"unit": "string"}, "scope": "global"}
    assert store.unit_add("p", "lane", {"unit": "a"}, author="t")["steps"] == ["a-mine"]
    assert store.unit_add("q", "lane", {"unit": "a"}, author="t")["steps"] == ["a"]


def test_broken_recipes_are_reported_never_fatal(store):
    create(store, "p", {})
    d = store.home / "recipes"
    d.mkdir(parents=True)
    (d / "bad-json.json").write_text("{nope")
    (d / "list.json").write_text("[]")
    write_recipe(d, "shape", {}, params={"x": "strng", "Bad": "int"}, extra=1)
    (d / "named.json").write_text(json.dumps({"name": "other",
                                              "steps": {"a": {"doc": "{who}"}}}))
    write_recipe(d, "good", {"{unit}": {"run": "core.echo", "in": {"value": {"default": 1}}}})
    (d / "notes.txt").write_text("not a recipe")
    listing = {r["name"]: r for r in store.recipes("p")}
    assert listing["good"] == {"name": "good", "doc": "", "params": {"unit": "string"},
                               "scope": "global"}
    assert listing["bad-json"]["error"].startswith("bad JSON: ")
    assert listing["list"]["error"] == "expected an object {name, doc?, params?, steps}"
    assert listing["shape"]["error"] == (
        "extra: unknown key; params.x: unknown type 'strng'; params.Bad: param names match "
        "^[a-z0-9][a-z0-9_-]*$; steps: required, an object of step id -> step")
    assert listing["named"]["error"] == ("name: must be the file's name, 'named', got 'other'; "
                                         "steps.a.doc: unknown param {who}")
    assert set(listing) == {"good", "bad-json", "list", "shape", "named"}
    with pytest.raises(InvalidPlan) as e:
        store.unit_add("p", "named", {"unit": "u"}, author="t")
    assert e.value.errors[0] == ("recipe named (global): name: must be the file's name, "
                                 "'named', got 'other'")
    with pytest.raises(NotFound):
        store.unit_add("p", "missing", {"unit": "u"}, author="t")


def test_unit_add_is_one_edit_tagged_and_starts_when_ready(store, runner):
    create(store, "p", {"old": {"run": "core.echo", "in": {"value": {"default": 0}}}})
    write_recipe(store.home / "recipes", "pair", {
        "{unit}-a": {"run": "test.add", "in": {"a": {"default": "{n}"}, "b": {"default": 1}},
                     "tags": ["heavy"]},
        "{unit}-b": {"run": "test.add", "in": {"a": {"source": "{unit}-a/sum"},
                                               "b": {"default": 1}}}},
        params={"n": "int"})
    rev = store.get("p")["rev"]
    out = store.unit_add("p", "pair", {"unit": "one", "n": 5}, author="orch", reason="go")
    assert out == {"rev": rev + 1, "steps": ["one-a", "one-b"]}
    plan = store.get("p")
    assert plan["steps"]["one-a"]["tags"] == ["unit:one", "heavy"]
    assert plan["steps"]["one-b"]["tags"] == ["unit:one"]
    assert "paused" not in plan["steps"]["one-a"] and "paused" not in plan["steps"]["one-b"]
    edits = [h for h in store.history("p") if h["kind"] == "plan.edit"]
    assert edits[-1]["rev"] == rev + 1 and edits[-1]["author"] == "orch" and \
        edits[-1]["reason"] == "go"
    assert store.select_steps("p", tags=["unit:one"]) == ["one-a", "one-b"]
    runner.tick()
    assert statuses(store, "p")["one-a"] != "pending"  # ready, so started on the next tick

    drafted = store.unit_add("p", "pair", {"unit": "two", "n": 1}, start=False, author="orch")
    two = store.get("p")["steps"]
    assert two["two-a"]["paused"] is True and two["two-b"]["paused"] is True
    rec = L.read(store.home, "p", kinds=["plan.edit"])["records"][-1]
    assert rec["rev"] == drafted["rev"] and rec["reason"] == "add unit two (recipe pair)"
    runner.tick()
    assert statuses(store, "p")["two-a"] == "pending"  # drafted: held
    with pytest.raises(BadRequest) as e:  # the unit exists already: nothing is added
        store.unit_add("p", "pair", {"unit": "one", "n": 2}, author="orch")
    assert str(e.value) == "steps one-a, one-b already exist in the plan of project p"
    assert store.get("p")["rev"] == drafted["rev"]
    with pytest.raises(InvalidPlan) as e:  # expansion errors change nothing either
        store.unit_add("p", "pair", {"unit": "three", "n": "x"}, author="orch")
    assert e.value.errors == ['params.n: expected int, got "x"']
    write_recipe(store.home / "recipes", "bad-id", {"{unit}/x": {"run": "core.echo"}})
    with pytest.raises(InvalidPlan) as e:
        store.unit_add("p", "bad-id", {"unit": "u"}, author="orch")
    assert e.value.errors == ["steps.u/x: ids match ^[a-z0-9][a-z0-9_-]*$"]


def spec_recipe(name: str) -> dict:
    """The recipe of that name in a json block of SPEC.md."""
    for block in re.findall(r"```json\n(.*?)```", (ROOT / "SPEC.md").read_text(), re.DOTALL):
        doc = json.loads(block)
        if isinstance(doc, dict) and doc.get("name") == name and "params" in doc:
            return doc
    raise AssertionError(f"SPEC.md has no recipe {name}")


def test_the_spec_lane_recipe_expands_and_validates_with_the_packs(home, tmp_path):
    write_config(home, fn_dirs=[str(PACKS / "git"), str(PACKS / "agents")])
    from sluice.store import Store

    store = Store(home)
    lane = spec_recipe("lane")
    (home / "recipes").mkdir()
    (home / "recipes" / "lane.json").write_text(json.dumps(lane))
    create(store, "p", {})
    spec = tmp_path / "fix-login.md"
    out = store.unit_add("p", "lane", {"unit": "fix-login", "repo": "/src/app",
                                       "base": "origin/main", "spec": str(spec),
                                       "engine": "devin"}, author="orch")
    assert out["steps"] == ["fix-login-fork", "fix-login-work", "fix-login-cleanup"]
    _, plan = store.plan("p")
    work = plan.steps["fix-login-work"]
    assert work.fn.name == "agent.run" and work.sources["spec"].file == str(spec)
    assert work.sources["cwd"].refs[0].step == "fix-login-fork"
    assert plan.steps["fix-login-fork"].fn.name == "git.worktree"
    assert plan.steps["fix-login-cleanup"].after == ["fix-login-work"]
    assert all(s.tags[0] == "unit:fix-login" for s in plan.steps.values())
    with pytest.raises(InvalidPlan) as e:  # an engine the enum does not have
        store.unit_add("p", "lane", {"unit": "x", "repo": "/r", "base": "b", "spec": "/s",
                                     "engine": "gpt"}, author="orch")
    assert e.value.errors == [('params.engine: expected one of [devin, codex, claude], '
                               'got "gpt"')]


def test_the_tools_through_sluice_tool(store, monkeypatch, capsys):
    from sluice import cli

    monkeypatch.setenv("SLUICE_HOME", str(store.home))
    create(store, "p", {})
    write_recipe(store.home / "recipes", "one", {"{unit}-x": {
        "run": "core.echo", "in": {"value": {"default": "{v}"}}}}, params={"v": "Any"},
        doc="one echo")
    assert cli.main(["tool", "recipe_list", '{"project": "p"}']) == 0
    assert json.loads(capsys.readouterr().out) == [
        {"name": "one", "doc": "one echo", "params": {"unit": "string", "v": "Any"},
         "scope": "global"}]
    args = {"project": "p", "recipe": "one", "params": {"unit": "u", "v": [1]},
            "reason": "try it"}
    assert cli.main(["tool", "unit_add", json.dumps(args)]) == 0
    assert json.loads(capsys.readouterr().out) == {"rev": 3, "steps": ["u-x"]}
    assert store.get("p")["steps"]["u-x"] == {
        "run": "core.echo", "in": {"value": {"default": [1]}}, "tags": ["unit:u"]}
    assert cli.main(["tool", "unit_add", json.dumps(args)]) == 1
    assert json.loads(capsys.readouterr().err)["error"] == "bad_request"
