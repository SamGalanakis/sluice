"""Projects (SPEC §2): isolation, function scopes, name collisions and fn_save."""

import json

import pytest

from sluice.errors import BadRequest, InvalidPlan, NotFound
from sluice.runner import Runner
from tests.conftest import create, settle, write_fn

UPPER = """# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import run


def main(inp, ctx):
    return {"text": TAG + inp["text"].upper()}


if __name__ == "__main__":
    run(main)
"""


def upper(tag: str) -> tuple[dict, str]:
    fn = {"name": "text.upper", "doc": "Upper-case a string.", "inputs": {"text": "string"},
          "outputs": {"text": "string"}}
    return fn, UPPER.replace("TAG", json.dumps(tag))


def step(fn: str, **inputs) -> dict:
    return {"run": fn, "in": {k: {"default": v} for k, v in inputs.items()}}


def listing(store, project=None) -> dict:
    return {e["name"]: e for e in store.registry(project).listing()}


def test_two_projects_have_separate_plans_state_fns_and_env(store, runner):
    create(store, "p", {}, inputs={"n": "int"})
    create(store, "q", {}, inputs={"n": "string"})
    store.fn_save(*upper("p:"), project="p")
    store.fn_save(*upper("q:"), project="q")
    (store.project_dir("q") / ".env").write_text("TEST_WHO=q\n")
    store.patch("p", 2, [{"op": "add", "path": "/steps/u", "value": step("text.upper", text="a")},
                         {"op": "add", "path": "/steps/e", "value": step("test.env")}], "t", "x")
    store.patch("q", 2, [{"op": "add", "path": "/steps/u", "value": step("text.upper", text="b")},
                         {"op": "add", "path": "/steps/e", "value": step("test.env")}], "t", "x")
    store.set_input("p", "n", 1, "t", "x")
    with pytest.raises(InvalidPlan):  # q's n is a string
        store.set_input("q", "n", 1, "t", "x")
    p, q = settle(runner, store, "p"), settle(runner, store, "q")
    assert p["u"]["outputs"] == {"text": "p:A"} and q["u"]["outputs"] == {"text": "q:B"}
    assert "TEST_WHO" not in p["e"]["outputs"]["env"]
    assert q["e"]["outputs"]["env"]["TEST_WHO"] == "q"
    assert store.read_state("p")["inputs"] == {"n": 1} and store.read_state("q")["inputs"] == {}
    assert store.get("p")["rev"] == 3 and len(store.history("q")) == 3
    [run_id] = p["u"]["run_ids"]
    assert (store.project_dir("p") / "runs" / run_id / "output.json").is_file()
    assert not (store.project_dir("q") / "runs" / run_id).exists()
    with pytest.raises(InvalidPlan) as e:  # a project fn is not visible elsewhere
        create(store, "r", {"u": step("text.upper", text="c")})
    assert e.value.errors == ["steps.u.run: unknown fn 'text.upper'"]


def test_a_project_sees_builtin_global_and_its_own_fns(store):
    create(store, "p", {})
    create(store, "q", {})
    write_fn(store.home / "fns", "home.fn")
    write_fn(store.project_dir("p") / "fns", "p.only")
    p, q, top = listing(store, "p"), listing(store, "q"), listing(store)
    assert (p["core.echo"]["scope"], p["test.add"]["scope"], p["home.fn"]["scope"],
            p["p.only"]["scope"]) == ("builtin", "global", "global", "project")
    assert "p.only" not in q and "p.only" not in top and "home.fn" in q
    names = [e["name"] for e in store.registry("p").listing()]
    assert names.index("core.echo") < names.index("test.add") < names.index("p.only")
    assert store.fn("p.only", "p").dir == store.project_dir("p") / "fns" / "p.only"
    with pytest.raises(NotFound):
        store.fn("p.only", "q")
    with pytest.raises(NotFound):
        store.registry("zz")


def test_the_same_local_name_in_two_projects_is_fine(store):
    create(store, "p", {})
    create(store, "q", {})
    assert store.fn_save(*upper("p"), project="p")["scope"] == "project"
    assert store.fn_save(*upper("q"), project="q")["scope"] == "project"
    assert store.registry("p").problems == [] and store.registry("q").problems == []
    assert store.fn("text.upper", "p").dir != store.fn("text.upper", "q").dir


