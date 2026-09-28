"""Small shared helpers: timestamps, atomic writes, canonical JSON."""

from __future__ import annotations

import datetime as _dt
import json
import os
from pathlib import Path
from typing import Any


def now_iso() -> str:
    """UTC timestamp like 2026-09-26T14:02:11Z."""
    return _dt.datetime.now(_dt.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def canonical(obj: Any) -> str:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def atomic_write_json(path: Path, obj: Any) -> None:
    """Write `<file>.tmp`, fsync, os.replace (SPEC §2)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(json.dumps(obj, indent=2, ensure_ascii=False) + "\n")
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def atomic_write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(text)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def atomic_write_bytes(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    with open(tmp, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def tail_text(path: Path, limit: int = 2000) -> str:
    """The last `limit` characters of a text file, or '' if it does not exist."""
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - limit * 4))
            return f.read().decode("utf-8", errors="replace")[-limit:]
    except OSError:
        return ""


def parse_dotenv(text: str) -> tuple[dict[str, str], list[int]]:
    """`KEY=value` lines of a .env file (blank lines, `#` comments, `export ` and quotes handled).

    Returns (the values, the 1-based numbers of lines that are none of these).
    """
    env, bad = {}, []
    for n, line in enumerate(text.splitlines(), 1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        key, sep, value = stripped.removeprefix("export ").partition("=")
        key, value = key.strip(), value.strip()
        if not sep or not key.isidentifier():
            bad.append(n)
            continue
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
            value = value[1:-1]
        env[key] = value
    return env, bad


def read_dotenv(path: Path) -> dict[str, str]:
    """The values of a .env file ({} if it does not exist); malformed lines are skipped."""
    try:
        return parse_dotenv(path.read_text(encoding="utf-8"))[0]
    except (OSError, UnicodeDecodeError):
        return {}
