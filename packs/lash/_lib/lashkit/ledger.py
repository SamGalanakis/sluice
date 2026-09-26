"""The known-red ledger: reds registered with an owner and an expiry.

A red is either registered here (and live) or new. Schema: see packs/lash/README.md.
"""

from __future__ import annotations

import datetime as dt
import json
from dataclasses import dataclass
from fnmatch import fnmatchcase
from pathlib import Path

from lashkit import PACK_DIR

DEFAULT_PATH = PACK_DIR / "known-reds.json"
REQUIRED = ("test", "owner", "reason", "expires")


@dataclass(frozen=True)
class Entry:
    test: str
    owner: str
    reason: str
    expires: dt.date

    def live(self, today: dt.date) -> bool:
        return today <= self.expires

    def matches(self, item: str) -> bool:
        """An item is `<gate id>`, `<label>` or `<label> <case>`; a label entry covers its cases."""
        if fnmatchcase(item, self.test):
            return True
        label, sep, _ = item.partition(" ")
        return bool(sep) and fnmatchcase(label, self.test)


def load(path: str | Path | None) -> list[Entry]:
    p = Path(path) if path else DEFAULT_PATH
    doc = json.loads(p.read_text())
    if not isinstance(doc, dict) or not isinstance(doc.get("entries"), list):
        raise TypeError(f"{p}: expected an object with an 'entries' list")
    entries, problems = [], []
    for i, raw in enumerate(doc["entries"]):
        missing = [k for k in REQUIRED if not isinstance(raw, dict) or not raw.get(k)]
        if missing:
            problems.append(f"entries[{i}]: missing {', '.join(missing)}")
            continue
        try:
            expires = dt.date.fromisoformat(str(raw["expires"]))
        except ValueError:
            problems.append(f"entries[{i}].expires: not an ISO date: {raw['expires']!r}")
            continue
        entries.append(Entry(str(raw["test"]), str(raw["owner"]), str(raw["reason"]), expires))
    if problems:
        raise ValueError(f"{p}: invalid known-red ledger: " + "; ".join(problems))
    return entries


@dataclass(frozen=True)
class Verdict:
    item: str
    kind: str  # "known" | "expired" | "new"
    entry: Entry | None

    def describe(self) -> str:
        e = self.entry
        if self.kind == "known" and e:
            return f"{self.item} (known red: {e.owner}, until {e.expires}; {e.reason})"
        if self.kind == "expired" and e:
            return f"{self.item} (known-red entry expired {e.expires}, owner {e.owner})"
        return self.item


def classify(item: str, gate_id: str, entries: list[Entry], today: dt.date) -> Verdict:
    """Live entries matching the item (or its whole gate) excuse it; expired ones do not."""
    hits = [e for e in entries if e.matches(item) or e.matches(gate_id)]
    live = [e for e in hits if e.live(today)]
    if live:
        return Verdict(item, "known", live[0])
    if hits:
        return Verdict(item, "expired", max(hits, key=lambda e: e.expires))
    return Verdict(item, "new", None)
