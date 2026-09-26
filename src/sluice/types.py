"""The structural type language of SPEC §3: parsing, `fits`, `check_value`, navigation."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

PRIMITIVES = ("string", "int", "float", "bool", "any")
SPECIAL = ("list", "map", "optional", "union", "record")


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
        return f"list<{self.of}>"


@dataclass(frozen=True)
class Map:
    of: Type

    def __str__(self) -> str:
        return f"map<{self.of}>"


@dataclass(frozen=True)
class Optional:
    of: Type

    def __str__(self) -> str:
        return f"{self.of}?"


@dataclass(frozen=True)
class Record:
    fields: tuple[tuple[str, Type], ...]

    def field(self, name: str) -> Type | None:
        for k, t in self.fields:
            if k == name:
                return t
        return None

    def __str__(self) -> str:
        parts = [f"{k}?" if isinstance(t, Optional) else k for k, t in self.fields]
        return "{" + ", ".join(parts) + "}"


@dataclass(frozen=True)
class Union:
    tags: tuple[tuple[str, Record], ...]

    def variant(self, tag: str) -> Record | None:
        for k, r in self.tags:
            if k == tag:
                return r
        return None

    def __str__(self) -> str:
        return "union<" + "|".join(k for k, _ in self.tags) + ">"


Type = Prim | Enum | List | Map | Optional | Record | Union

ANY = Prim("any")
STRING = Prim("string")


def _join(path: str, key: str) -> str:
    return f"{path}.{key}" if path else key


def parse(form: Any, path: str = "type") -> Type:
    """Parse the JSON form of a type. Raises TypeSyntaxError naming `path`."""
    if isinstance(form, str):
        base, opt = (form[:-1], True) if form.endswith("?") else (form, False)
        if base not in PRIMITIVES:
            raise TypeSyntaxError(f"{path}: unknown type {form!r}")
        return Optional(Prim(base)) if opt else Prim(base)
    if isinstance(form, list):
        if not form or not all(isinstance(v, str) for v in form):
            raise TypeSyntaxError(f"{path}: an enum is a non-empty list of strings")
        if len(set(form)) != len(form):
            raise TypeSyntaxError(f"{path}: enum values repeat")
        return Enum(tuple(form))
    if isinstance(form, dict):
        if len(form) == 1 and next(iter(form)) in SPECIAL:
            key, inner = next(iter(form.items()))
            sub = f"{path}.{key}"
            if key == "list":
                return List(parse(inner, sub))
            if key == "map":
                return Map(parse(inner, sub))
            if key == "optional":
                t = parse(inner, sub)
                return t if isinstance(t, Optional) else Optional(t)
            if key == "record":
                if not isinstance(inner, dict):
                    raise TypeSyntaxError(f"{sub}: expected an object of fields")
                return _record(inner, sub)
            if not isinstance(inner, dict) or not inner:
                raise TypeSyntaxError(f"{sub}: expected an object of tag -> record")
            tags = []
            for tag, rec in inner.items():
                t = parse(rec, f"{sub}.{tag}")
                if not isinstance(t, Record):
                    raise TypeSyntaxError(f"{sub}.{tag}: a union variant must be a record")
                tags.append((tag, t))
            return Union(tuple(tags))
        return _record(form, path)
    raise TypeSyntaxError(f"{path}: not a type: {form!r}")


def _record(fields: dict[str, Any], path: str) -> Record:
    return Record(tuple((k, parse(v, _join(path, k))) for k, v in fields.items()))


def record_of(ports: dict[str, Type]) -> Record:
    return Record(tuple(ports.items()))


# ---- fits -------------------------------------------------------------------------------


def fits(out: Type, inp: Type) -> tuple[bool, str]:
    """Can every value of `out` be used where `inp` is expected? Returns (ok, reason)."""
    reason = _fits(out, inp, "")
    return (reason is None, reason or "")


def _fits(o: Type, i: Type, p: str) -> str | None:
    if i == ANY or o == ANY:
        return None
    if isinstance(i, Optional):
        return _fits(o.of if isinstance(o, Optional) else o, i.of, p)
    if isinstance(o, Optional):
        return f"{p} is optional" if p else f"{o} is optional"
    where = f"{p}: " if p else ""
    if isinstance(i, Prim):
        if isinstance(o, Prim):
            if o.name == i.name or (o.name == "int" and i.name == "float"):
                return None
            return f"{where}{o} is not {i}"
        if isinstance(o, Enum) and i.name == "string":
            return None
        return f"{where}{o} is not {i}"
    if isinstance(i, Enum):
        if isinstance(o, Enum):
            extra = [v for v in o.values if v not in i.values]
            return f"{where}{', '.join(extra)} not in {i}" if extra else None
        return f"{where}{o} is not {i}"
    if isinstance(i, List) and isinstance(o, List):
        return _fits(o.of, i.of, f"{p}[]")
    if isinstance(i, Map) and isinstance(o, Map):
        return _fits(o.of, i.of, f"{p}.*" if p else "*")
    if isinstance(i, Record) and isinstance(o, Record):
        for name, it in i.fields:
            ot = o.field(name)
            fp = _join(p, name)
            if ot is None:
                if isinstance(it, Optional):
                    continue
                return f"{fp} is missing"
            r = _fits(ot, it, fp)
            if r:
                return r
        return None
    if isinstance(i, Union) and isinstance(o, Union):
        for tag, rec in o.tags:
            target = i.variant(tag)
            if target is None:
                return f"{where}tag {tag} is not accepted"
            r = _fits(rec, target, _join(p, tag))
            if r:
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
    if t == ANY:
        return
    if isinstance(t, Optional):
        if v is not None:
            _check(t.of, v, p, errs)
        return
    if isinstance(t, Prim):
        ok = {
            "string": isinstance(v, str),
            "int": isinstance(v, int) and not isinstance(v, bool),
            "float": isinstance(v, int | float) and not isinstance(v, bool),
            "bool": isinstance(v, bool),
        }[t.name]
        if not ok:
            errs.append(f"{where}expected {t.name}, got {_show(v)}")
        return
    if isinstance(t, Enum):
        if not (isinstance(v, str) and v in t.values):
            errs.append(f"{where}expected one of {t}, got {_show(v)}")
        return
    if isinstance(t, List):
        if not isinstance(v, list):
            errs.append(f"{where}expected a list, got {_show(v)}")
            return
        for n, item in enumerate(v):
            _check(t.of, item, f"{p}[{n}]", errs)
        return
    if isinstance(t, Map):
        if not isinstance(v, dict):
            errs.append(f"{where}expected an object, got {_show(v)}")
            return
        for k, item in v.items():
            _check(t.of, item, _join(p, k), errs)
        return
    if isinstance(t, Record):
        if not isinstance(v, dict):
            errs.append(f"{where}expected an object {t}, got {_show(v)}")
            return
        for name, ft in t.fields:
            if name not in v:
                if not isinstance(ft, Optional):
                    errs.append(f"{_join(p, name)}: missing required field")
                continue
            _check(ft, v[name], _join(p, name), errs)
        return
    if isinstance(t, Union):
        if not isinstance(v, dict):
            errs.append(f"{where}expected an object with a kind, got {_show(v)}")
            return
        kind = v.get("kind")
        rec = t.variant(kind) if isinstance(kind, str) else None
        if rec is None:
            tags = ", ".join(k for k, _ in t.tags)
            errs.append(f"{_join(p, 'kind')}: expected one of [{tags}], got {_show(kind)}")
            return
        _check(rec, v, p, errs)


# ---- navigation -------------------------------------------------------------------------


def navigate(t: Type, fields: tuple[str, ...] | list[str]) -> tuple[Type | None, str]:
    """The type of `value.f1.f2...` for a value of type `t`. Returns (type, error)."""
    optional = False
    for f in fields:
        while isinstance(t, Optional):
            optional, t = True, t.of
        if t == ANY:
            return ANY, ""
        if isinstance(t, Record):
            ft = t.field(f)
            if ft is None:
                return None, f"no field {f} in {t}"
            t = ft
        elif isinstance(t, Map):
            optional, t = True, t.of
        elif isinstance(t, Union) and f == "kind":
            t = Enum(tuple(k for k, _ in t.tags))
        else:
            return None, f"cannot read field {f} of {t}"
    if optional and not isinstance(t, Optional) and t != ANY:
        t = Optional(t)
    return t, ""


def navigate_value(value: Any, fields: tuple[str, ...] | list[str]) -> Any:
    """Runtime counterpart of `navigate`: missing fields and nulls yield None."""
    for f in fields:
        if not isinstance(value, dict):
            return None
        value = value.get(f)
    return value