def test_a_project_fn_colliding_with_a_global_one_blocks_that_project(store, runner):
    create(store, "p", {"a": step("core.echo", value=1)})
    create(store, "q", {"a": step("core.echo", value=2)})
    bad = write_fn(store.project_dir("p") / "fns", "test.add", {"a": "int"}, {"sum": "int"})
    [problem] = store.registry("p").problems
    assert problem == {"where": "projects/p/fns/test.add/fn.json",
                       "message": f"fn test.add collides with the global fn at "
                                  f"{store.fn('test.add').dir}"}
    assert listing(store, "p")["test.add"]["error"].startswith("fn test.add collides")
    assert store.fn("test.add", "p").scope == "global"  # lookup order: the earlier scope
    with pytest.raises(InvalidPlan) as e:
        store.patch("p", 2, [], "t", "blocked")
    assert "function problems block edits and runs" in e.value.message
    assert e.value.errors == [f"{problem['where']}: {problem['message']}"]
    for refused in (lambda: store.set_output("p", "a", {"value": 1}, "t", "x"),
                    lambda: store.retry("p", "a", "t", "x")):
        with pytest.raises(InvalidPlan):
            refused()
    runner.tick()
    assert store.read_state("p")["steps"]["a"]["status"] == "pending"  # not run
    assert store.read_state("q")["steps"]["a"]["status"] == "succeeded"  # q is unaffected
    assert store.patch("q", 2, [], "t", "fine") == 3
    assert store.status("p")["rev"] == 2  # reads still work
    (bad / "fn.json").unlink()
    (bad / "main.py").unlink()
    bad.rmdir()
    assert store.patch("p", 2, [], "t", "fixed") == 3
    runner.tick()
    assert store.read_state("p")["steps"]["a"]["status"] == "succeeded"


def test_a_global_fn_colliding_with_a_builtin_blocks_every_project(store, runner):
    create(store, "p", {"a": step("core.echo", value=1)})
    write_fn(store.home / "fns", "git.head", {"path": "string"}, {"sha": "string"})
    problems = store.registry().problems
    assert [p["where"] for p in problems] == ["fns/git.head/fn.json"]
    assert problems[0]["message"].startswith("fn git.head collides with the builtin fn at ")
    assert store.registry("p").problems == problems
    heads = [e for e in store.registry().listing() if e["name"] == "git.head"]
    assert [(e["scope"], "error" in e) for e in heads] == [("builtin", False), ("global", True)]
    assert store.fn("git.head").scope == "builtin"
    with pytest.raises(InvalidPlan, match="the global functions"):
        store.usable_registry()
    with pytest.raises(InvalidPlan, match="project p"):
        store.patch("p", 2, [], "t", "blocked")
    runner.tick()
    assert store.read_state("p")["steps"]["a"]["status"] == "pending"


def test_fn_save_writes_a_valid_fn_into_its_scope(store):
    create(store, "p", {})
    fn, main = upper("")
    res = store.fn_save(fn, main, project="p")
    target = store.project_dir("p") / "fns" / "text.upper"
    assert res == {"scope": "project", "path": str(target)}
    assert json.loads((target / "fn.json").read_text()) == fn
    assert (target / "main.py").read_text() == main
    assert store.fn_save({**fn, "doc": "v2"}, main, "p") == res  # saving again replaces it
    assert store.fn("text.upper", "p").doc == "v2"
    res = store.fn_save({**fn, "name": "text.lower"}, main)
    assert res == {"scope": "global", "path": str(store.home / "fns" / "text.lower")}
    assert store.fn("text.lower").scope == "global"
    with pytest.raises(NotFound):
        store.fn_save(fn, main, project="zz")


def test_fn_save_rejects_an_invalid_fn_json(store):
    with pytest.raises(InvalidPlan) as e:
        store.fn_save({"name": "Bad", "inputs": {"x": "str"}, "extra": 1}, "")
    assert e.value.errors == [
        "unknown key 'extra'", "name must be dotted lowercase like 'git.head', got 'Bad'",
        "inputs.x: unknown type 'str'", "outputs is required, an object of name -> type",
        "main_py: the fn's Python source is required"]
    assert not (store.home / "fns").exists()


def test_fn_save_refuses_a_colliding_name(store):
    create(store, "p", {})
    fn, main = upper("")
    with pytest.raises(BadRequest, match="fn core.echo would collide with the builtin fn"):
        store.fn_save({**fn, "name": "core.echo"}, main, project="p")
    with pytest.raises(BadRequest, match="fn test.add would collide with the global fn"):
        store.fn_save({**fn, "name": "test.add"}, main, project="p")
    with pytest.raises(BadRequest, match="with the builtin fn"):
        store.fn_save({**fn, "name": "git.head"}, main)
    with pytest.raises(BadRequest, match="with the global fn"):  # in a config fn_dir
        store.fn_save({**fn, "name": "test.add"}, main)
    store.fn_save(fn, main, project="p")
    with pytest.raises(BadRequest, match="with the fn of project p"):
        store.fn_save(fn, main)
    assert not (store.home / "fns" / "text.upper").exists()
    assert not (store.project_dir("p") / "fns" / "core.echo").exists()


def test_a_saved_fn_is_picked_up_without_a_restart(store):
    create(store, "p", {})
    with pytest.raises(InvalidPlan):
        store.patch("p", 2, [{"op": "add", "path": "/steps/u",
                              "value": step("text.upper", text="x")}], "t", "too early")
    store.fn_save(*upper(""), project="p")
    assert store.patch("p", 2, [{"op": "add", "path": "/steps/u",
                                 "value": step("text.upper", text="x")}], "t", "now") == 3
    runner = Runner(store)
    try:
        assert settle(runner, store, "p")["u"]["outputs"] == {"text": "X"}
    finally:
        for a in runner.active.values():
            a.kill()
