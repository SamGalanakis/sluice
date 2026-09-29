"""Threads (SPEC §10), shared by the thread.* fns: messages are `message` records in the
project's log (`{"seq", "at", "kind": "message", "thread", "from", "to"?, "body",
"needs_reply", "data"?}`), so a thread is the log filtered by kind and thread name. Standard
library only.
"""

from __future__ import annotations

import os
import re
import time
from pathlib import Path
from typing import Any

from sluice import db
from sluice import log as L

ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]*$")


def project_log() -> tuple[Path, str]:
    """(SLUICE_HOME, the project) from the environment the runner sets."""
    home = Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")
    project = os.environ.get("SLUICE_PROJECT", "")
    if not project:
        raise RuntimeError("threads live in a project's log, but SLUICE_PROJECT is empty: "
                           "call thread fns with a project (fn_call(..., project=...)) or as "
                           "steps of a project's plan")
    with db.read(home) as conn:
        if db.one(conn, "SELECT 1 FROM projects WHERE name = ?", (project,)) is None:
            raise RuntimeError(f"no project {project!r} in {home}")
    return home, project


def check_thread(name: Any) -> str:
    if not isinstance(name, str) or not ID_RE.match(name):
        raise ValueError(f"thread names match {ID_RE.pattern}, got {name!r}")
    return name


def post(thread: str, body: str, sender: str, to: str | None = None,
         data: Any = None, needs_reply: bool = True) -> int:
    """Append one message to the project's log (project_log); returns its seq."""
    home, project = project_log()
    return post_to(home, project, thread, body, sender, to, data, needs_reply)


def post_to(home: Path, project: str, thread: str, body: str, sender: str,
            to: str | None = None, data: Any = None, needs_reply: bool = True) -> int:
    """Append one message to `project`'s log (thread.post and the thread_post tool); returns
    its seq (distinct and increasing across processes). `needs_reply` false marks a note (a
    heads-up, a decision already made) rather than a question; the record always says
    which."""
    rec: dict[str, Any] = {"kind": "message", "thread": check_thread(thread), "from": sender}
    if to is not None:
        rec["to"] = to
    rec["body"] = body
    rec["needs_reply"] = needs_reply is not False
    if data is not None:
        rec["data"] = data
    with db.write(home) as conn:
        return L.append(conn, project, [rec], L.cap_of(home))[0]


def addressed(rec: dict[str, Any], to: str | None) -> bool:
    """With `to`: a message addressed to it, or to nobody."""
    return to is None or rec.get("to") in (None, to)


def wait(thread: str, since_seq: int | None = None, to: str | None = None,
         timeout: float = 300, interval: float = 0.5, wake: str = "any") -> dict[str, Any]:
    """Messages on the thread after since_seq (optionally only those for `to`), waiting until
    there is at least one or `timeout` seconds pass: {messages, last_seq}. With wake
    "questions", notes (needs_reply false) do not end the wait; they come back with the next
    question, or at the timeout."""
    home, project = project_log()
    check_thread(thread)
    if wake not in L.WAKES:
        raise ValueError(f"wake: expected one of {', '.join(L.WAKES)}, got {wake!r}")
    seq = since_seq or 0
    deadline = time.monotonic() + max(0.0, timeout)
    found: list[dict[str, Any]] = []
    while True:
        res = L.wait(home, project, seq, ["message"], [thread], wake,
                     deadline - time.monotonic(), interval)
        found += [m for m in res["records"] + res["held"] if addressed(m, to)]
        seq = res["last_seq"]
        if any(L.wakes(m, wake) for m in found) or time.monotonic() >= deadline:
            return {"messages": found, "last_seq": seq}
