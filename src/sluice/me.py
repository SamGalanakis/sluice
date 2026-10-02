"""`sluice me` and the `step_context` tool (SPEC §9, §10): where a step stands, for the agent
doing it — one compact answer instead of a pasted note.
"""

from __future__ import annotations

import datetime as dt
import json
import re
from typing import Any

from . import leases as LS
from . import log as L
from . import plan as P
from . import resources as RS
from . import state as S
from . import types as T
from .errors import NotFound
from .store import Store, _brief
from .util import now_iso

FINAL_CUT = 300  # characters of an upstream step's `final` a `me` keeps


def _iso(at: Any) -> dt.datetime | None:
    try:
        return dt.datetime.strptime(str(at), "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.UTC)
    except ValueError:
        return None


def _dur(secs: float) -> str:
    """`5s`, `14m`, `2h 3m`, `1d 4h` — how long the step has run."""
    d, rem = divmod(int(secs), 86400)
    h, rem = divmod(rem, 3600)
    m, s = divmod(rem, 60)
    parts = [(d, "d"), (h, "h"), (m, "m")] + ([(s, "s")] if not d and not h else [])
    return " ".join(f"{n}{u}" for n, u in parts if n) or "0s"


def _type(t: T.Type) -> str:
    form = T.form(t)
    return form if isinstance(form, str) else json.dumps(form)


def _short(outputs: dict[str, Any]) -> dict[str, Any]:
    """The upstream outputs an agent reads first: `summary` whole, else `final` cut, plus
    every output whose name ends in `report` or `path` (paths to files)."""
    out = {}
    if "summary" in outputs:
        out["summary"] = outputs["summary"]
    elif "final" in outputs:
        value = outputs["final"]
        if isinstance(value, str) and len(value) > FINAL_CUT:
            value = f"{value[:FINAL_CUT]}… [{len(value) - FINAL_CUT} more characters]"
        out["final"] = value
    for k, v in outputs.items():
        if k not in out and k.endswith(("report", "path")):
            out[k] = v
    return out


def _unanswered(msgs: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """The messages still asking for a reply (needs_reply, true unless a note) with no later
    message from their addressee — or, addressed to nobody, from anyone but the sender."""
    out = []
    for i, m in enumerate(msgs):
        if m.get("needs_reply", True) is False:
            continue
        to = m.get("to")
        if any(n.get("from") == to if to else n.get("from") != m.get("from")
               for n in msgs[i + 1:]):
            continue
        out.append(m)
    return out


def context(store: Store, project: str, step: str, run: str | None = None) -> dict[str, Any]:
    """The step's context: {project, step, fn, doc, status, queued?, needs?, leases?, started,
    finished, elapsed (s), run, inputs (cut like status(brief)), upstream, messages
    (unanswered, newest last), submit {outputs, command}, thread, ask}; `queued` says why a
    step waiting on resources has not started (`queued: needs lane 1 (56/56 held)`)."""
    _, plan = store.plan(project)
    if step not in plan.steps:
        raise NotFound(f"the plan of project {project} has no step {step!r}")
    s = plan.steps[step]
    with store.rx() as conn:
        state, resources = store.read_state(project), store.resources(project)
        leases = LS.rows(conn, project)
    e = S.entry_of(state, step)
    queued = RS.queued_reason(s, state, RS.capacities(resources, state),
                              RS.held(plan, state, leases))
    mine = [{"resource": x["resource"], "amount": x["amount"], "held": x["granted"] is not None}
            for x in leases if x["step"] == step]
    runs = e.get("run_ids") or []
    run = run or (runs[-1] if len(runs) == 1 else None)
    started, finished = _iso(e.get("started")), _iso(e.get("finished"))
    elapsed = None if started is None else ((finished or _iso(now_iso())) - started
                                            ).total_seconds()
    inp: Any = None
    if run:
        f = store.runs_dir(project) / run / "input.json"
        try:
            inp = json.loads(f.read_text())
        except (OSError, ValueError):
            pass
    if inp is None:
        inp = P.resolved_inputs(s, plan, state)
    upstream = []
    for u in s.waits:
        ue = S.entry_of(state, u)
        row = {"step": u, "status": ue.get("status", "pending")}
        if u in plan.steps:
            row["fn"] = plan.steps[u].fn.name
        if isinstance(ue.get("outputs"), dict) and ue["outputs"]:
            row["outputs"] = _short(ue["outputs"])
        if ue.get("error"):
            row["error"] = str(ue["error"]).strip().splitlines()[-1][:200]
        upstream.append(row)
    thread = "step-" + re.sub(r"[^a-z0-9_-]", "-", step.lower())
    msgs = L.read(store.home, project, threads=[thread])["records"]
    outs = [{"name": n, "type": _type(t), "required": not isinstance(t, T.Optional),
             **({"doc": s.output_docs[n]} if n in s.output_docs else {})}
            for n, t in s.declared.items()]
    outs.sort(key=lambda o: not o["required"])  # required first, declared order kept
    args = {"project": project, "step": step, "run": run or "<run>",
            "outputs": {o["name"]: f"<{o['type']}>" for o in outs}}
    ask = {"name": "thread.post", "project": project, "direct": True,
           "inputs": {"thread": thread, "from": step, "to": "orchestrator", "body": "..."}}
    return {"project": project, "step": step, "fn": s.fn.name, "doc": s.doc,
            "status": e.get("status", "pending"),
            **({"queued": queued} if queued else {}),
            **({"needs": s.needs} if s.needs else {}),
            **({"leases": mine} if mine else {}),
            "started": e.get("started"),
            "finished": e.get("finished"), "elapsed": elapsed, "run": run,
            "inputs": _brief(inp), "upstream": upstream,
            "messages": _unanswered(msgs),
            "submit": {"outputs": outs,
                       "command": f"sluice tool step_submit '{json.dumps(args)}'"},
            "thread": thread,
            "ask": f"sluice tool fn_call '{json.dumps(ask)}'"}


def render(ctx: dict[str, Any]) -> str:
    """The context as short text for an agent at a checkpoint."""
    head = f"step {ctx['step']} ({ctx['fn']}) — {ctx['status']}"
    if ctx.get("elapsed") is not None:
        head += f" {_dur(ctx['elapsed'])}"
    if ctx.get("run"):
        head += f" · run {ctx['run']}"
    lines = [head]
    if ctx.get("queued"):
        lines.append(ctx["queued"])
    for x in ctx.get("leases") or []:
        lines.append(f"lease: {x['resource']} {x['amount']} "
                     f"({'held' if x['held'] else 'waiting'})")
    if ctx.get("doc"):
        lines.append(f"doc: {ctx['doc']}")
    if ctx.get("inputs"):
        lines.append("inputs: " + "; ".join(
            f"{k}={json.dumps(v, ensure_ascii=False)}" for k, v in ctx["inputs"].items()))
    for u in ctx["upstream"]:
        row = f"upstream {u['step']} ({u.get('fn', '?')}): {u['status']}"
        if u.get("outputs"):
            row += " — " + "; ".join(f"{k}={json.dumps(v, ensure_ascii=False)}"
                                     for k, v in u["outputs"].items())
        if u.get("error"):
            row += f" — error: {u['error']}"
        lines.append(row)
    for m in ctx["messages"]:
        body = re.sub(r"\s+", " ", str(m.get("body") or "")).strip()[:400]
        lines.append(f"message {m.get('from')}: {body}")
    if ctx["submit"]["outputs"]:
        names = ", ".join(o["name"] + ("" if o["required"] else " (optional)")
                          for o in ctx["submit"]["outputs"])
        lines.append(f"submit: {names}")
        lines.append(f"  {ctx['submit']['command']}")
    lines.append(f"thread {ctx['thread']} — ask: {ctx['ask']}")
    return "\n".join(lines)
