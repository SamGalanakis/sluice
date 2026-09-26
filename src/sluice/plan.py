"""Plan documents (SPEC §5): parsing, validation per §5.2, composite expansion, dependency graph."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Any

from . import types as T
from .errors import InvalidPlan
from .fns import PORT_RE, Fn, Registry
from .util import parse_duration, sha256_json

ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]*$")
EXP_ID_RE = re.compile(r"^[a-z0-9][a-z0-9_-]*(/[a-z0-9][a-z0-9_-]*)*$")
DOC_KEYS = {"id", "title", "paused", "resources", "meta", "nodes"}
NODE_KEYS = {"fn", "in", "after", "when", "claims", "hold", "timeout", "note"}
OPS = {"eq", "ne", "in", "truthy", "falsy"}
MAX_DEPTH = 8
MISSING: Any = type("Missing", (), {"__repr__": lambda self: "MISSING"})()
LIVE = ("running", "waiting")


@dataclass(frozen=True)
class Ref:
    node: str
    port: str
    fields: tuple[str, ...] = ()

    def __str__(self) -> str:
        return ".".join((self.node, self.port, *self.fields))


@dataclass(frozen=True)
class Binding:
    kind: str  # "value" | "from" | "file"
    value: Any = None
    ref: Ref | None = None
    file: str | None = None

    def to_json(self) -> dict[str, Any]:
        if self.kind == "from":
            return {"from": str(self.ref)}
        if self.kind == "file":
            return {"file": self.file}
        return {"value": self.value}


@dataclass(frozen=True)
class Cond:
    source: Binding
    op: str
    value: Any = MISSING

    def to_json(self) -> dict[str, Any]:
        d = {"source": self.source.to_json(), "op": self.op}
        if self.value is not MISSING:
            d["value"] = self.value
        return d

    def describe(self) -> str:
        src = str(self.source.ref) if self.source.ref else repr(self.source.value)
        return f"{src} {self.op}" + ("" if self.value is MISSING else f" {self.value!r}")


@dataclass
class NodeDef:
    """A node as written, in a plan or in a composite graph."""

    id: str
    fn: str
    bindings: dict[str, Binding]
    after: list[str]
    when: list[Cond]
    claims: list[str]
    hold: bool
    timeout: float | None
    raw: dict[str, Any]


@dataclass
class ENode:
    """A node of the expanded graph. Composite nodes have children and out bindings."""

    id: str
    fn: Fn
    bindings: dict[str, Binding]
    after: list[str]
    when: list[Cond]
    claims: list[str]
    hold: bool
    timeout: float | None
    parent: str | None
    deps: list[str]
    definition: dict[str, Any]
    children: list[str] = field(default_factory=list)
    outs: dict[str, Binding] | None = None
    def_sha: str = ""

    @property
    def composite(self) -> bool:
        return self.fn.composite


@dataclass
class Expanded:
    nodes: dict[str, ENode]
    order: list[str]  # every id, dependencies first

    def leaves(self) -> list[str]:
        return [i for i in self.order if not self.nodes[i].composite]

    def composites_bottom_up(self) -> list[str]:
        """Composite ids, innermost first."""
        return sorted((i for i in self.nodes if self.nodes[i].composite),
                      key=lambda i: -i.count("/"))

    def leaves_under(self, nid: str) -> list[str]:
        en = self.nodes[nid]
        if not en.composite:
            return [nid]
        out: list[str] = []
        for c in en.children:
            out.extend(self.leaves_under(c))
        return out

    def ids_under(self, nid: str) -> list[str]:
        out = [nid]
        for c in self.nodes[nid].children:
            out.extend(self.ids_under(c))
        return out

    def ancestors(self, nid: str) -> list[str]:
        out = []
        p = self.nodes[nid].parent
        while p is not None:
            out.append(p)
            p = self.nodes[p].parent
        return out

    def dependents(self) -> dict[str, list[str]]:
        rev: dict[str, list[str]] = {i: [] for i in self.nodes}
        for en in self.nodes.values():
            for d in en.deps:
                if d in rev:
                    rev[d].append(en.id)
        return rev


# ---- parsing ------------------------------------------------------------------------------


def parse_ref(text: Any, local: bool) -> tuple[Ref | None, str]:
    if not isinstance(text, str):
        return None, f"a ref is a string like 'node.port', got {text!r}"
    parts = text.split(".")
    if len(parts) < 2 or not all(parts):
        return None, f"bad ref {text!r}: expected <node>.<port>[.<field>...]"
    node, port = parts[0], parts[1]
    ok_node = (node == "$in" or ID_RE.match(node)) if local else EXP_ID_RE.match(node)
    if not ok_node:
        return None, f"bad ref {text!r}: bad node id {node!r}"
    if not PORT_RE.match(port):
        return None, f"bad ref {text!r}: bad port name {port!r}"
    return Ref(node, port, tuple(parts[2:])), ""


def parse_binding(raw: Any, path: str, errs: list[str], local: bool) -> Binding | None:
    if not isinstance(raw, dict) or len(raw) != 1 or next(iter(raw)) not in ("value", "from",
                                                                              "file"):
        errs.append(f"{path}: a binding is exactly one of {{value}}, {{from}} or {{file}}")
        return None
    kind, v = next(iter(raw.items()))
    if kind == "value":
        return Binding("value", value=v)
    if kind == "file":
        if not isinstance(v, str) or not v:
            errs.append(f"{path}.file: expected a path string")
            return None
        return Binding("file", file=v)
    ref, err = parse_ref(v, local)
    if ref is None:
        errs.append(f"{path}.from: {err}")
        return None
    return Binding("from", ref=ref)


def _str_list(raw: dict[str, Any], key: str, path: str, errs: list[str]) -> list[str]:
    v = raw.get(key, [])
    if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
        errs.append(f"{path}.{key}: expected a list of strings")
        return []
    return list(v)


def parse_node(nid: str, raw: Any, path: str, errs: list[str], local: bool) -> NodeDef | None:
    """Shape-check one node definition. Appends errors; returns None if unusable."""
    if not isinstance(raw, dict):
        errs.append(f"{path}: a node is an object")
        return None
    n0 = len(errs)
    for k in raw:
        if k not in NODE_KEYS:
            errs.append(f"{path}.{k}: unknown key")
    fn = raw.get("fn")
    if not isinstance(fn, str):
        errs.append(f"{path}.fn: required, the name of a fn")
    bindings: dict[str, Binding] = {}
    ins = raw.get("in", {})
    if not isinstance(ins, dict):
        errs.append(f"{path}.in: expected an object of port -> binding")
    else:
        for port, b in ins.items():
            parsed = parse_binding(b, f"{path}.in.{port}", errs, local)
            if parsed is not None:
                bindings[port] = parsed
    after = _str_list(raw, "after", path, errs)
    for i, a in enumerate(after):
        if not (ID_RE.match(a) if local else EXP_ID_RE.match(a)):
            errs.append(f"{path}.after[{i}]: bad node id {a!r}")
    when: list[Cond] = []
    conds = raw.get("when", [])
    if not isinstance(conds, list):
        errs.append(f"{path}.when: expected a list of conditions")
        conds = []
    for i, c in enumerate(conds):
        cp = f"{path}.when[{i}]"
        if not isinstance(c, dict) or set(c) - {"from", "op", "value"} or "from" not in c:
            errs.append(f"{cp}: a condition is {{from, op, value?}}")
            continue
        ref, err = parse_ref(c["from"], local)
        if ref is None:
            errs.append(f"{cp}.from: {err}")
            continue
        op = c.get("op")
        if op not in OPS:
            errs.append(f"{cp}.op: expected one of {sorted(OPS)}, got {op!r}")
            continue
        if op in ("eq", "ne", "in") and "value" not in c:
            errs.append(f"{cp}.value: required for op {op}")
            continue
        if op in ("truthy", "falsy") and "value" in c:
            errs.append(f"{cp}.value: not used by op {op}")
            continue
        if op == "in" and not isinstance(c["value"], list):
            errs.append(f"{cp}.value: op in needs a list")
            continue
        when.append(Cond(Binding("from", ref=ref), op, c.get("value", MISSING)))
    claims = _str_list(raw, "claims", path, errs)
    hold = raw.get("hold", False)
    if not isinstance(hold, bool):
        errs.append(f"{path}.hold: expected a bool")
    timeout = None
    if "timeout" in raw:
        try:
            timeout = parse_duration(raw["timeout"])
        except ValueError as e:
            errs.append(f"{path}.timeout: {e}")
    note = raw.get("note")
    if note is not None and not isinstance(note, str):
        errs.append(f"{path}.note: expected a string")
    if len(errs) > n0 or not isinstance(fn, str):
        return None
    return NodeDef(nid, fn, bindings, after, when, claims, bool(hold), timeout, raw)


def parse_graph(fn: Fn) -> tuple[dict[str, NodeDef], dict[str, Binding]]:
    """The parsed graph of a composite that already passed `check_composite`."""
    cached = getattr(fn, "_parsed_graph", None)
    if cached is not None:
        return cached
    assert fn.graph is not None
    errs: list[str] = []
    defs = {}
    for iid, raw in fn.graph["nodes"].items():
        nd = parse_node(iid, raw, f"graph.nodes.{iid}", errs, local=True)
        if nd is not None:
            defs[iid] = nd
    outs = {}
    for port, raw in fn.graph["out"].items():
        b = parse_binding(raw, f"graph.out.{port}", errs, local=True)
        if b is not None:
            outs[port] = b
    fn._parsed_graph = (defs, outs)  # type: ignore[attr-defined]
    return defs, outs


# ---- type checks shared by plans and composite graphs -------------------------------------


class Scope:
    def __init__(self, ids: dict[str, Fn], in_types: dict[str, T.Type] | None = None,
                 resources: dict[str, int] | None = None):
        self.ids, self.in_types, self.resources = ids, in_types, resources

    def ref_type(self, ref: Ref) -> tuple[T.Type | None, str]:
        if ref.node == "$in":
            if self.in_types is None:
                return None, "$in is only valid inside a composite graph"
            base = self.in_types.get(ref.port)
            if base is None:
                return None, f"the composite has no input port {ref.port}"
        else:
            fn = self.ids.get(ref.node)
            if fn is None:
                return None, f"unknown node {ref.node}"
            base = fn.out_types.get(ref.port)
            if base is None:
                return None, f"node {ref.node} (fn {fn.name}) has no output port {ref.port}"
        t, err = T.navigate(base, ref.fields)
        return (t, "") if t is not None else (None, f"{ref}: {err}")


def check_binding(b: Binding, target: T.Type, scope: Scope, path: str, errs: list[str]) -> None:
    if b.kind == "value":
        errs.extend(T.check_value(target, b.value, path))
    elif b.kind == "file":
        ok, _ = T.fits(T.STRING, target)
        if not ok:
            errs.append(f"{path}: a file binding needs a port that accepts string, not {target}")
    else:
        assert b.ref is not None
        src, err = scope.ref_type(b.ref)
        if src is None:
            errs.append(f"{path}: {err}")
            return
        ok, reason = T.fits(src, target)
        if not ok:
            errs.append(f"{path}: out type {src} does not fit {target}: {reason}")


def check_node(nd: NodeDef, fn: Fn, scope: Scope, path: str, errs: list[str]) -> None:
    if fn.composite and nd.timeout is not None:
        errs.append(f"{path}.timeout: does not apply to a composite fn")
    for port, b in nd.bindings.items():
        target = fn.in_types.get(port)
        if target is None:
            errs.append(f"{path}.in.{port}: fn {fn.name} has no input port {port}")
            continue
        check_binding(b, target, scope, f"{path}.in.{port}", errs)
    for port, t in fn.in_types.items():
        if port not in nd.bindings and not isinstance(t, T.Optional):
            errs.append(f"{path}.in.{port}: required input port is not bound")
    for i, a in enumerate(nd.after):
        if a not in scope.ids:
            errs.append(f"{path}.after[{i}]: unknown node {a}")
    for i, c in enumerate(nd.when):
        assert c.source.ref is not None
        t, err = scope.ref_type(c.source.ref)
        cp = f"{path}.when[{i}]"
        if t is None:
            errs.append(f"{cp}.from: {err}")
        elif c.op in ("eq", "ne"):
            errs.extend(T.check_value(t, c.value, f"{cp}.value"))
        elif c.op == "in":
            for j, v in enumerate(c.value):
                errs.extend(T.check_value(t, v, f"{cp}.value[{j}]"))
    if scope.resources is not None:
        for i, c in enumerate(nd.claims):
            if c not in scope.resources:
                errs.append(f"{path}.claims[{i}]: unknown resource {c}")


def _local_deps(nd: NodeDef) -> list[str]:
    out = [b.ref.node for b in nd.bindings.values() if b.kind == "from" and b.ref]
    out += nd.after
    out += [c.source.ref.node for c in nd.when if c.source.ref]
    return out


def check_composite(fn: Fn, registry: Registry, depth: int = 1,
                    stack: tuple[str, ...] = ()) -> list[str]:
    """Check a composite's graph on its own terms. Errors are relative to the fn."""
    if fn.name in stack:
        return [f"composite {fn.name} contains itself"]
    if depth > MAX_DEPTH:
        return [f"composites nest deeper than {MAX_DEPTH}"]
    assert fn.graph is not None
    errs: list[str] = []
    defs: dict[str, NodeDef] = {}
    for iid, raw in fn.graph["nodes"].items():
        p = f"graph.nodes.{iid}"
        if not ID_RE.match(iid):
            errs.append(f"{p}: bad node id (must match {ID_RE.pattern})")
            continue
        nd = parse_node(iid, raw, p, errs, local=True)
        if nd is not None:
            defs[iid] = nd
    fns: dict[str, Fn] = {}
    for iid, nd in defs.items():
        inner = registry.get(nd.fn)
        if inner is None:
            errs.append(f"graph.nodes.{iid}.fn: unknown fn {nd.fn}")
            continue
        fns[iid] = inner
        if inner.composite:
            sub = check_composite(inner, registry, depth + 1, (*stack, fn.name))
            errs.extend(f"graph.nodes.{iid}: fn {inner.name}: {e}" for e in sub)
    scope = Scope(fns, fn.in_types)
    for iid, nd in defs.items():
        if iid in fns:
            check_node(nd, fns[iid], scope, f"graph.nodes.{iid}", errs)
    for port, raw in fn.graph["out"].items():
        p = f"graph.out.{port}"
        b = parse_binding(raw, p, errs, local=True)
        if port not in fn.out_types:
            errs.append(f"{p}: fn {fn.name} has no output port {port}")
        elif b is not None:
            check_binding(b, fn.out_types[port], scope, p, errs)
    for port, t in fn.out_types.items():
        if port not in fn.graph["out"] and not isinstance(t, T.Optional):
            errs.append(f"graph.out.{port}: required output port is not bound")
    if len(defs) == len(fn.graph["nodes"]):
        cyc = find_cycle({i: [d for d in _local_deps(nd) if d in defs] for i, nd in defs.items()})
        if cyc:
            errs.append(f"graph.nodes: dependency cycle {' -> '.join(cyc)}")
    return errs


