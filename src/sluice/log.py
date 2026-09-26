"""The log (SPEC §2, §6b): one append-only `log.jsonl` per project, and one in SLUICE_HOME for
calls without a project. Standard library only: thread fns append to it from their own process.

Every record is `{"seq", "at", "kind", ...}`; `seq` counts up per log. Appends happen under the
directory's `.lock` flock, so writers in any process get distinct, increasing seqs. The log is
history, not the source of truth (plan.json and state.json are), so it is capped at
`config.log_max` records: past the cap the oldest records are dropped, together with the run
dirs no remaining record (nor state.json) refers to. The latest record of a call that is still
pending or running is never dropped.
"""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import re
import shutil
from collections.abc import Iterable, Iterator
from pathlib import Path
from typing import Any

from .util import now_iso

KINDS = ("plan.edit", "plan.input", "step.output", "step.retry", "step.status", "call",
         "message")
GROUPS = ("plan", "step")  # a group name matches every kind under it, e.g. step -> step.status
HISTORY_KINDS = ("plan.edit", "plan.input", "step.output", "step.retry")
LIVE = ("pending", "running")
DEFAULT_MAX = 10000
FILE = "log.jsonl"
LOCK = ".lock"
BLOCK = 1 << 16
RUN_ID_RE = re.compile(r"^[0-9A-Za-z][0-9A-Za-z_.-]*$")


@contextlib.contextmanager
def flock(lock_path: Path) -> Iterator[None]:
    """An exclusive flock on `lock_path` (created if missing). Not re-entrant: Store.lock is."""
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o644)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)  # closing the descriptor releases the flock


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


def matches(rec: dict[str, Any], kinds: Iterable[str] | None = None,
            threads: Iterable[str] | None = None) -> bool:
    """The one filter behind log_read, log_wait, thread.wait and `sluice watch`.

    `kinds`: record kinds (a group name such as `step` matches `step.*`). `threads`: messages
    only on these threads; given without `kinds`, only messages are wanted at all.
    """
    kind = rec.get("kind", "")
    kinds = list(kinds or ())
    threads = list(threads or ())
    if not kinds and threads:
        kinds = ["message"]
    if kinds and not any(kind == k or kind.startswith(k + ".") for k in kinds):
        return False
    return not (threads and kind == "message" and rec.get("thread") not in threads)


# ---- reading --------------------------------------------------------------------------------


def _parse(line: bytes) -> dict[str, Any] | None:
    try:
        rec = json.loads(line)
    except (ValueError, UnicodeDecodeError):
        return None
    return rec if isinstance(rec, dict) and isinstance(rec.get("seq"), int) else None


def _all(path: Path) -> list[dict[str, Any]]:
    """Every complete record, oldest first (a half-written last line is left out)."""
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        return []
    lines = data.split(b"\n")[:-1]  # the part after the last newline is not complete yet
    return [r for r in map(_parse, lines) if r is not None]


def _backwards(path: Path) -> Iterator[dict[str, Any]]:
    """Complete records from the newest back, reading the file in blocks from its end so a
    reader interested in the latest records pays only for those."""
    try:
        f = open(path, "rb")  # noqa: SIM115 - a generator keeps it open while iterated
    except FileNotFoundError:
        return
    with f:
        pos = f.seek(0, os.SEEK_END)
        tail = b""
        partial = True  # still inside the text after the last newline (not complete yet)
        while pos > 0:
            step = min(BLOCK, pos)
            pos -= step
            f.seek(pos)
            lines = (f.read(step) + tail).split(b"\n")
            if partial:
                if len(lines) == 1:
                    tail = b""
                    continue
                lines.pop()
                partial = False
            tail = lines.pop(0) if pos > 0 else b""
            for line in reversed(lines):
                rec = _parse(line)
                if rec is not None:
                    yield rec


def _after(path: Path, since_seq: int) -> tuple[list[dict[str, Any]], int]:
    """Records with seq > since_seq (oldest first) and the last seq in the log."""
    out: list[dict[str, Any]] = []
    last = 0
    for rec in _backwards(path):
        last = last or rec["seq"]
        if rec["seq"] <= since_seq:
            break
        out.append(rec)
    out.reverse()
    return out, last


def last_seq(directory: Path) -> int:
    return _after(Path(directory) / FILE, 2 ** 62)[1]


def last_record(directory: Path) -> dict[str, Any] | None:
    recs = _after(Path(directory) / FILE, last_seq(directory) - 1)[0]
    return recs[-1] if recs else None


