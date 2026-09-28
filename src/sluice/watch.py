"""`sluice watch` (SPEC §10): follow a log and print each matching record as one JSON line.

The shell form of log_wait for harnesses with monitors (e.g. Claude Code's Monitor tool). It
reads the home's database only (no runner or server needed) through the same filter as
log_read.

Also `sluice next` and the `next` tool: wait for the records an orchestrator acts on
(next_up, with the wake rule in _classify: attention at once, a unit once when it settles)
and print each compactly (line).
"""

from __future__ import annotations

import dataclasses
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


# ---- next: the records an orchestrator acts on ----------------------------------------------

ATTENTION = ("failed", "stale", "skipped")  # a step going to one of these always wakes
FINAL = ("succeeded", "skipped", "failed", "stale")  # a step in one of these will not run now
CUT = 600  # the text form's cut for one output value


def _merged(home: Path, projects: list[str], seq: int) -> tuple[list[dict[str, Any]], int]:
    """Records after `seq` across `projects`, oldest first, each with its `project`, and the
    seq past them all (seqs are the home's, so one pass per project loses nothing)."""
    found, last = [], seq
    for p in projects:
        res = L.read(home, p, seq)
        found += [{**r, "project": p} for r in res["records"]]
        last = max(last, res["last_seq"])
    return sorted(found, key=lambda r: r["seq"]), last


View = tuple[P.Plan, dict[str, Any], bool]  # a project now: its plan, state and paused flag


def _view(store: Store, project: str, views: dict[str, View | None]) -> View | None:
    """The project now (None when its plan does not load), read once per poll."""
    if project not in views:
        try:
            with store.rx():
                views[project] = (store.plan(project)[1], store.read_state(project),
                                  store.paused(project))
        except SluiceError:
            views[project] = None
    return views[project]


def _unit(plan: P.Plan, sid: str) -> tuple[str, list[str]] | None:
    """The step's unit as (name, its step ids in plan order): the steps sharing its
    `unit:<name>` tag, else its plan.units component, named by its first step, when that has
    more than one step. None for a standalone step or one the plan no longer has."""
    step = plan.steps.get(sid)
    if step is None:
        return None
    tag = _tag(step)
    if tag is not None:
        return tag.removeprefix("unit:"), [s for s, x in plan.steps.items() if tag in x.tags]
    # an untagged step's component among the untagged steps: a recipe unit it follows stays
    # a unit of its own
    loose = dataclasses.replace(plan, steps={s: x for s, x in plan.steps.items()
                                             if _tag(x) is None})
    ids = next(u for u in P.units(loose) if sid in u)
    return (ids[0], ids) if len(ids) > 1 else None


def _tag(step: P.Step) -> str | None:
    return next((t for t in step.tags if t.startswith("unit:")), None)


def _upstream(plan: P.Plan, ids: list[str]) -> list[str]:
    """`ids` and every step they wait on, transitively."""
    seen, todo = dict.fromkeys(ids), list(ids)
    while todo:
        for w in plan.steps[todo.pop()].waits:
            if w in plan.steps and w not in seen:
                seen[w] = None
                todo.append(w)
    return list(seen)


def _marks(view: View, ids: list[str], history: dict[str, list[dict[str, Any]]],
           seq: int) -> dict[str, tuple[str, bool]]:
    """Each step of `ids` (closed over what they wait on) as of `seq`: (its status, whether
    it is pending and held). Its status is the `to` of its last step.status record up to
    `seq`, else the `from` of its first one after (null: pending), else, with no record at
    all, its status now. Held: paused, its project paused, core.external, reading a plan input
    with no value, or waiting on a step that is failed, stale or itself held."""
    plan, state, paused = view
    status = {}
    for sid in ids:
        recs = history.get(sid)
        if not recs:
            status[sid] = S.entry_of(state, sid)["status"]
            continue
        before = [r for r in recs if r["seq"] <= seq]
        status[sid] = before[-1]["to"] if before else recs[0].get("from") or "pending"
    held: dict[str, bool] = {}

    def hold(sid: str) -> bool:
        if sid not in held:
            step = plan.steps[sid]
            held[sid] = status[sid] == "pending" and (
                paused or step.paused or step.fn.external
                or any(r.step is None and not P.value_of(r, plan, state)[0] for r in step.reads)
                or any(status[w] in ("failed", "stale") or hold(w)
                       for w in step.waits if w in plan.steps))
        return held[sid]

    return {sid: (status[sid], hold(sid)) for sid in ids}


