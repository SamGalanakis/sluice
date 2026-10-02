"""Project resources and admission (SPEC §6 "Resources"): what a project declares, what its
running steps hold, and why a ready step that asks for more than is free stays queued.

A project's resources are `{name: {"capacity": n}}` (a fixed integer >= 0) or `{name:
{"capacity_fn": "<fn>"}}` (a fn the project sees, returning `{capacity}`; the runner calls it
and keeps the last good value in the state's `resources`). A step asks for amounts with
`needs: {name: n}`; a running step holds them, once, however many runs it scatters into. A
step's fn may also hold an amount for part of its work, a lease (leases.py); granted leases
count in the same totals.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from . import leases as LS
from . import plan as P
from . import state as S
from . import types as T
from .registry import Registry

INT = T.Prim("int")


def _amount(v: Any) -> bool:
    return isinstance(v, int) and not isinstance(v, bool) and v >= 0


def parse(raw: Any, where: str = "resources", removing: bool = False
          ) -> tuple[dict[str, dict[str, Any] | None], list[str]]:
    """A `resources` argument as {name: {"capacity": n} or {"capacity_fn": fn}}, an integer
    standing for {"capacity": n}; with `removing`, null (remove it) too. Returns (them, every
    problem)."""
    if not isinstance(raw, dict):
        return {}, [f"{where}: expected an object of resource name -> capacity"]
    out: dict[str, dict[str, Any] | None] = {}
    errs = []
    shape = ('an integer >= 0, {"capacity": <integer >= 0>} or {"capacity_fn": "<fn>"}'
             + (", or null to remove it" if removing else ""))
    for name, v in raw.items():
        at = f"{where}.{name}"
        if not isinstance(name, str) or not P.ID_RE.match(name):
            errs.append(f"{at}: resource names match {P.ID_RE.pattern}")
        elif v is None and removing:
            out[name] = None
        elif _amount(v):
            out[name] = {"capacity": v}
        elif isinstance(v, dict) and set(v) == {"capacity"} and _amount(v["capacity"]):
            out[name] = {"capacity": v["capacity"]}
        elif isinstance(v, dict) and set(v) == {"capacity_fn"} and \
                isinstance(v["capacity_fn"], str) and v["capacity_fn"]:
            out[name] = {"capacity_fn": v["capacity_fn"]}
        else:
            errs.append(f"{at}: expected {shape}")
    return out, errs


def check_fns(resources: dict[str, dict[str, Any]], registry: Registry,
              where: str = "resources") -> list[str]:
    """Problems with the capacity fns: each must be a fn the project sees, not
    core.external, with an integer output `capacity` and no required input."""
    errs = []
    for name, spec in resources.items():
        fn_name = spec.get("capacity_fn")
        if fn_name is None:
            continue
        at = f"{where}.{name}.capacity_fn"
        fn = registry.get(fn_name)
        if fn is None:
            errs.append(f"{at}: the project sees no fn {fn_name!r}")
            continue
        out = fn.outputs.get("capacity")
        if fn.external:
            errs.append(f"{at}: {fn_name} is done outside sluice; it cannot be called")
        elif out is None or out not in (INT, T.ANY):
            errs.append(f"{at}: fn {fn_name} has no int output `capacity`")
        required = [k for k, t in fn.inputs.items() if not isinstance(t, T.Optional)]
        if required:
            errs.append(f"{at}: fn {fn_name} takes required inputs ({', '.join(required)}); "
                        "a capacity fn is called with none")
    return errs


def needs_errors(plan: P.Plan, resources: dict[str, dict[str, Any]],
                 steps: list[str]) -> list[str]:
    """What the plan edit path refuses for `steps` (those whose needs it adds or changes): a
    need naming a resource the project does not declare, or asking for more than a fixed
    capacity."""
    errs = []
    names = ", ".join(resources) or "none"
    for sid in steps:
        step = plan.steps.get(sid)
        for r, n in (step.needs if step is not None else {}).items():
            at = f"steps.{sid}.needs.{r}"
            spec = resources.get(r)
            if spec is None:
                errs.append(f"{at}: the project declares no resource {r} (its resources: "
                            f"{names}; project_update sets them)")
            elif "capacity" in spec and n > spec["capacity"]:
                errs.append(f"{at}: asks for {n}, more than {r}'s capacity {spec['capacity']}")
    return errs


def capacities(resources: dict[str, dict[str, Any]], state: dict[str, Any]
               ) -> dict[str, int | None]:
    """Each resource's capacity now: the fixed one, or a capacity fn's last good value kept
    in the state (None until there is one: nothing new is admitted on it)."""
    seen = state.get("resources") if isinstance(state.get("resources"), dict) else {}
    out: dict[str, int | None] = {}
    for name, spec in resources.items():
        if "capacity" in spec:
            out[name] = spec["capacity"]
        else:
            got = seen.get(name) if isinstance(seen.get(name), dict) else {}
            value = got.get("capacity") if got.get("fn") == spec["capacity_fn"] else None
            out[name] = value if _amount(value) else None
    return out


def held(plan: P.Plan, state: dict[str, Any],
         leases: Sequence[dict[str, Any]] = ()) -> dict[str, int]:
    """What the project holds, per resource: the needs of its running steps (an adopted run's
    step counts like any other: it is `running` in the state) plus its granted leases."""
    out: dict[str, int] = {}
    for sid, step in plan.steps.items():
        if step.needs and S.entry_of(state, sid).get("status") == "running":
            take(out, step.needs)
    take(out, LS.granted(leases))
    return out


def take(held: dict[str, int], needs: dict[str, int]) -> None:
    for r, n in needs.items():
        held[r] = held.get(r, 0) + n


def short(needs: dict[str, int], caps: dict[str, int | None],
          held: dict[str, int]) -> list[str]:
    """The resources whose free amount (capacity - held) is less than the step asks for, in
    its order; empty when it fits. An undeclared resource, or one with no capacity yet, never
    fits a request over 0."""
    out = []
    for r, n in needs.items():
        cap = caps.get(r)
        if n > 0 and (cap is None or cap - held.get(r, 0) < n):
            out.append(r)
    return out


def reason(needs: dict[str, int], blocked: list[str], caps: dict[str, int | None],
           held: dict[str, int]) -> str:
    """Why a queued step waits, with the amounts now: `needs lane 1 (56/56 held)`, one part
    per resource it is short of (`, `-joined); `(capacity unknown)` for a capacity fn with no
    value yet, `(not declared)` for a resource the project does not have."""
    parts = []
    for r in blocked:
        n = needs.get(r, 0)
        if r not in caps:
            parts.append(f"{r} {n} (not declared)")
        elif caps[r] is None:
            parts.append(f"{r} {n} (capacity unknown)")
        else:
            parts.append(f"{r} {n} ({held.get(r, 0)}/{caps[r]} held)")
    return "needs " + ", ".join(parts)


def queued(e: dict[str, Any]) -> list[str]:
    """The resources a pending step's entry says it is queued on (none when it is not)."""
    q = e.get("queued") if e.get("status") == "pending" else None
    return [r for r in q if isinstance(r, str)] if isinstance(q, list) else []