# ---- graph helpers ------------------------------------------------------------------------


def _dfs(adj: dict[str, list[str]]) -> tuple[list[str], list[str] | None]:
    """Iterative DFS. Returns (post-order, first cycle found or None)."""
    color: dict[str, int] = {}
    order: list[str] = []
    for root in adj:
        if root in color:
            continue
        stack = [(root, iter(adj.get(root, ())))]
        path = [root]
        color[root] = 1
        while stack:
            node, it = stack[-1]
            nxt = next(it, None)
            if nxt is None:
                stack.pop()
                path.pop()
                color[node] = 2
                order.append(node)
                continue
            if nxt not in adj:
                continue
            c = color.get(nxt, 0)
            if c == 1:
                return order, path[path.index(nxt):] + [nxt]
            if c == 0:
                color[nxt] = 1
                stack.append((nxt, iter(adj.get(nxt, ()))))
                path.append(nxt)
    return order, None


def find_cycle(adj: dict[str, list[str]]) -> list[str] | None:
    return _dfs(adj)[1]


# ---- expansion ----------------------------------------------------------------------------


def _extend(outer: Binding | None, fields: tuple[str, ...]) -> Binding:
    if outer is None:
        return Binding("value", value=None)
    if outer.kind == "value":
        return Binding("value", value=T.navigate_value(outer.value, fields))
    if outer.kind == "file":
        return Binding("value", value=None) if fields else outer
    assert outer.ref is not None
    return Binding("from", ref=Ref(outer.ref.node, outer.ref.port, outer.ref.fields + fields))