def _settled(marks: dict[str, tuple[str, bool]], ids: list[str]) -> bool:
    """None of the steps is running or pending and startable, and at least one has finished
    (a unit added paused, nothing run yet, has not settled)."""
    return all(marks[s][0] in FINAL or marks[s][1] for s in ids) and \
        any(marks[s][0] in FINAL for s in ids)


def _settles(store: Store, view: View, rec: dict[str, Any],
             unit: tuple[str, list[str]]) -> dict[str, Any] | None:
    """The unit as a record carries it when `rec`, a step.status record of one of its steps,
    settles it: settled as of rec's seq and not as of the unit's step.status record before
    it. So each settling is one record, however late the log is read."""
    name, ids = unit
    closure = _upstream(view[0], ids)
    history: dict[str, list[dict[str, Any]]] = {}
    for r in L.statuses(store.home, rec["project"], closure):
        history.setdefault(r["step"], []).append(r)
    marks = _marks(view, closure, history, rec["seq"])
    if not _settled(marks, ids):
        return None
    prev = max((r["seq"] for s in ids for r in history.get(s, ()) if r["seq"] < rec["seq"]),
               default=None)
    if prev is not None and _settled(_marks(view, closure, history, prev), ids):
        return None
    steps = []
    for sid in ids:
        status, held = marks[sid]
        outs = S.entry_of(view[1], sid).get("outputs") if status == "succeeded" else None
        step = view[0].steps[sid]  # the outputs it declares when it has any: its contract
        outs = {k: outs[k] for k in (step.declared or step.outputs)
                if outs.get(k) not in (None, "", [], {})} if isinstance(outs, dict) else {}
        steps.append({"id": sid, "status": status, **({"held": True} if held else {}),
                      "outputs": outs})
    return {"name": name, "settled": True, "steps": steps}


def _classify(store: Store, rec: dict[str, Any], me: str,
              views: dict[str, View | None]) -> str:
    """"wake" when the record is one an orchestrator acts on (with `unit` set on it when it
    settles its unit), "note" when it is a note held for the next wake, else "skip".

    Wakes: a step failing, going stale or being skipped (inside a unit too); a step.status
    record that settles its unit (SPEC §9: none of its steps running or pending and
    startable, as of that record, and not so before it); a standalone step succeeding when
    its fn is open; a message needing a reply, not from `me`, addressed to `me` or to
    nobody; an inbox post or answer. A step inside a unit never wakes on its own success. A
    message from `me`, a note, and everything else is skipped — except a note not from
    `me`, which is held to print just before the waking records."""
    kind = rec.get("kind")
    if kind == "message":
        if rec.get("from") == me:
            return "skip"
        if rec.get("needs_reply", True) is False:
            return "note"
        return "wake" if rec.get("to") in (None, me) else "skip"
    if kind != "step.status":
        return "wake" if kind in ("inbox.post", "inbox.answer") else "skip"
    view = _view(store, rec["project"], views)
    step = view[0].steps.get(rec.get("step", "")) if view else None
    unit = _unit(view[0], step.id) if view and step else None
    if unit is not None and (block := _settles(store, view, rec, unit)):
        rec["unit"] = block
        return "wake"
    if rec.get("to") in ATTENTION:
        return "wake"
    return "wake" if (unit is None and step is not None and step.fn.open
                      and rec.get("to") == "succeeded") else "skip"


