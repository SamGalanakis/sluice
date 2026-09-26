"""verify (SPEC §6a): one failing case per check, each with its expected `where`."""

import json

import pytest

from sluice.errors import NotFound
from sluice.store import Store
from sluice.verify import verify
from tests.conftest import create, write_fn


def where(store, project=None) -> dict[str, str]:
    res = verify(store, project)
    assert res["ok"] == (not res["problems"])
    return {p["where"]: p["message"] for p in res["problems"]}


def test_a_fresh_home_verifies_clean_including_every_builtin(tmp_path):
    assert verify(Store(tmp_path / "fresh")) == {"ok": True, "problems": []}


def test_a_working_home_verifies_clean(store):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": {"source": "n"},
                                                        "b": {"default": 1}}}},
           inputs={"n": "int"})
    store.set_input("p", "n", 1, "t", "x")
    store.set_output("p", "a", {"sum": 2}, "t", "x")
    (store.home / ".env").write_text("# c\nexport A=1\nB='x y'\n\n")
    assert verify(store) == {"ok": True, "problems": []}
    assert verify(store, "p") == {"ok": True, "problems": []}
    with pytest.raises(NotFound):
        verify(store, "zz")


@pytest.mark.parametrize("spec, main, expected", [
    ({"extra": 1}, "", "unknown key 'extra'"),                      # shape
    ({"name": "x.other"}, "", "name x.other does not match its directory x.fn"),
    ({"inputs": {"a": "strin"}}, "", "inputs.a: unknown type 'strin'"),
    ({}, None, "main.py is missing"),
])
def test_fn_json_checks(store, spec, main, expected):
    write_fn(store.home / "fns", "x.fn", main=main, spec=spec)
    assert where(store) == {"fns/x.fn/fn.json": expected}


def test_collisions_across_scopes(store):
    create(store, "p", {})
    write_fn(store.home / "fns", "core.echo")
    write_fn(store.project_dir("p") / "fns", "test.add")
    got = where(store)
    assert set(got) == {"fns/core.echo/fn.json", "projects/p/fns/test.add/fn.json"}
    assert got["fns/core.echo/fn.json"].startswith("fn core.echo collides with the builtin fn")
    assert got["projects/p/fns/test.add/fn.json"].startswith(
        "fn test.add collides with the global fn")


def test_project_json_and_env_files(store):
    create(store, "p", {})
    (store.project_dir("p") / "project.json").write_text(
        json.dumps({"name": "q", "description": 3, "owner": "me"}))
    (store.project_dir("p") / ".env").write_text("OK=1\nnot a line\n")
    (store.home / ".env").write_text("= nothing\n")
    (store.home / "projects" / "stray").mkdir()
    assert where(store) == {
        "projects/p/project.json#owner": "unknown key",
        "projects/p/project.json#name": "expected 'p' (the directory name), got 'q'",
        "projects/p/project.json#description": "expected a string",
        "projects/p/.env:2": "not a KEY=value line",
        ".env:1": "not a KEY=value line",
        "projects/stray/project.json": "missing"}


def test_the_plan_is_fully_validated(store):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": {"default": 1},
                                                        "b": {"default": 1}}}})
    path = store.project_dir("p") / "plan.json"
    doc = json.loads(path.read_text())
    doc["steps"]["a"]["in"]["a"] = {"default": "x"}
    doc["steps"]["b"] = {"run": "no.such", "in": {}}
    path.write_text(json.dumps(doc))
    assert where(store, "p") == {"projects/p/plan.json#steps.a.in.a": 'expected int, got "x"',
                                 "projects/p/plan.json#steps.b.run": "unknown fn 'no.such'"}


def test_state_agrees_with_the_plan(store):
    create(store, "p", {"a": {"run": "test.add", "in": {"a": {"default": 1},
                                                        "b": {"default": 1}}}},
           inputs={"n": "int"})
    with store.lock("p"):
        store.write_state("p", {"inputs": {"n": "one", "gone": 1}, "steps": {
            "a": {"status": "succeeded", "outputs": {"sum": "two"}},
            "zz": {"status": "pending"}}})
    assert where(store, "p") == {
        "projects/p/state.json#inputs.n": 'expected int, got "one"',
        "projects/p/state.json#inputs.gone": "a value for gone, which the plan does not declare",
        "projects/p/state.json#steps.a.outputs.sum": 'expected int, got "two"',
        "projects/p/state.json#steps.zz": "state for step zz, which is not in the plan"}


def test_a_project_verify_covers_the_shared_scopes_but_not_other_projects(store):
    create(store, "p", {})
    create(store, "q", {})
    (store.project_dir("q") / ".env").write_text("bad\n")
    write_fn(store.home / "fns", "x.fn", main=None)
    assert set(where(store, "p")) == {"fns/x.fn/fn.json"}
    assert set(where(store)) == {"fns/x.fn/fn.json", "projects/q/.env:1"}
