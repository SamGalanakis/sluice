"""What finished work leaves behind (SPEC §2, §6): a step a plan edit removes keeps its
outcome in `outcomes`, written in the edit's transaction and never trimmed; and the schema
change that brought the table (version 1 → 2)."""

import json
import sqlite3
import sys

import pytest
from mcp import Client

from sluice import db, query
from sluice.cli import main
from sluice.errors import InvalidPlan, SluiceError
from sluice.mcp_server import build_server
from sluice.store import Store
from tests.conftest import add, create, d, settle, src, write_config
from tests.schema_v1 import SCHEMA_V1


def peek(home, sql, params=()):
    conn = sqlite3.connect(home / db.FILE)
    try:
        return conn.execute(sql, params).fetchall()
    finally:
        conn.close()


def outcomes(store, project="p"):
    with store.rx() as conn:
        rows = db.all_rows(conn, "SELECT * FROM outcomes WHERE project = ? ORDER BY rev, step",
                           (project,))
    return {r["step"]: dict(r) for r in rows}


def done(outputs=None, **extra):
    return {"status": "succeeded", "started": "2026-09-28T10:00:00Z",
            "finished": "2026-09-28T10:05:00Z", "outputs": outputs or {"sum": 2},
            "run_ids": ["r1"], "error": None, **extra}


# ---- the schema -------------------------------------------------------------------------

def v1_home(tmp_path):
    """A real version-1 file (the SCHEMA of main before `outcomes`) holding a project with a
    plan, its edit, a state and a record, as a v1 sluice left it."""
    home = tmp_path / "h"
    write_config(home)
    conn = sqlite3.connect(home / db.FILE, autocommit=True)
    conn.execute("PRAGMA journal_mode = WAL")
    conn.executescript(SCHEMA_V1)
    conn.execute("PRAGMA user_version = 1")
    plan = {"inputs": {}, "outputs": {}, "steps": {"a": add(d(1), d(1))}}
    conn.execute("INSERT INTO projects (name, description, created) VALUES "
                 "('p', 'old', '2026-09-01T00:00:00Z')")
    conn.execute("INSERT INTO plans (project, rev, doc) VALUES ('p', 2, ?)", (json.dumps(plan),))
    conn.execute("INSERT INTO states (project, doc) VALUES ('p', ?)",
                 (json.dumps({"inputs": {}, "steps": {"a": done()}}),))
    conn.execute("INSERT INTO records (project, at, kind, data) VALUES "
                 "('p', '2026-09-01T00:00:00Z', 'plan.edit', '{\"rev\": 2}')")
    conn.execute("INSERT INTO plan_edits (project, rev, seq, at, author, reason, ops) VALUES "
                 "('p', 2, 1, '2026-09-01T00:00:00Z', 'orch', 'grow', '[]')")
    conn.close()
    return home


def test_a_version_1_file_is_upgraded_in_place_keeping_its_rows(tmp_path):
    home = v1_home(tmp_path)
    conn = db.connect(home)
    assert conn.execute("PRAGMA user_version").fetchone()[0] == db.VERSION == 2
    assert peek(home, "SELECT type, name FROM sqlite_master WHERE tbl_name = 'outcomes' AND "
                      "sql IS NOT NULL ORDER BY name") == [("table", "outcomes"),
                                                           ("index", "outcomes_unit")]
    assert peek(home, "SELECT name, description FROM projects") == [("p", "old")]
    assert peek(home, "SELECT author, reason FROM plan_edits") == [("orch", "grow")]
    assert peek(home, "SELECT count(*) FROM records") == [(1,)]
    store = Store(home)
    assert store.status("p", all=True)["steps"][0]["status"] == "succeeded"
    store.remove_steps("p", ["a"], author="orch", reason="done")  # the new table takes rows
    assert outcomes(store)["a"]["outputs"] == '{"sum": 2}'
    db.connect(home)  # a second open needs nothing
    assert peek(home, "PRAGMA user_version") == [(2,)]


def test_a_version_1_file_that_has_the_table_already_is_upgraded(tmp_path):
    # the live home once went 1 -> 2 and was set back to 1 by hand, keeping table and index
    home = v1_home(tmp_path)
    conn = sqlite3.connect(home / db.FILE, autocommit=True)
    conn.executescript(db.OUTCOMES)
    conn.execute("INSERT INTO outcomes (project, step, rev, fn, status, removed) VALUES "
                 "('p', 'old', 2, 'test.add', 'succeeded', '2026-09-01T00:00:00Z')")
    conn.close()
    db.connect(home)
    assert peek(home, "PRAGMA user_version") == [(2,)]
    assert peek(home, "SELECT step FROM outcomes") == [("old",)]
    assert peek(home, "SELECT count(*) FROM sqlite_master WHERE name = 'outcomes_unit'") == \
        [(1,)]


