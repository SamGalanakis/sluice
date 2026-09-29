"""The `query` MCP tool (SPEC §8): read-only SELECTs against the home's sluice.db."""

import json
import os
import re

import pytest
from mcp import Client

from sluice import db
from sluice import log as L
from sluice import query as q
from sluice.errors import BadRequest
from sluice.mcp_server import build_server
from tests.conftest import add, create, d, message

SVG = b'<svg xmlns="http://www.w3.org/2000/svg"/>'


def test_each_view_returns_columns_and_rows(store):
    create(store, "p", {"a": add(d(1), d(2))})
    store.append("p", message("t", "hi"),
                 {"kind": "step.status", "step": "a", "from": "pending", "to": "running"})
    expected = {"steps": {"project", "step", "status"}, "messages": {"thread", "body"},
                "step_changes": {"step", "to"}, "edits": {"rev", "author"},
                "log": {"seq", "kind", "record"}}
    for view, cols in expected.items():
        res = q.run(store.home, f"SELECT * FROM {view}")
        assert res["columns"] and res["rows"] and res["truncated"] is False
        assert cols <= set(res["columns"]), view
    steps = q.run(store.home, "SELECT step, status FROM steps WHERE project = 'p'")
    assert steps["rows"] == [["a", "pending"]]
    assert q.run(store.home, "SELECT body FROM messages")["rows"] == [["hi"]]
    assert q.run(store.home, "SELECT \"to\" FROM step_changes")["rows"] == [["running"]]
    assert q.run(store.home, "SELECT max(rev) FROM edits")["rows"] == [[2]]


def test_params_bind(store):
    create(store, "p", {"a": add(d(1), d(2))})
    res = q.run(store.home, "SELECT step FROM steps WHERE project = ? AND step = ?",
                ["p", "a"])
    assert res["rows"] == [["a"]]
    assert q.run(store.home, "SELECT ? + ?, ?", [2, 3, "x"])["rows"] == [[5, "x"]]
    assert q.run(store.home, "SELECT step FROM steps WHERE project = ?",
                 ["nope"])["rows"] == []


def test_limit_and_truncated(store):
    store.create_project("p")
    store.append("p", *[message("t", f"m{i}") for i in range(5)])
    res = q.run(store.home, "SELECT body FROM messages ORDER BY seq", limit=2)
    assert res["rows"] == [["m0"], ["m1"]] and res["truncated"] is True
    res = q.run(store.home, "SELECT body FROM messages", limit=10)
    assert len(res["rows"]) == 5 and res["truncated"] is False


@pytest.mark.parametrize("sql, action", [
    ("INSERT INTO projects (name, created) VALUES ('x', 'now')", "SQLITE_INSERT"),
    ("UPDATE projects SET description = 'x'", "SQLITE_UPDATE"),
    ("DELETE FROM projects", "SQLITE_DELETE"),
    ("CREATE TABLE t (a)", "SQLITE_INSERT"),  # a write to sqlite_master
    ("ATTACH ':memory:' AS x", "SQLITE_ATTACH"),
    ("PRAGMA user_version = 2", "SQLITE_PRAGMA"),
    ("PRAGMA table_info(projects)", "SQLITE_PRAGMA"),
    ("SELECT load_extension('/tmp/x.so')", "not authorized"),
])
def test_everything_but_reads_is_refused(store, sql, action):
    db.connect(store.home)  # creates sluice.db
    with pytest.raises(BadRequest, match=f"read-only.*{action}|not authorized"):
        q.run(store.home, sql)


def test_vacuum_into_writes_no_file(store, tmp_path):
    db.connect(store.home)
    out = tmp_path / "copy.db"
    with pytest.raises(BadRequest):
        q.run(store.home, f"VACUUM INTO '{out}'")
    assert not out.exists()


def test_a_runaway_query_is_interrupted(store):
    db.connect(store.home)
    with pytest.raises(BadRequest, match="interrupted: the query ran past"):
        q.run(store.home,
              "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) "
              "SELECT sum(n) FROM r", timeout=0.01)


