"""`sluice backup PATH` (SPEC §9): an online, consistent copy of the home's database through
SQLite's backup API, taken while writers go on, and refusing to overwrite unless --force."""

import sqlite3
import threading
import time

from sluice import db
from sluice import log as L
from sluice.store import Store
from tests.test_cli import sluice  # noqa: F401 - the fixture


def test_a_copy_taken_while_a_writer_appends_is_consistent(sluice):  # noqa: F811
    store = Store(sluice.home)
    store.create_project("p")
    with db.write(store.home) as conn:  # enough pages that the copy takes a moment
        for n in range(20):
            L.append(conn, "p", [{"kind": "message", "thread": "t", "from": "x",
                                  "body": f"{n} " + "x" * 2000}])
    stop, written = threading.Event(), []

    def writer():  # two records per transaction: a consistent copy never holds half of one
        while not stop.is_set():
            with db.write(store.home) as conn:
                L.append(conn, "p", [{"kind": "message", "thread": "pair", "from": "w",
                                      "body": b} for b in ("one", "two")])
            written.append(1)

    t = threading.Thread(target=writer)
    t.start()
    try:
        while len(written) < 5:
            time.sleep(0.01)
        dest = sluice.tmp / "copies" / "sluice-backup.db"
        dest.parent.mkdir()
        for _ in range(3):
            out = sluice("backup", str(dest), "--force").stdout
            assert out.strip() == f"{dest} {dest.stat().st_size} bytes"
            copy = sqlite3.connect(dest)
            try:
                assert copy.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
                pairs = copy.execute("SELECT count(*) FROM records WHERE thread = 'pair'"
                                     ).fetchone()[0]
                assert pairs % 2 == 0 and pairs > 0
                assert copy.execute("SELECT count(*) FROM records WHERE thread = 't'"
                                    ).fetchone()[0] == 20
                assert copy.execute("SELECT name FROM projects").fetchall() == [("p",)]
                assert copy.execute("PRAGMA user_version").fetchone()[0] == db.VERSION
            finally:
                copy.close()
    finally:
        stop.set()
        t.join()
    assert len(written) > 5  # the writer kept going through the copies
    assert sorted(p.name for p in dest.parent.iterdir()) == ["sluice-backup.db"]


def test_it_refuses_an_existing_path_without_force(sluice):  # noqa: F811
    Store(sluice.home).create_project("p")
    dest = sluice.tmp / "b.db"
    dest.write_text("keep me")
    r = sluice("backup", str(dest), check=False)
    assert r.returncode == 1 and "--force" in r.stderr
    assert dest.read_text() == "keep me"
    r = sluice("backup", str(sluice.tmp), check=False)  # a directory never
    assert r.returncode == 1 and "directory" in r.stderr
    r = sluice("backup", str(sluice.home / db.FILE), "--force", check=False)
    assert r.returncode == 1 and "own database" in r.stderr
    sluice("backup", str(dest), "--force")
    copy = sqlite3.connect(dest)
    assert copy.execute("SELECT name FROM projects").fetchall() == [("p",)]
    copy.close()
