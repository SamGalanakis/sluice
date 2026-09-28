"""`sluice watch` (SPEC §10): follow a log and print each matching record as one JSON line.

The shell form of log_wait for harnesses with monitors (e.g. Claude Code's Monitor tool). It
reads the home's database only (no runner or server needed) through the same filter as
log_read.

Also `sluice next` and the `next` tool: wait for the one record an orchestrator acts on
(next_up, with the wake rule in _classify) and print it compactly (line).
"""

from __future__ import annotations

import json
import re
import time
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import IO, Any

from . import log as L
from . import plan as P
from . import state as S
from .errors import SluiceError
from .store import Store

UNIT_DONE = P.SETTLED  # a unit is done when every step of it succeeded or was skipped


def follow(home: Path, project: str | None, out: IO[str], kinds: Iterable[str] | None = None,
           threads: Iterable[str] | None = None, since_seq: int | None = None,
           interval: float = 0.5, stop: Callable[[], bool] = lambda: False,
           wake: str = "any") -> None:
    """Print records after `since_seq` (default: from now on) as they are appended, flushing
    each line, until `stop()` is true. With wake "questions", notes (needs_reply false) are
    held and printed just before the next record that wakes (L.wakes)."""
    kinds, threads = list(kinds or ()), list(threads or ())
    seq = L.last_seq(home, project) if since_seq is None else since_seq
    held: list[str] = []
    while not stop():
        res = L.wait(home, project, seq, kinds, threads, wake, interval, interval)
        seq = res["last_seq"]
        if res["records"]:
            out.write("".join(held) + "".join(json.dumps(r, ensure_ascii=False) + "\n"
                                              for r in res["records"]))
            out.flush()
            held.clear()
        held += [json.dumps(r, ensure_ascii=False) + "\n" for r in res["held"]]


# ---- next: the record an orchestrator acts on -----------------------------------------------


def _merged(home: Path, projects: list[str], seq: int) -> tuple[list[dict[str, Any]], int]:
    """Records after `seq` across `projects`, oldest first, each with its `project`, and the
    seq past them all (seqs are the home's, so one pass per project loses nothing)."""
    found, last = [], seq
    for p in projects:
        res = L.read(home, p, seq)
        found += [{**r, "project": p} for r in res["records"]]
        last = max(last, res["last_seq"])
    return sorted(found, key=lambda r: r["seq"]), last


def _unit(store: Store, project: str, sid: str) -> list[str]:
    """The step's unit, in plan order: it and every step connected to it by a handoff
    (reading its outputs) or an `after`, transitively — one independent piece of work."""
    _, plan = store.plan(project)
    joined: dict[str, set[str]] = {s: set() for s in plan.steps}
    for s, step in plan.steps.items():
        for w in step.waits:
            if w in joined:
                joined[s].add(w)
                joined[w].add(s)
    unit, todo = set(), [sid]
    while todo:
        s = todo.pop()
        if s not in unit:
            unit.add(s)
            todo += joined.get(s, ())
    return [s for s in P.topo_order(plan) if s in unit]


def _classify(store: Store, rec: dict[str, Any], me: str) -> str:
    """"wake" when the record is one an orchestrator acts on (a `unit` is set on it when the
    success completed it), "note" when it is a note held for the next wake, else "skip".

    Wakes: a step failing, going stale or being skipped; a step succeeding when its fn is
    open (agent work done) or its success settles its whole unit; a message needing a reply,
    not from `me`, addressed to `me` or to nobody; an inbox post or answer. A message from
    `me`, a note, and everything else is skipped — except a note not from `me`, which is
    held to print just before the waking record."""
    kind = rec.get("kind")
    if kind == "message":
        if rec.get("from") == me:
            return "skip"
        if rec.get("needs_reply", True) is False:
            return "note"
        return "wake" if rec.get("to") in (None, me) else "skip"
    if kind == "step.status":
        to = rec.get("to")
        if to in ("failed", "stale", "skipped"):
            return "wake"
        if to != "succeeded":
            return "skip"
        try:
            sid = rec.get("step", "")
            unit = _unit(store, rec["project"], sid)
            step = store.plan(rec["project"])[1].steps.get(sid)
            state = store.read_state(rec["project"])
        except SluiceError:
            return "skip"
        done = all(S.entry_of(state, s)["status"] in UNIT_DONE for s in unit)
        if step is not None and step.fn.open or done:
            if done:
                rec["unit"] = unit
            return "wake"
        return "skip"
    return "wake" if kind in ("inbox.post", "inbox.answer") else "skip"


