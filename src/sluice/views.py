"""Read-side views shared by the MCP tools and the CLI, plus ad-hoc fn calls."""

from __future__ import annotations

import copy
import secrets
import time
from collections import Counter
from typing import Any

from . import lifecycle as L
from . import types as T
from .errors import InvalidPlan, NotFound
from .store import Store
from .util import tail_text

CALL_NODE = "call"


def _snapshot(store: Store, pid: str):
    doc, exp = store.expanded(pid)
    state = store.read_state(pid)
    nodes = copy.deepcopy(state["nodes"])
    L.ensure_entries(exp, nodes)
    return doc, exp, state, nodes


def _slots_used(store: Store) -> Counter[str]:
    used: Counter[str] = Counter()
    for pid in store.plan_ids():
        used.update(L.slot_usage(store.read_state(pid)["nodes"]))
    return used


def plans_list(store: Store, include_adhoc: bool = False) -> list[dict[str, Any]]:
    out = []
    for pid in store.plan_ids():
        doc = store.get(pid)
        if doc.get("meta", {}).get("adhoc") and not include_adhoc:
            continue
        try:
            _, exp, _, nodes = _snapshot(store, pid)
            counts = dict(Counter(nodes[i]["status"] for i in exp.nodes))
        except InvalidPlan:
            counts = {}
        out.append({"id": pid, "title": doc.get("title", ""), "rev": doc["rev"],
                    "paused": bool(doc.get("paused", False)), "counts": counts})
    return out


def decisions(store: Store, pid: str) -> list[L.Decision]:
    doc, exp, _, nodes = _snapshot(store, pid)
    return L.decide(store, pid, doc, exp, nodes, now=time.time(), slots_used=_slots_used(store))


def dry_run(store: Store, pid: str) -> dict[str, list[dict[str, Any]]]:
    """What the next tick would start, skip, or leave blocked, with reasons."""
    out: dict[str, list[dict[str, Any]]] = {"start": [], "skip": [], "blocked": []}
    for d in decisions(store, pid):
        out[d.action].append(d.to_json())
    return out


def status(store: Store, pid: str) -> dict[str, Any]:
    doc, exp, state, nodes = _snapshot(store, pid)
    rows = []
    for nid, en in exp.nodes.items():
        e = nodes[nid]
        row = {"id": nid, "fn": en.fn.name, "status": e["status"],
               "attempt": e.get("attempt"), "started": e.get("started"),
               "finished": e.get("finished")}
        if e.get("error"):
            row["error"] = e["error"]
        rows.append(row)
    ready = [d.id for d in L.decide(store, pid, doc, exp, nodes, now=time.time(),
                                    slots_used=_slots_used(store)) if d.action == "start"]
    return {"rev": doc["rev"], "state_rev": state["rev"],
            "counts": dict(Counter(r["status"] for r in rows)), "nodes": rows, "ready": ready}


def node_get(store: Store, pid: str, nid: str) -> dict[str, Any]:
    _, exp, _, nodes = _snapshot(store, pid)
    en = exp.nodes.get(nid)
    if en is None:
        raise NotFound(f"plan {pid} has no node {nid!r}")
    e = nodes[nid]
    run_id = e.get("run_id")
    return {"definition": en.definition, "expanded_ids": exp.ids_under(nid), "state": e,
            "output": L.Values(store, pid, exp, nodes).output(nid),
            "stderr_tail": tail_text(store.runs_dir / run_id / "stderr.log") if run_id else ""}


# ---- ad-hoc calls ------------------------------------------------------------------------


def fn_call(store: Store, name: str, inp: Any, author: str) -> str:
    """Check `inp` against the fn's in types, then create a one-node ad-hoc plan."""
    fn = store.registry.get(name)
    if fn is None:
        raise NotFound(f"no fn {name!r}")
    if not isinstance(inp, dict):
        raise InvalidPlan(["input: expected an object keyed by input port"])
    errs = T.check_value(fn.in_record, inp, "input")
    errs += [f"input.{k}: fn {name} has no input port {k}" for k in inp if k not in fn.in_types]
    if errs:
        raise InvalidPlan(errs, f"input does not match fn {name}")
    call = f"call-{time.strftime('%Y%m%dt%H%M%S', time.gmtime())}-{secrets.token_hex(3)}"
    doc = {"id": call, "title": f"call {name}", "meta": {"adhoc": True, "fn": name},
           "nodes": {CALL_NODE: {"fn": name, "in": {k: {"value": v} for k, v in inp.items()}}}}
    store.create(call, doc, author, f"fn_call {name}")
    return call


def fn_result(store: Store, call: str) -> dict[str, Any]:
    doc = store.get(call)
    if not doc.get("meta", {}).get("adhoc"):
        raise NotFound(f"{call!r} is a plan, not an ad-hoc call")
    _, exp, _, nodes = _snapshot(store, call)
    e = nodes[CALL_NODE]
    out: dict[str, Any] = {"call": call, "status": e["status"]}
    output = L.Values(store, call, exp, nodes).output(CALL_NODE)
    if output is not None:
        out["output"] = output
    errors = [f"{x}: {nodes[x]['error']}" if x != CALL_NODE else nodes[x]["error"]
              for x in exp.leaves_under(CALL_NODE) if nodes[x].get("error")]
    if errors:
        out["error"] = "; ".join(errors)
    run_ids = [nodes[x]["run_id"] for x in exp.leaves_under(CALL_NODE) if nodes[x].get("run_id")]
    if run_ids:
        out["stderr_tail"] = tail_text(store.runs_dir / run_ids[-1] / "stderr.log")
    return out


def call_settled(result: dict[str, Any]) -> bool:
    return result["status"] in L.TERMINAL or result["status"] == "waiting"
