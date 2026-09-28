"""The database contract (db.py): transactions, nesting, connections per home, thread and
process, bootstrap, Busy, and the schema's own checks."""

import json
import os
import sqlite3
import subprocess
import sys
import threading

import pytest

from sluice import db
from sluice import log as L
from sluice.errors import SluiceError
from sluice.store import Store


def peek(home, sql, params=()):
    """Read through an independent connection: only what is committed."""
    conn = sqlite3.connect(home / db.FILE)
    try:
        return conn.execute(sql, params).fetchall()
    finally:
        conn.close()


def records(home):
    return [r[0] for r in peek(home, "SELECT kind FROM records ORDER BY seq")]


def writable(home):
    """Whether another connection gets the write lock at once (nothing left holding it)."""
    conn = sqlite3.connect(home / db.FILE, timeout=0)
    try:
        conn.execute("BEGIN IMMEDIATE")
        conn.execute("ROLLBACK")
        return True
    except sqlite3.OperationalError:
        return False
    finally:
        conn.close()


def msg(body="hi"):
    return {"kind": "message", "thread": "t", "from": "a", "body": body}


def test_a_write_commits_and_every_failure_rolls_back(tmp_path):
    home = tmp_path / "h"
    with db.write(home) as conn:
        L.append(conn, None, [msg("kept")])
    assert records(home) == ["message"]
    for exc in (ValueError, KeyboardInterrupt):
        with pytest.raises(exc), db.write(home) as conn:
            L.append(conn, None, [msg("gone")])
            raise exc()
        assert records(home) == ["message"] and writable(home)
        assert not db.connect(home).in_transaction


def test_a_failed_commit_leaves_nothing_written_and_the_connection_clean(tmp_path):
    home = tmp_path / "h"
    db.connect(home)
    with pytest.raises(sqlite3.IntegrityError), db.write(home) as conn:
        conn.execute("PRAGMA defer_foreign_keys = ON")  # checked at COMMIT, which then fails
        conn.execute("INSERT INTO states (project, doc) VALUES ('ghost', '{}')")
    assert peek(home, "SELECT count(*) FROM states") == [(0,)] and writable(home)
    with db.write(home) as conn:
        L.append(conn, None, [msg()])
    assert records(home) == ["message"]


def test_a_nested_write_joins_and_its_failure_dooms_the_outer_one(tmp_path):
    home = tmp_path / "h"
    heard = []
    with pytest.raises(SluiceError, match="nothing was written"), db.write(home) as conn:
        db.after_commit(home, lambda: heard.append("outer"))
        L.append(conn, None, [msg("outer")])
        try:
            with db.write(home) as inner:
                assert inner is conn
                L.append(inner, None, [msg("inner")])
                raise ValueError("inner fails")
        except ValueError:
            pass  # caught: the outer transaction still must not commit
        L.append(conn, None, [msg("after")])
    assert records(home) == [] and heard == [] and writable(home)


def test_listeners_hear_of_a_change_only_after_the_outermost_commit(tmp_path):
    store = Store(tmp_path / "h")
    store.create_project("p")
    seen = []
    store.listeners.append(lambda: seen.append(
        peek(store.home, "SELECT count(*) FROM records WHERE kind = 'step.cancel'")[0][0]))
    with store.tx():
        store.notify()
        store.append("p", {"kind": "step.cancel", "step": "s", "author": "", "reason": ""})
        assert seen == []
    assert seen == [1]
    with pytest.raises(ValueError), store.tx():
        store.notify()
        raise ValueError
    assert seen == [1]


def test_a_read_is_one_snapshot_and_never_becomes_a_write(tmp_path):
    home = tmp_path / "h"
    with db.write(home) as conn:
        L.append(conn, None, [msg()])
    with db.read(home) as conn:
        before = db.one(conn, "SELECT count(*) FROM records")[0]
        other = sqlite3.connect(home / db.FILE, isolation_level=None)
        other.execute("INSERT INTO records (at, kind, data) VALUES ('x', 'message', '{}')")
        other.close()
        assert db.one(conn, "SELECT count(*) FROM records")[0] == before
        with pytest.raises(RuntimeError), db.write(home):
            pass
    assert L.last_seq(home, None) == 2


