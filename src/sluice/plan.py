"""Plan documents (SPEC §5): sources, scatter, validation, dependencies."""

from __future__ import annotations

import hashlib
import re
from dataclasses import dataclass, field
from typing import Any

from . import types as T
from .registry import Fn, Registry
from .util import canonical

ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]*$")
DOC_KEYS = {"inputs", "outputs", "steps"}
EMPTY: dict[str, Any] = {"inputs": {}, "outputs": {}, "steps": {}}
STEP_KEYS = {"run", "in", "scatter", "doc", "outputs", "paused", "after", "tags"}
INPUT_KEYS = {"type", "doc"}  # a plan input's or step output's object form {"type": T, "doc"}


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
    """A step. Running an open fn it may also bind extra inputs (typed by their sources) and
    declare outputs of its own, which the agent submits (`step_submit`)."""

    id: str
    fn: Fn
    sources: dict[str, Source]
    scatter: str | None
    doc: str = ""
    extra: dict[str, T.Type] = field(default_factory=dict)  # extra input -> its source's type
    declared: dict[str, T.Type] = field(default_factory=dict)  # declared output -> type
    output_docs: dict[str, str] = field(default_factory=dict)  # only outputs that have one
    paused: bool = False  # held: it does not start, even when ready, until unpaused
    pause_reason: str = ""  # why, when `paused` was given as a string
    after: list[str] = field(default_factory=list)  # steps it waits for without reading them
    tags: list[str] = field(default_factory=list)  # free-form labels to select steps by

    @property
    def inputs(self) -> dict[str, T.Type]:
        return {**self.fn.inputs, **self.extra}

    @property
    def outputs(self) -> dict[str, T.Type]:
        return {**self.fn.outputs, **self.declared}

    @property
    def reads(self) -> list[Ref]:
        return [r for s in self.sources.values() for r in s.refs]

    @property
    def deps(self) -> list[str]:
        """The steps it reads from (a data dependency: their staleness is its staleness)."""
        return list(dict.fromkeys(r.step for r in self.reads if r.step))

    @property
    def waits(self) -> list[str]:
        """Every step it waits for: those it reads from, then those it runs `after`."""
        return list(dict.fromkeys([*self.deps, *self.after]))

    def output_type(self, name: str) -> T.Type | None:
        t = self.outputs.get(name)
        return T.List(t) if t is not None and self.scatter else t

    def ports(self) -> dict[str, dict[str, Any]]:
        """What an open fn is told about the step (SPEC §4): its extra inputs {name: {type}}
        and declared outputs {name: {type, doc}}, types in their CWL spelling."""
        return {"inputs": {k: {"type": T.form(t)} for k, t in self.extra.items()},
                "outputs": {k: {"type": T.form(t), "doc": self.output_docs.get(k, "")}
                            for k, t in self.declared.items()}}


@dataclass
class Plan:
    inputs: dict[str, T.Type]
    outputs: dict[str, Ref]
    steps: dict[str, Step]
    input_docs: dict[str, str] = field(default_factory=dict)  # only inputs that have one


def parse_input(form: Any, path: str, errs: list[str]) -> tuple[T.Type | None, str]:
    """A plan input or step output declaration: a type, or `{"type": T, "doc": "..."}` (CWL's
    object form; no type form has only these keys). Returns (its type, its doc)."""
    doc = ""
    if isinstance(form, dict) and form.keys() <= INPUT_KEYS:
        if "type" not in form:
            errs.append(f"{path}.type: required")
            return None, ""
        doc = form.get("doc", "")
        if not isinstance(doc, str):
            errs.append(f"{path}.doc: expected a string")
            doc = ""
        form, path = form["type"], f"{path}.type"
    try:
        return T.parse(form, path), doc
    except T.TypeSyntaxError as e:
        errs.append(str(e))
        return None, doc


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