def test_a_huge_scalar_hits_the_length_limit(store):
    db.connect(store.home)
    with pytest.raises(BadRequest, match="too big"):
        q.run(store.home, "SELECT hex(randomblob(2000000))")


def test_a_blob_cell_is_refused_with_the_hex_length_hint(store, tmp_path):
    icon = tmp_path / "icon.svg"
    icon.write_bytes(SVG)
    store.create_project("p", icon=str(icon))
    with pytest.raises(BadRequest) as e:
        q.run(store.home, "SELECT icon FROM projects WHERE name = 'p'")
    assert "binary data" in str(e.value) and 'hex("icon")' in str(e.value)
    rows = q.run(store.home, "SELECT length(icon) FROM projects WHERE name = 'p'")["rows"]
    assert rows == [[len(SVG)]]


def test_the_response_budget_truncates_many_wide_rows(store):
    db.connect(store.home)
    res = q.run(store.home,
                "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r "
                "LIMIT 200) SELECT n, hex(randomblob(8000)) FROM r")
    assert res["truncated"] is True and 0 < len(res["rows"]) < 200
    assert len(json.dumps(res["rows"])) > q.RESPONSE


def test_byte_budget_stops_before_evaluating_later_invalid_rows(store, monkeypatch):
    db.connect(store.home)
    monkeypatch.setattr(q, "RESPONSE", 10)
    result = q.run(store.home, "SELECT 'the crossing row' AS value UNION ALL SELECT 'next' "
                              "UNION ALL SELECT json('invalid json')")
    assert result == {"columns": ["value"], "rows": [["the crossing row"]], "truncated": True}
    conn = db.connect(store.home)
    assert conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0] == 0


def test_exact_row_limit_is_not_truncated(store):
    db.connect(store.home)
    assert q.run(store.home, "SELECT 1 UNION ALL SELECT 2", limit=2)["truncated"] is False


@pytest.mark.parametrize("limit", [0, 1001, "10", None])
def test_a_bad_limit_is_refused(store, limit):
    db.connect(store.home)
    with pytest.raises(BadRequest, match="limit"):
        q.run(store.home, "SELECT 1", limit=limit)


async def test_the_tool_and_its_docstring(store):
    """The tool is served like the others, and its docstring names every table and view
    of the schema (parsed from db.py's CREATE statements, so it cannot drift)."""
    create(store, "p", {"a": add(d(1), d(2))})
    async with Client(build_server(store)) as c:
        tools = {t.name: t.description for t in (await c.list_tools()).tools}
        r = await c.call_tool("query", {"sql": "SELECT step, status FROM steps"})
    assert not r.is_error
    assert json.loads(r.content[0].text) == {
        "columns": ["step", "status"], "rows": [["a", "pending"]], "truncated": False}
    for name in re.findall(r"CREATE (?:TABLE|VIEW) (?:IF NOT EXISTS )?(\w+)", db.SCHEMA):
        assert name in tools["query"]


def test_readers_do_not_block_on_a_held_write_and_nothing_leaks(store):
    create(store, "p", {"a": add(d(1), d(2))})
    with db.write(store.home) as conn:
        L.append(conn, "p", [message("t", "in-flight")])
        res = q.run(store.home, "SELECT body FROM messages WHERE body = 'in-flight'")
        assert res["rows"] == []  # its snapshot predates the uncommitted write
        assert q.run(store.home, "SELECT count(*) FROM steps")["rows"] == [[1]]
    q.run(store.home, "SELECT 1")  # WAL keeps one shared fd while the writer lives
    before = len(os.listdir("/proc/self/fd"))
    for _ in range(3):
        res = q.run(store.home, "SELECT body FROM messages WHERE body = 'in-flight'")
    assert res["rows"] == [["in-flight"]]
    assert len(os.listdir("/proc/self/fd")) == before  # every call closed its connection
    conn = db.connect(store.home)
    assert conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0] == 0