def _unique(xs: list[str]) -> list[str]:
    return list(dict.fromkeys(xs))


def expand(defs: dict[str, NodeDef], registry: Registry) -> Expanded:
    """Expand composites into `<outer>/<inner>` nodes. Assumes the definitions are valid."""
    nodes: dict[str, ENode] = {}

    def add(eid: str, nd: NodeDef, fn: Fn, parent: str | None, sub_b, sub_id,
            inherited: list[str], held: bool) -> ENode:
        bindings = {p: sub_b(b) for p, b in nd.bindings.items()}
        after = [sub_id(a) for a in nd.after]
        when = [Cond(sub_b(c.source), c.op, c.value) for c in nd.when]
        own = [b.ref.node for b in bindings.values() if b.kind == "from" and b.ref]
        own += after + [c.source.ref.node for c in when if c.source.ref]
        en = ENode(eid, fn, bindings, after, when, list(nd.claims), held or nd.hold, nd.timeout,
                   parent, _unique(own + inherited), nd.raw)
        nodes[eid] = en
        if fn.composite:
            inner_defs, outs = parse_graph(fn)

            def ib(b: Binding) -> Binding:
                if b.kind != "from" or b.ref is None:
                    return b
                if b.ref.node == "$in":
                    return _extend(bindings.get(b.ref.port), b.ref.fields)
                return Binding("from", ref=Ref(f"{eid}/{b.ref.node}", b.ref.port, b.ref.fields))

            for iid, ind in inner_defs.items():
                inner_fn = registry.get(ind.fn)
                assert inner_fn is not None
                child = add(f"{eid}/{iid}", ind, inner_fn, eid, ib, lambda x: f"{eid}/{x}",
                            en.deps, en.hold)
                en.children.append(child.id)
            en.outs = {p: ib(b) for p, b in outs.items()}
        en.def_sha = sha256_json({"fn": fn.name, "in": {p: b.to_json() for p, b in bindings.items()},
                                  "after": after, "when": [c.to_json() for c in when]})
        return en

    for nid, nd in defs.items():
        fn = registry.get(nd.fn)
        assert fn is not None
        add(nid, nd, fn, None, lambda b: b, lambda x: x, [], False)
    order, _ = _dfs(_graph(nodes))
    return Expanded(nodes, order)