def source_type(s: Source, plan: Plan, path: str, errs: list[str]) -> T.Type:
    """The type an extra input takes from its source: a ref's type, an array of the refs'
    type for a list source (`Any[]` when they differ), `Any` for a default."""
    if not s.refs and not s.fan_in:
        return T.ANY
    found = []
    for i, ref in enumerate(s.refs):
        t, err = ref_type(ref, plan)
        if t is None:
            errs.append(f"{path}.source[{i}]: {err}" if s.fan_in else f"{path}: {err}")
        found.append(t or T.ANY)
    if not s.fan_in:
        return found[0]
    return T.List(found[0] if len(set(found)) == 1 else T.ANY)


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
        t, text = parse_input(form, f"inputs.{name}", errs)
        if t is not None:
            plan.inputs[name] = t
            if text:
                plan.input_docs[name] = text
    if "steps" not in doc:
        errs.append("steps: required, an object of id -> step")
    for sid, raw in _ids(doc.get("steps", {}), "steps", errs).items():
        p = f"steps.{sid}"
        if not isinstance(raw, dict):
            errs.append(f"{p}: a step is {{run, in, scatter?, doc?, outputs?, paused?, after?, "
                        "tags?}")
            continue
        errs.extend(f"{p}.{k}: unknown key" for k in raw if k not in STEP_KEYS)
        text = raw.get("doc", "")
        if not isinstance(text, str):
            errs.append(f"{p}.doc: expected a string")
            text = ""
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
        for k in ins:
            if k in fn.inputs:
                continue
            if not fn.open:
                errs.append(f"{p}.in.{k}: fn {fn.name} has no input {k} (only an open fn "
                            "takes extra inputs)")
            elif not ID_RE.match(k):
                errs.append(f"{p}.in.{k}: extra input names match {ID_RE.pattern}")
        errs.extend(f"{p}.in.{k}: required input is not bound" for k, t in fn.inputs.items()
                    if k not in ins and not isinstance(t, T.Optional))
        scatter = raw.get("scatter")
        if scatter is not None and scatter not in ins:
            errs.append(f"{p}.scatter: {scatter!r} is not a bound input of the step")
            scatter = None
        paused = raw.get("paused", False)
        if not (isinstance(paused, bool) or isinstance(paused, str) and paused.strip()):
            errs.append(f"{p}.paused: expected true, false or the reason (a string)")
        after = raw.get("after", [])
        if not (isinstance(after, list) and all(isinstance(a, str) for a in after)):
            errs.append(f"{p}.after: expected an array of step ids")
            after = []
        tags = raw.get("tags", [])
        if not (isinstance(tags, list) and all(isinstance(t, str) and ID_RE.match(t)
                                               for t in tags)):
            errs.append(f"{p}.tags: expected an array of tags matching {ID_RE.pattern}")
            tags = []
        step = plan.steps[sid] = Step(
            sid, fn, sources, scatter, text, paused=paused is True or isinstance(paused, str),
            pause_reason=paused if isinstance(paused, str) else "",
            after=list(dict.fromkeys(after)), tags=list(dict.fromkeys(tags)))
        if "outputs" in raw:
            if not fn.open:
                errs.append(f"{p}.outputs: fn {fn.name} is not open; only a step running an "
                            "open fn declares outputs")
            else:
                for name, form in _ids(raw["outputs"], f"{p}.outputs", errs).items():
                    if name in fn.outputs:
                        errs.append(f"{p}.outputs.{name}: fn {fn.name} already has an output "
                                    f"{name}")
                        continue
                    t, out_doc = parse_input(form, f"{p}.outputs.{name}", errs)
                    if t is not None:
                        step.declared[name] = t
                        if out_doc:
                            step.output_docs[name] = out_doc
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
            path = f"steps.{step.id}.in.{k}"
            if k in step.fn.inputs:
                target = step.fn.inputs[k]
                check_source(s, T.List(target) if k == step.scatter else target, plan, path,
                             errs)
            elif step.fn.open:
                t = source_type(s, plan, path, errs)
                if k == step.scatter:
                    inner = t.of if isinstance(t, T.Optional) else t
                    if inner != T.ANY and not isinstance(inner, T.List):
                        errs.append(f"{path}: the scatter input needs an array, not {t}")
                    t = inner.of if isinstance(inner, T.List) else T.ANY
                step.extra[k] = t
    for step in plan.steps.values():
        for a in step.after:
            if a == step.id:
                errs.append(f"steps.{step.id}.after: a step cannot run after itself")
            elif a not in plan.steps and a not in doc.get("steps", {}):
                errs.append(f"steps.{step.id}.after: no step {a}")
    cycle = find_cycle({s.id: s.waits for s in plan.steps.values()})
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


