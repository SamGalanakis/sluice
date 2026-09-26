"""Plan documents (SPEC §5): sources, scatter, validation, dependencies."""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

from . import types as T
from .registry import Fn, Registry

ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]*$")
DOC_KEYS = {"inputs", "outputs", "steps"}
EMPTY: dict[str, Any] = {"inputs": {}, "outputs": {}, "steps": {}}
STEP_KEYS = {"run", "in", "scatter"}


@dataclass(frozen=True)
class Ref:
    """A plan input (`step` is None) or `<step>/<output>`, then `.field`/`.index` path."""

    name: str
    step: str | None = None
    fields: tuple[str, ...] = ()

    def __str__(self) -> str:
        head = f"{self.step}/{self.name}" if self.step else self.name
        return ".".join((head, *self.fields))


@dataclass(frozen=True)
class Source:
    """`{"default": v}` (no refs), `{"source": "ref"}` or `{"source": [refs]}` (fan-in)."""

    default: Any = None
    refs: tuple[Ref, ...] = ()
    fan_in: bool = False


@dataclass
class Step:
    id: str
    fn: Fn
    sources: dict[str, Source]
    scatter: str | None

    @property
    def reads(self) -> list[Ref]:
        return [r for s in self.sources.values() for r in s.refs]

    @property
    def deps(self) -> list[str]:
        return list(dict.fromkeys(r.step for r in self.reads if r.step))

    def output_type(self, name: str) -> T.Type | None:
        t = self.fn.outputs.get(name)
        return T.List(t) if t is not None and self.scatter else t


@dataclass
class Plan:
    inputs: dict[str, T.Type]
    outputs: dict[str, Ref]
    steps: dict[str, Step]


def parse_ref(text: Any) -> tuple[Ref | None, str]:
    if not isinstance(text, str):
        return None, f"a ref is a string like 'step/output' or 'input', got {text!r}"
    head, *fields = text.split(".")
    step, _, name = head.rpartition("/")
    ids = [name, step] if step else [name]
    if not all(ID_RE.match(x) for x in ids) or not all(f and "/" not in f for f in fields):
        return None, f"bad ref {text!r}: expected <input> or <step>/<output>, then .<field>..."
    return Ref(name, step or None, tuple(fields)), ""


def parse_source(raw: Any, path: str, errs: list[str]) -> Source | None:
    if not isinstance(raw, dict) or len(raw) != 1 or next(iter(raw)) not in ("default", "source"):
        errs.append(f'{path}: expected {{"default": ...}} or {{"source": ...}}')
        return None
    if "default" in raw:
        return Source(default=raw["default"])
    src = raw["source"]
    refs = []
    for i, text in enumerate(src if isinstance(src, list) else [src]):
        ref, err = parse_ref(text)
        if ref is None:
            errs.append(f"{path}.source{f'[{i}]' if isinstance(src, list) else ''}: {err}")
            return None
        refs.append(ref)
    return Source(refs=tuple(refs), fan_in=isinstance(src, list))


def ref_type(ref: Ref, plan: Plan) -> tuple[T.Type | None, str]:
    if ref.step is None:
        base = plan.inputs.get(ref.name)
        if base is None:
            return None, f"unknown plan input {ref.name}"
    else:
        step = plan.steps.get(ref.step)
        if step is None:
            return None, f"unknown step {ref.step}"
        base = step.output_type(ref.name)
        if base is None:
            return None, f"step {ref.step} (fn {step.fn.name}) has no output {ref.name}"
    t, err = T.navigate(base, ref.fields)
    return (t, "") if t is not None else (None, f"{ref}: {err}")


def check_source(s: Source, target: T.Type, plan: Plan, path: str, errs: list[str]) -> None:
    if not s.refs and not s.fan_in:
        errs.extend(T.check_value(target, s.default, path))
        return
    if s.fan_in:
        inner = target.of if isinstance(target, T.Optional) else target
        if inner != T.ANY and not isinstance(inner, T.List):
            errs.append(f"{path}: a list source needs an array or Any input, not {target}")
            return
        target = inner.of if isinstance(inner, T.List) else T.ANY
    for i, ref in enumerate(s.refs):
        where = f"{path}.source[{i}]" if s.fan_in else path
        src, err = ref_type(ref, plan)
        if src is None:
            errs.append(f"{where}: {err}")
            continue
        ok, reason = T.fits(src, target)
        if not ok:
            errs.append(f"{where}: {ref} is {src}, which does not fit {target}: {reason}")