def test_a_failed_upgrade_leaves_the_file_at_version_1(tmp_path, monkeypatch):
    home = v1_home(tmp_path)
    monkeypatch.setitem(db.MIGRATIONS, 1, db.OUTCOMES + "CREATE TABLE projects (x);")
    with pytest.raises(sqlite3.OperationalError):
        db.connect(home)
    assert peek(home, "PRAGMA user_version") == [(1,)]
    assert peek(home, "SELECT count(*) FROM sqlite_master WHERE name = 'outcomes'") == [(0,)]


def test_a_new_home_starts_at_version_2_with_the_table(tmp_path):
    home = tmp_path / "h"
    db.connect(home)
    assert peek(home, "PRAGMA user_version") == [(2,)]
    assert peek(home, "SELECT count(*) FROM sqlite_master WHERE name = 'outcomes'") == [(1,)]


def test_a_version_newer_than_this_sluice_is_refused(tmp_path):
    home = tmp_path / "h"
    home.mkdir()
    conn = sqlite3.connect(home / db.FILE)
    conn.execute("PRAGMA user_version = 3")
    conn.close()
    with pytest.raises(SluiceError, match="schema version 3; this sluice knows version 2"):
        db.connect(home)
    assert peek(home, "PRAGMA user_version") == [(3,)]


# ---- writing outcomes -------------------------------------------------------------------

def test_a_removed_succeeded_open_step_keeps_its_typed_outputs(store, runner, monkeypatch):
    monkeypatch.setenv("TEST_PYTHON", sys.executable)
    report = {"type": "record", "fields": {"ok": "boolean", "notes": "string[]"}}
    submitted = {"word": "héllo", "report": {"ok": True, "notes": ["a", "b"]}}
    create(store, "p", {"agent": {"run": "test.open", "in": {"attempts": d([submitted])},
                                  "outputs": {"word": "string", "report": report},
                                  "tags": ["lane"]}})
    steps = settle(runner, store, "p")
    assert steps["agent"]["status"] == "succeeded", steps
    rev = store.get("p")["rev"]
    out = store.remove_steps("p", tags=["lane"], author="orch", reason="done with it")
    assert out == {"rev": rev + 1, "steps": ["agent"], "outcomes": 1}
    row = outcomes(store)["agent"]
    kept = json.loads(row["outputs"])
    assert {k: kept[k] for k in submitted} == submitted
    assert kept == steps["agent"]["outputs"]
    assert json.loads(row["run_ids"]) == steps["agent"]["run_ids"]
    assert (row["rev"], row["fn"], row["status"], row["error"], row["manual"]) == \
        (rev + 1, "test.open", "succeeded", None, 0)
    assert (row["started"], row["finished"]) == (steps["agent"]["started"],
                                                 steps["agent"]["finished"])
    assert (row["author"], row["reason"], row["unit"]) == ("orch", "done with it", None)
    assert row["removed"] >= row["finished"]


def test_the_unit_comes_from_a_unit_tag_else_the_component(store):
    create(store, "p", {"t1": add(d(1), d(1), tags=["x", "unit:lane-1"]),
                        "c1": add(d(1), d(1)), "c2": add(src("c1/sum"), d(1)),
                        "solo": add(d(1), d(1))})
    store.write_state("p", {"inputs": {}, "steps": {s: done() for s in
                                                    ("t1", "c1", "c2", "solo")}})
    store.remove_steps("p", ["t1", "c1", "c2", "solo"])
    assert {s: r["unit"] for s, r in outcomes(store).items()} == \
        {"t1": "lane-1", "c1": "c1", "c2": "c1", "solo": None}


def test_plan_prune_keeps_every_step_of_the_units_it_removes(store):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(src("a/sum"), d(1), tags=["t"]),
                        "c": add(d(1), d(1), after=["b"]), "busy": add(d(1), d(1))})
    skipped = {"status": "skipped", "skipped": "b/sum is false",
               "finished": "2026-09-28T10:06:00Z"}
    store.write_state("p", {"inputs": {}, "steps": {"a": done(), "b": done({"sum": 3}),
                                                    "c": skipped}})
    out = store.prune("p", author="tidy", reason="weekly")
    assert (out["steps"], out["outcomes"]) == (["a", "b", "c"], 3)
    rows = outcomes(store)
    assert {s: (r["status"], r["unit"], r["error"]) for s, r in rows.items()} == {
        "a": ("succeeded", "a", None), "b": ("succeeded", "a", None),
        "c": ("skipped", "a", "b/sum is false")}
    assert {(r["rev"], r["author"], r["reason"]) for r in rows.values()} == \
        {(out["rev"], "tidy", "weekly")}