def next_up(store: Store, projects: list[str], since_seq: int | None = None,
            me: str = "orchestrator", timeout: float | None = None, every: bool = False,
            interval: float = 0.25) -> dict[str, Any]:
    """Wait for the next record an orchestrator acts on across `projects` (the wake rule is
    _classify; `every` wakes on any record), then return {records: [it], notes, last_seq,
    timed_out}: `notes` are the held notes that came before it, `last_seq` the seq of the
    last record consumed (read, waking or not) — pass it as `since_seq` to continue and
    never miss or repeat one. `since_seq` None starts from now. `timeout` None waits
    forever; a timeout ends with records empty and `timed_out` true. Polls with short
    reads; holds no transaction in between."""
    if since_seq is None:
        since_seq = max((L.last_seq(store.home, p) for p in projects), default=0)
    seq, notes = since_seq, []
    deadline = None if timeout is None else time.monotonic() + timeout
    while True:
        batch, seq = _merged(store.home, projects, seq)
        for rec in batch:
            how = "wake" if every else _classify(store, rec, me)
            if how == "wake":  # stop at it: what follows is the next call's
                return {"records": [rec], "notes": notes, "last_seq": rec["seq"],
                        "timed_out": False}
            if how == "note":
                notes.append(rec)
        if deadline is not None and time.monotonic() >= deadline:
            return {"records": [], "notes": notes, "last_seq": seq, "timed_out": True}
        time.sleep(interval)


def _one(value: Any, cut: int = 400) -> str:
    """A value on one line, cut to `cut` characters."""
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
    return re.sub(r"\s+", " ", text).strip()[:cut]


def line(rec: dict[str, Any]) -> str:
    """The record as one compact line (SPEC §9): `STEP fix-x running -> failed: <error>`,
    `MSG step-x x -> orchestrator: <body>`, `NOTE …` for a held note, `INBOX post i3
    <title>`, `UNIT done: a … c (3 steps)` for a success that settled its unit."""
    kind = rec.get("kind")
    if kind == "step.status":
        if rec.get("unit") and len(rec["unit"]) > 1:
            unit = rec["unit"]
            return f"UNIT done: {unit[0]} … {unit[-1]} ({len(unit)} steps)"
        tail = ""
        if rec.get("error"):
            errs = str(rec["error"]).strip().splitlines()
            tail = f": {errs[-1][:200]}" if errs else ""
        return (f"STEP {rec.get('step')} {rec.get('from') or 'pending'} -> "
                f"{rec.get('to')}{tail}")
    if kind == "message":
        tag = "NOTE" if rec.get("needs_reply") is False else "MSG"
        return (f"{tag} {rec.get('thread')} {rec.get('from')} -> {rec.get('to') or '-'}: "
                f"{_one(rec.get('body'))}")
    if isinstance(kind, str) and kind.startswith("inbox."):
        what = rec.get("title")
        if what is None:
            answer = rec.get("answer")
            what = " ".join(str(answer.get(k)) for k in ("action", "text") if answer.get(k)) \
                if isinstance(answer, dict) else answer
        return f"INBOX {kind.split('.')[1]} {rec.get('item')} {_one(what)}"
    rest = {k: v for k, v in rec.items() if k not in ("seq", "at", "kind")}
    return f"{str(kind).upper().replace('.', ' ')} {_one(rest, 300)}"
