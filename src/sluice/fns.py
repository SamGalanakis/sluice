"""The fn registry (SPEC §4): packs of fn.json files plus the native built-ins."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import types as T
from .util import parse_duration

NAME_RE = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$")
PORT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
FN_KEYS = {"name", "version", "description", "in", "out", "effects", "timeout", "slots", "retry",
           "graph"}

BUILTINS: list[dict[str, Any]] = [
    {
        "name": "core.echo",
        "version": 1,
        "description": "Pass the input value through. Useful for joins, renames and tests.",
        "in": {"value": "any"},
        "out": {"value": "any"},
        "effects": False,
    },
    {
        "name": "core.ask",
        "version": 1,
        "description": "Open an inbox item and wait until it is resolved with an answer.",
        "in": {"question": "string", "context": "any?",
               "to": {"optional": ["orchestrator", "human"]}},
        "out": {"answer": "any"},
    },
    {
        "name": "core.fail",
        "version": 1,
        "description": "Always fail with the given message.",
        "in": {"message": "string"},
        "out": {},
    },
]


BUILTIN_PACKS = Path(__file__).resolve().parent / "packs"


def builtin_packs() -> list[Path]:
    """The packs shipped inside the sluice package: every directory under sluice/packs."""
    if not BUILTIN_PACKS.is_dir():
        return []
    return sorted(p for p in BUILTIN_PACKS.iterdir()
                  if p.is_dir() and not p.name.startswith(("_", ".")))


class RegistryError(Exception):
    def __init__(self, errors: list[str]):
        super().__init__("; ".join(errors))
        self.errors = errors


@dataclass
class Fn:
    name: str
    version: int
    description: str
    in_types: dict[str, T.Type]
    out_types: dict[str, T.Type]
    raw: dict[str, Any]
    effects: bool = True
    timeout: float = 3600.0
    slots: dict[str, int] = field(default_factory=lambda: {"default": 1})
    retry_transient: int = 0
    retry_backoff: float = 30.0
    graph: dict[str, Any] | None = None
    dir: Path | None = None
    native: bool = False

    @property
    def composite(self) -> bool:
        return self.graph is not None

    @property
    def out_record(self) -> T.Record:
        return T.record_of(self.out_types)

    @property
    def in_record(self) -> T.Record:
        return T.record_of(self.in_types)

    def summary(self) -> dict[str, Any]:
        return {"name": self.name, "version": self.version, "description": self.description,
                "in": self.raw.get("in", {}), "out": self.raw.get("out", {}),
                "composite": self.composite, "effects": self.effects}


def parse_fn(raw: Any, where: str, fn_dir: Path | None = None, native: bool = False) -> Fn:
    """Validate one fn.json. Raises RegistryError with every problem found."""
    errs: list[str] = []
    if not isinstance(raw, dict):
        raise RegistryError([f"{where}: fn.json must be an object"])
    for k in raw:
        if k not in FN_KEYS:
            errs.append(f"{where}: unknown key {k!r}")
    name = raw.get("name")
    if not isinstance(name, str) or not NAME_RE.match(name):
        errs.append(f"{where}: name must be dotted lowercase like 'git.head', got {name!r}")
    version = raw.get("version", 1)
    if not isinstance(version, int) or isinstance(version, bool):
        errs.append(f"{where}: version must be an int")
    desc = raw.get("description", "")
    if not isinstance(desc, str):
        errs.append(f"{where}: description must be a string")

    def ports(key: str) -> dict[str, T.Type]:
        spec = raw.get(key)
        if not isinstance(spec, dict):
            errs.append(f"{where}: {key} must be an object of port -> type")
            return {}
        out = {}
        for port, form in spec.items():
            if not PORT_RE.match(port):
                errs.append(f"{where}: {key}.{port}: bad port name")
                continue
            try:
                out[port] = T.parse(form, f"{key}.{port}")
            except T.TypeSyntaxError as e:
                errs.append(f"{where}: {e}")
        return out

    in_types, out_types = ports("in"), ports("out")
    fn = Fn(name=name if isinstance(name, str) else "?", version=version if isinstance(version, int)
            else 0, description=desc if isinstance(desc, str) else "", in_types=in_types,
            out_types=out_types, raw=raw, dir=fn_dir, native=native)

    effects = raw.get("effects", True)
    if not isinstance(effects, bool):
        errs.append(f"{where}: effects must be a bool")
    else:
        fn.effects = effects
    try:
        fn.timeout = parse_duration(raw.get("timeout", "1h"))
    except ValueError as e:
        errs.append(f"{where}: timeout: {e}")
    slots = raw.get("slots", {"default": 1})
    if (not isinstance(slots, dict) or not all(
            isinstance(v, int) and not isinstance(v, bool) and v >= 1 for v in slots.values())):
        errs.append(f"{where}: slots must be an object of name -> positive int")
    else:
        fn.slots = dict(slots)
    retry = raw.get("retry", {})
    if not isinstance(retry, dict) or set(retry) - {"transient", "backoff"}:
        errs.append(f"{where}: retry must be {{transient: int, backoff: duration}}")
    else:
        n = retry.get("transient", 0)
        if not isinstance(n, int) or isinstance(n, bool) or n < 0:
            errs.append(f"{where}: retry.transient must be an int >= 0")
        else:
            fn.retry_transient = n
        try:
            fn.retry_backoff = parse_duration(retry.get("backoff", "30s"))
        except ValueError as e:
            errs.append(f"{where}: retry.backoff: {e}")

    graph = raw.get("graph")
    if graph is not None:
        if (not isinstance(graph, dict) or set(graph) - {"nodes", "out"}
                or not isinstance(graph.get("nodes"), dict)
                or not isinstance(graph.get("out", {}), dict)):
            errs.append(f"{where}: graph must be {{nodes: {{...}}, out: {{...}}}}")
        else:
            fn.graph = {"nodes": graph["nodes"], "out": graph.get("out", {})}
    elif not native and fn_dir is not None and not (fn_dir / "main.py").is_file():
        errs.append(f"{where}: needs main.py or a graph")
    if errs:
        raise RegistryError(errs)
    return fn


class Registry:
    def __init__(self, fns: dict[str, Fn]):
        self.fns = fns

    def get(self, name: str) -> Fn | None:
        return self.fns.get(name)

    def names(self) -> list[str]:
        return sorted(self.fns)

    @classmethod
    def load(cls, packs: list[str | Path]) -> Registry:
        """Load the native built-ins and every `<pack>/<dir>/fn.json` (subdirectories without
        fn.json are not fns and are skipped). Collects every error."""
        fns: dict[str, Fn] = {}
        where: dict[str, str] = {}
        errs: list[str] = []
        for raw in BUILTINS:
            fn = parse_fn(raw, raw["name"], native=True)
            fns[fn.name], where[fn.name] = fn, "built-in"
        seen: set[Path] = set()
        for pack in packs:
            pack = Path(pack).resolve()
            if pack in seen:
                continue
            seen.add(pack)
            if not pack.is_dir():
                errs.append(f"{pack}: pack directory does not exist")
                continue
            for fn_json in sorted(pack.glob("*/fn.json")):
                try:
                    raw = json.loads(fn_json.read_text(encoding="utf-8"))
                    fn = parse_fn(raw, str(fn_json), fn_json.parent)
                except json.JSONDecodeError as e:
                    errs.append(f"{fn_json}: bad JSON: {e}")
                    continue
                except RegistryError as e:
                    errs.extend(e.errors)
                    continue
                if fn.name in fns:
                    errs.append(f"{fn_json}: duplicate fn name {fn.name} (also in {where[fn.name]})")
                    continue
                fns[fn.name], where[fn.name] = fn, str(fn_json)
        if errs:
            raise RegistryError(errs)
        return cls(fns)