def read(directory: Path, since_seq: int | None = None, kinds: Iterable[str] | None = None,
         threads: Iterable[str] | None = None, limit: int | None = None) -> dict[str, Any]:
    """`{records, last_seq}`: matching records, oldest first.

    With `since_seq`: those after it, at most `limit` of them (then `last_seq` is the seq of the
    last one returned, so passing it back continues); otherwise `last_seq` is the log's last seq.
    Without `since_seq`: the last `limit` matching records.
    """
    path = Path(directory) / FILE
    if since_seq is None:
        recs = _all(path)
        last = recs[-1]["seq"] if recs else 0
        found = [r for r in recs if matches(r, kinds, threads)]
        return {"records": found[-limit:] if limit else found, "last_seq": last}
    recs, last = _after(path, since_seq)
    found = [r for r in recs if matches(r, kinds, threads)]
    if limit and len(found) > limit:
        found = found[:limit]
        last = found[-1]["seq"]
    return {"records": found, "last_seq": max(last, 0)}


def latest_call(directory: Path, call: str) -> dict[str, Any] | None:
    """The most recent `call` record for this call id."""
    for rec in _backwards(Path(directory) / FILE):
        if rec.get("kind") == "call" and rec.get("call") == call:
            return rec
    return None


# ---- writing --------------------------------------------------------------------------------


def append(directory: Path, records: list[dict[str, Any]], cap: int = DEFAULT_MAX) -> list[int]:
    """Append records (kind and fields; seq and at are added). The caller holds the dir's flock.
    Returns their seqs. Trims the log when it has grown past `cap`."""
    directory = Path(directory)
    path = directory / FILE
    directory.mkdir(parents=True, exist_ok=True)
    _, last = _after(path, 2 ** 62)
    first = _first_seq(path)
    at = now_iso()
    seqs, lines = [], []
    for i, rec in enumerate(records):
        seq = last + 1 + i
        seqs.append(seq)
        lines.append(json.dumps({"seq": seq, "at": at, **rec}, ensure_ascii=False))
    with open(path, "ab") as f:
        f.write(("\n".join(lines) + "\n").encode())
        f.flush()
        os.fsync(f.fileno())
    if first is not None and seqs[-1] - first + 1 > cap:  # maybe over the cap: count for real
        trim(directory, cap)
    return seqs


def append_locked(directory: Path, records: list[dict[str, Any]],
                  cap: int = DEFAULT_MAX) -> list[int]:
    """append() taking the directory's flock itself (for writers outside the Store)."""
    with flock(Path(directory) / LOCK):
        return append(directory, records, cap)


def _first_seq(path: Path) -> int | None:
    try:
        with open(path, "rb") as f:
            rec = _parse(f.readline())
    except FileNotFoundError:
        return None
    return rec["seq"] if rec else None


def refs(rec: dict[str, Any]) -> set[str]:
    """Run ids a record refers to: a call's run dir, a finished step's run dirs."""
    if rec.get("kind") == "call":
        return {rec.get("call", "")}
    if rec.get("kind") == "step.status":
        return set(rec.get("run_ids") or [])
    return set()


def trim(directory: Path, cap: int) -> None:
    """Past `cap` records, drop the oldest down to 90% of the cap (so a full log is not
    rewritten on every append), keeping the latest record of every pending or running call,
    then remove the run dirs only dropped records referred to. The caller holds the flock."""
    directory = Path(directory)
    path = directory / FILE
    raw = [ln for ln in path.read_bytes().split(b"\n")[:-1] if ln.strip()]
    if len(raw) <= cap:
        return
    recs = [_parse(ln) or {} for ln in raw]
    latest: dict[str, int] = {}
    for i, r in enumerate(recs):
        if r.get("kind") == "call":
            latest[r.get("call", "")] = i
    pinned = {i for i in latest.values() if recs[i].get("status") in LIVE}
    keep_n = max(1, cap - cap // 10)
    drop_n = len(raw) - keep_n
    dropped: set[int] = set()
    for i in range(len(raw)):
        if len(dropped) >= drop_n:
            break
        if i not in pinned:
            dropped.add(i)
    kept = [i for i in range(len(raw)) if i not in dropped]
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "wb") as f:
        f.write(b"".join(raw[i] + b"\n" for i in kept))
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)
    still = set().union(*(refs(recs[i]) for i in kept)) | _state_run_ids(directory)
    gone = set().union(*(refs(recs[i]) for i in dropped)) - still
    for run_id in gone:
        if RUN_ID_RE.match(run_id):
            shutil.rmtree(directory / "runs" / run_id, ignore_errors=True)


def _state_run_ids(directory: Path) -> set[str]:
    try:
        state = json.loads((directory / "state.json").read_text(encoding="utf-8"))
        return {r for e in state["steps"].values() for r in e.get("run_ids") or []}
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        return set()
