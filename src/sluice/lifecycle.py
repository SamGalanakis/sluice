"""Node state: resolving values, derived composite status, readiness, operator actions.

Shared by the runner (which owns the loop) and by the tools and CLI (operator actions and
inbox resolution act on state directly, under the plan lock).
"""

from __future__ import annotations

import contextlib
import os
import signal
import subprocess
import time
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import types as T
from .errors import BadRequest, NotFound
from .plan import MISSING, Binding, Cond, Expanded, Ref
from .store import Store
from .util import atomic_write_json, canonical, now_iso, read_json, sha256_json

TERMINAL = frozenset({"succeeded", "failed", "skipped", "cancelled"})


def new_leaf() -> dict[str, Any]:
    return {"status": "pending", "attempt": 1, "run_id": None, "pid": None, "started": None,
            "finished": None, "output": None, "error": None, "cache_hit": False,
            "claims_held": [], "retries": 0}


def new_composite() -> dict[str, Any]:
    return {"status": "pending", "composite": True, "started": None, "finished": None,
            "claims_held": []}


def ensure_entries(exp: Expanded, nodes: dict[str, Any]) -> None:
    for nid, en in exp.nodes.items():
        if nid not in nodes:
            nodes[nid] = new_composite() if en.composite else new_leaf()


def slot_usage(nodes: dict[str, Any]) -> Counter[str]:
    c: Counter[str] = Counter()
    for e in nodes.values():
        if e.get("status") == "running":
            c.update(e.get("slots") or {})
    return c


# ---- outputs ----------------------------------------------------------------------------


def write_output(store: Store, pid: str, nid: str, out: dict[str, Any]) -> dict[str, str]:
    path = store.plan_dir(pid) / "outputs" / f"{nid}.json"
    atomic_write_json(path, out)
    return {"path": str(path), "sha": sha256_json(out)}


def load_output(entry: dict[str, Any]) -> dict[str, Any]:
    ref = entry.get("output")
    if not ref:
        raise Unresolvable("no output recorded")
    return read_json(Path(ref["path"]))


class Unresolvable(Exception):
    pass


class Values:
    """Statuses and values of an expanded plan against a state snapshot.

    `overrides` lets a decision pass see its own earlier decisions without mutating state.
    """

    def __init__(self, store: Store, pid: str, exp: Expanded, nodes: dict[str, Any]):
        self.store, self.pid, self.exp, self.nodes = store, pid, exp, nodes
        self.overrides: dict[str, str] = {}

    def status(self, nid: str) -> str:
        en = self.exp.nodes.get(nid)
        if en is None:
            return "missing"
        if en.composite:
            return self.derive(nid)
        if nid in self.overrides:
            return self.overrides[nid]
        e = self.nodes.get(nid, {})
        if e.get("forward"):
            target = self.final_target(nid)
            st = self.status(target) if target != nid else "forwarded"
            if st == "missing":
                return "cancelled"
            return st if st in TERMINAL else "forwarded"
        return e.get("status", "pending")

    def final_target(self, nid: str) -> str:
        """Follow `forward` links from a node to the node that finally answers for it."""
        seen, t = {nid}, nid
        while (f := self.nodes.get(t, {}).get("forward")) and f not in seen:
            seen.add(f)
            t = f
        return t

    def forwarders(self) -> dict[str, str]:
        """target -> the live forwarded node that points at it."""
        return {e["forward"]: i for i, e in self.nodes.items()
                if e.get("forward") and e.get("status") == "forwarded"}

    def derive(self, nid: str) -> str:
        """SPEC §6 derived status of a composite."""
        en = self.exp.nodes[nid]
        kids = [self.status(c) for c in en.children]
        if "failed" in kids:
            return "failed"
        if kids and all(k == "skipped" for k in kids):
            return "skipped"
        if all(k in TERMINAL for k in kids):
            ok = all(self.ready(b) for b in (en.outs or {}).values())
            return "succeeded" if ok else "skipped"
        return "running" if any(k != "pending" for k in kids) else "pending"

    def ready(self, b: Binding) -> bool:
        """Can this binding be resolved now (status only, no file reads)?"""
        if b.kind != "from" or b.ref is None:
            return True
        en = self.exp.nodes.get(b.ref.node)
        if en is None:
            return False
        if en.composite:
            ob = (en.outs or {}).get(b.ref.port)
            return ob is None or self.ready(ob)
        return self.status(b.ref.node) == "succeeded"

    def resolve(self, b: Binding) -> Any:
        if b.kind == "value":
            return b.value
        if b.kind == "file":
            assert b.file is not None
            path = Path(b.file)
            if not path.is_absolute():
                path = self.store.plan_dir(self.pid) / path
            return path.read_text(encoding="utf-8")
        assert b.ref is not None
        return self.resolve_ref(b.ref)

    def resolve_ref(self, ref: Ref) -> Any:
        en = self.exp.nodes.get(ref.node)
        if en is None:
            raise Unresolvable(f"unknown node {ref.node}")
        if not en.composite and self.nodes.get(ref.node, {}).get("forward"):
            target = self.final_target(ref.node)
            if target != ref.node:
                return self.resolve_ref(Ref(target, ref.port, ref.fields))
        if en.composite:
            ob = (en.outs or {}).get(ref.port)
            v = None if ob is None else self.resolve(ob)
        else:
            st = self.status(ref.node)
            if st != "succeeded":
                raise Unresolvable(f"{ref.node} is {st}")
            v = load_output(self.nodes[ref.node]).get(ref.port)
        return T.navigate_value(v, ref.fields)

    def output(self, nid: str) -> dict[str, Any] | None:
        if self.status(nid) != "succeeded":
            return None
        if self.nodes.get(nid, {}).get("forward"):
            nid = self.final_target(nid)
        en = self.exp.nodes.get(nid)
        if en is None:
            return None
        try:
            if en.composite:
                if self.status(nid) != "succeeded":
                    return None
                return {p: self.resolve(b) for p, b in (en.outs or {}).items()}
            if self.status(nid) != "succeeded":
                return None
            return load_output(self.nodes[nid])
        except (Unresolvable, OSError):
            return None

    def holds(self, c: Cond) -> bool:
        v = self.resolve(c.source)
        if c.op == "truthy":
            return bool(v)
        if c.op == "falsy":
            return not v
        if c.op == "in":
            return any(canonical(v) == canonical(x) for x in c.value)
        same = canonical(v) == canonical(c.value)
        return same if c.op == "eq" else not same