def _graph(nodes: dict[str, ENode]) -> dict[str, list[str]]:
    """Edges from a node to what it waits for: its deps, and for a composite its children."""
    return {i: en.deps + en.children for i, en in nodes.items()}


def _inner_fns(prefix: str, fn: Fn, registry: Registry, out: dict[str, Fn]) -> None:
    defs, _ = parse_graph(fn)
    for iid, nd in defs.items():
        inner = registry.get(nd.fn)
        if inner is not None:
            out[f"{prefix}/{iid}"] = inner
            if inner.composite:
                _inner_fns(f"{prefix}/{iid}", inner, registry, out)


# ---- validation ---------------------------------------------------------------------------


def validate(doc: Any, registry: Registry, *, state: dict[str, Any] | None = None,
             previous: Expanded | None = None) -> tuple[list[str], Expanded | None]:
    """Validate a plan document (without rev) per SPEC §5.2. Returns (errors, expansion)."""
    errs: list[str] = []
    if not isinstance(doc, dict):
        return ["document: expected an object"], None
    for k in doc:
        if k == "rev":
            errs.append("rev: maintained by the store; it cannot be set or patched")
        elif k not in DOC_KEYS:
            errs.append(f"{k}: unknown key")
    pid = doc.get("id")
    if not isinstance(pid, str) or not ID_RE.match(pid):
        errs.append(f"id: plan ids match {ID_RE.pattern}, got {pid!r}")
    if not isinstance(doc.get("title", ""), str):
        errs.append("title: expected a string")
    if not isinstance(doc.get("paused", False), bool):
        errs.append("paused: expected a bool")
    if not isinstance(doc.get("meta", {}), dict):
        errs.append("meta: expected an object")
    resources = doc.get("resources", {})
    if not isinstance(resources, dict):
        errs.append("resources: expected an object of name -> units")
        resources = {}
    for name, units in resources.items():
        if not isinstance(units, int) or isinstance(units, bool) or units < 1:
            errs.append(f"resources.{name}: expected a positive int, got {units!r}")
    nodes = doc.get("nodes")
    if not isinstance(nodes, dict):
        errs.append("nodes: required, an object of id -> node")
        return errs, None

    # 1. shape and ids
    defs: dict[str, NodeDef] = {}
    for nid, raw in nodes.items():
        p = f"nodes.{nid}"
        if not ID_RE.match(nid):
            errs.append(f"{p}: node ids match {ID_RE.pattern}")
            continue
        nd = parse_node(nid, raw, p, errs, local=False)
        if nd is not None:
            defs[nid] = nd
    structural = len(defs) == len(nodes)

    # 2. fns exist, 3. composites expand
    fns: dict[str, Fn] = {}
    for nid, nd in defs.items():
        fn = registry.get(nd.fn)
        if fn is None:
            errs.append(f"nodes.{nid}.fn: unknown fn {nd.fn}")
            structural = False
            continue
        fns[nid] = fn
        if fn.composite:
            sub = check_composite(fn, registry)
            errs.extend(f"nodes.{nid}: fn {fn.name}: {e}" for e in sub)
            if sub:
                structural = False

    # 4.-6. refs, ports, types (8. claims on the plan's own nodes)
    scope_ids = dict(fns)
    for nid, fn in fns.items():
        if fn.composite and structural:
            _inner_fns(nid, fn, registry, scope_ids)
    scope = Scope(scope_ids, None, resources)
    for nid, nd in defs.items():
        if nid in fns:
            check_node(nd, fns[nid], scope, f"nodes.{nid}", errs)
    if not structural:
        return errs, None

    exp = expand(defs, registry)
    # 8. claims of expanded inner nodes
    for en in exp.nodes.values():
        if en.parent is not None:
            for i, c in enumerate(en.claims):
                if c not in resources:
                    errs.append(f"nodes.{en.id}.claims[{i}]: unknown resource {c}")
    # 7. acyclic
    cyc = find_cycle(_graph(exp.nodes))
    if cyc:
        errs.append(f"nodes.{cyc[0]}: dependency cycle {' -> '.join(cyc)}")
    # 9. no deleting or rewiring a succeeded node while a dependent runs
    if state is not None and previous is not None:
        st = state.get("nodes", {})
        dependents = previous.dependents()
        for oid, old in previous.nodes.items():
            if st.get(oid, {}).get("status") != "succeeded":
                continue
            new = exp.nodes.get(oid)
            if new is not None and new.def_sha == old.def_sha:
                continue
            what = "delete" if new is None else "rewire"
            for d in dependents.get(oid, []):
                if st.get(d, {}).get("status") in LIVE:
                    errs.append(f"nodes.{oid}: cannot {what} a succeeded node while its "
                                f"dependent {d} is {st[d]['status']}")
    return errs, exp


def expand_doc(doc: dict[str, Any], registry: Registry) -> Expanded:
    """Expand a stored document. Raises InvalidPlan if it no longer validates."""
    errs, exp = validate({k: v for k, v in doc.items() if k != "rev"}, registry)
    if exp is None or errs:
        raise InvalidPlan(errs, "the stored plan does not validate against the loaded fns")
    return exp
