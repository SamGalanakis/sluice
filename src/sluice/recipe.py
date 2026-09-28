"""Recipes (SPEC §5): a named step shape, `recipes/<name>.json` in a scope dir, expanded into a
unit of steps by `unit_add`. Substitution is deliberately tiny: `{param}` in step ids and in
every string of the steps, `{{`/`}}` for literal braces, nothing else."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from . import types as T
from .plan import ID_RE

KEYS = {"name", "doc", "params", "steps"}
UNIT = "unit"  # the implicit param: the unit's name, a valid step id
# `{{`, `}}`, a `{placeholder}`, or a brace left alone
TOKEN = re.compile(r"\{\{|\}\}|\{([^{}]*)\}|[{}]")


@dataclass
class Recipe:
    name: str
    scope: str  # global (SLUICE_HOME/recipes) or project (projects/<p>/recipes)
    path: Path
    doc: str = ""
    params: dict[str, T.Type] = field(default_factory=dict)  # `unit` included
    raw: dict[str, Any] = field(default_factory=dict)
    errors: list[str] = field(default_factory=list)

    def summary(self) -> dict[str, Any]:
        """recipe_list's entry: {name, doc, params, scope}, or {name, scope, error}."""
        if self.errors:
            return {"name": self.name, "scope": self.scope, "error": "; ".join(self.errors)}
        params = self.raw.get("params") or {}
        return {"name": self.name, "doc": self.doc,
                "params": {UNIT: "string", **params}, "scope": self.scope}


def _placeholders(value: Any, where: str, found: list[tuple[str, str]]) -> None:
    """Every placeholder in the strings (keys too) of `value`, as (name, where)."""
    if isinstance(value, str):
        for m in TOKEN.finditer(value):
            if m.group(0) not in ("{{", "}}"):
                found.append((m.group(1) if m.group(1) is not None else m.group(0), where))
    elif isinstance(value, dict):
        for k, v in value.items():
            _placeholders(k, where, found)
            _placeholders(v, f"{where}.{k}", found)
    elif isinstance(value, list):
        for i, v in enumerate(value):
            _placeholders(v, f"{where}[{i}]", found)


def _unknown(found: list[tuple[str, str]], params: dict[str, Any]) -> list[str]:
    out = []
    for name, where in found:
        if name in ("{", "}"):
            out.append(f"{where}: a lone {name!r}; write {name * 2} for a literal brace")
        elif name not in params:
            out.append(f"{where}: unknown param {{{name}}}")
    return list(dict.fromkeys(out))


def load(path: Path, scope: str) -> Recipe:
    """One recipe file, with every problem found (a broken recipe is reported, never fatal)."""
    r = Recipe(path.stem, scope, path)
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as e:
        r.errors.append(f"bad JSON: {e}")
        return r
    if not isinstance(raw, dict):
        r.errors.append("expected an object {name, doc?, params?, steps}")
        return r
    r.raw = raw
    errs = r.errors
    errs += [f"{k}: unknown key" for k in raw if k not in KEYS]
    if raw.get("name") != path.stem:
        errs.append(f"name: must be the file's name, {path.stem!r}, got {raw.get('name')!r}")
    if not ID_RE.match(path.stem):
        errs.append(f"name: recipe names match {ID_RE.pattern}")
    r.doc = raw.get("doc", "")
    if not isinstance(r.doc, str):
        errs.append("doc: expected a string")
        r.doc = ""
    params = raw.get("params", {})
    if not isinstance(params, dict):
        errs.append("params: expected an object of name -> type")
        params = {}
    r.params[UNIT] = T.Prim("string")
    for name, form in params.items():
        if not ID_RE.match(name):
            errs.append(f"params.{name}: param names match {ID_RE.pattern}")
            continue
        t, _ = T.parse_decl(form, f"params.{name}", errs)
        if name == UNIT and t is not None and t != T.Prim("string"):
            errs.append(f"params.{UNIT}: the unit is always a string")
        elif t is not None:
            r.params[name] = t
    steps = raw.get("steps")
    if not isinstance(steps, dict) or not steps:
        errs.append("steps: required, an object of step id -> step")
    else:
        found: list[tuple[str, str]] = []
        _placeholders(steps, "steps", found)
        errs += _unknown(found, r.params)
    return r


def scan(dirs: list[tuple[str, Path]]) -> dict[str, Recipe]:
    """The recipes a project sees, by name: each scope's `*.json` in order, a later scope's
    recipe replacing an earlier one of the same name (the project's wins)."""
    out: dict[str, Recipe] = {}
    for scope, d in dirs:
        if d.is_dir():
            for path in sorted(d.glob("*.json")):
                out[path.stem] = load(path, scope)
    return out


def _text(value: Any) -> str:
    return value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)


def substitute(value: Any, params: dict[str, Any], where: str = "steps",
               errs: list[str] | None = None) -> Any:
    """`value` with `{param}` replaced in every string (object keys too): a string that is
    exactly one `{param}` becomes the value itself, typed; inside a longer string a value is
    its text (non-strings as JSON). `{{` and `}}` are literal braces. An unknown `{x}` or a
    lone brace is an error, appended to `errs` with where it is."""
    errs = [] if errs is None else errs
    if isinstance(value, str):
        m = TOKEN.fullmatch(value)
        if m and m.group(1) is not None and m.group(1) in params:
            return params[m.group(1)]
        found: list[tuple[str, str]] = []
        _placeholders(value, where, found)
        bad = _unknown(found, params)
        if bad:
            errs += bad
            return value
        return TOKEN.sub(lambda m: m.group(0)[0] if m.group(0) in ("{{", "}}")
                         else _text(params[m.group(1)]), value)
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            key = substitute(k, params, where, errs)
            key = _text(key)
            out[key] = substitute(v, params, f"{where}.{key}", errs)
        return out
    if isinstance(value, list):
        return [substitute(v, params, f"{where}[{i}]", errs) for i, v in enumerate(value)]
    return value


def expand(recipe: Recipe, params: Any) -> tuple[dict[str, Any], list[str]]:
    """The recipe's steps for these params (`unit` among them): (steps by id, every error).
    Params are checked against their types; an optional one left out is null."""
    if recipe.errors:
        return {}, [f"recipe {recipe.name} ({recipe.scope}): {e}" for e in recipe.errors]
    if not isinstance(params, dict):
        return {}, ["params: expected an object of param -> value"]
    errs = [f"params.{k}: recipe {recipe.name} has no param {k} (its params: "
            f"{', '.join(recipe.params)})" for k in params if k not in recipe.params]
    unit = params.get(UNIT)
    if not isinstance(unit, str) or not ID_RE.match(unit):
        errs.append(f"params.{UNIT}: the unit's name, a string matching {ID_RE.pattern}; "
                    f"got {unit!r}")
    values = {}
    for name, t in recipe.params.items():
        if name not in params and not isinstance(t, T.Optional):
            if name != UNIT:
                errs.append(f"params.{name}: required ({T.form(t)})")
            continue
        values[name] = params.get(name)
        if name != UNIT:
            errs += T.check_value(t, values[name], f"params.{name}")
    if errs:
        return {}, errs
    steps = substitute(recipe.raw["steps"], values, "steps", errs)
    return (steps if not errs else {}), errs
