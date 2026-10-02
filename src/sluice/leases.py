"""Section leases (SPEC §6 "Resources", §7 `ctx.acquire`): a running step's fn holds an amount
of a project resource for part of its work. Standard library only: fn processes import it.

A lease is a `leases` row: the fn inserts it (waiting), the runner grants it when the resource
has room — in its step's priority order, ties first come — and the fn deletes it when its
`with` block exits. The runner deletes the leases of runs that ended (a crash, a kill, a
cancel), so a dead step never keeps a hold. Granted leases count in the same `held` totals as
the steps' `needs`.
"""

from __future__ import annotations

import json
import sqlite3
import time
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import Any

from . import db
from . import log as L
from .util import now_iso

POLL = 0.2  # seconds between a waiting fn's looks at its lease


def rows(conn: sqlite3.Connection, project: str) -> list[dict[str, Any]]:
    """The project's leases, oldest first: {id, resource, amount, step, run, granted (time or
    None while waiting), created}."""
    return [dict(r) for r in db.all_rows(
        conn, "SELECT id, resource, amount, step, run, granted, created FROM leases "
              "WHERE project = ? ORDER BY id", (project,))]


def granted(leases: Iterable[dict[str, Any]]) -> dict[str, int]:
    """What the granted leases hold, per resource."""
    out: dict[str, int] = {}
    for x in leases:
        if x["granted"] is not None:
            out[x["resource"]] = out.get(x["resource"], 0) + x["amount"]
    return out


def grant_order(waiting: list[dict[str, Any]],
                priority: Callable[[str], int]) -> list[dict[str, Any]]:
    """Waiting leases in the order the runner considers them: their step's priority, higher
    first, then first come (the lease's id)."""
    return sorted(waiting, key=lambda x: (-priority(x["step"]), x["id"]))


def _amount(v: Any) -> bool:
    return isinstance(v, int) and not isinstance(v, bool) and v >= 0


def request(home: Path, project: str, step: str, run: str, resource: str,
            amount: int) -> int:
    """Insert a waiting lease for the run of a running step; returns its id. Raises
    ValueError at once for a resource the project does not declare or an amount over its
    fixed capacity (one a capacity fn gives may grow: that waits), RuntimeError outside a
    running step's run."""
    if not _amount(amount):
        raise ValueError(f"acquire: the amount is an integer >= 0, got {amount!r}")
    if not (project and step and run):
        raise RuntimeError("acquire works only inside a plan step's run (SLUICE_PROJECT, "
                           "SLUICE_STEP and SLUICE_RUN_ID set by the runner)")
    with db.write(home) as conn:
        row = db.one(conn, "SELECT resources FROM projects WHERE name = ?", (project,))
        if row is None:
            raise RuntimeError(f"acquire: no project {project!r} in {home}")
        resources = json.loads(row["resources"])
        spec = resources.get(resource)
        if spec is None:
            raise ValueError(f"acquire: project {project} declares no resource {resource!r} "
                             f"(its resources: {', '.join(resources) or 'none'})")
        if "capacity" in spec and amount > spec["capacity"]:
            raise ValueError(f"acquire: {amount} of {resource} is more than its capacity "
                             f"{spec['capacity']}")
        entry = db.one(conn, "SELECT s.doc -> '$.steps' -> ? AS e FROM states s "
                             "WHERE s.project = ?", (f"$.{json.dumps(step)}", project))
        e = json.loads(entry["e"]) if entry is not None and entry["e"] else {}
        if e.get("status") != "running" or run not in (e.get("run_ids") or []):
            raise RuntimeError(f"acquire: run {run} is not a running run of step {step}")
        cur = conn.execute("INSERT INTO leases (project, resource, amount, step, run, "
                           "created) VALUES (?, ?, ?, ?, ?, ?)",
                           (project, resource, amount, step, run, now_iso()))
        return int(cur.lastrowid)


def wait(home: Path, project: str, lease: int, timeout: float | None = None,
         poll: float = POLL) -> None:
    """Block until the runner grants the lease (TimeoutError after `timeout` seconds;
    RuntimeError when the lease is gone: the runner dropped it, its run having ended)."""
    deadline = None if timeout is None else time.monotonic() + timeout
    while True:
        with db.read(home) as conn:
            row = db.one(conn, "SELECT granted FROM leases WHERE id = ? AND project = ?",
                         (lease, project))
        if row is None:
            raise RuntimeError(f"lease {lease} is gone (its run is no longer running)")
        if row["granted"] is not None:
            return
        if deadline is not None and time.monotonic() >= deadline:
            raise TimeoutError(f"lease {lease} not granted within {timeout:g}s")
        time.sleep(poll)


def release(home: Path, project: str, lease: int, reason: str = "") -> None:
    """Delete the lease (granted or not); a granted one leaves a `step.lease` record,
    `released`. Nothing when it is gone already."""
    with db.write(home) as conn:
        row = db.one(conn, "SELECT * FROM leases WHERE id = ? AND project = ?",
                     (lease, project))
        if row is None:
            return
        conn.execute("DELETE FROM leases WHERE id = ?", (lease,))
        if row["granted"] is not None:
            L.append(conn, project, [record(dict(row), "released", reason)], L.cap_of(home))


def record(x: dict[str, Any], state: str, reason: str = "") -> dict[str, Any]:
    """The `step.lease` record of a lease being granted (`held`) or let go (`released`)."""
    return {"kind": "step.lease", "step": x["step"], "run": x["run"],
            "resource": x["resource"], "amount": x["amount"], "state": state,
            **({"reason": reason} if reason else {})}
