"""The inbox (SPEC §2, §8): things waiting on a person, the database's `inbox` rows. Standard
library only: the `inbox.ask` fn posts and waits from its own process.

An item is `{id, title, body?, ui?, input?, from?, run?, status, created, answer?, answered?,
closed?, reason?}`, its id `i<n>` in posting order; `status` is open, answered or closed, and
only an open item changes. Each change appends its log record (`inbox.post`, `inbox.answer`,
`inbox.close`, `inbox.adopt`) in the same write transaction, so `log_wait` wakes on it; the
items themselves live here, not in the capped log. The Store checks answers and refusals; this
module only reads and writes.

`run` is the run that asks, when a step's run posted the item (or took it up again, `adopt`):
the `sender` and `run` columns hold these identities separately. An item sluice posts itself
(`from` sluice: a waking record nobody has read, watch.unread_alerts) keeps in its `run`
column instead the `seq` of the record it is about. An open item that came from a step or a
call also carries `waiting`, derived from the state as it is read (`attend`).
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
FIELDS = ("title", "body", "ui", "input", "sender", "run", "status", "created", "answer", "answered",
          "closed", "reason")
RUN = r"[0-9A-Za-z][0-9A-Za-z_.-]*"  # log.RUN_ID_RE's
STEP_RE = re.compile(r"^(?:step:)?([a-z0-9][a-z0-9_-]*)$")  # a step id, or `step:<id>`
CALL_RE = re.compile(rf"^call ({RUN})$")  # inbox.ask run as a call
SLUICE = "sluice"  # the `from` of the items sluice posts itself


def _item(row: sqlite3.Row) -> dict[str, Any]:
    item: dict[str, Any] = {"id": f"i{row['n']}"}
    for k in FIELDS:
        v = row[k]
        if v is not None:
            item["from" if k == "sender" else k] = json.loads(v) if k == "answer" else v
    if item.get("from") == SLUICE and str(item.get("run", "")).isdigit():
        item["seq"] = int(item.pop("run"))  # sluice's own: the record it is about
    return item


def _asker(conn: sqlite3.Connection, project: str, item: dict[str, Any]) -> str | None:
    """Why nobody waits for an item any more ("build is failed"): "" while its asker is
    running the run that asked it, None when it did not come from a step or a call. A step
    asks from `from` = its id (or `step:<id>`); without a recorded `run` (an item posted
    before runs were recorded), any run of the step counts."""
    sender, run = str(item.get("from", "")), item.get("run")
    if "seq" in item:  # sluice's own
        return None
    if m := CALL_RE.match(sender):
        row = db.one(conn, "SELECT status FROM calls WHERE call = ?", (m[1],))
        status = row["status"] if row else "gone"
        return "" if status in ("pending", "running") else f"{sender} is {status}"
    m = STEP_RE.match(sender)
    if not m:
        return None
    step = m[1]
    row = db.one(conn, "SELECT status, run_ids, error, entry ->> '$.cancel' AS cancel "
                       "FROM steps WHERE project = ? AND step = ?", (project, step))
    if row is None:
        # a step that left the plan, or a sender that only looks like one
        return f"{step} is not in the plan" if run or sender != step else None
    status, runs = row["status"], json.loads(row["run_ids"] or "[]")
    if status == "running":
        if row["cancel"] is not None:
            return f"{step} is being cancelled"
        if run is None or run in runs:
            return ""
        return f"{step} is running another run"
    if status == "failed" and str(row["error"] or "").startswith("cancelled"):
        status = "cancelled"
    return f"{step} is {status}"


def attend(conn: sqlite3.Connection, items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Mark each open item that came from a step or a call with `waiting` (whether its asker
    still waits for it), and one nobody waits for with `stopped` ("build is failed")."""
    for item in items:
        if item["status"] != "open":
            continue
        why = _asker(conn, item["project"], item)
        if why is not None:
            item["waiting"] = not why
            if why:
                item["stopped"] = why
    return items


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
    return attend(conn, [{"project": r["project"], **_item(r)} for r in rows])


def find(conn: sqlite3.Connection, project: str, item_id: str) -> dict[str, Any] | None:
    m = ID_RE.match(item_id) if isinstance(item_id, str) else None
    row = m and db.one(conn, "SELECT * FROM inbox WHERE project = ? AND n = ?",
                       (project, int(m[1])))
    return _item(row) if row else None


