"""verify (SPEC §6a): check functions, .env files, plans and state; report every problem found
with where it is, and leftover project directories as warnings. Changes nothing."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from . import plan as P
from . import state as S
from . import types as T
from .store import Store
from .util import parse_dotenv

STATUSES = S.STATUSES


class Report:
    def __init__(self) -> None:
        self.problems: list[dict[str, str]] = []
        self.warnings: list[dict[str, str]] = []

    def add(self, where: str, message: str) -> None:
        p = {"where": where, "message": message}
        if p not in self.problems:
            self.problems.append(p)

    def warn(self, where: str, message: str) -> None:
        self.warnings.append({"where": where, "message": message})

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
        names = store.project_names()
        root = store.home / "projects"
        for d in sorted(root.iterdir()) if root.is_dir() else []:
            if d.is_dir() and d.name not in names:
                r.warn(store.show(d), "a directory of no project: left over, or prepared "
                                      "(fns/, .env) for a project not created yet")
    else:
        store.project(project)
        names = [project]
    for name in names:
        _check_project(store, r, name)
    out: dict[str, Any] = {"ok": not r.problems, "problems": r.problems}
    if r.warnings:
        out["warnings"] = r.warnings
    return out


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
    _check_env(store, r, store.project_dir(name) / ".env")
    reg = store.registry(name)
    shared = store.registry(None).problems
    for p in reg.problems:
        if p not in shared:
            r.add(p["where"], p["message"])
    with store.rx():
        doc, state = store.get(name), store.read_state(name)
    body = {k: v for k, v in doc.items() if k != "rev"}
    errs, plan = P.validate(body, reg)
    r.add_path_errors(f"project {name}: plan", errs)
    where = f"project {name}: state"
    if not isinstance(state.get("inputs"), dict) or not isinstance(state.get("steps"), dict):
        r.add(where, "expected an object {inputs, steps}")
        return
    declared = body.get("inputs") if isinstance(body.get("inputs"), dict) else {}
    for n, v in state["inputs"].items():
        if n not in declared:
            r.add(f"{where}#inputs.{n}", f"a value for {n}, which the plan does not declare")
        elif n in plan.inputs:
            r.add_path_errors(where, T.check_value(plan.inputs[n], v, f"inputs.{n}"))
    steps = body.get("steps") if isinstance(body.get("steps"), dict) else {}
    for sid, e in state["steps"].items():
        at = f"{where}#steps.{sid}"
        if sid not in steps:
            r.add(at, f"state for step {sid}, which is not in the plan")
            continue
        if not isinstance(e, dict) or e.get("status") not in STATUSES:
            r.add(at, f"status must be one of {sorted(STATUSES)}")
            continue
        step = plan.steps.get(sid)
        if e["status"] in ("succeeded", "stale") and step is not None:
            types = {k: step.output_type(k) for k in step.outputs}
            r.add_path_errors(where, T.check_value(T.record_of(types), e.get("outputs"),
                                                   f"steps.{sid}.outputs"))
