"""The workspace on disk (SPEC §2, §5.3, §6): plans, the edit log, state, events, inbox."""

from __future__ import annotations

import contextlib
import copy
import fcntl
import json
import os
import threading
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import jsonpatch
import jsonpointer

from . import plan as P
from .errors import BadRequest, Conflict, InvalidPlan, NotFound
from .fns import Registry
from .util import append_line, atomic_write_json, now_iso, parse_duration, read_json

DEFAULT_CONFIG: dict[str, Any] = {
    "packs": [],
    "slots": {"default": 8, "agent": 6, "heavy": 2},
    "tick": "2s",
    "http": {"host": "127.0.0.1", "port": 7420},
}


def default_home() -> Path:
    return Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")


def load_config(home: Path) -> dict[str, Any]:
    path = home / "config.json"
    cfg = copy.deepcopy(DEFAULT_CONFIG)
    if path.exists():
        raw = read_json(path)
        if not isinstance(raw, dict):
            raise BadRequest(f"{path}: expected an object")
        cfg.update(raw)
    cfg["packs"] = [str((home / p).resolve()) if not os.path.isabs(p) else p
                    for p in cfg.get("packs", [])]
    parse_duration(cfg["tick"])
    return cfg


def empty_state() -> dict[str, Any]:
    return {"rev": 0, "plan_rev": 0, "nodes": {}}