# ---- readiness --------------------------------------------------------------------------


@dataclass
class Decision:
    id: str
    action: str  # "start" | "skip" | "blocked"
    reason: str
    claims: dict[str, list[str]] = field(default_factory=dict)
    skipped_by: str | None = None

    def to_json(self) -> dict[str, Any]:
        return {"id": self.id, "reason": self.reason}


def decide(store: Store, pid: str, doc: dict[str, Any], exp: Expanded, nodes: dict[str, Any],
           *, now: float, slots_used: Counter[str]) -> list[Decision]:
    """What the next pass would do with each pending leaf (SPEC §7 step 5). Pure."""
    vals = Values(store, pid, exp, nodes)
    resources = doc.get("resources", {})
    claims_used: Counter[str] = Counter(c for e in nodes.values() for c in e.get("claims_held", []))
    slots: Counter[str] = Counter(slots_used)
    held = {i for i, e in nodes.items() if e.get("composite") and e.get("claims_held")}
    out: list[Decision] = []
    for nid in exp.leaves():
        e = nodes.get(nid)
        if e is None or e["status"] != "pending":
            continue
        en = exp.nodes[nid]
        if doc.get("paused"):
            out.append(Decision(nid, "blocked", "plan is paused"))
            continue
        if en.hold:
            out.append(Decision(nid, "blocked", "held"))
            continue
        sts = {d: vals.status(d) for d in en.deps}
        failed = [d for d, s in sts.items() if s == "failed"]
        if failed:
            out.append(Decision(nid, "blocked", f"dependency {failed[0]} failed"))
            continue
        gone = [(d, s) for d, s in sts.items() if s in ("skipped", "cancelled", "missing")]
        if gone:
            vals.overrides[nid] = "skipped"
            out.append(Decision(nid, "skip", f"dependency {gone[0][0]} is {gone[0][1]}",
                                skipped_by="dep"))
            continue
        waiting = [(d, s) for d, s in sts.items() if s != "succeeded"]
        if waiting:
            out.append(Decision(nid, "blocked", f"waiting for {waiting[0][0]} ({waiting[0][1]})"))
            continue
        ancestors = exp.ancestors(nid)
        conds = list(en.when) + [c for a in ancestors for c in exp.nodes[a].when]
        try:
            false = next((c for c in conds if not vals.holds(c)), None)
        except (Unresolvable, OSError) as ex:
            out.append(Decision(nid, "blocked", f"when: {ex}"))
            continue
        if false is not None:
            vals.overrides[nid] = "skipped"
            out.append(Decision(nid, "skip", f"when is false: {false.describe()}",
                                skipped_by="when"))
            continue
        nb = e.get("not_before")
        if nb and nb > now:
            out.append(Decision(nid, "blocked", f"retry backoff until {now_iso(nb)}"))
            continue
        cover = _cover(exp, nodes, vals.forwarders(), nid)
        take = {nid: list((Counter(en.claims) - cover).elements())}
        cover -= Counter(en.claims)
        for a in ancestors:
            if exp.nodes[a].claims and a not in held:
                take[a] = list((Counter(exp.nodes[a].claims) - cover).elements())
                cover -= Counter(exp.nodes[a].claims)
        need: Counter[str] = Counter(c for cl in take.values() for c in cl)
        busy = [r for r in need if claims_used[r] + need[r] > resources.get(r, 0)]
        if busy:
            out.append(Decision(nid, "blocked", f"claim {busy[0]} is busy"))
            continue
        fn_slots = {} if en.fn.native else en.fn.slots
        full = [s for s, n in fn_slots.items() if slots[s] + n > store.slot_capacity(s)]
        if full:
            s = full[0]
            out.append(Decision(nid, "blocked",
                                f"slot {s} is full ({slots[s]}/{store.slot_capacity(s)})"))
            continue
        claims_used.update(need)
        slots.update(fn_slots)
        held.update(a for a in take if a != nid)
        vals.overrides[nid] = "running"
        out.append(Decision(nid, "start", "ready", take))
    return out


