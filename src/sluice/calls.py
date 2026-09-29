"""One-off fn calls outside the plan (SPEC §8 fn_call). A call is a row of the `calls` table
(the call's truth, its inputs kept for its whole life), and every status change updates the row
and appends a `call` record to the log (the project's, or the home's for a call without a
project) in one transaction, so log_wait wakes on it: `{"kind": "call", "call", "fn",
"status", "inputs"?, "outputs"?, "error"?}`. Its run dir is `runs/<call id>/`.

The runner starts pending calls; a direct call is run by the process that made it.
"""

from __future__ import annotations

import json
import os
import re
import secrets
import time
from pathlib import Path
from typing import Any

from . import db
from . import log as L
from . import types as T
from .errors import BadRequest, InvalidPlan, NotFound
from .registry import Fn
from .store import Store
from .util import now_iso, tail_text

CALL_RE = re.compile(r"^[0-9]{8}-[0-9]{6}-[0-9a-f]{6}$")
DONE = ("succeeded", "failed")
LIVE = ("pending", "running")
GONE = "the process running this direct call is gone"
FIELDS = ("call", "fn", "status", "inputs", "outputs", "error", "direct", "pid", "pid_start")


def check_inputs(fn: Fn, inputs: Any) -> None:
    if not isinstance(inputs, dict):
        raise InvalidPlan(["inputs: expected an object keyed by input name"])
    errs = T.check_value(T.record_of(fn.inputs), inputs, "inputs")
    errs += [f"inputs.{k}: fn {fn.name} has no input {k}" for k in inputs if k not in fn.inputs]
    if errs:
        raise InvalidPlan(errs, f"inputs do not match fn {fn.name}")


def create(store: Store, name: str, inputs: Any, project: str | None,
           direct: bool = False, author: str | None = None) -> str:
    """Check the fn and its inputs, then add the call: pending (for the runner) or, when
    `direct`, running in this process. Its first record names the `author`, when given."""
    reg = store.usable_registry(project)
    fn = reg.get(name)
    if fn is None:
        raise NotFound(f"no fn {name!r}" + (f" in project {project}" if project else ""))
    if fn.external:
        raise BadRequest(f"fn {name} is work done outside sluice and never runs: use it as a "
                         "step of a plan and set its outputs with step_set_output")
    check_inputs(fn, inputs)
    stamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
    call = f"{stamp}-{secrets.token_hex(3)}"
    full = {k: None for k in fn.inputs}
    full.update(inputs)
    rec: dict[str, Any] = {"call": call, "fn": name,
                           "status": "running" if direct else "pending", "inputs": full}
    if direct:
        rec.update(direct=True, pid=os.getpid())
        if (start := pid_start(os.getpid())) is not None:
            rec["pid_start"] = start
    by = {"author": author} if author else {}
    with store.tx() as conn:
        L.append(conn, project, [{"kind": "call", **rec, **by}], store.log_cap())
        conn.execute("INSERT INTO calls (call, project, fn, status, inputs, direct, pid, "
                     "pid_start, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                     (call, project, name, rec["status"], json.dumps(full, ensure_ascii=False),
                      int(direct), rec.get("pid"), rec.get("pid_start"), now_iso()))
    return call


def _call(row: Any) -> dict[str, Any]:
    rec = {"call": row["call"], "fn": row["fn"], "status": row["status"],
           "inputs": json.loads(row["inputs"])}
    if row["outputs"] is not None:
        rec["outputs"] = json.loads(row["outputs"])
    rec.update({k: row[k] for k in ("error", "pid", "pid_start") if row[k] is not None})
    if row["direct"]:
        rec["direct"] = True
    return rec


def latest(store: Store, call: str, project: str | None) -> dict[str, Any]:
    """The call as it stands: {call, fn, status, inputs, outputs?, error?, direct?, pid?,
    pid_start?}."""
    if not isinstance(call, str) or not CALL_RE.match(call):
        raise NotFound(f"no call {call!r}")
    with store.rx() as conn:
        if project is not None:
            store._row(conn, project)
        row = db.one(conn, "SELECT * FROM calls WHERE call = ? AND project IS ?",
                     (call, project))
    if row is None:
        raise NotFound(f"no call {call!r}" + (f" in project {project}" if project else ""))
    return _call(row)


def live(store: Store, project: str | None) -> list[dict[str, Any]]:
    """The project's (or the home's) pending and running calls, oldest first."""
    with store.rx() as conn:
        rows = db.all_rows(conn, "SELECT * FROM calls WHERE project IS ? AND status IN "
                                 "('pending', 'running') ORDER BY created, call", (project,))
    return [_call(r) for r in rows]


def record(store: Store, project: str | None, rec: dict[str, Any],
           was: tuple[str, ...] = LIVE) -> bool:
    """Record the call's new status (`rec` is the call, updated): its row and its `call` record,
    in one transaction, and only while the row's status is still one of `was` (a finished call
    never changes again). Returns whether it was recorded; nothing is written for a call (or a
    project) that is gone."""
    new = {k: rec[k] for k in FIELDS if rec.get(k) is not None}
    if new["status"] != "pending":
        new.pop("inputs", None)  # the pending record has them; so does the row
    if new["status"] in DONE:
        new.pop("pid", None)
    with store.tx() as conn:
        cur = conn.execute(
            "UPDATE calls SET status = ?, outputs = ?, error = ?, pid = ?, pid_start = ?, "
            f"finished = ? WHERE call = ? AND project IS ? AND status IN "
            f"({', '.join('?' * len(was))})",
            (rec["status"], None if rec.get("outputs") is None else
             json.dumps(rec["outputs"], ensure_ascii=False), rec.get("error"), rec.get("pid"),
             rec.get("pid_start"), now_iso() if rec["status"] in DONE else None, rec["call"],
             project, *was))
        if cur.rowcount == 0:
            return False
        L.append(conn, project, [{"kind": "call", **new}], store.log_cap())
    return True


def result(rec: dict[str, Any]) -> dict[str, Any]:
    """{call, status, outputs?, error?}"""
    out = {"call": rec["call"], "status": rec["status"]}
    out.update({k: rec[k] for k in ("outputs", "error") if rec.get(k) is not None})
    return out


def pid_start(pid: int) -> str | None:
    """The process's start time (/proc/<pid>/stat field 22): together with the pid it pins a
    process's identity, so a reused pid doesn't pass for the recorded one. None where /proc
    is missing or unreadable."""
    try:
        return Path(f"/proc/{int(pid)}/stat").read_text().rsplit(")", 1)[1].split()[19]
    except (OSError, IndexError, ValueError):
        return None


def alive(pid: Any, started: Any = None) -> bool:
    """Whether the recorded process is still there: the pid exists and — when a start time
    was recorded and /proc can be read — it is the same process, not a reused pid."""
    try:
        pid = int(pid)
    except (ValueError, TypeError):
        return False
    if started is not None and (now := pid_start(pid)) is not None:
        return now == str(started)
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    return True


def status(store: Store, call: str, project: str | None) -> dict[str, Any]:
    """call_status: the call's result plus the tail of the fn's stderr."""
    rec = latest(store, call, project)
    out = result(rec)
    if rec["status"] == "running" and rec.get("direct") \
            and not alive(rec.get("pid"), rec.get("pid_start")):
        out.update(status="failed", error=GONE)
    tail = tail_text(store.runs_dir(project) / call / "stderr.log", 2000).strip()
    if tail:
        out["stderr_tail"] = tail
    return out
