"""`sluice drain` (SPEC §9): pause projects for maintenance and wait for their running work
to finish.

`SLUICE_HOME/drain.json` records which projects `drain` paused ({"paused": [...], "at"}),
so `drain --release` unpauses exactly those and no project someone paused otherwise.
"""

from __future__ import annotations

import contextlib
from typing import Any

from . import calls
from .errors import NotFound
from .store import Store
from .util import atomic_write_json, now_iso, read_json


def targets(store: Store, projects: list[str] | None) -> list[str]:
    """The projects a drain works on: the given ones (each must exist), or every project not
    archived."""
    if projects:
        return [store.project(p)["name"] for p in projects]
    return [p["name"] for p in store.projects() if not p["archived"]]


def _recorded(store: Store) -> dict[str, Any]:
    """drain.json as it stands ({} when absent or unreadable)."""
    try:
        doc = read_json(store.home / "drain.json")
    except (OSError, ValueError):
        return {}
    return doc if isinstance(doc, dict) else {}


def pause(store: Store, projects: list[str], author: str = "drain") -> list[str]:
    """Pause each of `projects` that is not paused already, and merge the ones it paused
    into drain.json's `paused` (an earlier drain's stay listed). Returns the newly paused
    names."""
    paused = []
    for name in projects:
        if not store.paused(name):
            store.update_project(name, paused=True, author=author,
                                 reason="drain: paused for maintenance")
            paused.append(name)
    doc = _recorded(store)
    doc["paused"] = sorted({*doc.get("paused", []), *paused})
    doc["at"] = now_iso()
    atomic_write_json(store.home / "drain.json", doc)
    return paused


def release(store: Store, author: str = "drain") -> list[str]:
    """Unpause exactly the projects drain.json lists (not ones paused otherwise), delete it,
    and return them."""
    names = [p for p in _recorded(store).get("paused", []) if isinstance(p, str)]
    for name in names:
        with contextlib.suppress(NotFound):  # a deleted project needs no release
            store.update_project(name, paused=False, author=author,
                                 reason="drain released")
    (store.home / "drain.json").unlink(missing_ok=True)
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
