import json
import sqlite3
import threading
from concurrent.futures import ThreadPoolExecutor

import pytest

from sluice import db, drain
from sluice import log as L
from sluice.store import Store
from tests.conftest import add, create, d, settle
from tests.test_outcomes import outcomes, v1_home


@pytest.mark.parametrize("status", ["succeeded", "failed", "skipped", "stale"])
def test_reintroduced_step_has_new_state_and_keeps_previous_outcome(store, runner, status):
    create(store, "p", {"a": add(d(1), d(2))})
    entry = {"status": status, "outputs": {"sum": 3}, "error": "old failure",
             "manual": True, "run_ids": ["old"], "total": 2, "done": 2,
             "instances": {"0": {"status": "succeeded"}}}
    store.write_state("p", {"inputs": {}, "steps": {"a": entry}})
    store.remove_steps("p", ["a"])
    assert store.read_state("p")["steps"]["a"] == entry
    store.patch("p", store.get("p")["rev"],
                [{"op": "add", "path": "/steps/a", "value": add(d(3), d(4))}], "test", "reuse")
    assert "a" not in store.read_state("p")["steps"]
    assert outcomes(store)["a"]["status"] == status
    assert settle(runner, store, "p")["a"]["outputs"] == {"sum": 7}


def test_reintroduced_input_drops_previous_value_and_rolls_back_atomically(store):
    create(store, "p", {}, inputs={"n": "int"})
    store.set_input("p", "n", 3, "test", "old")
    store.patch("p", 2, [{"op": "remove", "path": "/inputs/n"}], "test", "remove")
    state = store.read_state("p")
    ops = [{"op": "add", "path": "/inputs/n", "value": "string"}]
    with pytest.raises(RuntimeError), store.tx():
        store.patch("p", 3, ops, "test", "reuse")
        assert store.read_state("p")["inputs"] == {}
        raise RuntimeError("rollback")
    assert store.get("p")["rev"] == 3 and store.read_state("p") == state
    store.patch("p", 3, ops, "test", "reuse")
    assert store.read_state("p")["inputs"] == {}


def test_same_identifier_within_one_patch_keeps_its_state(store):
    create(store, "p", {"a": add(d(1), d(2))})
    state = {"inputs": {}, "steps": {"a": {"status": "succeeded", "outputs": {"sum": 3}}}}
    store.write_state("p", state)
    store.patch("p", 2, [{"op": "remove", "path": "/steps/a"},
                         {"op": "add", "path": "/steps/a", "value": add(d(3), d(4))}],
                "test", "same lifetime")
    assert store.read_state("p") == state
    assert outcomes(store) == {}


def test_optional_input_section_can_be_removed_and_reintroduced(store):
    create(store, "p", {}, inputs={"n": "int"})
    store.set_input("p", "n", 3, "test", "old")
    store.patch("p", 2, [{"op": "remove", "path": "/inputs"}], "test", "remove section")
    store.patch("p", 3, [{"op": "add", "path": "/inputs", "value": {"n": "string"}}],
                "test", "new section")
    assert store.read_state("p")["inputs"] == {}
    store.patch("p", 4, [{"op": "replace", "path": "", "value": {"steps": {}}}],
                "test", "minimal plan")


def version_2_home(tmp_path):
    home = v1_home(tmp_path)
    conn = sqlite3.connect(home / db.FILE, autocommit=True)
    conn.executescript(db.OUTCOMES)
    conn.execute("PRAGMA user_version = 2")
    return home, conn


def test_version_2_sender_migration_preserves_previous_decoder(tmp_path):
    home, conn = version_2_home(tmp_path)
    senders = ["reviewer#r-7", "step:a#r.2", "display name#r-3", "call c-1", "a#bad value", None]
    for n, sender in enumerate(senders, 1):
        conn.execute("INSERT INTO inbox (project, n, title, sender, status, created) "
                     "VALUES ('p', ?, 'q', ?, 'open', 'now')", (n, sender))
    conn.close()
    store = Store(home)
    result = [(i.get("from"), i.get("run")) for i in store.inbox("p")]
    assert result == [("reviewer", "r-7"), ("step:a", "r.2"), ("display name#r-3", None),
                      ("call c-1", None), ("a#bad value", None), (None, None)]
    assert db.connect(home).execute("PRAGMA user_version").fetchone()[0] == db.VERSION


