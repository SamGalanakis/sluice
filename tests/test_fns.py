from sluice.registry import BUILTIN_DIR, load
from tests.conftest import TESTPACK, write_fn


def test_loads_the_builtins_and_extra_dirs_with_their_scope():
    reg = load({"builtin": [BUILTIN_DIR], "global": [TESTPACK]})
    assert reg.problems == []
    assert {"core.echo", "core.collect", "core.format", "thread.post", "thread.wait",
            "inbox.ask", "test.add"} <= set(reg.names())
    echo, add = reg.get("core.echo"), reg.get("test.add")
    assert echo.native and not add.native and add.dir == TESTPACK / "test.add"
    assert (echo.scope, add.scope) == ("builtin", "global")
    assert add.summary() == {"name": "test.add", "doc": "Add two ints.", "scope": "global",
                             "inputs": {"a": "int", "b": "int"}, "outputs": {"sum": "int"}}
    assert reg.get("test.boom").doc == ""  # doc is optional


def test_non_fn_entries_are_skipped(tmp_path):
    write_fn(tmp_path, "x.one")
    for junk in ("_lib", "tests", "examples"):
        (tmp_path / junk).mkdir()
        (tmp_path / junk / "helper.py").write_text("")
    (tmp_path / "README.md").write_text("# fns")
    reg = load({"global": [tmp_path]})
    assert reg.names() == ["x.one"] and reg.problems == []


def test_a_name_used_twice_in_one_scope_is_a_problem_of_the_later_dir(tmp_path):
    write_fn(tmp_path / "a", "x.one")
    write_fn(tmp_path / "b", "x.one")
    reg = load({"global": [tmp_path / "a", tmp_path / "b"]})
    assert reg.get("x.one").dir == (tmp_path / "a" / "x.one")
    assert reg.problems == [{"where": str(tmp_path / "b" / "x.one" / "fn.json"),
                             "message": f"fn x.one collides with the global fn at "
                                        f"{tmp_path / 'a' / 'x.one'}"}]


def test_a_native_name_needs_main_py_outside_the_builtins(tmp_path):
    write_fn(tmp_path, "core.echo", main=None)
    reg = load({"builtin": [BUILTIN_DIR], "global": [tmp_path]})
    assert [p["message"] for p in reg.problems] == ["main.py is missing"]
    assert reg.get("core.echo").scope == "builtin"


def test_every_fn_json_problem_is_reported_without_raising(tmp_path):
    write_fn(tmp_path, "bad", main=None, spec={"name": "Bad", "doc": 3, "inputs": {"x": "str"},
                                              "outputs": [], "version": 1})
    write_fn(tmp_path, "x.dir", spec={"name": "x.other"})
    (tmp_path / "x.broken").mkdir()
    (tmp_path / "x.broken" / "fn.json").write_text("{not json")
    write_fn(tmp_path, "x.fine")
    reg = load({"global": [tmp_path, tmp_path / "missing"]})
    assert reg.names() == ["x.fine"]
    by_where: dict[str, list[str]] = {}
    for p in reg.problems:
        by_where.setdefault(p["where"], []).append(p["message"])
    assert by_where[str(tmp_path / "bad" / "fn.json")] == [
        "unknown key 'version'", "name must be dotted lowercase like 'git.head', got 'Bad'",
        "doc must be a string", "inputs.x: unknown type 'str'",
        "outputs is required, an object of name -> type", "main.py is missing"]
    assert by_where[str(tmp_path / "x.dir" / "fn.json")] == [
        "name x.other does not match its directory x.dir"]
    assert by_where[str(tmp_path / "x.broken" / "fn.json")][0].startswith("bad JSON:")
    assert by_where[str(tmp_path / "missing")] == ["fn directory does not exist"]
    listing = {e["name"]: e for e in reg.listing()}
    assert listing["x.other"]["error"] == "name x.other does not match its directory x.dir"
    assert "error" not in listing["x.fine"]
