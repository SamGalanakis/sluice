"""Functions and their scopes (SPEC §2, §4): built-in, global and project fn dirs.

A scope is scanned into entries (one per fn dir, loaded or not); a Registry combines the scopes a
project sees in lookup order and records every problem: a bad fn.json, a name that does not match
its directory, or a name that collides with one in an earlier scope (or the same scope).
"""

from __future__ import annotations

import json
import re
from collections.abc import Callable, Iterable
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any

from . import types as T

BUILTIN_DIR = Path(__file__).resolve().parent / "fns"


def _format(inp: dict[str, Any]) -> dict[str, Any]:
    def show(v: Any) -> str:
        return v if isinstance(v, str) else json.dumps(v)

    values = inp["values"]
    if isinstance(values, list):
        return {"text": inp["template"].format(*map(show, values))}
    if isinstance(values, dict):
        return {"text": inp["template"].format(**{k: show(v) for k, v in values.items()})}
    return {"text": inp["template"].format(show(values))}


NATIVE = {"core.echo": lambda inp: {"value": inp["value"]},
          "core.collect": lambda inp: {"items": inp["items"]},
          "core.format": _format}  # built-ins run inline; no main.py
NAME_RE = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$")
KEYS = {"name", "doc", "inputs", "outputs", "open", "submits"}
SCOPES = ("builtin", "global", "project")

Show = Callable[[Path], str]


@dataclass
class Fn:
    name: str
    doc: str
    inputs: dict[str, T.Type]
    outputs: dict[str, T.Type]
    raw: dict[str, Any]
    dir: Path
    scope: str = "global"
    open: bool = False  # a step running it may bind extra inputs and declare outputs (§5)
    # an open fn's outputs its agent submits (step_submit): every step running it declares them
    submits: dict[str, T.Type] = field(default_factory=dict)
    submit_docs: dict[str, str] = field(default_factory=dict)

    @property
    def native(self) -> bool:
        return self.scope == "builtin" and self.name in NATIVE

    def summary(self) -> dict[str, Any]:
        out = {"name": self.name, "doc": self.doc, "inputs": self.raw["inputs"],
               "outputs": self.raw["outputs"], "scope": self.scope}
        if self.submits:
            out["submits"] = self.raw["submits"]
        return {**out, "open": True} if self.open else out


@dataclass
class Entry:
    """One fn dir of a scope: its fn when it loaded and does not collide, else its errors."""

    name: str  # the fn.json name when it is a string, else the directory name
    scope: str
    dir: Path
    fn: Fn | None
    errors: list[str] = field(default_factory=list)

    def summary(self) -> dict[str, Any]:
        if self.fn is not None and not self.errors:
            return self.fn.summary()
        out: dict[str, Any] = {"name": self.name, "scope": self.scope}
        if self.fn is not None:
            out.update(self.fn.summary())
        return {**out, "error": "; ".join(self.errors)}


def parse_fn(raw: Any, fn_dir: Path, scope: str = "global",
             check_dir: bool = True) -> tuple[Fn | None, list[str]]:
    """Validate one fn.json (SPEC §4, §6a). Returns (fn or None, every problem found)."""
    if not isinstance(raw, dict):
        return None, ["expected an object {name, doc?, inputs, outputs, open?, submits?}"]
    errs = [f"unknown key {k!r}" for k in raw if k not in KEYS]
    name = raw.get("name")
    if not isinstance(name, str) or not NAME_RE.match(name):
        errs.append(f"name must be dotted lowercase like 'git.head', got {name!r}")
    elif check_dir and fn_dir.name != name:
        errs.append(f"name {name} does not match its directory {fn_dir.name}")
    if not isinstance(raw.get("doc", ""), str):
        errs.append("doc must be a string")
    if not isinstance(raw.get("open", False), bool):
        errs.append("open must be a boolean")
    ports: dict[str, dict[str, T.Type]] = {}
    for key in ("inputs", "outputs"):
        spec = raw.get(key)
        if not isinstance(spec, dict):
            errs.append(f"{key} is required, an object of name -> type")
            continue
        ports[key] = {}
        for port, form in spec.items():
            try:
                ports[key][port] = T.parse(form, f"{key}.{port}")
            except T.TypeSyntaxError as e:
                errs.append(str(e))
    submits, submit_docs = _submits(raw, ports.get("outputs", {}), errs)
    if check_dir and not (scope == "builtin" and name in NATIVE) \
            and not (fn_dir / "main.py").is_file():
        errs.append("main.py is missing")
    if errs:
        return None, errs
    return Fn(name, raw.get("doc", ""), ports["inputs"], ports["outputs"], raw, fn_dir,
              scope, raw.get("open", False), submits, submit_docs), []


