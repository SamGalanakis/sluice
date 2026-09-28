"""A binding that reads a file (SPEC §5): `{"file": "/abs/path"}` is the file's text, read
each time its step starts; its staleness covers the path, not the content (SPEC §6)."""

import pytest

from sluice import plan as P
from sluice import types as T
from sluice.errors import InvalidPlan
from sluice.verify import verify
from tests.conftest import add, create, d, settle, statuses


def split(path, **extra):
    return {"run": "test.split", "in": {"text": {"file": str(path)}}, **extra}


def test_the_file_is_read_when_the_step_starts(store, runner, tmp_path):
    spec = tmp_path / "spec.md"
    spec.write_text("one two")
    create(store, "p", {"s": split(spec, paused=True)})
    runner.tick()
    assert statuses(store, "p") == {"s": "pending"}
    spec.write_text("three four five")  # edited after the add, before the start
    store.pause_steps("p", "s", paused=False)
    steps = settle(runner, store, "p")
    assert steps["s"]["outputs"] == {"parts": ["three", "four", "five"]}
    run = store.runs_dir("p") / steps["s"]["run_ids"][0] / "input.json"
    assert '"three four five"' in run.read_text()  # the run got the text, not the path


def test_staleness_covers_the_path_not_the_content(store, runner, tmp_path):
    spec = tmp_path / "spec.md"
    spec.write_text("a b")
    create(store, "p", {"s": split(spec)})
    steps = settle(runner, store, "p")
    h = steps["s"]["inputs_hash"]
    assert h == P.inputs_hash({"text": {"file": str(spec)}})
    spec.write_text("c d e")  # edited after the step succeeded
    for _ in range(3):
        runner.tick()
    assert statuses(store, "p") == {"s": "succeeded"}
    # set by hand (so it can be retried) and retried: the run reads the file again
    store.set_output("p", "s", {"parts": ["x"]}, "test", "by hand")
    assert store.read_state("p")["steps"]["s"]["inputs_hash"] == h  # a manual value too
    store.retry("p", "s", author="test", reason="read the new spec")
    steps = settle(runner, store, "p")
    assert steps["s"]["outputs"] == {"parts": ["c", "d", "e"]}
    # another path is another input: the binding's edit makes it stale
    other = tmp_path / "other.md"
    other.write_text("c d e")
    store.update_step("p", "s", {"in": {"text": {"file": str(other)}}}, "test", "moved")
    settle(runner, store, "p", until=lambda s: s["s"]["status"] == "stale")


def test_a_missing_file_fails_the_step_naming_it(store, runner, tmp_path):
    gone = tmp_path / "nowhere" / "spec.md"
    create(store, "p", {"s": split(gone), "after": add(d(1), d(1), after=["s"])})
    steps = settle(runner, store, "p", until=lambda s: s["s"]["status"] == "failed")
    assert steps["s"]["error"] == \
        f"input text: cannot read the file {gone}: No such file or directory"
    assert steps["s"]["run_ids"] == []  # nothing started
    st = store.status("p", steps=["s"])
    assert str(gone) in st["steps"][0]["error"]
    gone.parent.mkdir()
    gone.write_text("here now")
    store.retry("p", "s", author="test", reason="the file is there")
    steps = settle(runner, store, "p")
    assert steps["s"]["outputs"] == {"parts": ["here", "now"]}


def test_a_file_binding_is_a_string_and_an_absolute_path(store, tmp_path):
    reg = store.registry()
    ok = {"steps": {"s": split(tmp_path / "x.md"),
                    "o": {"run": "test.open", "in": {"brief": {"file": "/x/brief.md"}}}}}
    errs, plan = P.validate(ok, reg)
    assert errs == []
    assert plan.steps["o"].extra == {"brief": T.Prim("string")}  # typed string
    assert plan.steps["o"].ports()["inputs"] == {"brief": {"type": "string"}}
    absolute = "steps.s.in.text.file: expected an absolute path (a string), got "
    shapes = 'steps.s.in.text: expected {"default": ...}, {"source": ...} or {"file": "/abs/path"}'
    for bad, want in [
            (add({"file": "/x/a.txt"}, d(1)),
             "steps.s.in.a: a file binding is a string, which does not fit int"),
            (split("spec.md"), absolute + "'spec.md'"),
            ({"run": "test.split", "in": {"text": {"file": 3}}}, absolute + "3"),
            ({"run": "test.split", "in": {"text": {"file": "/x", "default": "y"}}}, shapes)]:
        errs, _ = P.validate({"steps": {"s": bad}}, reg)
        assert len(errs) == 1 and errs[0].startswith(want), errs
    create(store, "p", {})
    with pytest.raises(InvalidPlan):
        store.add_step("p", "s", add({"file": "/x/a.txt"}, d(1)), "test", "")


def test_verify_warns_about_a_missing_file(store, tmp_path):
    here = tmp_path / "here.md"
    here.write_text("x")
    create(store, "p", {"a": split(here), "b": split(tmp_path / "missing.md")})
    report = verify(store, "p")
    assert report["ok"] is True
    assert report["warnings"] == [{
        "where": "project p: plan#steps.b.in.text.file",
        "message": f"no readable file at {tmp_path / 'missing.md'} now (the step reads it "
                   "when it starts, and fails if it is still missing)"}]
