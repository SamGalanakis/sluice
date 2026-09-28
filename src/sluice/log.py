"""The log (SPEC §2, §6b): the database's `records`, one log per project and one (project
null) for the home's calls without a project. Standard library only: thread fns append to it
and read it from their own process.

Every record is `{"seq", "at", "kind", ...}`. `seq` increases across the whole home and a
committed one is never reused, so a log's seqs increase but are not contiguous. The log is
history, not the source of truth, so each log is capped at `config.log_max` records: an append
past the cap drops the oldest down to 90% of it in the same transaction, together with the
finished calls and the submissions no remaining record or state entry refers to. Run dirs are
never removed here (a rollback could not bring them back): the runner's GC does that.
"""

from __future__ import annotations

import json
import re
import sqlite3
import time
from collections.abc import Iterable
from pathlib import Path
from typing import Any

from . import db
from .errors import NotFound
from .util import now_iso

KINDS = ("plan.edit", "plan.input", "step.output", "step.retry", "step.status", "step.submit",
         "step.cancel", "call", "message", "inbox.post", "inbox.answer", "inbox.close",
         "run.adopt", "run.orphan")
GROUPS = ("plan", "step", "inbox", "run")  # a group name matches every kind under it
HISTORY_KINDS = ("plan.edit", "plan.input", "step.output", "step.retry")
DEFAULT_MAX = 10000
RUN_ID_RE = re.compile(r"^[0-9A-Za-z][0-9A-Za-z_.-]*$")
WAKES = ("any", "questions")
# the plan's history: every edit (plan_edits, never trimmed) and the manual values the log
# still has, as one source of records
HISTORY = ("(SELECT seq, project, at, kind, thread, data FROM records WHERE kind != 'plan.edit' "
           "UNION ALL SELECT seq, project, at, 'plan.edit', NULL, json_object('rev', rev, "
           "'author', author, 'reason', reason, 'ops', json(ops)) FROM plan_edits)")
# the run ids a log's retained records and its project's state refer to (:p, the project)
REFS = """
SELECT run FROM records WHERE project IS :p AND run IS NOT NULL
UNION SELECT call FROM records WHERE project IS :p AND call IS NOT NULL
UNION SELECT j.value FROM records r, json_each(r.data, '$.run_ids') j
  WHERE r.project IS :p AND r.kind = 'step.status'
UNION SELECT j.value FROM states s, json_each(s.doc, '$.steps') e,
  json_each(e.value, '$.run_ids') j WHERE s.project IS :p
UNION SELECT j.value FROM states s, json_each(s.doc, '$.steps') e,
  json_each(e.value, '$.kept.run_ids') j WHERE s.project IS :p
"""


def cap_of(home: Path) -> int:
    """`log_max` from SLUICE_HOME/config.json (default 10000)."""
    try:
        value = json.loads((Path(home) / "config.json").read_text(encoding="utf-8"))["log_max"]
        return max(1, int(value))
    except (OSError, ValueError, KeyError, TypeError):
        return DEFAULT_MAX


def check_kinds(kinds: Iterable[str] | None) -> list[str]:
    """Problems with a kinds filter (unknown names)."""
    known = (*KINDS, *GROUPS)
    return [f"unknown kind {k!r}; kinds: {', '.join(known)}" for k in kinds or ()
            if k not in known]


def _filter(kinds: Iterable[str] | None, threads: Iterable[str] | None) -> tuple[str, list]:
    """The one filter behind log_read, log_wait, thread.wait and `sluice watch`, as SQL.

    `kinds`: record kinds (a group name such as `step` matches `step.*`). `threads`: messages
    only on these threads; given without `kinds`, only messages are wanted at all.
    """
    kinds, threads = list(kinds or ()), list(threads or ())
    if not kinds and threads:
        kinds = ["message"]
    where, params = [], []
    if kinds:
        groups = [k for k in kinds if k in GROUPS]
        where.append("(kind IN (" + ", ".join("?" * len(kinds)) + ")"
                     + "".join(" OR kind GLOB ?" for _ in groups) + ")")
        params += [*kinds, *(g + ".*" for g in groups)]
    if threads:
        where.append("(kind != 'message' OR thread IN (" + ", ".join("?" * len(threads)) + "))")
        params += threads
    return " AND ".join(where) or "1", params