def test_two_homes_and_worker_threads_each_use_their_own_connection(tmp_path):
    a, b = tmp_path / "a", tmp_path / "b"
    with db.write(a) as conn:
        L.append(conn, None, [msg("a")])
    assert db.connect(a) is not db.connect(b) and L.last_seq(b, None) == 0
    got = {}

    def worker():
        got["conn"] = db.connect(a)
        got["seq"] = L.last_seq(a, None)
        with db.write(b) as conn:
            L.append(conn, None, [msg("b")])

    t = threading.Thread(target=worker)
    t.start()
    t.join()
    assert got["conn"] is not db.connect(a) and got["seq"] == 1
    assert [r["body"] for r in L.read(b, None)["records"]] == ["b"]


def test_a_connection_is_never_used_again_after_a_fork(tmp_path):
    home = tmp_path / "h"
    parent = db.connect(home)
    r, w = os.pipe()
    pid = os.fork()
    if pid == 0:  # the child: a connection of its own, which writes
        try:
            ok = db.connect(home) is not parent
            with db.write(home) as conn:
                L.append(conn, None, [msg("child")])
            os.write(w, b"1" if ok else b"0")
        finally:
            os._exit(0)
    os.close(w)
    assert os.read(r, 1) == b"1"
    os.waitpid(pid, 0)
    os.close(r)
    assert db.connect(home) is parent
    assert [x["body"] for x in L.read(home, None)["records"]] == ["child"]