def waiting(conn: sqlite3.Connection, project: str, item: dict[str, Any]) -> bool | None:
    """Whether the item's asker still waits for it (None: it did not come from a step or a
    call)."""
    why = _asker(conn, project, item)
    return None if why is None else not why


def open_count(conn: sqlite3.Connection) -> int:
    return db.one(conn, "SELECT count(*) FROM inbox WHERE status = 'open'")[0]


def post(conn: sqlite3.Connection, project: str, cap: int, title: str, body: str | None = None,
         ui: str | None = None, input: str | None = None, sender: str | None = None,
         run: str | None = None, seq: int | None = None) -> dict[str, Any]:
    """Add an open item (id `i<n>`, one more than the highest so far) and log `inbox.post`,
    inside the caller's write transaction. `run`: the run of the step `sender` that asks.
    `seq`: the record an item from sluice is about (then `sender` is sluice and no `run`)."""
    n = db.one(conn, "SELECT coalesce(max(n), 0) + 1 FROM inbox WHERE project = ?",
               (project,))[0]
    if seq is not None:  # sluice's own item about a record: its seq in the run column
        sender, run = SLUICE, str(int(seq))
    L.append(conn, project, [{"kind": "inbox.post", "item": f"i{n}", "title": title,
                              **{k: v for k, v in (("from", sender), ("run", run if seq is None else None),
                                                   ("input", input)) if v is not None}}], cap)
    conn.execute("INSERT INTO inbox (project, n, title, body, ui, input, sender, run, status, created) "
                 "VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'open', ?)",
                 (project, n, title, body, ui, input, sender, run, now_iso()))
    return find(conn, project, f"i{n}")


def about(conn: sqlite3.Connection, project: str, seq: int) -> dict[str, Any] | None:
    """The item sluice posted about the record `seq` of the project's log, or None."""
    row = db.one(conn, "SELECT * FROM inbox WHERE project = ? AND sender = ? AND run = ?",
                 (project, SLUICE, str(int(seq))))
    return _item(row) if row else None


def orphaned(conn: sqlite3.Connection, project: str, item_id: str) -> bool:
    """Whether an answered item's answer never reached its asker: its `inbox.answer` record
    says nobody was waiting, and no run has taken the item up since. False once the log has
    dropped that record (an answer is reused only when the log shows it was never read)."""
    row = db.one(conn, "SELECT kind, data ->> '$.waiting' AS waiting FROM records "
                       "WHERE project = ? AND kind IN ('inbox.answer', 'inbox.adopt') "
                       "AND data ->> '$.item' = ? ORDER BY seq DESC LIMIT 1", (project, item_id))
    return row is not None and row["kind"] == "inbox.answer" and row["waiting"] == 0


def ask(conn: sqlite3.Connection, project: str, cap: int, title: str, body: str | None,
        ui: str | None, sender: str, run: str | None) -> dict[str, Any]:
    """inbox.ask's item, inside the caller's write transaction: this step's own earlier item
    with the same title when nobody else waits for it (open, its run gone: a failed,
    cancelled or restarted run; or answered while nobody waited), else a new one. Taking up
    an earlier run's item records the new run on it (an open one) and logs `inbox.adopt`."""
    rows = db.all_rows(conn, "SELECT * FROM inbox WHERE project = ? AND title = ? AND "
                             "status IN ('open', 'answered') AND sender = ? "
                             "ORDER BY n DESC", (project, title, sender))
    for row in rows:
        item = _item(row)
        if item.get("run") == run and item["status"] == "open":
            return item  # this run asked it already
        if item["status"] == "open" and item.get("run") and waiting(conn, project, item):
            continue  # another live run's (a scattered step's)
        if item["status"] == "answered" and not orphaned(conn, project, item["id"]):
            continue
        if item["status"] == "open":
            conn.execute("UPDATE inbox SET run = ? WHERE project = ? AND n = ?",
                         (run, project, row["n"]))
        L.append(conn, project, [{"kind": "inbox.adopt", "item": item["id"], "from": sender,
                                  **({"run": run} if run else {}),
                                  **({"was": item["run"]} if item.get("run") else {}),
                                  "status": item["status"]}], cap)
        return find(conn, project, item["id"])
    return post(conn, project, cap, title, body, ui, sender=sender, run=run)


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