def wakes(rec: dict[str, Any], wake: str = "any") -> bool:
    """Whether a record ends a wait. With wake "questions", a note (a message posted with
    needs_reply false) does not: it comes back with the next record that does, or at the
    timeout. Every other record wakes."""
    return (wake != "questions" or rec.get("kind") != "message"
            or rec.get("needs_reply", True) is not False)


# ---- reading --------------------------------------------------------------------------------


def _high(conn: sqlite3.Connection, project: str | None, src: str = "records") -> int:
    """The log's high-water mark: the greatest seq it holds (0 when it is empty)."""
    return db.one(conn, f"SELECT max(seq) FROM {src} WHERE project IS ?", (project,))[0] or 0


def _rows(conn: sqlite3.Connection, src: str, project: str | None, where: str, params: list,
          tail: str, extra: tuple = ()) -> list[dict[str, Any]]:
    rows = db.all_rows(conn, f"SELECT seq, at, kind, data FROM {src} WHERE project IS ? AND "
                             f"{where} {tail}", [project, *params, *extra])
    return [db.record_of(r) for r in rows]


def last_seq(home: Path, project: str | None) -> int:
    """The log's last seq (0 when it has none)."""
    with db.read(home) as conn:
        return _high(conn, project)


def read(home: Path, project: str | None, since_seq: int | None = None,
         kinds: Iterable[str] | None = None, threads: Iterable[str] | None = None,
         limit: int | None = None, history: bool = False) -> dict[str, Any]:
    """`{records, last_seq}`: matching records, oldest first, from one snapshot.

    With `since_seq`: those after it, at most `limit` of them (then `last_seq` is the seq of the
    last one returned, so passing it back continues); otherwise `last_seq` is the log's last seq
    (or `since_seq`, when that is later). Without `since_seq`: the last `limit` matching records.
    `history`: the plan's history (HISTORY) instead of the log.
    """
    where, params = _filter(kinds, threads)
    src = HISTORY if history else "records"
    with db.read(home) as conn:
        if since_seq is None:
            found = _rows(conn, src, project, where, params, "ORDER BY seq DESC"
                          + (" LIMIT ?" if limit else ""), (limit,) if limit else ())
            return {"records": found[::-1], "last_seq": _high(conn, project, src)}
        found = _rows(conn, src, project, where + " AND seq > ?", [*params, since_seq],
                      "ORDER BY seq" + (" LIMIT ?" if limit else ""),
                      (limit + 1,) if limit else ())
        if limit and len(found) > limit:
            found = found[:limit]
            return {"records": found, "last_seq": found[-1]["seq"]}
        return {"records": found, "last_seq": max(since_seq, _high(conn, project, src))}


def statuses(home: Path, project: str, steps: Iterable[str]) -> list[dict[str, Any]]:
    """The `step.status` records of these steps, oldest first, in one read."""
    steps = list(steps)
    with db.read(home) as conn:
        return _rows(conn, "records", project, "kind = 'step.status' AND step IN ("
                     + ", ".join("?" * len(steps)) + ")", steps, "ORDER BY seq")


def page(home: Path, project: str | None, kinds: Iterable[str] | None = None,
         threads: Iterable[str] | None = None, before: int | None = None,
         after: int | None = None, size: int = 50, history: bool = False) -> dict[str, Any]:
    """One page of matching records, newest first, for the log viewer:
    `{records, newer, older, last_seq}`, from one snapshot.

    Without `before`/`after`: the newest `size`. With `before`: the newest `size` with a lower
    seq. With `after`: the oldest `size` with a higher seq. `newer`/`older` say whether matching
    records exist on either side of the page; `last_seq` is the log's last seq.
    """
    where, params = _filter(kinds, threads)
    src = HISTORY if history else "records"
    with db.read(home) as conn:
        def exists(cond: str, seq: int) -> bool:
            return bool(_rows(conn, src, project, f"{where} AND {cond}", [*params, seq],
                              "LIMIT 1"))

        newer = older = False
        if after is not None:
            found = _rows(conn, src, project, where + " AND seq > ?", [*params, after],
                          "ORDER BY seq LIMIT ?", (size + 1,))
            newer = len(found) > size
            found = found[:size][::-1]
            older = exists("seq <= ?", after)
        else:
            cond, extra = ("seq < ?", [before]) if before is not None else ("1", [])
            found = _rows(conn, src, project, f"{where} AND {cond}", [*params, *extra],
                          "ORDER BY seq DESC LIMIT ?", (size + 1,))
            older = len(found) > size
            found = found[:size]
            newer = before is not None and exists("seq >= ?", before)
        return {"records": found, "newer": newer, "older": older,
                "last_seq": _high(conn, project, src)}


