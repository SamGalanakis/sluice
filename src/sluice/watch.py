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
           interval: float = 0.5, stop: Callable[[], bool] = lambda: False) -> None:
    """Print records after `since_seq` (default: from now on) as they are appended, flushing
    each line, until `stop()` is true."""
    kinds, threads = list(kinds or ()), list(threads or ())
    seq = L.last_seq(directory) if since_seq is None else since_seq
    while not stop():
        res = L.read(directory, seq, kinds, threads)
        for rec in res["records"]:
            out.write(json.dumps(rec, ensure_ascii=False) + "\n")
            out.flush()
        seq = res["last_seq"]
        time.sleep(interval)
