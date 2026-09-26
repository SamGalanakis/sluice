"""The inbox (SPEC §2, §8): things waiting on a person, one `inbox.json` per project. Standard
library only: the `inbox.ask` fn posts and waits from its own process.

`inbox.json` is `{"items": [item, ...]}` in posting order. An item is `{id, title, body?, ui?,
input?, from?, status, created, answer?, answered?, closed?, reason?}`; `status` is open,
answered or closed, and only an open item changes. Writers hold the project's `.lock` flock
(the lock of its state and log) and append one log record per change (`inbox.post`,
`inbox.answer`, `inbox.close`), so `log_wait` wakes on it; the items themselves live here, not
in the capped log. The Store checks answers and refusals; this module only reads and writes.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

from . import log as L
from .util import atomic_write_json, now_iso

FILE = "inbox.json"
STATUSES = ("open", "answered", "closed")
ID_RE = re.compile(r"^i(\d+)$")


def items(directory: Path) -> list[dict[str, Any]]:
    """Every item of the project in `directory`, oldest first."""
    try:
        return json.loads((Path(directory) / FILE).read_text(encoding="utf-8"))["items"]
    except FileNotFoundError:
        return []


def find(directory: Path, item_id: str) -> dict[str, Any] | None:
    return next((i for i in items(directory) if i["id"] == item_id), None)


def post(directory: Path, cap: int, title: str, body: str | None = None,
         ui: str | None = None, input: str | None = None,
         sender: str | None = None) -> dict[str, Any]:
    """Add an open item (id `i<n>`, one more than the highest so far) and log `inbox.post`.
    The caller holds the directory's flock."""
    all_items = items(directory)
    n = max((int(m[1]) for i in all_items if (m := ID_RE.match(i["id"]))), default=0) + 1
    item: dict[str, Any] = {"id": f"i{n}", "title": title}
    extra = {"body": body, "ui": ui, "input": input, "from": sender}
    item.update({k: v for k, v in extra.items() if v is not None})
    item.update(status="open", created=now_iso())
    atomic_write_json(Path(directory) / FILE, {"items": [*all_items, item]})
    rec = {"kind": "inbox.post", "item": item["id"], "title": title,
           **{k: extra[k] for k in ("from", "input") if extra[k] is not None}}
    L.append(directory, [rec], cap)
    return item


def finish(directory: Path, cap: int, item_id: str, changes: dict[str, Any],
           record: dict[str, Any]) -> dict[str, Any]:
    """Apply `changes` to an item (its new status and what goes with it) and log `record`.
    The caller holds the directory's flock and has checked the item is open."""
    all_items = items(directory)
    item = next(i for i in all_items if i["id"] == item_id)
    item.update(changes)
    atomic_write_json(Path(directory) / FILE, {"items": all_items})
    L.append(directory, [{"kind": record["kind"], "item": item_id, **record}], cap)
    return item
