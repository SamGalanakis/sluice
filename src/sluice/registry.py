"""The fn registry (SPEC §2, §4): fn dirs from the package's fns/ plus config.fn_dirs."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from . import types as T

BUILTIN_DIR = Path(__file__).resolve().parent / "fns"
NATIVE = {"core.echo", "core.collect", "core.format"}  # run inline; no main.py
NAME_RE = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$")
KEYS = {"name", "doc", "inputs", "outputs"}


class RegistryError(Exception):
    def __init__(self, errors: list[str]):
        super().__init__("; ".join(errors))
        self.errors = errors


@dataclass
class Fn:
    name: str
    doc: str
    inputs: dict[str, T.Type]
    outputs: dict[str, T.Type]
    raw: dict[str, Any]
    dir: Path

    @property
    def native(self) -> bool:
        return self.name in NATIVE

    def summary(self) -> dict[str, Any]:
        return {"name": self.name, "doc": self.doc, "inputs": self.raw["inputs"],
                "outputs": self.raw["outputs"]}


def parse_fn(raw: Any, fn_dir: Path) -> Fn:
    """Validate one fn.json. Raises RegistryError with every problem found."""
    where = str(fn_dir / "fn.json")
    if not isinstance(raw, dict):
        raise RegistryError([f"{where}: expected an object"])
    errs = [f"{where}: unknown key {k!r}" for k in raw if k not in KEYS]
    name = raw.get("name")
    if not isinstance(name, str) or not NAME_RE.match(name):
        errs.append(f"{where}: name must be dotted lowercase like 'git.head', got {name!r}")
    if not isinstance(raw.get("doc", ""), str):
        errs.append(f"{where}: doc must be a string")
    ports: dict[str, dict[str, T.Type]] = {}
    for key in ("inputs", "outputs"):
        spec = raw.get(key)
        if not isinstance(spec, dict):
            errs.append(f"{where}: {key} is required, an object of name -> type")
            continue
        ports[key] = {}
        for port, form in spec.items():
            try:
                ports[key][port] = T.parse(form, f"{key}.{port}")
            except T.TypeSyntaxError as e:
                errs.append(f"{where}: {e}")
    if name not in NATIVE and not (fn_dir / "main.py").is_file():
        errs.append(f"{where}: main.py is missing")
    if errs:
        raise RegistryError(errs)
    return Fn(name, raw.get("doc", ""), ports["inputs"], ports["outputs"], raw, fn_dir)


class Registry:
    def __init__(self, fns: dict[str, Fn]):
        self.fns = fns

    def get(self, name: str) -> Fn | None:
        return self.fns.get(name)

    def names(self) -> list[str]:
        return sorted(self.fns)

    @classmethod
    def load(cls, dirs: list[str | Path]) -> Registry:
        """Load every immediate subdirectory holding fn.json, in each dir. Collects errors."""
        fns: dict[str, Fn] = {}
        errs: list[str] = []
        for d in dict.fromkeys(Path(x).resolve() for x in dirs):
            if not d.is_dir():
                errs.append(f"{d}: fn directory does not exist")
                continue
            for fn_json in sorted(d.glob("*/fn.json")):
                try:
                    fn = parse_fn(json.loads(fn_json.read_text(encoding="utf-8")),
                                  fn_json.parent)
                except json.JSONDecodeError as e:
                    errs.append(f"{fn_json}: bad JSON: {e}")
                    continue
                except RegistryError as e:
                    errs.extend(e.errors)
                    continue
                if fn.name in fns:
                    errs.append(f"{fn_json}: duplicate fn name {fn.name} "
                                f"(also in {fns[fn.name].dir})")
                    continue
                fns[fn.name] = fn
        if errs:
            raise RegistryError(errs)
        return cls(fns)