def _cover(exp: Expanded, nodes: dict[str, Any], forwarders: dict[str, str],
           nid: str) -> Counter[str]:
    """Claims already held on behalf of `nid`: by every node forwarding (transitively) to it
    or to one of its composite ancestors, and by those forwarders' own ancestors."""
    cover: Counter[str] = Counter()
    for x in [nid, *exp.ancestors(nid)]:
        seen = set()
        f = forwarders.get(x)
        while f is not None and f not in seen:
            seen.add(f)
            for holder in [f, *(exp.ancestors(f) if f in exp.nodes else [])]:
                cover.update(nodes.get(holder, {}).get("claims_held", []))
            f = forwarders.get(f)
    return cover


# ---- processes --------------------------------------------------------------------------


def launcher_alive(pid: int | None) -> bool:
    """Is `pid` a live (non-zombie) sluice launcher?"""
    if not pid:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        if stat.rsplit(")", 1)[1].split()[0] == "Z":
            return False
        cmdline = Path(f"/proc/{pid}/cmdline").read_bytes()
        return b"sluice.launch" in cmdline
    except (OSError, IndexError):
        return True


def kill_group(pid: int | None, proc: subprocess.Popen | None = None, grace: float = 2.0) -> None:
    """SIGTERM the launcher's process group, then SIGKILL whatever is left after `grace`."""
    if not pid:
        return
    try:
        os.killpg(pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return
    deadline = time.time() + grace
    while time.time() < deadline:
        if (proc.poll() is not None) if proc is not None else not launcher_alive(pid):
            break
        time.sleep(0.05)
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(pid, signal.SIGKILL)
    if proc is not None:
        with contextlib.suppress(subprocess.TimeoutExpired):
            proc.wait(timeout=5)


# ---- transitions shared by the runner and operator actions ------------------------------


def close_items(store: Store, pid: str, nid: str, resolution: dict[str, Any], author: str) -> None:
    for item in store.inbox_list(pid, open_only=True):
        if item["node"] == nid:
            store.inbox_close(item["id"], resolution, author)


def succeed(store: Store, pid: str, nid: str, e: dict[str, Any], out: dict[str, Any],
            cache_hit: bool = False) -> None:
    e.update(status="succeeded", finished=now_iso(), output=write_output(store, pid, nid, out),
             error=None, cache_hit=cache_hit, pid=None, claims_held=[], not_before=None)
    store.append_event(pid, "node_succeeded", nid, {"attempt": e["attempt"],
                                                    "cache_hit": cache_hit,
                                                    "run_id": e.get("run_id")})


def reset(e: dict[str, Any], bump: bool) -> None:
    e.update(status="pending", pid=None, finished=None, output=None, error=None,
             cache_hit=False, not_before=None, retries=0, skipped_by=None, claims_held=[],
             forward=None)
    if bump:
        e["attempt"] = e.get("attempt", 1) + 1


def _load(store: Store, pid: str) -> tuple[dict[str, Any], Expanded, dict[str, Any]]:
    doc, exp = store.expanded(pid)
    state = store.read_state(pid)
    ensure_entries(exp, state["nodes"])
    return doc, exp, state


def node_action(store: Store, pid: str, nid: str, action: str, reason: str,
                author: str) -> dict[str, Any]:
    """Operator actions of SPEC §7: retry, skip, cancel. Applies to every leaf of a composite."""
    if action not in ("retry", "skip", "cancel"):
        raise BadRequest(f"unknown action {action!r}")
    with store.lock(pid):
        _, exp, state = _load(store, pid)
        nodes = state["nodes"]
        if nid not in exp.nodes:
            raise NotFound(f"plan {pid} has no node {nid!r}")
        eligible_st = {"retry": ("failed", "cancelled", "skipped"),
                       "skip": ("pending", "waiting", "failed"),
                       "cancel": ("pending", "waiting", "running", "failed")}[action]
        vals = Values(store, pid, exp, nodes)
        targets = []
        for x in exp.leaves_under(nid):
            # A forwarded node is answered by its final target: act on that instead.
            t = vals.final_target(x) if nodes[x].get("forward") else x
            targets.extend(exp.leaves_under(t) if t in exp.nodes else [])
        leaves = [x for x in dict.fromkeys(targets) if nodes[x]["status"] in eligible_st]
        if not leaves:
            st = nodes[nid]["status"]
            hint = "; use node_cancel" if action == "skip" and st == "running" else ""
            raise BadRequest(f"node {nid} is {st}; {action} applies to "
                             f"{', '.join(eligible_st)} nodes{hint}")
        data = {"by": author, "reason": reason}
        for x in leaves:
            e = nodes[x]
            close_items(store, pid, x, {"action": action}, author)
            if action == "retry":
                reset(e, bump=True)
                store.append_event(pid, "node_retrying", x, {**data, "attempt": e["attempt"]})
            elif action == "skip":
                e.update(status="skipped", skipped_by="operator", finished=now_iso(),
                         claims_held=[], pid=None)
                store.append_event(pid, "node_skipped", x, data)
            else:
                if e["status"] == "running":
                    kill_group(e.get("pid"))
                e.update(status="cancelled", finished=now_iso(), claims_held=[], pid=None)
                store.append_event(pid, "node_cancelled", x, data)
        if action == "retry":
            _reopen_dependents(store, pid, exp, nodes, leaves, author)
        store.write_state(pid, state)
    store._notify(pid)
    return {"ok": True, "nodes": leaves}


def _reopen_dependents(store: Store, pid: str, exp: Expanded, nodes: dict[str, Any],
                       retried: list[str], author: str) -> None:
    """Nodes skipped because of what was retried (dependency or when) become pending again."""
    dependents = exp.dependents()
    frontier = set(retried)
    for x in retried:
        frontier.update(exp.ancestors(x))
    seen = set(frontier)
    while frontier:
        nxt: set[str] = set()
        for x in frontier:
            for d in dependents.get(x, []):
                if d in seen:
                    continue
                seen.add(d)
                if exp.nodes[d].composite:
                    nxt.add(d)
                    continue
                e = nodes[d]
                if e["status"] == "skipped" and e.get("skipped_by") in ("dep", "when"):
                    reset(e, bump=False)
                    store.append_event(pid, "node_retrying", d,
                                       {"by": author, "reason": "a dependency was retried"})
                    nxt.add(d)
                    nxt.update(exp.ancestors(d))
        frontier = nxt


def inbox_resolve(store: Store, item_id: str, resolution: Any, author: str) -> dict[str, Any]:
    """Resolve an inbox item: an answer for core.ask, or retry/skip/ack for a failure."""
    item = store.inbox_get(item_id)
    if item["status"] != "open":
        raise BadRequest(f"inbox item {item_id} is already resolved")
    if not isinstance(resolution, dict):
        raise BadRequest("resolution must be an object")
    pid, nid = item["plan"], item["node"]
    if item["kind"] == "ask":
        if set(resolution) != {"answer"}:
            raise BadRequest('an ask item is resolved with {"answer": ...}')
        with store.lock(pid):
            state = _load(store, pid)[2]
            e = state["nodes"].get(nid)
            store.inbox_close(item_id, resolution, author)
            if e is not None and e["status"] == "waiting":
                succeed(store, pid, nid, e, {"answer": resolution["answer"]})
                store.write_state(pid, state)
        store._notify(pid)
        return {"ok": True}
    action = resolution.get("action")
    if set(resolution) != {"action"} or action not in ("retry", "skip", "ack"):
        raise BadRequest('a failure item is resolved with {"action": "retry"|"skip"|"ack"}')
    if action == "ack":
        store.inbox_close(item_id, resolution, author)
        return {"ok": True}
    node_action(store, pid, nid, action, f"inbox {item_id}", author)
    return {"ok": True}


def make_input(exp: Expanded, nid: str, vals: Values) -> dict[str, Any]:
    """The input object of a leaf: every bound port resolved, unbound optional ports null."""
    en = exp.nodes[nid]
    inp = {p: None for p in en.fn.in_types}
    for p, b in en.bindings.items():
        inp[p] = vals.resolve(b)
    return inp


def describe_cond_value(c: Cond) -> Any:
    return None if c.value is MISSING else c.value