def test_concurrent_first_opens_create_the_schema_once(tmp_path):
    home = tmp_path / "h"
    code = ("import sys; from sluice import db; sys.stdin.read(1); "
            "c = db.connect(sys.argv[1]); print(c.execute('PRAGMA user_version').fetchone()[0])")
    procs = [subprocess.Popen([sys.executable, "-c", code, str(home)], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
             for _ in range(6)]
    for p in procs:  # released together
        p.stdin.write("x")
        p.stdin.flush()
    outs = [p.communicate(timeout=60) for p in procs]
    assert [o.strip() for o, _ in outs] == ["1"] * 6, outs
    assert peek(home, "SELECT count(*) FROM sqlite_master WHERE name = 'records'") == [(1,)]
    assert peek(home, "PRAGMA journal_mode") == [("wal",)]


def test_an_unknown_version_is_refused_and_left_alone(tmp_path):
    home = tmp_path / "h"
    home.mkdir()
    conn = sqlite3.connect(home / db.FILE)
    conn.execute("PRAGMA user_version = 7")
    conn.close()
    with pytest.raises(SluiceError, match="schema version 7"):
        db.connect(home)
    assert peek(home, "PRAGMA user_version") == [(7,)]
    assert peek(home, "SELECT count(*) FROM sqlite_master") == [(0,)]


def test_a_home_from_before_the_database_is_refused_not_read(tmp_path):
    home = tmp_path / "h"
    (home / "projects" / "p").mkdir(parents=True)
    (home / "projects" / "p" / "project.json").write_text('{"name": "p"}')
    with pytest.raises(SluiceError, match="predates the SQLite store"):
        Store(home).project_names()
    assert not (home / db.FILE).exists()
    old = tmp_path / "old"
    old.mkdir()
    (old / "log.jsonl").write_text("")
    with pytest.raises(SluiceError, match="log.jsonl"):
        db.connect(old)


def test_busy_is_a_defined_error_with_nothing_written(tmp_path, monkeypatch):
    monkeypatch.setattr(db, "TIMEOUT", 0.2)
    store = Store(tmp_path / "h")
    store.create_project("p")
    holder = sqlite3.connect(store.home / db.FILE, isolation_level=None)
    holder.execute("BEGIN IMMEDIATE")
    try:
        with pytest.raises(db.Busy) as err:
            store.append("p", msg())
        assert err.value.payload() == {"error": "busy",
                                       "message": "the store is busy, try again"}
    finally:
        holder.execute("ROLLBACK")
    assert "message" not in records(store.home)
    holder.close()


def test_busy_writer_actually_waits_before_release(tmp_path):
    """A write that finds the lock held waits for it (up to TIMEOUT) and then goes through:
    the lock is released only once the write's BEGIN IMMEDIATE is running against it."""
    store = Store(tmp_path / "h")
    store.create_project("p")
    holder = sqlite3.connect(store.home / db.FILE, isolation_level=None)
    holder.execute("BEGIN IMMEDIATE")
    begun, done = threading.Event(), []

    def write():
        db.connect(store.home).set_trace_callback(
            lambda sql: sql == "BEGIN IMMEDIATE" and begun.set())
        done.append(store.append("p", msg()))

    t = threading.Thread(target=write)
    t.start()
    try:
        assert begun.wait(10)  # the writer is inside BEGIN IMMEDIATE, the lock still held
        assert not done
    finally:
        holder.execute("ROLLBACK")
        holder.close()
    t.join(10)
    assert done and records(store.home)[-1] == "message"


def test_bootstrap_wal_timeout_is_busy(tmp_path, monkeypatch):
    """A first open that cannot switch the new file to WAL within TIMEOUT (another opener
    holds it) is `busy`, like any write; once the lock is gone the home opens. Errors that are
    not contention stay what they are."""
    monkeypatch.setattr(db, "TIMEOUT", 0.05)
    home = tmp_path / "h"
    home.mkdir()
    holder = sqlite3.connect(home / db.FILE, isolation_level=None)
    holder.execute("BEGIN IMMEDIATE")
    try:
        with pytest.raises(db.Busy) as err:
            db.connect(home)
        assert err.value.payload() == {"error": "busy",
                                       "message": "the store is busy, try again"}
    finally:
        holder.execute("ROLLBACK")
        holder.close()
    assert peek(home, "PRAGMA user_version") == [(0,)]
    assert db.connect(home).execute("PRAGMA user_version").fetchone()[0] == db.VERSION
    bad = tmp_path / "bad"
    bad.mkdir()
    (bad / db.FILE).write_bytes(b"not a database, just some bytes" * 64)
    with pytest.raises(sqlite3.DatabaseError) as err:
        db.connect(bad)
    assert not isinstance(err.value, db.Busy)


def test_readers_release_the_wal(tmp_path):
    """A finished read (and a wait that timed out) holds no snapshot: a checkpoint can copy
    every frame back."""
    home = tmp_path / "h"
    with db.write(home) as conn:
        L.append(conn, None, [msg()])
    L.read(home, None)
    L.wait(home, None, 0, kinds=["call"], timeout=0.05, interval=0.01)
    L.page(home, None)
    writer = sqlite3.connect(home / db.FILE, isolation_level=None)
    for i in range(20):
        writer.execute("INSERT INTO records (at, kind, data) VALUES ('x', 'message', '{}')")
    busy, frames, done = writer.execute("PRAGMA wal_checkpoint(PASSIVE)").fetchone()
    writer.close()
    assert busy == 0 and frames == done


def test_the_schema_refuses_bad_shapes(tmp_path):
    home = tmp_path / "h"
    conn = db.connect(home)
    conn.execute("INSERT INTO projects (name, created) VALUES ('p', 'now')")
    bad = ["UPDATE projects SET paused = 'yes'",
           "UPDATE projects SET icon_text = 'x', icon = x'00', icon_type = 't', icon_hash = 'h'",
           "UPDATE projects SET icon = x'00'",
           "INSERT INTO plans (project, rev, doc) VALUES ('p', 'one', '{}')",
           "INSERT INTO plans (project, rev, doc) VALUES ('p', 1, '[]')",
           "INSERT INTO states (project, doc) VALUES ('p', 'null')",
           "INSERT INTO states (project, doc) VALUES ('nobody', '{}')",
           ("INSERT INTO calls (call, fn, status, inputs, created) "
            "VALUES ('c', 'f', 'lost', '{}', 'now')"),
           "INSERT INTO records (at, kind, data) VALUES ('now', 'message', '[1]')"]
    for sql in bad:
        with pytest.raises(sqlite3.IntegrityError):
            conn.execute(sql)


def test_every_change_to_a_project_moves_its_version_and_a_rollback_takes_it_back(tmp_path):
    store = Store(tmp_path / "h")
    store.create_project("p")

    def ver():
        return peek(store.home, "SELECT ver FROM projects WHERE name = 'p'")[0][0]

    v = ver()
    store.append("p", msg())
    assert ver() > v
    v = ver()
    store.update_project("p", description="new")
    assert ver() > v
    v = ver()
    store.update_project("p", description="new")  # no change, no new version
    store.append(None, {"kind": "call", "call": "c", "fn": "f", "status": "pending"})
    assert ver() == v
    with pytest.raises(ValueError), store.tx():
        store.append("p", msg())
        raise ValueError
    assert ver() == v


def test_records_keep_explicit_nulls_and_lift_their_filter_fields(tmp_path):
    home = tmp_path / "h"
    with db.write(home) as conn:
        seq = L.append(conn, None, [{"kind": "step.status", "step": "a", "from": None,
                                     "to": "pending", "run": None}])[0]
    rec = L.read(home, None)["records"][0]
    assert rec == {"seq": seq, "at": rec["at"], "kind": "step.status", "step": "a",
                   "from": None, "to": "pending", "run": None}
    assert peek(home, "SELECT step, run FROM records") == [("a", None)]
    assert peek(home, "SELECT json_extract(record, '$.from') IS NULL, "
                      "json_type(record, '$.from') FROM log") == [(1, "null")]


def test_the_views_agree_with_status(tmp_path):
    """`steps` reads the plan and state as `status` does (absent means pending, a step's pause
    and its project's apart, manual, skipped, stale, scatter progress, outputs with explicit
    nulls); `messages`, `step_changes`, `edits` and `log` project the records."""
    from tests.conftest import create, d, write_config

    home = tmp_path / "h"
    write_config(home)
    store = Store(home)
    create(store, "p", {
        "new": {"run": "core.echo", "in": {"value": d(1)}, "paused": "wait for me"},
        "held": {"run": "core.echo", "in": {"value": d(1)}, "paused": True},
        "man": {"run": "core.echo", "in": {"value": d(1)}},
        "skip": {"run": "core.echo", "in": {"value": d(1)}},
        "old": {"run": "core.echo", "in": {"value": d(1)}},
        "fan": {"run": "test.add", "scatter": "a", "in": {"a": d([1, 2]), "b": d(1)}}})
    store.update_project("p", paused=True)
    store.write_state("p", {"inputs": {}, "steps": {
        "man": {"status": "succeeded", "outputs": {"value": None}, "manual": True},
        "skip": {"status": "skipped", "skipped": "when is false"},
        "old": {"status": "stale", "outputs": {"value": 1}},
        "fan": {"status": "running", "run_ids": ["r0", "r1"], "done": 1, "total": 2}}})
    status = {s["id"]: s for s in store.status("p", all=True)["steps"]}
    with store.rx() as conn:
        rows = {r["step"]: dict(r) for r in conn.execute(
            "SELECT * FROM steps WHERE project = 'p'")}
    assert set(rows) == set(status)
    for sid, s in status.items():
        r = rows[sid]
        assert r["status"] == s["status"] and bool(r["manual"]) == s["manual"]
        assert r["fn"] == s["run"] and r["project_paused"] == 1
        assert (r["paused"] if r["paused"] != 1 else True) == s.get("paused")
        assert r["skipped"] == s.get("skipped")
    assert json.loads(rows["man"]["outputs"]) == {"value": None}  # an explicit null kept
    assert rows["new"]["entry"] is None and rows["new"]["paused"] == "wait for me"
    assert (rows["fan"]["done"], rows["fan"]["total"]) == (1, 2)
    assert json.loads(rows["fan"]["run_ids"]) == ["r0", "r1"]

    store.append("p", {"kind": "message", "thread": "t", "from": "a", "to": "b", "body": "hi",
                       "needs_reply": False, "data": {"k": [1]}},
                 {"kind": "step.status", "step": "man", "from": None, "to": "pending"})
    with store.rx() as conn:
        [m] = [dict(r) for r in conn.execute("SELECT * FROM messages")]
        [c] = [dict(r) for r in conn.execute("SELECT * FROM step_changes")]
        edits = [dict(r) for r in conn.execute("SELECT * FROM edits ORDER BY rev")]
        log = {r["seq"]: json.loads(r["record"]) for r in conn.execute(
            "SELECT seq, record FROM log WHERE project = 'p'")}
    assert (m["thread"], m["from"], m["to"], m["body"], m["needs_reply"]) == (
        "t", "a", "b", "hi", 0)
    assert json.loads(m["data"]) == {"k": [1]}
    assert (c["step"], c["from"], c["to"]) == ("man", None, "pending")
    assert [e["rev"] for e in edits] == list(range(1, store.get("p")["rev"] + 1))
    assert all(json.loads(e["ops"]) and e["author"] == "test" for e in edits)
    assert {s: log[s] for s in log} == {r["seq"]: r for r in L.read(home, "p")["records"]}
