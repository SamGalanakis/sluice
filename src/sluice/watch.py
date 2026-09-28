"""`sluice watch` (SPEC §10): follow a log and print each matching record as one JSON line.

The shell form of log_wait for harnesses with monitors (e.g. Claude Code's Monitor tool). It
reads the home's database only (no runner or server needed) through the same filter as
log_read.
"""

from __future__ import annotations

import json
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import IO

from . import log as L


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
