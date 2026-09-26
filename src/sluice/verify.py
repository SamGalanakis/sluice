"""verify (SPEC §6a): check functions, projects, plans and state; report every problem found
with where it is. Changes nothing."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from . import plan as P
from . import types as T
from .errors import NotFound
from .store import PROJECT_KEYS, Store
from .util import parse_dotenv

STATUSES = {"pending", "running", "succeeded", "failed"}


class Report:
    def __init__(self) -> None:
        self.problems: list[dict[str, str]] = []

    def add(self, where: str, message: str) -> None:
        p = {"where": where, "message": message}
        if p not in self.problems:
            self.problems.append(p)

    def add_path_errors(self, file: str, errors: list[str]) -> None:
        """Errors that read `<path>: <message>` become `<file>#<path>`."""
        for e in errors:
            path, sep, msg = e.partition(": ")
            self.add(f"{file}#{path}" if sep else file, msg if sep else e)


def verify(store: Store, project: str | None = None) -> dict[str, Any]:
    r = Report()
    for p in store.registry(None).problems:  # built-in and global fns
        r.add(p["where"], p["message"])
    _check_env(store, r, store.home / ".env")
    if project is None:
        root = store.home / "projects"
        names = sorted(d.name for d in root.iterdir() if d.is_dir()) if root.is_dir() else []
    else:
        if not store.project_dir(project).is_dir():
            raise NotFound(f"no project {project!r}")
        names = [project]
    for name in names:
        _check_project(store, r, name)
    return {"ok": not r.problems, "problems": r.problems}


def _read(r: Report, store: Store, path: Path, required: bool = True) -> tuple[bool, Any]:
    """(True, the JSON) or (False, None) after reporting why it is missing or unreadable."""
    try:
        return True, json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        if required:
            r.add(store.show(path), "missing")
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as e:
        r.add(store.show(path), f"not readable JSON: {e}")
    return False, None


def _check_env(store: Store, r: Report, path: Path) -> None:
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return
    except (OSError, UnicodeDecodeError) as e:
        r.add(store.show(path), f"not readable: {e}")
        return
    for n in parse_dotenv(text)[1]:
        r.add(f"{store.show(path)}:{n}", "not a KEY=value line")


def _check_project(store: Store, r: Report, name: str) -> None:
    d = store.home / "projects" / name
    show = store.show(d)
    if not P.ID_RE.match(name):
        r.add(show, f"not a project: names match {P.ID_RE.pattern}")
        return
    found, info = _read(r, store, d / "project.json")
    if not found:
        return
    if not isinstance(info, dict):
        r.add(f"{show}/project.json", "expected an object {name, description}")
    else:
        for k in info:
            if k not in PROJECT_KEYS:
                r.add(f"{show}/project.json#{k}", "unknown key")
        if info.get("name") != name:
            r.add(f"{show}/project.json#name",
                  f"expected {name!r} (the directory name), got {info.get('name')!r}")
        if not isinstance(info.get("description", ""), str):
            r.add(f"{show}/project.json#description", "expected a string")
    _check_env(store, r, d / ".env")

    reg = store.registry(name)
    shared = store.registry(None).problems
    for p in reg.problems:
        if p not in shared:
            r.add(p["where"], p["message"])

    found, doc = _read(r, store, d / "plan.json")
    if not found:
        return
    body = doc
    if isinstance(doc, dict):
        if not isinstance(doc.get("rev"), int):
            r.add(f"{show}/plan.json#rev", "expected an int")
        body = {k: v for k, v in doc.items() if k != "rev"}
    errs, plan = P.validate(body, reg)
    r.add_path_errors(f"{show}/plan.json", errs)

    found, state = _read(r, store, d / "state.json", required=False)
    if not found:
        return
    where = f"{show}/state.json"
    if not isinstance(state, dict) or not isinstance(state.get("inputs"), dict) \
            or not isinstance(state.get("steps"), dict):
        r.add(where, "expected an object {inputs, steps}")
        return
    declared = body.get("inputs") if isinstance(body, dict) else None
    declared = declared if isinstance(declared, dict) else {}
    for n, v in state["inputs"].items():
        if n not in declared:
            r.add(f"{where}#inputs.{n}", f"a value for {n}, which the plan does not declare")
        elif n in plan.inputs:
            r.add_path_errors(where, T.check_value(plan.inputs[n], v, f"inputs.{n}"))
    steps = body.get("steps") if isinstance(body, dict) else None
    steps = steps if isinstance(steps, dict) else {}
    for sid, e in state["steps"].items():
        at = f"{where}#steps.{sid}"
        if sid not in steps:
            r.add(at, f"state for step {sid}, which is not in the plan")
            continue
        if not isinstance(e, dict) or e.get("status") not in STATUSES:
            r.add(at, f"status must be one of {sorted(STATUSES)}")
            continue
        step = plan.steps.get(sid)
        if e["status"] == "succeeded" and step is not None:
            types = {k: step.output_type(k) for k in step.fn.outputs}
            r.add_path_errors(where, T.check_value(T.record_of(types), e.get("outputs"),
                                                   f"steps.{sid}.outputs"))
