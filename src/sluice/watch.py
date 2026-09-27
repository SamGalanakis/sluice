"""`sluice watch` (SPEC §10): follow a log and print each matching record as one JSON line.

The shell form of log_wait for harnesses with monitors (e.g. Claude Code's Monitor tool). It
reads the log file only (no runner or server needed) through the same filter as log_read.
"""

from __future__ import annotations

import json
import time
from collections.abc import Callable, Iterable
from pathlib import Path
from typing import IO

from . import log as L


def follow(directory: Path, out: IO[str], kinds: Iterable[str] | None = None,
           threads: Iterable[str] | None = None, since_seq: int | None = None,
           interval: float = 0.5, stop: Callable[[], bool] = lambda: False,
           wake: str = "any") -> None:
    """Print records after `since_seq` (default: from now on) as they are appended, flushing
    each line, until `stop()` is true. With wake "questions", notes (needs_reply false) are
    held and printed just before the next record that wakes (L.wakes)."""
    kinds, threads = list(kinds or ()), list(threads or ())
    seq = L.last_seq(directory) if since_seq is None else since_seq
    held: list[str] = []
    while not stop():
        res = L.read(directory, seq, kinds, threads)
        for rec in res["records"]:
            held.append(json.dumps(rec, ensure_ascii=False) + "\n")
            if L.wakes(rec, wake):
                out.write("".join(held))
                out.flush()
                held.clear()
        seq = res["last_seq"]
        time.sleep(interval)