def _submits(raw: dict[str, Any], outputs: dict[str, T.Type],
             errs: list[str]) -> tuple[dict[str, T.Type], dict[str, str]]:
    """An open fn's `submits`: {name: type or {"type", "doc"}}, outputs its agent submits
    with step_submit, as if every step running it declared them (SPEC §5)."""
    spec = raw.get("submits")
    if spec is None:
        return {}, {}
    if not isinstance(spec, dict):
        errs.append("submits must be an object of name -> type")
        return {}, {}
    if raw.get("open") is not True:
        errs.append("submits needs open: true (an open fn's agent submits outputs)")
    types, docs = {}, {}
    for port, form in spec.items():
        if port in outputs:
            errs.append(f"submits.{port}: already an output of the fn")
            continue
        t, doc = T.parse_decl(form, f"submits.{port}", errs)
        if t is not None:
            types[port] = t
        if doc:
            docs[port] = doc
    return types, docs


def fingerprint(dirs: Iterable[Path]) -> tuple:
    """What a scan depends on: every fn.json and main.py with its mtime and size."""
    out = []
    for d in dirs:
        out.append((str(d), d.is_dir()))
        for f in sorted([*d.glob("*/fn.json"), *d.glob("*/main.py")]):
            try:
                st = f.stat()
            except OSError:
                continue
            out.append((str(f), st.st_mtime_ns, st.st_size))
    return tuple(out)


def scan(scope: str, dirs: Iterable[Path], show: Show,
         missing_ok: Iterable[Path] = ()) -> tuple[list[Entry], list[dict[str, str]]]:
    """Every fn dir (an immediate subdirectory holding fn.json) of a scope, in order.

    Returns (entries, problems of the dirs themselves, e.g. a configured dir that is missing).
    """
    entries: list[Entry] = []
    problems: list[dict[str, str]] = []
    optional = set(missing_ok)
    for d in dict.fromkeys(dirs):
        if not d.is_dir():
            if d not in optional:
                problems.append({"where": show(d), "message": "fn directory does not exist"})
            continue
        for fn_json in sorted(d.glob("*/fn.json")):
            try:
                raw = json.loads(fn_json.read_text(encoding="utf-8"))
            except (OSError, UnicodeDecodeError, json.JSONDecodeError) as e:
                entries.append(Entry(fn_json.parent.name, scope, fn_json.parent, None,
                                     [f"bad JSON: {e}"]))
                continue
            fn, errs = parse_fn(raw, fn_json.parent, scope)
            name = raw.get("name") if isinstance(raw, dict) else None
            entries.append(Entry(name if isinstance(name, str) else fn_json.parent.name, scope,
                                 fn_json.parent, fn, errs))
    return entries, problems


class Registry:
    """The functions one project (or the global context) sees, in lookup order."""

    def __init__(self, entries: list[Entry], dir_problems: list[dict[str, str]], show: Show,
                 key: Any = None):
        self.entries = [replace(e, errors=list(e.errors)) for e in entries]  # scans are cached
        self.key = key
        self.fns: dict[str, Fn] = {}
        self.problems = list(dir_problems)
        # Only the project's own fns can block it (SPEC §2); a broken or colliding global/built-in
        # fn is just left out, and a plan that uses it fails validation on that step.
        self.blocking: list[dict[str, str]] = []
        for e in self.entries:
            if e.fn is not None and not e.errors:
                other = self.fns.get(e.name)
                if other is None:
                    self.fns[e.name] = e.fn
                    continue
                e.errors.append(f"fn {e.name} collides with the {other.scope} fn at "
                                f"{show(other.dir)}")
            found = [{"where": show(e.dir / "fn.json"), "message": m} for m in e.errors]
            self.problems.extend(found)
            if e.scope == "project":
                self.blocking.extend(found)

    def get(self, name: str) -> Fn | None:
        return self.fns.get(name)

    def names(self) -> list[str]:
        return sorted(self.fns)

    def listing(self) -> list[dict[str, Any]]:
        """fn_list: every fn dir in lookup order; ones with problems carry `error`."""
        return [e.summary() for e in self.entries]


def load(scopes: dict[str, list[Path]], show: Show = str) -> Registry:
    """A registry from explicit scope dirs, e.g. {"builtin": [BUILTIN_DIR], "global": [...]}."""
    entries: list[Entry] = []
    problems: list[dict[str, str]] = []
    for scope in SCOPES:
        found, probs = scan(scope, [Path(d).resolve() for d in scopes.get(scope, [])], show)
        entries += found
        problems += probs
    return Registry(entries, problems, show)