def next_up(store: Store, projects: list[str], since_seq: int | None = None,
            me: str = "orchestrator", timeout: float | None = None, every: bool = False,
            interval: float = 0.25, settle: float = 20, settle_max: float = 120) -> dict[str, Any]:
    """Wait for the records an orchestrator acts on across `projects` (the wake rule is
    _classify; `every` wakes on any record), then return {records, notes, last_seq,
    timed_out}. After the first waking record it keeps collecting until `settle` seconds pass
    with no new one, or `settle_max` seconds after the first; `settle` 0 returns at the first.
    `notes` are the held notes read on the way; `last_seq` the seq of the last record
    consumed (read, waking or not) — pass it as `since_seq` to continue and never miss or
    repeat one. `since_seq` None starts from now. `timeout` (None: forever) bounds the wait
    for the first waking record; a timeout ends with records empty and `timed_out` true.
    Polls with short reads; holds no transaction in between."""
    if since_seq is None:
        since_seq = max((L.last_seq(store.home, p) for p in projects), default=0)
    seq, records, notes = since_seq, [], []
    start = time.monotonic()
    first = last = 0.0
    while True:
        batch, top = _merged(store.home, projects, seq)
        views: dict[str, View | None] = {}
        for rec in batch:
            how = "wake" if every else _classify(store, rec, me, views)
            if how == "note":
                notes.append(rec)
            elif how == "wake":
                records.append(rec)
                last = time.monotonic()
                first = first or last
                if settle <= 0:  # stop at it: what follows is the next call's
                    return {"records": records, "notes": notes, "last_seq": rec["seq"],
                            "timed_out": False}
        seq = top
        now = time.monotonic()
        if records and (now - last >= settle or now - first >= settle_max):
            return {"records": records, "notes": notes, "last_seq": seq, "timed_out": False}
        if not records and timeout is not None and now - start >= timeout:
            return {"records": [], "notes": notes, "last_seq": seq, "timed_out": True}
        time.sleep(interval)


def _one(value: Any, cut: int = 400) -> str:
    """A value on one line, cut to `cut` characters."""
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)
    return re.sub(r"\s+", " ", text).strip()[:cut]


def _value(value: Any) -> str:
    """An output value on one line: a string as itself, anything else as compact JSON,
    whitespace collapsed, cut to CUT characters with "…" when cut."""
    text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False,
                                                           separators=(",", ":"))
    text = re.sub(r"\s+", " ", text).strip()
    return text if len(text) <= CUT else text[:CUT] + "…"


def _block(unit: dict[str, Any], head: str) -> list[str]:
    """`head` and the unit's step statuses (names without the `<unit>-` prefix), then one
    indented line per output of its succeeded steps."""
    pre = unit["name"] + "-"
    short = [(s, s["id"].removeprefix(pre)) for s in unit["steps"]]
    marks = " · ".join(f"{n} {s['status']}{' (held)' if s.get('held') else ''}"
                       for s, n in short)
    return [head + marks, *(f"  {n}.{k}: {_value(v)}" for s, n in short
                            for k, v in s["outputs"].items())]


def line(rec: dict[str, Any]) -> str:
    """The record as one compact block (SPEC §9): `STEP fix-x running -> failed: <error>`,
    `MSG step-x x -> orchestrator: <body>`, `NOTE …` for a held note, `INBOX post i3
    <title>`; a success that settled its unit as `UNIT <name> settled: fork succeeded · …`
    and its outputs, one indented line each; an attention record that settled its unit as
    its STEP line, then `  unit <name>: …` and the outputs."""
    kind = rec.get("kind")
    if kind == "step.status":
        unit = rec.get("unit")
        if unit and rec.get("to") not in ATTENTION:
            return "\n".join(_block(unit, f"UNIT {unit['name']} settled: "))
        tail = ""
        if rec.get("error"):
            errs = str(rec["error"]).strip().splitlines()
            tail = f": {errs[-1][:200]}" if errs else ""
        head = f"STEP {rec.get('step')} {rec.get('from') or 'pending'} -> {rec.get('to')}{tail}"
        return "\n".join([head, *(_block(unit, f"  unit {unit['name']}: ") if unit else [])])
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
