"""The structural type language of SPEC §3: parsing, `fits`, `check_value`, navigation."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

PRIMITIVES = ("string", "int", "float", "boolean", "Any")


class TypeSyntaxError(ValueError):
    pass


@dataclass(frozen=True)
class Prim:
    name: str

    def __str__(self) -> str:
        return self.name


@dataclass(frozen=True)
class Enum:
    values: tuple[str, ...]

    def __str__(self) -> str:
        return "[" + ", ".join(self.values) + "]"


@dataclass(frozen=True)
class List:
    of: Type

    def __str__(self) -> str:
        return f"{self.of}[]"


@dataclass(frozen=True)
class Optional:
    of: Type

    def __str__(self) -> str:
        return f"{self.of}?"


@dataclass(frozen=True)
class Record:
    fields: tuple[tuple[str, Type], ...]

    def field(self, name: str) -> Type | None:
        return dict(self.fields).get(name)

    def __str__(self) -> str:
        return "{" + ", ".join(f"{k}?" if isinstance(t, Optional) else k
                               for k, t in self.fields) + "}"


Type = Prim | Enum | List | Optional | Record

ANY = Prim("Any")


def _join(path: str, key: str) -> str:
    return f"{path}.{key}" if path else key


def optional(t: Type) -> Type:
    return t if isinstance(t, Optional) else Optional(t)


def parse(form: Any, path: str = "type") -> Type:
    """Parse a type in its CWL spelling (SPEC §3). Raises TypeSyntaxError naming `path`."""
    if isinstance(form, str):
        if form.endswith("?"):
            return optional(parse(form[:-1], path))
        if form.endswith("[]"):
            return List(parse(form[:-2], path))
        if form not in PRIMITIVES:
            raise TypeSyntaxError(f"{path}: unknown type {form!r}")
        return Prim(form)
    if isinstance(form, list) and len(form) == 2 and "null" in form:
        return optional(parse(form[1] if form[0] == "null" else form[0], path))
    if isinstance(form, dict):
        kind = form.get("type")
        if kind == "array" and set(form) == {"type", "items"}:
            return List(parse(form["items"], f"{path}.items"))
        if kind == "enum" and set(form) == {"type", "symbols"}:
            sym = form["symbols"]
            if (not isinstance(sym, list) or not sym or len(set(sym)) != len(sym)
                    or not all(isinstance(v, str) for v in sym)):
                raise TypeSyntaxError(f"{path}.symbols: a non-empty list of distinct strings")
            return Enum(tuple(sym))
        if kind == "record" and set(form) == {"type", "fields"} and isinstance(form["fields"],
                                                                               dict):
            return record_of({k: parse(v, f"{path}.{k}") for k, v in form["fields"].items()})
    raise TypeSyntaxError(f"{path}: not a type: {json.dumps(form)}")


def record_of(fields: dict[str, Type]) -> Record:
    return Record(tuple(fields.items()))


def form(t: Type) -> Any:
    """A type in its CWL spelling again (the inverse of `parse`): a string where one exists."""
    if isinstance(t, Prim):
        return t.name
    if isinstance(t, Optional):
        inner = form(t.of)
        return f"{inner}?" if isinstance(inner, str) else ["null", inner]
    if isinstance(t, List):
        inner = form(t.of)
        return f"{inner}[]" if isinstance(inner, str) else {"type": "array", "items": inner}
    if isinstance(t, Enum):
        return {"type": "enum", "symbols": list(t.values)}
    return {"type": "record", "fields": {k: form(v) for k, v in t.fields}}


def parse_decl(form: Any, path: str, errs: list[str]) -> tuple[Type | None, str]:
    """A declaration (a plan input, a step's declared or an open fn's submitted output): a
    type, or `{"type": T, "doc": "..."}` (CWL's object form; no type form has only these
    keys). Returns (its type, its doc)."""
    doc = ""
    if isinstance(form, dict) and form.keys() <= {"type", "doc"}:
        if "type" not in form:
            errs.append(f"{path}.type: required")
            return None, ""
        doc = form.get("doc", "")
        if not isinstance(doc, str):
            errs.append(f"{path}.doc: expected a string")
            doc = ""
        form, path = form["type"], f"{path}.type"
    try:
        return parse(form, path), doc
    except TypeSyntaxError as e:
        errs.append(str(e))
        return None, doc


# ---- fits -------------------------------------------------------------------------------


def fits(out: Type, inp: Type) -> tuple[bool, str]:
    """Can every value of `out` be used where `inp` is expected? Returns (ok, reason)."""
    reason = _fits(out, inp, "")
    return reason is None, reason or ""


def _fits(o: Type, i: Type, p: str) -> str | None:
    if ANY in (o, i):
        return None
    if isinstance(i, Optional):
        return _fits(o.of if isinstance(o, Optional) else o, i.of, p)
    if isinstance(o, Optional):
        return f"{p} is optional" if p else f"{o} is optional"
    where = f"{p}: " if p else ""
    if isinstance(i, Prim) and isinstance(o, Prim):
        if o.name == i.name or (o.name, i.name) == ("int", "float"):
            return None
    elif isinstance(i, Prim) and isinstance(o, Enum) and i.name == "string":
        return None
    elif isinstance(i, Enum) and isinstance(o, Enum):
        extra = [v for v in o.values if v not in i.values]
        return f"{where}{', '.join(extra)} not in {i}" if extra else None
    elif isinstance(i, List) and isinstance(o, List):
        return _fits(o.of, i.of, f"{p}[]")
    elif isinstance(i, Record) and isinstance(o, Record):
        for name, it in i.fields:
            ot = o.field(name)
            if ot is None:
                if isinstance(it, Optional):
                    continue
                return f"{_join(p, name)} is missing"
            if r := _fits(ot, it, _join(p, name)):
                return r
        return None
    return f"{where}{o} is not {i}"


# ---- check_value ------------------------------------------------------------------------


def _show(value: Any) -> str:
    s = json.dumps(value, ensure_ascii=False)
    return s if len(s) <= 60 else s[:57] + "..."


def check_value(t: Type, value: Any, path: str = "") -> list[str]:
    """Validate a runtime JSON value. Returns path-bearing error messages (empty if ok)."""
    errs: list[str] = []
    _check(t, value, path, errs)
    return errs


def _check(t: Type, v: Any, p: str, errs: list[str]) -> None:
    where = f"{p}: " if p else ""
    if t == ANY or (isinstance(t, Optional) and v is None):
        return
    if isinstance(t, Optional):
        _check(t.of, v, p, errs)
    elif isinstance(t, Prim):
        ok = {"string": isinstance(v, str),
              "int": isinstance(v, int) and not isinstance(v, bool),
              "float": isinstance(v, int | float) and not isinstance(v, bool),
              "boolean": isinstance(v, bool)}[t.name]
        if not ok:
            errs.append(f"{where}expected {t.name}, got {_show(v)}")
    elif isinstance(t, Enum):
        if not (isinstance(v, str) and v in t.values):
            errs.append(f"{where}expected one of {t}, got {_show(v)}")
    elif isinstance(t, List):
        if not isinstance(v, list):
            errs.append(f"{where}expected an array, got {_show(v)}")
            return
        for n, item in enumerate(v):
            _check(t.of, item, f"{p}[{n}]", errs)
    elif not isinstance(v, dict):
        errs.append(f"{where}expected an object {t}, got {_show(v)}")
    else:
        for name, ft in t.fields:
            if name in v:
                _check(ft, v[name], _join(p, name), errs)
            elif not isinstance(ft, Optional):
                errs.append(f"{_join(p, name)}: missing required field")


# ---- navigation -------------------------------------------------------------------------


def _index(f: str) -> bool:
    """Whether a path segment selects a list item: isdecimal, exactly what int() accepts
    (isdigit also takes '²', which int() rejects)."""
    return f.isdecimal()


def navigate(t: Type, fields: tuple[str, ...] | list[str]) -> tuple[Type | None, str]:
    """The type of `value.f1.f2...` (a digit selects a list item). Returns (type, error)."""
    maybe = False
    for f in fields:
        while isinstance(t, Optional):
            maybe, t = True, t.of
        if t == ANY:
            return ANY, ""
        if isinstance(t, Record) and (ft := t.field(f)) is not None:
            t = ft
        elif isinstance(t, List) and _index(f):
            t = t.of  # an index past the end yields null, caught by the runtime input check
        else:
            return None, f"cannot read {f} of {t}"
    return (optional(t) if maybe else t), ""


def navigate_value(value: Any, fields: tuple[str, ...] | list[str]) -> Any:
    """Runtime counterpart of `navigate`: missing fields, bad indexes and nulls yield None."""
    for f in fields:
        if isinstance(value, dict):
            value = value.get(f)
        elif isinstance(value, list) and _index(f) and int(f) < len(value):
            value = value[int(f)]
        else:
            return None
    return value