def find_cycle(adj: dict[str, list[str]]) -> list[str] | None:
    """A dependency cycle as a path (first id repeated at the end), or None."""
    state: dict[str, int] = {}
    for root, first in adj.items():
        if root in state:
            continue
        stack, path = [(root, iter(first))], [root]
        state[root] = 1
        while stack:
            node, it = stack[-1]
            nxt = next(it, None)
            if nxt is None:
                stack.pop()
                path.pop()
                state[node] = 2
            elif nxt in adj and state.get(nxt) == 1:
                return path[path.index(nxt):] + [nxt]
            elif nxt in adj and nxt not in state:
                state[nxt] = 1
                stack.append((nxt, iter(adj[nxt])))
                path.append(nxt)
    return None


def _ids(section: Any, path: str, errs: list[str]) -> dict[str, Any]:
    if not isinstance(section, dict):
        errs.append(f"{path}: expected an object")
        return {}
    bad = [k for k in section if not ID_RE.match(k)]
    errs.extend(f"{path}.{k}: ids match {ID_RE.pattern}" for k in bad)
    return {k: v for k, v in section.items() if k not in bad}


def validate(doc: Any, registry: Registry) -> tuple[list[str], Plan]:
    """Validate a plan (without rev) per SPEC §5. Returns (every error with a path, plan).

    Each error reads `<path>: <message>`, e.g. `steps.a.in.x: required input is not bound`.
    """
    plan = Plan({}, {}, {})
    if not isinstance(doc, dict):
        return ["plan: expected an object"], plan
    errs = [("rev: maintained by the store" if k == "rev" else f"{k}: unknown key")
            for k in doc if k not in DOC_KEYS]
    for name, form in _ids(doc.get("inputs", {}), "inputs", errs).items():
        try:
            plan.inputs[name] = T.parse(form, f"inputs.{name}")
        except T.TypeSyntaxError as e:
            errs.append(str(e))
    if "steps" not in doc:
        errs.append("steps: required, an object of id -> step")
    for sid, raw in _ids(doc.get("steps", {}), "steps", errs).items():
        p = f"steps.{sid}"
        if not isinstance(raw, dict):
            errs.append(f"{p}: a step is {{run, in, scatter?}}")
            continue
        errs.extend(f"{p}.{k}: unknown key" for k in raw if k not in STEP_KEYS)
        fn = registry.get(raw["run"]) if isinstance(raw.get("run"), str) else None
        if fn is None:
            errs.append(f"{p}.run: unknown fn {raw.get('run')!r}")
            continue
        ins = raw.get("in", {})
        if not isinstance(ins, dict):
            errs.append(f"{p}.in: expected an object of input -> source")
            ins = {}
        sources = {k: s for k, v in ins.items()
                   if (s := parse_source(v, f"{p}.in.{k}", errs)) is not None}
        errs.extend(f"{p}.in.{k}: fn {fn.name} has no input {k}" for k in ins
                    if k not in fn.inputs)
        errs.extend(f"{p}.in.{k}: required input is not bound" for k, t in fn.inputs.items()
                    if k not in ins and not isinstance(t, T.Optional))
        scatter = raw.get("scatter")
        if scatter is not None and scatter not in ins:
            errs.append(f"{p}.scatter: {scatter!r} is not a bound input of the step")
            scatter = None
        plan.steps[sid] = Step(sid, fn, sources, scatter)
    for name, raw in _ids(doc.get("outputs", {}), "outputs", errs).items():
        src = parse_source(raw, f"outputs.{name}", errs)
        if src is None or len(src.refs) != 1 or src.fan_in:
            if src is not None:
                errs.append(f'outputs.{name}: expected {{"source": "<ref>"}}')
            continue
        _, err = ref_type(src.refs[0], plan)
        if err:
            errs.append(f"outputs.{name}: {err}")
        plan.outputs[name] = src.refs[0]

    for step in plan.steps.values():
        for k, s in step.sources.items():
            if k in step.fn.inputs:
                target = step.fn.inputs[k]
                check_source(s, T.List(target) if k == step.scatter else target, plan,
                             f"steps.{step.id}.in.{k}", errs)
    cycle = find_cycle({s.id: s.deps for s in plan.steps.values()})
    if cycle:
        errs.append(f"steps.{cycle[0]}: dependency cycle {' -> '.join(cycle)}")
    return errs, plan


def value_of(ref: Ref, plan: Plan, state: dict[str, Any]) -> tuple[bool, Any]:
    """The runtime value of a ref: (available, value). Unset optional plan inputs are null."""
    if ref.step is None:
        if ref.name in state["inputs"]:
            v = state["inputs"][ref.name]
        elif isinstance(plan.inputs.get(ref.name), T.Optional):
            v = None
        else:
            return False, None
    else:
        e = state["steps"].get(ref.step, {})
        if e.get("status") != "succeeded":
            return False, None
        v = e["outputs"].get(ref.name)
    return True, T.navigate_value(v, ref.fields)
