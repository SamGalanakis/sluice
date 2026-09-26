"""One-off fn calls outside the plan (SPEC §8 fn_call). A call is a series of `call` records in
the log (the project's, or the home's for a call without a project), one per status change:
`{"kind": "call", "call", "fn", "status", "inputs"?, "outputs"?, "error"?}`. The latest record
is the call's status. Its run dir is `runs/<call id>/` next to the log.

The runner starts pending calls; a direct call is run by the process that made it.
"""

from __future__ import annotations

import os
import re
import secrets
import time
from typing import Any

from . import log as L
from . import types as T
from .errors import InvalidPlan, NotFound
from .registry import Fn
from .store import Store
from .util import tail_text

CALL_RE = re.compile(r"^[0-9]{8}-[0-9]{6}-[0-9a-f]{6}$")
DONE = ("succeeded", "failed")
GONE = "the process running this direct call is gone"
FIELDS = ("call", "fn", "status", "inputs", "outputs", "error", "direct", "pid")


def check_inputs(fn: Fn, inputs: Any) -> None:
    if not isinstance(inputs, dict):
        raise InvalidPlan(["inputs: expected an object keyed by input name"])
    errs = T.check_value(T.record_of(fn.inputs), inputs, "inputs")
    errs += [f"inputs.{k}: fn {fn.name} has no input {k}" for k in inputs if k not in fn.inputs]
    if errs:
        raise InvalidPlan(errs, f"inputs do not match fn {fn.name}")


def create(store: Store, name: str, inputs: Any, project: str | None,
           direct: bool = False) -> str:
    """Check the fn and its inputs, then log the call: pending (for the runner) or, when
    `direct`, running in this process."""
    reg = store.usable_registry(project)
    fn = reg.get(name)
    if fn is None:
        raise NotFound(f"no fn {name!r}" + (f" in project {project}" if project else ""))
    check_inputs(fn, inputs)
    stamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
    call = f"{stamp}-{secrets.token_hex(3)}"
    full = {k: None for k in fn.inputs}
    full.update(inputs)
    rec: dict[str, Any] = {"kind": "call", "call": call, "fn": name,
                           "status": "running" if direct else "pending", "inputs": full}
    if direct:
        rec.update(direct=True, pid=os.getpid())
    store.append(project, rec)
    return call


def latest(store: Store, call: str, project: str | None) -> dict[str, Any]:
    """The call's latest record."""
    if not isinstance(call, str) or not CALL_RE.match(call):
        raise NotFound(f"no call {call!r}")
    if project is not None:
        store.project(project)
    rec = L.latest_call(store.log_dir(project), call)
    if rec is None:
        raise NotFound(f"no call {call!r}" + (f" in project {project}" if project else ""))
    return rec


def record(store: Store, project: str | None, rec: dict[str, Any]) -> None:
    """Log the call's new status (`rec` is its latest record, updated)."""
    new = {k: rec[k] for k in FIELDS if rec.get(k) is not None}
    if new["status"] != "pending":
        new.pop("inputs", None)  # the pending record has them; the run dir has input.json
    if new["status"] in DONE:
        new.pop("pid", None)
    store.append(project, {"kind": "call", **new})


def result(rec: dict[str, Any]) -> dict[str, Any]:
    """{call, status, outputs?, error?}"""
    out = {"call": rec["call"], "status": rec["status"]}
    out.update({k: rec[k] for k in ("outputs", "error") if rec.get(k) is not None})
    return out


def alive(pid: Any) -> bool:
    try:
        os.kill(int(pid), 0)
    except (OSError, ValueError, TypeError):
        return False
    return True


def status(store: Store, call: str, project: str | None) -> dict[str, Any]:
    """call_status: the latest record's result plus the tail of the fn's stderr."""
    rec = latest(store, call, project)
    out = result(rec)
    if rec["status"] == "running" and rec.get("direct") and not alive(rec.get("pid")):
        out.update(status="failed", error=GONE)
    tail = tail_text(store.runs_dir(project) / call / "stderr.log", 2000).strip()
    if tail:
        out["stderr_tail"] = tail
    return out
