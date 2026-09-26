"""Threads (SPEC §10), shared by the thread.* fns: messages are `message` records in the
project's log (`{"seq", "at", "kind": "message", "thread", "from", "to"?, "body", "data"?}`), so
a thread is the log filtered by kind and thread name. Standard library only.
"""

from __future__ import annotations

import os
import re
import time
from pathlib import Path
from typing import Any

from sluice import log as L

ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]*$")


def project_log() -> tuple[Path, Path]:
    """(the project's dir, SLUICE_HOME) from the environment the runner sets."""
    home = Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")
    project = os.environ.get("SLUICE_PROJECT", "")
    if not project:
        raise RuntimeError("threads live in a project's log, but SLUICE_PROJECT is empty: "
                           "call thread fns with a project (fn_call(..., project=...)) or as "
                           "steps of a project's plan")
    d = home / "projects" / project
    if not (d / "project.json").is_file():
        raise RuntimeError(f"no project {project!r} in {home}")
    return d, home


def check_thread(name: Any) -> str:
    if not isinstance(name, str) or not ID_RE.match(name):
        raise ValueError(f"thread names match {ID_RE.pattern}, got {name!r}")
    return name


def post(thread: str, body: str, sender: str, to: str | None = None,
         data: Any = None) -> int:
    """Append one message; returns its seq (distinct and increasing across processes)."""
    d, home = project_log()
    rec: dict[str, Any] = {"kind": "message", "thread": check_thread(thread), "from": sender}
    if to is not None:
        rec["to"] = to
    rec["body"] = body
    if data is not None:
        rec["data"] = data
    return L.append_locked(d, [rec], L.cap_of(home))[0]


def addressed(rec: dict[str, Any], to: str | None) -> bool:
    """With `to`: a message addressed to it, or to nobody."""
    return to is None or rec.get("to") in (None, to)


def wait(thread: str, since_seq: int | None = None, to: str | None = None,
         timeout: float = 300, interval: float = 0.5) -> dict[str, Any]:
    """Messages on the thread after since_seq (optionally only those for `to`), waiting until
    there is at least one or `timeout` seconds pass: {messages, last_seq}."""
    d, _ = project_log()
    check_thread(thread)
    seq = since_seq or 0
    deadline = time.monotonic() + max(0.0, timeout)
    while True:
        res = L.read(d, seq, ["message"], [thread])
        found = [m for m in res["records"] if addressed(m, to)]
        seq = max(seq, res["last_seq"])
        if found or time.monotonic() >= deadline:
            return {"messages": found, "last_seq": seq}
        time.sleep(min(interval, max(0.0, deadline - time.monotonic())))
