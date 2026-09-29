"""`sluice watch` (SPEC §10): follow a log and print each matching record as one JSON line.

The shell form of log_wait for harnesses with monitors (e.g. Claude Code's Monitor tool). It
reads the home's database only (no runner or server needed) through the same filter as
log_read.

Also `sluice next` and the `next` tool: wait for the records an orchestrator acts on
(next_up, with the wake rule in _classify: attention at once, a unit once when it settles)
and print each compactly (line).
"""

from __future__ import annotations

import datetime as dt
import json
import re
import sqlite3
import time
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import IO, Any

from . import db
from . import inbox as I
from . import log as L
from . import plan as P
from . import state as S
from .errors import BadRequest, SluiceError
from .store import Store
from .util import now_iso


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
BEAT = 30.0  # seconds between a waiting next_up's notes that it is still reading


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
    """The step's unit (P.named_units) as (name, its step ids in plan order); None for a
    standalone step or one the plan no longer has."""
    if sid not in plan.steps:
        return None
    name, ids, tagged = next(u for u in P.named_units(plan) if sid in u[1])
    return (name, ids) if tagged or len(ids) > 1 else None


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
    if kind in ("project.pause", "project.archive"):  # someone else stopped or started it
        return "wake" if rec.get("author") != me else "skip"
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
    beat = -BEAT
    while True:
        if time.monotonic() - beat >= BEAT:  # still reading, up to since_seq
            beat = time.monotonic()
            _reading(store, projects, me, since_seq)
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
                    _reading(store, projects, me, rec["seq"])
                    return {"records": records, "notes": notes, "last_seq": rec["seq"],
                            "timed_out": False}
        seq = top
        now = time.monotonic()
        if records and (now - last >= settle or now - first >= settle_max):
            _reading(store, projects, me, seq)
            return {"records": records, "notes": notes, "last_seq": seq, "timed_out": False}
        if not records and timeout is not None and now - start >= timeout:
            _reading(store, projects, me, seq)
            return {"records": [], "notes": notes, "last_seq": seq, "timed_out": True}
        time.sleep(interval)


# ---- nobody reading: a waking record no orchestrator has read -------------------------------

def _reading(store: Store, projects: list[str], me: str, seq: int) -> None:
    """Note in the `readers` table that `me` reads `projects` now and has read their logs up
    to `seq`: one row per project, its seq never going back. Best effort: next never fails
    over it (a project deleted meanwhile just gets no row)."""
    at = now_iso()
    try:
        with store.tx() as conn:
            for p in projects:
                conn.execute("INSERT INTO readers (project, seq, at, me) SELECT name, ?, ?, ? "
                             "FROM projects WHERE name = ? ON CONFLICT (project) DO UPDATE SET "
                             "seq = max(seq, excluded.seq), at = excluded.at, me = excluded.me",
                             (seq, at, me, p))
    except (SluiceError, sqlite3.Error):
        pass


