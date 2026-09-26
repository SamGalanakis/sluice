"""Shared code for the lash pack's fns. Standard library only.

Each fn's main.py puts `packs/lash/_lib` on sys.path and imports from here.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

PACK_DIR = Path(__file__).resolve().parents[2]
LOG_TAIL = 8000


def kiln() -> str:
    return os.environ.get("SLUICE_KILN_BIN", "kiln")


def just() -> str:
    return os.environ.get("SLUICE_JUST_BIN", "just")


def python() -> str:
    return os.environ.get("SLUICE_PYTHON_BIN", "python3")


def tail(text: str, limit: int = LOG_TAIL) -> str:
    return text if len(text) <= limit else "...\n" + text[-limit:]


def log(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)
