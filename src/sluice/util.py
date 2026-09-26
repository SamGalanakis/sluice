"""Small shared helpers: durations, timestamps, atomic writes, canonical JSON."""

from __future__ import annotations

import datetime as _dt
import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any

_DURATION = re.compile(r"^(\d+)([smh])$")
_UNIT = {"s": 1, "m": 60, "h": 3600}


def parse_duration(text: Any) -> float:
    """Parse `<int><s|m|h>` into seconds. Raises ValueError on anything else."""
    m = _DURATION.match(text) if isinstance(text, str) else None
    if not m:
        raise ValueError(f"bad duration {text!r}: expected <int><s|m|h>, like '90s', '30m' or '3h'")
    return float(int(m.group(1)) * _UNIT[m.group(2)])


def now_iso(ts: float | None = None) -> str:
    """UTC timestamp like 2026-09-26T14:02:11Z."""
    t = _dt.datetime.fromtimestamp(ts, _dt.UTC) if ts is not None else _dt.datetime.now(_dt.UTC)
    return t.strftime("%Y-%m-%dT%H:%M:%SZ")


def canonical(obj: Any) -> str:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def sha256_json(obj: Any) -> str:
    return hashlib.sha256(canonical(obj).encode()).hexdigest()


def atomic_write_text(path: Path, text: str) -> None:
    """Write `<file>.tmp`, fsync, os.replace (SPEC §2)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(text)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def atomic_write_json(path: Path, obj: Any) -> None:
    atomic_write_text(path, json.dumps(obj, indent=2, ensure_ascii=False) + "\n")


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def append_line(path: Path, obj: Any) -> None:
    """Append one JSON line and fsync. Callers hold the plan lock."""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a", encoding="utf-8") as f:
        f.write(json.dumps(obj, ensure_ascii=False) + "\n")
        f.flush()
        os.fsync(f.fileno())


def tail_text(path: Path, limit: int = 4000) -> str:
    """The last `limit` characters of a text file, or '' if it does not exist."""
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            f.seek(max(0, size - limit * 4))
            data = f.read().decode("utf-8", errors="replace")
    except OSError:
        return ""
    return data[-limit:]
