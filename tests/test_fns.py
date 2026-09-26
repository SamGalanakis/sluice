import json
from pathlib import Path

import pytest

from sluice.fns import Registry, RegistryError
from tests.conftest import TESTPACK


def write_fn(pack: Path, dirname: str, spec: dict, main: bool = True) -> None:
    d = pack / dirname
    d.mkdir(parents=True)
    (d / "fn.json").write_text(json.dumps(spec))
    if main:
        (d / "main.py").write_text("")


def test_loads_builtins_and_the_test_pack():
    reg = Registry.load([TESTPACK])
    assert {"core.echo", "core.ask", "core.fail", "test.add", "test.twice"} <= set(reg.names())
    add = reg.get("test.add")
    assert add.effects is False and add.dir == TESTPACK / "test.add"
    assert reg.get("test.twice").composite and not add.composite
    flaky = reg.get("test.flaky")
    assert (flaky.retry_transient, flaky.retry_backoff, flaky.timeout) == (2, 1.0, 3600.0)
    assert reg.get("test.heavy").slots == {"heavy": 1}
    assert reg.get("test.echo_log").slots == {"default": 1}


def test_description_is_optional():
    reg = Registry.load([TESTPACK])
    assert "description" not in reg.get("test.echo_log").raw
    assert reg.get("test.echo_log").description == ""


def test_duplicate_names_are_a_load_error(tmp_path):
    write_fn(tmp_path, "a", {"name": "test.add", "in": {}, "out": {}})
    with pytest.raises(RegistryError) as e:
        Registry.load([TESTPACK, tmp_path])
    assert any("duplicate fn name test.add" in x for x in e.value.errors)


def test_a_builtin_name_cannot_be_reused(tmp_path):
    write_fn(tmp_path, "echo", {"name": "core.echo", "in": {}, "out": {}})
    with pytest.raises(RegistryError, match="duplicate fn name core.echo"):
        Registry.load([tmp_path])


def test_subdirectories_without_fn_json_are_skipped(tmp_path):
    write_fn(tmp_path, "x.one", {"name": "x.one", "in": {}, "out": {}})
    for junk in ("_lib", "tests", "examples", "standards"):
        (tmp_path / junk).mkdir()
        (tmp_path / junk / "helper.py").write_text("raise SystemExit('not a fn')")
    (tmp_path / "README.md").write_text("# pack")
    reg = Registry.load([tmp_path])
    assert [n for n in reg.names() if not n.startswith("core.")] == ["x.one"]


def test_every_fn_json_problem_is_reported(tmp_path):
    write_fn(tmp_path, "bad", {"name": "Bad Name", "version": "1", "in": {"x": "str"},
                               "out": [], "timeout": "1d", "slots": {"a": 0}, "effects": "no",
                               "retry": {"transient": -1}, "extra": 1}, main=False)
    with pytest.raises(RegistryError) as e:
        Registry.load([tmp_path])
    text = "\n".join(e.value.errors)
    for needle in ("name must be dotted lowercase", "version must be an int",
                   "in.x: unknown type 'str'", "out must be an object", "timeout: bad duration",
                   "slots must be", "effects must be a bool", "retry.transient must be",
                   "unknown key 'extra'", "needs main.py or a graph"):
        assert needle in text


def test_missing_pack_dir_is_an_error(tmp_path):
    with pytest.raises(RegistryError, match="pack directory does not exist"):
        Registry.load([tmp_path / "nope"])