def source_value(src: Source, plan: Plan, state: dict[str, Any]) -> Any:
    if not src.refs and not src.fan_in:
        return src.default
    values = [value_of(r, plan, state)[1] for r in src.refs]
    return values if src.fan_in else values[0]


def is_ready(step: Step, plan: Plan, state: dict[str, Any]) -> bool:
    """Every plan input the step reads has a value, and every step it reads or runs after has
    succeeded."""
    return all(value_of(r, plan, state)[0] for r in step.reads) and all(
        state["steps"].get(a, {}).get("status") == "succeeded" for a in step.after)


def not_ready(step: Step, plan: Plan, state: dict[str, Any]) -> list[str]:
    """Why a step is not ready: `step a is pending`, `plan input n has no value`."""
    out = []
    for r in step.reads:
        if value_of(r, plan, state)[0]:
            continue
        if r.step is None:
            out.append(f"plan input {r.name} has no value")
        else:
            status = state["steps"].get(r.step, {}).get("status", "pending")
            out.append(f"step {r.step} is {status}")
    for a in step.after:
        status = state["steps"].get(a, {}).get("status", "pending")
        if status != "succeeded":
            out.append(f"after step {a}, which is {status}")
    return list(dict.fromkeys(out))


def resolved_inputs(step: Step, plan: Plan, state: dict[str, Any]) -> dict[str, Any]:
    """The input object a step runs with (unbound optional inputs are null); for a scattered
    step, with the whole array."""
    inp: dict[str, Any] = {k: None for k in step.inputs}
    inp.update({k: source_value(s, plan, state) for k, s in step.sources.items()})
    return inp


def inputs_hash(inp: dict[str, Any]) -> str:
    """SPEC §6 staleness: a hash of the canonical JSON of the resolved inputs."""
    return hashlib.sha256(canonical(inp).encode()).hexdigest()[:32]


def topo_order(plan: Plan) -> list[str]:
    """Step ids with every step after the steps it reads (the plan is acyclic)."""
    order: list[str] = []
    seen: set[str] = set()

    def visit(sid: str) -> None:
        if sid in seen:
            return
        seen.add(sid)
        for dep in plan.steps[sid].waits:
            if dep in plan.steps:
                visit(dep)
        order.append(sid)

    for sid in plan.steps:
        visit(sid)
    return order


def mark_stale(plan: Plan, state: dict[str, Any]) -> bool:
    """SPEC §6 staleness, one pass in dependency order. A succeeded step turns stale when a step
    it reads is stale, or when its inputs now hash differently from those it was computed from
    (a null hash, from a forced manual value, counts as different once all its inputs are
    there). A stale step whose inputs hash as recorded again is succeeded again. A step with no
    recorded hash at all (state from before hashes existed) adopts the current one.
    Returns whether anything changed."""
    changed = False
    st = state["steps"]
    for sid in topo_order(plan):
        e = st.get(sid)
        if e is None or e.get("status") not in ("succeeded", "stale"):
            continue
        step = plan.steps[sid]
        upstream_stale = any(st.get(d, {}).get("status") == "stale" for d in step.deps)
        ready = not upstream_stale and is_ready(step, plan, state)
        h = inputs_hash(resolved_inputs(step, plan, state)) if ready else None
        if e["status"] == "succeeded":
            if upstream_stale:
                e["status"] = "stale"
                changed = True
            elif ready and "inputs_hash" not in e:
                e["inputs_hash"] = h
                changed = True
            elif ready and e["inputs_hash"] != h:
                e["status"] = "stale"
                changed = True
        elif ready and e.get("inputs_hash") is not None and e["inputs_hash"] == h:
            e["status"] = "succeeded"
            changed = True
    return changed
