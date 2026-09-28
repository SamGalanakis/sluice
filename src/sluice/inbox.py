"""The inbox (SPEC §2, §8): things waiting on a person, the database's `inbox` rows. Standard
library only: the `inbox.ask` fn posts and waits from its own process.

An item is `{id, title, body?, ui?, input?, from?, status, created, answer?, answered?,
closed?, reason?}`, its id `i<n>` in posting order; `status` is open, answered or closed, and
only an open item changes. Each change appends its log record (`inbox.post`, `inbox.answer`,
`inbox.close`) in the same write transaction, so `log_wait` wakes on it; the items themselves
live here, not in the capped log. The Store checks answers and refusals; this module only reads
and writes.
"""

from __future__ import annotations

import json
import re
import sqlite3
from typing import Any

from . import db
from . import log as L
from .util import now_iso

STATUSES = db.INBOX_STATUSES
ID_RE = re.compile(r"^i(\d+)$")
FIELDS = ("title", "body", "ui", "input", "sender", "status", "created", "answer", "answered",
          "closed", "reason")


def _item(row: sqlite3.Row) -> dict[str, Any]:
    item: dict[str, Any] = {"id": f"i{row['n']}"}
    for k in FIELDS:
        v = row[k]
        if v is not None:
            item["from" if k == "sender" else k] = json.loads(v) if k == "answer" else v
    return item


def items(conn: sqlite3.Connection, project: str | None = None,
          status: str | None = None) -> list[dict[str, Any]]:
    """The items of a project (of every project when None), oldest first; each carries its
    `project`."""
    where, params = ["1"], []
    if project is not None:
        where.append("project = ?")
        params.append(project)
    if status is not None:
        where.append("status = ?")
        params.append(status)
    rows = db.all_rows(conn, f"SELECT * FROM inbox WHERE {' AND '.join(where)} "
                             "ORDER BY created, project, n", params)
    return [{"project": r["project"], **_item(r)} for r in rows]


def find(conn: sqlite3.Connection, project: str, item_id: str) -> dict[str, Any] | None:
    m = ID_RE.match(item_id) if isinstance(item_id, str) else None
    row = m and db.one(conn, "SELECT * FROM inbox WHERE project = ? AND n = ?",
                       (project, int(m[1])))
    return _item(row) if row else None


def open_count(conn: sqlite3.Connection) -> int:
    return db.one(conn, "SELECT count(*) FROM inbox WHERE status = 'open'")[0]


def post(conn: sqlite3.Connection, project: str, cap: int, title: str, body: str | None = None,
         ui: str | None = None, input: str | None = None,
         sender: str | None = None) -> dict[str, Any]:
    """Add an open item (id `i<n>`, one more than the highest so far) and log `inbox.post`,
    inside the caller's write transaction."""
    n = db.one(conn, "SELECT coalesce(max(n), 0) + 1 FROM inbox WHERE project = ?",
               (project,))[0]
    L.append(conn, project, [{"kind": "inbox.post", "item": f"i{n}", "title": title,
                              **{k: v for k, v in (("from", sender), ("input", input))
                                 if v is not None}}], cap)
    conn.execute("INSERT INTO inbox (project, n, title, body, ui, input, sender, status, created) "
                 "VALUES (?, ?, ?, ?, ?, ?, ?, 'open', ?)",
                 (project, n, title, body, ui, input, sender, now_iso()))
    return find(conn, project, f"i{n}")


def finish(conn: sqlite3.Connection, project: str, cap: int, item_id: str,
           changes: dict[str, Any], record: dict[str, Any]) -> dict[str, Any]:
    """Apply `changes` to an item (its new status and what goes with it) and log `record`,
    inside the caller's write transaction; the caller has checked the item is open."""
    n = int(ID_RE.match(item_id)[1])
    cols = {k: json.dumps(v, ensure_ascii=False) if k == "answer" else v
            for k, v in changes.items()}
    conn.execute(f"UPDATE inbox SET {', '.join(f'{k} = ?' for k in cols)} "
                 "WHERE project = ? AND n = ?", (*cols.values(), project, n))
    L.append(conn, project, [{"kind": record["kind"], "item": item_id, **record}], cap)
    return find(conn, project, item_id)
