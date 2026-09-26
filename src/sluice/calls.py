"""One-off fn calls outside the plan (SPEC §8 fn_call): `calls/<call_id>/` in the home or in a
project, holding call.json (the record), input.json, output.json and stderr.log.

The runner starts pending calls; a direct call is run by the process that made it.
"""

from __future__ import annotations

import os
import re
import secrets
import time
from pathlib import Path
from typing import Any

from . import types as T
from .errors import InvalidPlan, NotFound
from .registry import Fn
from .store import Store
from .util import atomic_write_json, now_iso, read_json, tail_text

CALL_RE = re.compile(r"^[0-9]{8}-[0-9]{6}-[0-9a-f]{6}$")
DONE = ("succeeded", "failed")


def call_dir(store: Store, call: str, project: str | None) -> Path:
    if not isinstance(call, str) or not CALL_RE.match(call):
        raise NotFound(f"no call {call!r}")
    if project is not None:
        store.project(project)
    d = store.calls_dir(project) / call
    if not (d / "call.json").is_file():
        raise NotFound(f"no call {call!r}" + (f" in project {project}" if project else ""))
    return d


def check_inputs(fn: Fn, inputs: Any) -> None:
    if not isinstance(inputs, dict):
        raise InvalidPlan(["inputs: expected an object keyed by input name"])
    errs = T.check_value(T.record_of(fn.inputs), inputs, "inputs")
    errs += [f"inputs.{k}: fn {fn.name} has no input {k}" for k in inputs if k not in fn.inputs]
    if errs:
        raise InvalidPlan(errs, f"inputs do not match fn {fn.name}")


def create(store: Store, name: str, inputs: Any, project: str | None,
           direct: bool = False) -> str:
    """Check the fn and its inputs, then record a call: pending (for the runner) or, when
    `direct`, running in this process."""
    reg = store.usable_registry(project)
    fn = reg.get(name)
    if fn is None:
        raise NotFound(f"no fn {name!r}" + (f" in project {project}" if project else ""))
    check_inputs(fn, inputs)
    stamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
    call = f"{stamp}-{secrets.token_hex(3)}"
    d = store.calls_dir(project) / call
    d.mkdir(parents=True)
    full = {k: None for k in fn.inputs}
    full.update(inputs)
    atomic_write_json(d / "input.json", full)
    rec = {"call": call, "fn": name, "project": project, "created": now_iso(),
           "status": "running" if direct else "pending"}
    if direct:
        rec.update(direct=True, pid=os.getpid(), started=rec["created"])
    atomic_write_json(d / "call.json", rec)  # written last: the runner keys off call.json
    return call


def read(d: Path) -> dict[str, Any]:
    return read_json(d / "call.json")


def write(d: Path, rec: dict[str, Any]) -> None:
    atomic_write_json(d / "call.json", rec)


def result(rec: dict[str, Any]) -> dict[str, Any]:
    """{call, status, outputs?, error?}"""
    out = {"call": rec["call"], "status": rec["status"]}
    out.update({k: rec[k] for k in ("outputs", "error") if rec.get(k) is not None})
    return out


def _alive(pid: Any) -> bool:
    try:
        os.kill(int(pid), 0)
    except (OSError, ValueError, TypeError):
        return False
    return True


def status(store: Store, call: str, project: str | None) -> dict[str, Any]:
    """call_status: the result plus the tail of the fn's stderr."""
    d = call_dir(store, call, project)
    rec = read(d)
    out = result(rec)
    if rec["status"] == "running" and rec.get("direct") and not _alive(rec.get("pid")):
        out.update(status="failed", error="the process running this direct call is gone")
    tail = tail_text(d / "stderr.log", 2000).strip()
    if tail:
        out["stderr_tail"] = tail
    return out