def queued_reason(step: P.Step, state: dict[str, Any], caps: dict[str, int | None],
                  held_now: dict[str, int]) -> str | None:
    """`queued: needs lane 1 (56/56 held)` for a step the runner left queued, else None."""
    blocked = queued(S.entry_of(state, step.id))
    return f"queued: {reason(step.needs, blocked, caps, held_now)}" if blocked else None


def summary(resources: dict[str, dict[str, Any]], plan: P.Plan, state: dict[str, Any],
            leases: Sequence[dict[str, Any]] = ()) -> dict[str, dict[str, Any]]:
    """status's `resources`: {name: {capacity, held, queued, capacity_fn?, error?, holders?,
    waiting?}}; `queued` counts the pending steps queued on it, `capacity` is null while a
    capacity fn has given no value, and `error` is its last call's failure (the last good
    value stands). `holders` are its granted leases ({step, run, amount, since}), `waiting`
    the leases waiting for it ({step, run, amount, priority, since}) in the order they will
    be granted; each only when there are some."""
    caps, now = capacities(resources, state), held(plan, state, leases)
    prio = {sid: step.priority for sid, step in plan.steps.items()}
    seen = state.get("resources") if isinstance(state.get("resources"), dict) else {}
    waits: dict[str, int] = {}
    for sid in plan.steps:
        for r in queued(S.entry_of(state, sid)):
            waits[r] = waits.get(r, 0) + 1
    out = {}
    for name, spec in resources.items():
        row: dict[str, Any] = {"capacity": caps[name], "held": now.get(name, 0),
                               "queued": waits.get(name, 0)}
        if "capacity_fn" in spec:
            row["capacity_fn"] = spec["capacity_fn"]
            got = seen.get(name) if isinstance(seen.get(name), dict) else {}
            if got.get("fn") == spec["capacity_fn"] and got.get("error"):
                row["error"] = got["error"]
        mine = [x for x in leases if x["resource"] == name]
        holders = [{"step": x["step"], "run": x["run"], "amount": x["amount"],
                    "since": x["granted"]} for x in mine if x["granted"] is not None]
        waiting = [{"step": x["step"], "run": x["run"], "amount": x["amount"],
                    "priority": prio.get(x["step"], 0), "since": x["created"]}
                   for x in LS.grant_order([x for x in mine if x["granted"] is None],
                                           lambda sid: prio.get(sid, 0))]
        row.update({"holders": holders} if holders else {})
        row.update({"waiting": waiting} if waiting else {})
        out[name] = row
    return out
