import json
from pathlib import Path

import pytest

from sluice.fns import BUILTIN_DIR, Registry, RegistryError
from tests.conftest import TESTPACK


def write_fn(d: Path, name: str, spec: dict, main: bool = True) -> None:
    (d / name).mkdir(parents=True)
    (d / name / "fn.json").write_text(json.dumps(spec))
    if main:
        (d / name / "main.py").write_text("")


def test_loads_the_builtins_and_extra_dirs():
    reg = Registry.load([BUILTIN_DIR, TESTPACK])
    assert {"core.echo", "core.collect", "git.head", "agent.run", "test.add"} <= set(reg.names())
    echo, add = reg.get("core.echo"), reg.get("test.add")
    assert echo.native and not add.native and add.dir == TESTPACK / "test.add"
    assert add.summary() == {"name": "test.add", "doc": "Add two ints.",
                             "inputs": {"a": "int", "b": "int"}, "outputs": {"sum": "int"}}
    assert reg.get("test.boom").doc == ""  # doc is optional


def test_non_fn_entries_are_skipped(tmp_path):
    write_fn(tmp_path, "x.one", {"name": "x.one", "inputs": {}, "outputs": {}})
    for junk in ("_lib", "tests", "examples"):
        (tmp_path / junk).mkdir()
        (tmp_path / junk / "helper.py").write_text("")
    (tmp_path / "README.md").write_text("# fns")
    assert Registry.load([tmp_path]).names() == ["x.one"]


def test_duplicate_names_are_a_load_error(tmp_path):
    write_fn(tmp_path, "mine", {"name": "test.add", "inputs": {}, "outputs": {}})
    with pytest.raises(RegistryError, match="duplicate fn name test.add"):
        Registry.load([TESTPACK, tmp_path])


def test_every_fn_json_problem_is_reported(tmp_path):
    write_fn(tmp_path, "bad", {"name": "Bad", "doc": 3, "inputs": {"x": "str"},
                               "outputs": [], "version": 1}, main=False)
    with pytest.raises(RegistryError) as e:
        Registry.load([tmp_path, tmp_path / "missing"])
    text = "\n".join(e.value.errors)
    for needle in ("name must be dotted lowercase", "doc must be a string",
                   "inputs.x: unknown type 'str'", "outputs is required",
                   "unknown key 'version'", "main.py is missing",
                   "missing: fn directory does not exist"):
        assert needle in text