def wait(home: Path, project: str | None, since_seq: int | None,
         kinds: Iterable[str] | None = None, threads: Iterable[str] | None = None,
         wake: str = "any", timeout: float = 300, interval: float = 0.25,
         limit: int | None = None) -> dict[str, Any]:
    """Wait for records after `since_seq`: `{records, held, last_seq}`.

    Polls with a short read every `interval` seconds (holding nothing in between), the cursor
    moving with each poll so every record is read once, until a record wakes (`wakes`),
    `limit` records have accumulated, or `timeout` seconds pass. `records` ends at the last
    record that wakes; what follows it is `held` (with wake "questions", trailing notes), still
    to come back with a later wait's records. `last_seq` is past everything read, so passing it
    back keeps watching.
    """
    seq = since_seq or 0
    deadline = time.monotonic() + max(0.0, timeout)
    found: list[dict[str, Any]] = []
    while True:
        left = None if limit is None else limit - len(found)
        res = read(home, project, seq, kinds, threads, left)
        found += res["records"]
        seq = max(seq, res["last_seq"])
        if (found and (any(wakes(r, wake) for r in found)
                       or (limit is not None and len(found) >= limit))
                or time.monotonic() >= deadline):
            if limit is not None and len(found) > limit:
                seq = found[limit - 1]["seq"]
                found = found[:limit]
            held = 0
            for r in reversed(found):
                if wakes(r, wake):
                    break
                held += 1
            return {"records": found[:len(found) - held], "held": found[len(found) - held:],
                    "last_seq": seq}
        time.sleep(min(interval, max(0.0, deadline - time.monotonic())))


# ---- writing --------------------------------------------------------------------------------


def append(conn: sqlite3.Connection, project: str | None, records: list[dict[str, Any]],
           cap: int = DEFAULT_MAX) -> list[int]:
    """Append records (kind and fields; seq and at are added) inside the caller's write
    transaction; returns their seqs. Refuses a project that does not exist (NotFound). Trims
    the log when it has grown past `cap`."""
    assert conn.in_transaction, "log.append runs inside db.write()"
    if project is not None and db.one(conn, "SELECT 1 FROM projects WHERE name = ?",
                                      (project,)) is None:
        raise NotFound(f"no project {project!r}")
    at = now_iso()
    seqs = [db.insert_record(conn, project, at, rec) for rec in records]
    trim(conn, project, cap)
    return seqs


def trim(conn: sqlite3.Connection, project: str | None, cap: int) -> None:
    """Past `cap` records, drop the oldest down to 90% of the cap (so a full log is not
    trimmed on every append), then the finished calls and the submissions nothing retained
    refers to any more. Rows only, in the caller's transaction."""
    over = db.one(conn, "SELECT seq FROM records WHERE project IS ? ORDER BY seq DESC "
                        "LIMIT 1 OFFSET ?", (project, cap))
    if over is None:
        return
    keep = max(1, cap - cap // 10)
    oldest = db.one(conn, "SELECT seq FROM records WHERE project IS ? ORDER BY seq DESC "
                          "LIMIT 1 OFFSET ?", (project, keep - 1))[0]
    conn.execute("DELETE FROM records WHERE project IS ? AND seq < ?", (project, oldest))
    conn.execute(f"DELETE FROM calls WHERE project IS :p AND status IN ('succeeded', 'failed') "
                 f"AND call NOT IN ({REFS})", {"p": project})
    conn.execute(f"DELETE FROM submissions WHERE project IS :p AND run NOT IN ({REFS})",
                 {"p": project})


def refs(conn: sqlite3.Connection, project: str | None) -> set[str]:
    """Every run id the log (the project's, or the home's) and the project still refer to: its
    records, state entries, calls and submissions."""
    rows = db.all_rows(conn, f"{REFS} UNION SELECT call FROM calls WHERE project IS :p "
                             f"UNION SELECT run FROM submissions WHERE project IS :p",
                       {"p": project})
    return {r[0] for r in rows if isinstance(r[0], str)}
