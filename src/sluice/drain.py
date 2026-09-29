"""`sluice drain` (SPEC §9): pause projects for maintenance and wait for their running work
to finish.

SQLite records which projects `drain` paused in the same transaction as their pauses,
so `drain --release` unpauses exactly those and no project someone paused otherwise.
"""

from __future__ import annotations

import contextlib
import json
from pathlib import Path
from sqlite3 import Connection
from typing import Any

from . import calls, db
from .store import Store
from .util import now_iso, read_json


def targets(store: Store, projects: list[str] | None) -> list[str]:
    """The projects a drain works on: the given ones (each must exist), or every project not
    archived."""
    if projects:
        return [store.project(p)["name"] for p in projects]
    return [p["name"] for p in store.projects() if not p["archived"]]


def _recorded(store: Store) -> dict[str, Any]:
    """The maintenance ledger, including its metadata and current project ownership."""
    with store.rx() as conn:
        row = db.one(conn, "SELECT metadata FROM drain WHERE id = 1")
        if row is None:
            return {}
        names = [r[0] for r in db.all_rows(
            conn, "SELECT project FROM drain_projects ORDER BY project")]
        return {**json.loads(row[0]), "paused": names}


def _import_legacy(store: Store, conn: Connection) -> None:
    """The persistent singleton marks a legacy ledger imported, even after release."""
    path = store.home / "drain.json"
    if db.one(conn, "SELECT id FROM drain WHERE id = 1") is None:
        try:
            doc = read_json(path)
        except (OSError, ValueError):
            doc = {}
        if not isinstance(doc, dict):
            doc = {}
        names = doc.pop("paused", [])
        conn.execute("INSERT INTO drain (id, metadata) VALUES (1, ?)",
                     (json.dumps(doc, ensure_ascii=False),))
        if isinstance(names, list):
            for name in names:
                if isinstance(name, str):
                    conn.execute("INSERT OR IGNORE INTO drain_projects (project) "
                                 "SELECT name FROM projects WHERE name = ?", (name,))
    db.after_commit(store.home, lambda: _remove_legacy(path))


def _remove_legacy(path: Path) -> None:
    """A leftover file cannot be reimported if cleanup is interrupted or refused."""
    with contextlib.suppress(OSError):
        path.unlink(missing_ok=True)


def pause(store: Store, projects: list[str], author: str = "drain") -> list[str]:
    """Pause each of `projects` that is not paused already, and merge the ones it paused
    into the SQLite ledger (an earlier drain's stay listed). Returns the newly paused
    names."""
    paused = []
    with store.tx() as conn:
        _import_legacy(store, conn)
        for name in projects:
            if not store.paused(name):
                store.update_project(name, paused=True, author=author,
                                     reason="drain: paused for maintenance")
                conn.execute("INSERT OR IGNORE INTO drain_projects (project) VALUES (?)", (name,))
                paused.append(name)
        metadata = json.loads(db.one(conn, "SELECT metadata FROM drain WHERE id = 1")[0])
        metadata["at"] = now_iso()
        conn.execute("UPDATE drain SET metadata = ? WHERE id = 1", (json.dumps(metadata),))
    return paused


def release(store: Store, author: str = "drain") -> list[str]:
    """Unpause exactly the projects the ledger lists (not ones paused otherwise), clear it,
    and return them."""
    with store.tx() as conn:
        _import_legacy(store, conn)
        names = _recorded(store)["paused"]
        for name in names:
            store.update_project(name, paused=False, author=author,
                                 reason="drain released")
        conn.execute("DELETE FROM drain_projects")
        conn.execute("UPDATE drain SET metadata = '{}' WHERE id = 1")
    return names


def pending(store: Store, projects: list[str]) -> dict[str, Any]:
    """What the drain still waits on: {running: {project: [running step ids]}, calls: the
    non-direct calls pending or running in them}."""
    running = {}
    for name in projects:
        state = store.read_state(name)
        running[name] = sorted(s for s, e in state["steps"].items()
                               if e.get("status") == "running")
    live = sum(1 for name in projects for c in calls.live(store, name)
               if not c.get("direct"))
    return {"running": running, "calls": live}


def line(left: dict[str, Any]) -> str:
    """The drain's progress line: `running: web 1 (fix-x), api 0; calls 0`."""
    running = ", ".join(f"{p} {len(ids)}" + (f" ({', '.join(ids)})" if ids else "")
                        for p, ids in left["running"].items())
    return f"running: {running}; calls {left['calls']}"