def test_failed_version_2_upgrade_keeps_its_schema_and_sender(tmp_path, monkeypatch):
    home, conn = version_2_home(tmp_path)
    conn.execute("INSERT INTO inbox (project, n, title, sender, status, created) "
                 "VALUES ('p', 1, 'q', 'a#r-1', 'open', 'now')")
    monkeypatch.setitem(db.MIGRATIONS, 2, db.MIGRATIONS[2] + "CREATE TABLE projects (x);")
    with pytest.raises(sqlite3.OperationalError):
        db.connect(home)
    assert conn.execute("PRAGMA user_version").fetchone()[0] == 2
    assert "run" not in [r[1] for r in conn.execute("PRAGMA table_info(inbox)")]
    assert conn.execute("SELECT sender FROM inbox").fetchone()[0] == "a#r-1"
    assert conn.execute("SELECT count(*) FROM sqlite_master WHERE name = 'drain'").fetchone()[0] == 0
    conn.close()


@pytest.mark.parametrize("operation", ["pause", "release"])
def test_drain_pause_and_ownership_roll_back_together(store, monkeypatch, operation):
    for name in "ab":
        store.create_project(name)
    if operation == "release":
        drain.pause(store, ["a", "b"])
    before = (store.projects(), drain._recorded(store), L.read(store.home, None)["records"])
    update = store.update_project

    def fail_second(name, **kwargs):
        if name == "b":
            raise RuntimeError("interrupted")
        return update(name, **kwargs)

    monkeypatch.setattr(store, "update_project", fail_second)
    with pytest.raises(RuntimeError):
        drain.pause(store, ["a", "b"]) if operation == "pause" else drain.release(store)
    assert (store.projects(), drain._recorded(store), L.read(store.home, None)["records"]) == before


def test_concurrent_drains_merge_ownership_and_release_once(store):
    for name in "ab":
        store.create_project(name)
    barrier = threading.Barrier(2)

    def pause(name):
        barrier.wait(timeout=5)
        return drain.pause(Store(store.home), [name])

    with ThreadPoolExecutor(max_workers=2) as pool:
        assert list(pool.map(pause, "ab")) == [["a"], ["b"]]
    assert drain._recorded(store)["paused"] == ["a", "b"]
    assert drain.release(store) == ["a", "b"]
    assert drain.release(store) == []


def test_legacy_cleanup_failure_never_reimports_released_ownership(store, monkeypatch):
    store.create_project("p")
    store.update_project("p", paused=True)
    path = store.home / "drain.json"
    path.write_text(json.dumps({"paused": ["p", "gone"], "note": "keep me"}))
    monkeypatch.setattr(drain, "_remove_legacy", lambda path: None)
    drain.pause(store, [])
    assert drain._recorded(store)["note"] == "keep me"
    assert drain.release(store) == ["p"]
    store.update_project("p", paused=True)
    assert drain.release(store) == [] and store.paused("p")
    assert path.exists()


def test_deleted_project_does_not_transfer_drain_ownership_to_replacement(store):
    store.create_project("p")
    drain.pause(store, ["p"])
    store.update_project("p", archived=True)
    store.delete_project("p")
    store.create_project("p")
    store.update_project("p", paused=True)
    assert drain.release(store) == [] and store.paused("p")


def test_database_backup_retains_drain_ownership(store, tmp_path):
    store.create_project("p")
    drain.pause(store, ["p"])
    dest = tmp_path / "restored"
    dest.mkdir()
    with sqlite3.connect(dest / db.FILE) as backup:
        db.connect(store.home).backup(backup)
    restored = Store(dest)
    assert drain.release(restored) == ["p"] and not restored.paused("p")
    assert store.paused("p") and drain._recorded(store)["paused"] == ["p"]