class Store:
    """All reads and writes of a SLUICE_HOME. Safe across threads and processes (flock)."""

    def __init__(self, home: Path | str | None = None, registry: Registry | None = None):
        self.home = Path(home) if home is not None else default_home()
        self.config = load_config(self.home)
        self._registry = registry
        self.listeners: list[Callable[[str], None]] = []
        self._held = threading.local()
        self._exp_cache: dict[str, tuple[int, P.Expanded]] = {}
        self._exp_lock = threading.Lock()

    # ---- paths and config ----

    @property
    def registry(self) -> Registry:
        if self._registry is None:
            self._registry = Registry.load(self.config["packs"])
        return self._registry

    def plan_dir(self, pid: str) -> Path:
        return self.home / "plans" / pid

    @property
    def runs_dir(self) -> Path:
        return self.home / "runs"

    @property
    def cache_dir(self) -> Path:
        return self.home / "cache"

    def slot_capacity(self, name: str) -> int:
        """Configured capacity of a slot; a name missing from config has capacity 1."""
        return int(self.config.get("slots", {}).get(name, 1))

    # ---- locking ----

    @contextlib.contextmanager
    def lock(self, pid: str) -> Iterator[None]:
        """Exclusive flock on the plan dir's .lock; re-entrant within a thread."""
        held: dict[str, list[int]] = self._held.__dict__.setdefault("locks", {})
        entry = held.get(pid)
        if entry is not None:
            entry[1] += 1
            try:
                yield
            finally:
                entry[1] -= 1
            return
        d = self.plan_dir(pid)
        d.mkdir(parents=True, exist_ok=True)
        fd = os.open(d / ".lock", os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            held[pid] = [fd, 1]
            try:
                yield
            finally:
                del held[pid]
                fcntl.flock(fd, fcntl.LOCK_UN)
        finally:
            os.close(fd)

    def _notify(self, pid: str) -> None:
        for fn in list(self.listeners):
            fn(pid)

    # ---- plans ----

    def plan_ids(self) -> list[str]:
        root = self.home / "plans"
        if not root.is_dir():
            return []
        return sorted(p.name for p in root.iterdir() if (p / "plan.json").is_file())

    def _require(self, pid: str) -> Path:
        d = self.plan_dir(pid)
        if not P.ID_RE.match(pid) or not (d / "plan.json").is_file():
            raise NotFound(f"no plan {pid!r}")
        return d

    def get(self, pid: str) -> dict[str, Any]:
        """The current document, including `rev`."""
        return read_json(self._require(pid) / "plan.json")

    def expanded(self, pid: str) -> tuple[dict[str, Any], P.Expanded]:
        """The current document and its expansion (cached per rev)."""
        doc = self.get(pid)
        with self._exp_lock:
            hit = self._exp_cache.get(pid)
        if hit is not None and hit[0] == doc["rev"]:
            return doc, hit[1]
        exp = P.expand_doc(doc, self.registry)
        with self._exp_lock:
            self._exp_cache[pid] = (doc["rev"], exp)
        return doc, exp

    def validate(self, doc: Any, pid: str | None = None,
                 state: dict[str, Any] | None = None) -> list[str]:
        """Validate a document without writing. With `pid`, rule 9 runs against that plan."""
        previous = None
        if pid is not None and state is None:
            with contextlib.suppress(NotFound):
                state = self.read_state(pid)
        if pid is not None:
            with contextlib.suppress(NotFound, InvalidPlan):
                previous = self.expanded(pid)[1]
        errs, _ = P.validate(doc, self.registry, state=state, previous=previous)
        return errs

    def create(self, pid: str, doc: Any, author: str, reason: str) -> int:
        if not isinstance(pid, str) or not P.ID_RE.match(pid):
            raise BadRequest(f"plan ids match {P.ID_RE.pattern}, got {pid!r}")
        if not isinstance(doc, dict):
            raise InvalidPlan(["document: expected an object"])
        doc = {k: v for k, v in doc.items() if k != "rev"}
        doc.setdefault("id", pid)
        if doc["id"] != pid:
            raise InvalidPlan([f"id: the document says {doc['id']!r} but the plan is {pid!r}"])
        errs, _ = P.validate(doc, self.registry)
        if errs:
            raise InvalidPlan(errs)
        d = self.plan_dir(pid)
        (self.home / "plans").mkdir(parents=True, exist_ok=True)
        with self.lock(pid):
            if (d / "plan.json").exists():
                raise BadRequest(f"plan {pid!r} already exists")
            append_line(d / "plan.log.jsonl", {"rev": 1, "at": now_iso(), "author": author,
                                               "reason": reason,
                                               "ops": [{"op": "add", "path": "", "value": doc}]})
            atomic_write_json(d / "plan.json", {**doc, "rev": 1})
            self.append_event(pid, "plan_created", data={"rev": 1, "author": author,
                                                         "reason": reason})
        self._notify(pid)
        return 1

    def patch(self, pid: str, rev: int, ops: Any, author: str, reason: str, *,
              state: dict[str, Any] | None = None) -> int:
        """Apply an RFC 6902 patch at `rev`. Raises Conflict, InvalidPlan or NotFound."""
        d = self._require(pid)
        with self.lock(pid):
            cur = self.get(pid)
            if rev != cur["rev"]:
                raise Conflict(cur["rev"])
            doc = {k: v for k, v in cur.items() if k != "rev"}
            new = apply_ops(doc, ops)
            if new.get("id") != pid:
                raise InvalidPlan(["id: the plan id cannot change"])
            if state is None:
                state = self.read_state(pid)
            try:
                previous = self.expanded(pid)[1]
            except InvalidPlan:
                previous = None
            errs, _ = P.validate(new, self.registry, state=state, previous=previous)
            if errs:
                raise InvalidPlan(errs)
            new_rev = cur["rev"] + 1
            append_line(d / "plan.log.jsonl", {"rev": new_rev, "at": now_iso(), "author": author,
                                               "reason": reason, "ops": ops})
            atomic_write_json(d / "plan.json", {**new, "rev": new_rev})
            self.append_event(pid, "plan_patched", data={"rev": new_rev, "author": author,
                                                         "reason": reason})
        self._notify(pid)
        return new_rev

    def history(self, pid: str, since_rev: int | None = None) -> list[dict[str, Any]]:
        d = self._require(pid)
        out = []
        with open(d / "plan.log.jsonl", encoding="utf-8") as f:
            for line in f:
                if line.strip():
                    e = json.loads(line)
                    if since_rev is None or e["rev"] > since_rev:
                        out.append(e)
        return out

    def plan_at(self, pid: str, rev: int) -> dict[str, Any]:
        """Replay the log up to `rev`. Returns the document without rev."""
        doc: dict[str, Any] | None = None
        for e in self.history(pid):
            if e["rev"] > rev:
                break
            if e["rev"] == 1:
                doc = copy.deepcopy(e["ops"][0]["value"])
            else:
                doc = jsonpatch.apply_patch(doc, e["ops"])
        if doc is None or rev < 1 or rev > self.get(pid)["rev"]:
            raise NotFound(f"plan {pid!r} has no rev {rev}")
        return doc

    def revert(self, pid: str, rev: int, to_rev: int, author: str, reason: str) -> int:
        target = self.plan_at(pid, to_rev)
        cur = self.get(pid)
        if rev != cur["rev"]:
            raise Conflict(cur["rev"])
        doc = {k: v for k, v in cur.items() if k != "rev"}
        ops = jsonpatch.make_patch(doc, target).patch
        return self.patch(pid, rev, ops, author, reason or f"revert to rev {to_rev}")

    # ---- state ----

    def read_state(self, pid: str) -> dict[str, Any]:
        path = self._require(pid) / "state.json"
        return read_json(path) if path.exists() else empty_state()

    def write_state(self, pid: str, state: dict[str, Any]) -> int:
        """Persist state with rev + 1. Callers hold the plan lock."""
        state["rev"] = int(state.get("rev", 0)) + 1
        atomic_write_json(self.plan_dir(pid) / "state.json", state)
        return state["rev"]

    # ---- events ----

    def _last_seq(self, path: Path) -> int:
        if not path.exists():
            return 0
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            back = min(size, 65536)
            f.seek(size - back)
            lines = [ln for ln in f.read().splitlines() if ln.strip()]
        return json.loads(lines[-1])["seq"] if lines else 0

    def append_event(self, pid: str, type_: str, node: str | None = None,
                     data: dict[str, Any] | None = None) -> dict[str, Any]:
        path = self.plan_dir(pid) / "events.jsonl"
        with self.lock(pid):
            ev: dict[str, Any] = {"seq": self._last_seq(path) + 1, "at": now_iso(), "type": type_}
            if node is not None:
                ev["node"] = node
            ev["data"] = data or {}
            append_line(path, ev)
        return ev

    def events(self, pid: str, since_seq: int | None = None,
               limit: int | None = None) -> list[dict[str, Any]]:
        path = self._require(pid) / "events.jsonl"
        if not path.exists():
            return []
        out = []
        with open(path, encoding="utf-8") as f:
            for line in f:
                if line.strip():
                    e = json.loads(line)
                    if since_seq is None or e["seq"] > since_seq:
                        out.append(e)
        if limit is not None:
            out = out[:limit] if since_seq is not None else out[-limit:]
        return out

    # ---- inbox ----

    def inbox_open(self, pid: str, kind: str, node: str, data: dict[str, Any]) -> dict[str, Any]:
        d = self.plan_dir(pid) / "inbox"
        with self.lock(pid):
            d.mkdir(parents=True, exist_ok=True)
            n = len(list(d.glob("*.json"))) + 1
            item = {"id": f"{pid}.{n:04d}", "plan": pid, "node": node, "kind": kind,
                    "status": "open", "opened": now_iso(), **data,
                    "resolution": None, "resolved_by": None, "resolved_at": None}
            atomic_write_json(d / f"{item['id']}.json", item)
            self.append_event(pid, "inbox_opened", node, {"item": item["id"], "kind": kind})
        return item

    def _item_path(self, item_id: str) -> Path:
        pid, _, n = item_id.rpartition(".") if isinstance(item_id, str) else ("", "", "")
        path = self.plan_dir(pid) / "inbox" / f"{item_id}.json"
        if not pid or not P.ID_RE.match(pid) or not n.isdigit() or not path.exists():
            raise NotFound(f"no inbox item {item_id!r}")
        return path

    def inbox_get(self, item_id: str) -> dict[str, Any]:
        return read_json(self._item_path(item_id))

    def inbox_close(self, item_id: str, resolution: dict[str, Any], author: str) -> dict[str, Any]:
        """Mark an item resolved. Callers apply any state change themselves."""
        path = self._item_path(item_id)
        item = read_json(path)
        with self.lock(item["plan"]):
            item = read_json(path)
            if item["status"] != "open":
                raise BadRequest(f"inbox item {item_id} is already resolved")
            item.update(status="resolved", resolution=resolution, resolved_by=author,
                        resolved_at=now_iso())
            atomic_write_json(path, item)
            self.append_event(item["plan"], "inbox_resolved", item["node"],
                              {"item": item_id, "resolution": resolution, "author": author})
        return item

    def inbox_list(self, pid: str | None = None, open_only: bool = True) -> list[dict[str, Any]]:
        pids = [pid] if pid is not None else self.plan_ids()
        out = []
        for p in pids:
            d = self._require(p) / "inbox"
            if not d.is_dir():
                continue
            for f in sorted(d.glob("*.json")):
                item = read_json(f)
                if not open_only or item["status"] == "open":
                    out.append(item)
        return out


def apply_ops(doc: dict[str, Any], ops: Any) -> dict[str, Any]:
    """Apply a JSON Patch, turning every failure into InvalidPlan with the op index."""
    if not isinstance(ops, list) or not all(isinstance(o, dict) for o in ops):
        raise InvalidPlan(["ops: expected a list of JSON Patch operations"])
    cur: Any = copy.deepcopy(doc)
    for i, op in enumerate(ops):
        try:
            cur = jsonpatch.JsonPatch([op]).apply(cur)
        except (jsonpatch.JsonPatchException, jsonpointer.JsonPointerException, TypeError,
                KeyError) as e:
            raise InvalidPlan([f"ops[{i}]: {e}"]) from e
    if not isinstance(cur, dict):
        raise InvalidPlan(["document: expected an object"])
    return cur