def test_failed_stale_and_manual_are_kept_and_pending_is_not(store):
    create(store, "p", {"f": add(d(1), d(1)), "st": add(d(1), d(1)), "m": add(d(1), d(1)),
                        "never": add(d(1), d(1)), "held": add(d(1), d(1))})
    store.write_state("p", {"inputs": {}, "steps": {
        "f": {"status": "failed", "started": "2026-09-28T10:00:00Z",
              "finished": "2026-09-28T10:01:00Z", "outputs": None, "error": "exit 1: boom",
              "run_ids": ["r9"]},
        "st": done({"sum": 5}, status="stale"),
        "m": {"status": "succeeded", "started": None, "finished": "2026-09-28T10:02:00Z",
              "outputs": {"sum": 7}, "manual": True, "inputs_hash": None},
        "held": {"status": "pending"}}})
    out = store.remove_steps("p", ["f", "st", "m", "never", "held"])
    assert out["outcomes"] == 3
    rows = outcomes(store)
    assert set(rows) == {"f", "st", "m"}
    assert (rows["f"]["status"], rows["f"]["error"], rows["f"]["outputs"],
            rows["f"]["run_ids"]) == ("failed", "exit 1: boom", None, '["r9"]')
    assert (rows["st"]["status"], json.loads(rows["st"]["outputs"])) == ("stale", {"sum": 5})
    assert (rows["m"]["manual"], rows["m"]["started"], rows["m"]["run_ids"]) == (1, None, None)


def test_every_path_that_removes_steps_keeps_outcomes(store):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(d(1), d(1)), "c": add(d(1), d(1))})
    store.write_state("p", {"inputs": {}, "steps": {s: done() for s in "abc"}})
    rev = store.patch("p", store.get("p")["rev"], [{"op": "remove", "path": "/steps/a"}],
                      "orch", "patch")
    store.patch("p", rev, [{"op": "replace", "path": "/steps", "value": {
        "c": add(d(1), d(1)), "n": add(d(2), d(2))}}], "orch", "replace them all")
    rows = outcomes(store)
    assert {s: (r["rev"], r["reason"]) for s, r in rows.items()} == {
        "a": (rev, "patch"), "b": (rev + 1, "replace them all")}  # c stays in the plan


def test_nothing_is_kept_when_the_edit_fails(store):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(src("a/sum"), d(1))})
    store.write_state("p", {"inputs": {}, "steps": {"a": done(), "b": done()}})
    with pytest.raises(InvalidPlan):  # b still reads a
        store.remove_steps("p", ["a"])
    with pytest.raises(RuntimeError), store.tx():  # the edit's transaction rolls back
        store.remove_steps("p", ["a", "b"])
        assert len(outcomes(store)) == 2  # inside it, they are there
        raise RuntimeError("later part of the change fails")
    assert outcomes(store) == {}
    assert list(store.get("p")["steps"]) == ["a", "b"]


def test_a_removed_step_keeps_its_outcome_once_the_runner_drops_its_entry(store, runner):
    create(store, "p", {"a": add(d(1), d(2))})
    settle(runner, store, "p")
    store.remove_steps("p", ["a"])
    runner.tick()
    assert "a" not in store.read_state("p")["steps"]
    assert json.loads(outcomes(store)["a"]["outputs"]) == {"sum": 3}


def test_deleting_a_project_deletes_its_outcomes(store):
    for name in ("p", "q"):
        create(store, name, {"a": add(d(1), d(1))})
        store.write_state(name, {"inputs": {}, "steps": {"a": done()}})
        store.remove_steps(name, ["a"])
    store.update_project("p", archived=True)
    store.delete_project("p")
    assert peek(store.home, "SELECT project, step FROM outcomes") == [("q", "a")]


# ---- reading them -----------------------------------------------------------------------

def test_the_query_tool_sees_outcomes(store, monkeypatch, capsys):
    cols = ("outcomes(project, step, rev, unit, fn, status, outputs, error, started, finished, "
            "run_ids, manual, removed, author, reason)")
    assert f"  {cols}" in query.doc()
    monkeypatch.setenv("SLUICE_HOME", str(store.home))
    assert main(["query"]) == 0
    assert f"table {cols}" in capsys.readouterr().out.splitlines()
    create(store, "p", {"a": add(d(1), d(1))})
    store.write_state("p", {"inputs": {}, "steps": {"a": done()}})
    store.remove_steps("p", ["a"], author="orch")
    res = query.run(store.home, "SELECT step, status, outputs ->> '$.sum', author "
                                "FROM outcomes")
    assert res["rows"] == [["a", "succeeded", 2, "orch"]]


async def test_the_tools_say_how_many_outcomes_they_kept(store):
    create(store, "p", {"a": add(d(1), d(1)), "b": add(d(1), d(1)), "c": add(d(1), d(1))})
    store.write_state("p", {"inputs": {}, "steps": {"a": done(), "b": done()}})
    async with Client(build_server(store)) as c:
        async def call(tool, **args):
            r = await c.call_tool(tool, args)
            assert not r.is_error, r.content[0].text
            return json.loads(r.content[0].text)

        assert (await call("step_remove", project="p", steps=["a", "c"]))["outcomes"] == 1
        assert (await call("plan_prune", project="p"))["outcomes"] == 1
        rows = (await call("query", sql="SELECT step, author FROM outcomes ORDER BY step"))
        assert rows["rows"] == [["a", "mcp"], ["b", "mcp"]]