def _epoch(iso: Any) -> float | None:
    try:
        return dt.datetime.strptime(str(iso), "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=dt.UTC).timestamp()
    except ValueError:
        return None


def unread_alerts(store: Store, minutes: float, now: float | None = None) -> list[dict]:
    """Post one inbox item (`from` sluice) for each project `next` has read (the `readers`
    table) whose log holds a record that would wake it (the wake rule, as `me`), when that
    record is at least `minutes` old and no `next` has read the project for as long: its
    orchestrator is likely gone. One item per such record, whatever became of it: the item
    keeps the record's seq (inbox.about), so a new threshold does not post it again. Returns
    the items posted."""
    now = time.time() if now is None else now
    with store.rx() as conn:
        rows = db.all_rows(conn, "SELECT r.* FROM readers r JOIN projects p ON p.name = "
                                 "r.project WHERE NOT p.archived ORDER BY r.project")
    posted = []
    for r in rows:
        project, at, seq = r["project"], _epoch(r["at"]), r["seq"]
        if at is None or now - at < minutes * 60:
            continue
        me, views = r["me"] or "orchestrator", {}
        rec = next((x for x in L.read(store.home, project, seq)["records"]
                    if _classify(store, {**x, "project": project}, me, views) == "wake"), None)
        when = _epoch(rec["at"]) if rec else None
        if when is None or now - when < minutes * 60:
            continue
        title = (f"No orchestrator has read {project}'s log for {minutes:g} min "
                 f"(seq {rec['seq']})")
        body = (f"`next` last read this project at {r['at']} (as `{me}`). Since then this "
                f"record would have woken it, and nobody has read it:\n\n```\n"
                f"{line({**rec, 'project': project})}\n```\n\nThe orchestrator's session "
                f"may have ended. Restart it from where it stopped (`sluice next -p {project} "
                f"--since-seq {seq}`), or close this item.")
        with store.tx() as conn:  # the check and the post in one transaction
            if I.about(conn, project, rec["seq"]) is None:
                posted.append(store.inbox_post(project, title, body, sender=I.SLUICE,
                                               seq=rec["seq"]))
    return posted


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
    if kind in ("project.pause", "project.archive"):
        what = ("paused" if rec.get("paused") else "unpaused") if kind == "project.pause" \
            else ("archived" if rec.get("archived") else "unarchived")
        why = f": {_one(rec['reason'])}" if rec.get("reason") else ""
        return f"PROJECT {rec.get('project')} {what} by {rec.get('author')}{why}"
    if isinstance(kind, str) and kind.startswith("inbox."):
        what = rec.get("title")
        if what is None:
            answer = rec.get("answer")
            what = " ".join(str(answer.get(k)) for k in ("action", "text") if answer.get(k)) \
                if isinstance(answer, dict) else answer
        return f"INBOX {kind.split('.')[1]} {rec.get('item')} {_one(what)}"
    rest = {k: v for k, v in rec.items() if k not in ("seq", "at", "kind")}
    return f"{str(kind).upper().replace('.', ' ')} {_one(rest, 300)}"


# ---- status's units view: one compact row per unit ------------------------------------------

UNIT_STATES = ("running", "failed", "blocked", "settled", "pending")
MARK = {"succeeded": "✓", "running": "▶", "pending": "·", "failed": "✗", "stale": "~",
        "skipped": "–", "paused": "‖"}  # a step's mark; a unit's state marks as its step's
STATE_MARK = {"running": "▶", "failed": "✗", "blocked": "‖", "settled": "✓", "pending": "·"}
WIDTH = 80  # the most characters of a row's line


def _age(seconds: int | None) -> str:
    """42s, 42m, 5h, 3d; – when unknown."""
    if seconds is None:
        return "–"
    for unit, size in (("d", 86400), ("h", 3600), ("m", 60)):
        if seconds >= size:
            return f"{seconds // size}{unit}"
    return f"{seconds}s"


def _cut(text: str, width: int) -> str:
    return text if len(text) <= width else text[:max(0, width - 1)] + "…"


def _since(at: str | None, now: dt.datetime) -> int | None:
    try:
        return max(0, int((now - dt.datetime.fromisoformat(at)).total_seconds())) if at else None
    except ValueError:
        return None


def _engine(plan: P.Plan, state: dict[str, Any], ids: list[str]) -> str:
    """engine·model·effort from the unit's agent step (its first step whose fn is open): the
    values it binds now, each cut to 12 characters; empty when none."""
    step = next((plan.steps[s] for s in ids if plan.steps[s].fn.open), None)
    if step is None:
        return ""
    got = [P.source_value(step.sources[k], plan, state) for k in ("engine", "model", "effort")
           if k in step.sources]
    return "·".join(_cut(v, 12) for v in got if isinstance(v, str) and v)


def _blocked(view: View, marks: dict[str, tuple[str, bool]], ids: list[str]) -> str:
    """Why a blocked unit's first held step is held: `paused`, `project paused`,
    `external`, `input <n> (no value)`, or the first edge it waits on (`after <step>
    (<status>)`, `reads <step> (<status>)`)."""
    plan, state, paused = view
    step = plan.steps[next(s for s in ids if marks[s][1])]
    if step.paused:
        return f"paused: {step.pause_reason}" if step.pause_reason else "paused"
    if paused:
        return "project paused"
    if step.fn.external:
        return "external"
    for r in step.reads:
        if r.step is None and not P.value_of(r, plan, state)[0]:
            return f"input {r.name} (no value)"
    for w in step.waits:
        status, held = marks[w]
        if status in ("failed", "stale") or held:
            what = "paused" if held and plan.steps[w].paused else status
            return f"{'reads' if w in step.deps else 'after'} {w} ({what})"
    return ""


def _line(row: dict[str, Any], age: str) -> str:
    """The row in at most WIDTH characters: name, state and age, engine, step marks, then
    what it is blocked on and its last message. Room is kept for the whole blocked reason
    and the start of the message: step names are cut to 4, then 2 characters (the marks
    stay) to make it, and what still does not fit is cut with "…"."""
    head = f"{_cut(row['unit'], 18):<10}  {STATE_MARK[row['state']]} {age:>3}"
    head += f"  {_cut(row['engine'], 20)}" if row["engine"] else ""
    last = f'"{row["last"]}"' if row["last"] else ""
    tail = "  ".join(x for x in (row["blocked"], last) if x)
    keep = len(row["blocked"]) + 2 if row["blocked"] else min(len(last) + 2, 16) if last else 0
    room, steps = WIDTH - len(head) - 2, row["steps"]
    for n in (4, 2):
        if len(steps) <= room - keep:
            break
        steps = " ".join(t[:-1][:n] + t[-1] for t in row["steps"].split(" "))
    text = f"{head}  {_cut(steps, room)}"
    rest = WIDTH - len(text) - 2
    return f"{text}  {_cut(tail, rest)}" if tail and rest >= 6 else text


def units(store: Store, project: str, steps: Any = None, tags: Any = None, state: Any = None,
          every: bool = False) -> dict[str, Any]:
    """status(view="units"): {rev, paused, units: [{unit, state, age, engine, steps,
    blocked, last, line}], done_units?}, one row per unit (P.named_units), oldest first.
    `state` keeps the units in these states; `steps`/`tags` the units with a step they
    select; without them, unless `every`, the done units are left out and counted."""
    wanted = [state] if isinstance(state, str) else state
    if wanted is not None and (not isinstance(wanted, list) or
                               any(s not in UNIT_STATES for s in wanted)):
        raise BadRequest(f"state: expected one or a list of {', '.join(UNIT_STATES)}")
    with store.rx():
        only = set(store.select_steps(project, steps, tags)) if steps or tags else None
        doc, plan = store.plan(project)
        st, paused = store.read_state(project), store.paused(project)
    changed, last = L.latest(store.home, project)
    view: View = (plan, st, paused)
    marks = _marks(view, list(plan.steps), {}, 0)
    now, rows, done = dt.datetime.now(dt.UTC), [], []
    for name, ids, _ in P.named_units(plan):
        if only is not None and not only & set(ids):
            continue
        if only is None and not every and P.unit_done(ids, st):
            done.append(ids)
            continue
        status = [marks[s][0] for s in ids]
        held = [marks[s][1] for s in ids]
        unit_state = (
            "running" if "running" in status
            else "failed" if {"failed", "stale"} & set(status)
            else "settled" if set(status) <= {"succeeded", "skipped"}
            else "blocked" if any(held) and all(x in FINAL or h for x, h in zip(status, held))
            else "pending")
        if wanted is not None and unit_state not in wanted:
            continue
        entries = [S.entry_of(st, s) for s in ids]
        if unit_state == "running":
            age = max(_since(e.get("started"), now) or 0 for e in entries
                      if e["status"] == "running")
        else:
            ats = [a for s, e in zip(ids, entries)
                   for a in (changed.get(s), e.get("started"), e.get("finished")) if a]
            age = _since(max(ats), now) if ats else None
        pre = name + "-"
        msg = max((last[f"step-{s}"] for s in ids if f"step-{s}" in last),
                  key=lambda r: r["seq"], default=None)
        text = ""
        if msg is not None:
            q = "Q: " if msg.get("needs_reply") is not False else ""
            text = _cut(q + _one(msg.get("body")), 200)
        row = {"unit": name, "state": unit_state, "age": age,
               "engine": _engine(plan, st, ids),
               "steps": " ".join(s.removeprefix(pre) + MARK[
                   "paused" if x == "pending" and plan.steps[s].paused else x]
                   for s, x in zip(ids, status)),
               "blocked": _blocked(view, marks, ids) if unit_state == "blocked" else "",
               "last": text}
        rows.append({**row, "line": _line(row, _age(age))})
    rows.sort(key=lambda r: -1 if r["age"] is None else r["age"], reverse=True)
    out = {"rev": doc["rev"], "paused": paused, "units": rows}
    if done:
        out["done_units"] = {"units": len(done), "steps": sum(map(len, done))}
    return out
