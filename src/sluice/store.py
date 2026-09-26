"""The workspace on disk (SPEC §2, §5, §6): config, function scopes, projects with their plan,
edit log and state, and the edits made by hand (manual values)."""

from __future__ import annotations

import contextlib
import copy
import fcntl
import json
import os
import threading
from collections import Counter
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import jsonpatch
import jsonpointer

from . import plan as P
from . import registry as R
from . import types as T
from .errors import BadRequest, Conflict, InvalidPlan, NotFound
from .util import append_line, atomic_write_json, atomic_write_text, now_iso, read_json

DEFAULT_CONFIG: dict[str, Any] = {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420},
                                  "max_parallel": 8}
PROJECT_KEYS = {"name", "description"}


def default_home() -> Path:
    return Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")


class Store:
    """All reads and writes of a SLUICE_HOME. Safe across threads and processes (flock)."""

    def __init__(self, home: Path | str | None = None):
        self.home = Path(home) if home is not None else default_home()
        self.config = copy.deepcopy(DEFAULT_CONFIG)
        if (self.home / "config.json").exists():
            self.config.update(read_json(self.home / "config.json"))
        self.listeners: list[Callable[[], None]] = []  # called after every accepted edit
        self._held = threading.local()
        self._parsed: dict[str, tuple[Any, P.Plan]] = {}
        self._scans: dict[tuple, tuple[tuple, tuple[list[R.Entry], list]]] = {}
        self._scan_lock = threading.Lock()

    # ---- paths ----

    def show(self, path: Path) -> str:
        """A path for messages: relative to SLUICE_HOME when inside it."""
        try:
            return str(Path(path).resolve().relative_to(self.home.resolve()))
        except ValueError:
            return str(path)

    def project_dir(self, name: str) -> Path:
        if not isinstance(name, str) or not P.ID_RE.match(name):
            raise NotFound(f"no project {name!r} (project names match {P.ID_RE.pattern})")
        return self.home / "projects" / name

    def runs_dir(self, project: str) -> Path:
        return self.project_dir(project) / "runs"

    def calls_dir(self, project: str | None) -> Path:
        return (self.project_dir(project) if project else self.home) / "calls"

    def global_fn_dirs(self) -> list[Path]:
        return [self.home / "fns", *(self.home / d for d in self.config["fn_dirs"])]

    @contextlib.contextmanager
    def lock(self, project: str) -> Iterator[None]:
        """Exclusive flock on the project's .lock; re-entrant within a thread."""
        held: set[str] = self._held.__dict__.setdefault("projects", set())
        if project in held:
            yield
            return
        d = self.project_dir(project)
        d.mkdir(parents=True, exist_ok=True)
        fd = os.open(d / ".lock", os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            held.add(project)
            try:
                yield
            finally:
                held.discard(project)
        finally:
            os.close(fd)  # closing the descriptor releases the flock

    # ---- functions (SPEC §2 scopes) ----

    def _scan(self, scope: str, dirs: list[Path], missing_ok: list[Path]) -> tuple:
        """Scan a scope's dirs, cached until a fn.json or main.py in them changes."""
        dirs = [d.resolve() for d in dirs]
        missing_ok = [d.resolve() for d in missing_ok]
        key = (scope, tuple(dirs))
        fp = R.fingerprint(dirs)
        with self._scan_lock:
            hit = self._scans.get(key)
            if hit is None or hit[0] != fp:
                hit = self._scans[key] = (fp, R.scan(scope, dirs, self.show, missing_ok))
        return fp, hit[1]

    def registry(self, project: str | None = None) -> R.Registry:
        """What a project (or, without one, the global context) sees: built-in, global and
        project functions in lookup order, with every problem found. Never raises for them."""
        scopes = [("builtin", [R.BUILTIN_DIR], []),
                  ("global", self.global_fn_dirs(), [self.home / "fns"])]
        if project is not None:
            self.project(project)
            fns = self.project_dir(project) / "fns"
            scopes.append(("project", [fns], [fns]))
        entries: list[R.Entry] = []
        problems: list[dict[str, str]] = []
        key = []
        for scope, dirs, missing_ok in scopes:
            fp, (found, probs) = self._scan(scope, dirs, missing_ok)
            key.append(fp)
            entries += found
            problems += probs
        return R.Registry(entries, problems, self.show, key=tuple(key))

    def usable_registry(self, project: str | None = None) -> R.Registry:
        """The registry, refusing when any fn it covers has a problem (SPEC §2): plan edits,
        manual values, fn_call and runs wait until verify is clean for these scopes."""
        reg = self.registry(project)
        if reg.blocking:
            who = f"project {project}" if project else "the global functions"
            raise InvalidPlan([f"{p['where']}: {p['message']}" for p in reg.blocking],
                              f"{who}: function problems block edits and runs until fixed "
                              f"(see verify)")
        return reg

    def fn(self, name: str, project: str | None = None) -> R.Fn:
        fn = self.registry(project).get(name)
        if fn is None:
            raise NotFound(f"no fn {name!r}" + (f" in project {project}" if project else ""))
        return fn

    def fn_save(self, raw: Any, main_py: Any, project: str | None = None) -> dict[str, str]:
        """Validate a fn.json and write it with main.py into the project's (or the global)
        fns/<name>/. Refuses a name that collides with another scope (SPEC §2)."""
        scope = "project" if project else "global"
        fn, errs = R.parse_fn(raw, Path(), scope, check_dir=False)
        if not isinstance(main_py, str) or not main_py.strip():
            errs.append("main_py: the fn's Python source is required")
        if fn is None or errs:
            raise InvalidPlan(errs, "not a valid fn")
        name = fn.name
        if project is not None:
            self.project(project)
        root = (self.project_dir(project) if project else self.home) / "fns"
        target = root / name
        clash = []
        if project is not None:
            other = self.registry(None).get(name)  # built-in or global
            if other is not None:
                clash.append(f"the {other.scope} fn at {self.show(other.dir)}")
        else:
            for e in self.registry(None).entries:
                if e.name == name and e.dir.resolve() != target.resolve():
                    clash.append(f"the {e.scope} fn at {self.show(e.dir)}")
            for p in self.project_names():
                if (self.project_dir(p) / "fns" / name / "fn.json").exists():
                    clash.append(f"the fn of project {p}")
        if clash:
            raise BadRequest(f"fn {name} would collide with {', '.join(clash)}")
        target.mkdir(parents=True, exist_ok=True)
        atomic_write_text(target / "main.py", main_py)
        atomic_write_json(target / "fn.json", raw)
        return {"scope": scope, "path": str(target)}

    # ---- projects ----

    def project_names(self) -> list[str]:
        root = self.home / "projects"
        if not root.is_dir():
            return []
        return sorted(p.name for p in root.iterdir()
                      if P.ID_RE.match(p.name) and (p / "project.json").is_file())

    def project(self, name: str) -> dict[str, Any]:
        path = self.project_dir(name) / "project.json"
        if not path.is_file():
            raise NotFound(f"no project {name!r}")
        return read_json(path)

    def create_project(self, name: str, description: str = "", author: str = "",
                       reason: str = "") -> dict[str, str]:
        """A project with the empty plan at rev 1 (SPEC §5)."""
        if not isinstance(name, str) or not P.ID_RE.match(name):
            raise BadRequest(f"project names match {P.ID_RE.pattern}, got {name!r}")
        if not isinstance(description, str):
            raise BadRequest("description: expected a string")
        d = self.project_dir(name)
        with self.lock(name):
            if (d / "project.json").exists():
                raise BadRequest(f"project {name!r} already exists")
            doc = copy.deepcopy(P.EMPTY)
            (d / "plan.log.jsonl").unlink(missing_ok=True)
            self._log(name, 1, author, reason or "project created",
                      [{"op": "add", "path": "", "value": doc}])
            atomic_write_json(d / "plan.json", {**doc, "rev": 1})
            atomic_write_json(d / "project.json", {"name": name, "description": description})
        self.notify()
        return {"name": name}

    def update_project(self, name: str, description: str) -> dict[str, str]:
        if not isinstance(description, str):
            raise BadRequest("description: expected a string")
        with self.lock(name):
            cur = self.project(name)
            atomic_write_json(self.project_dir(name) / "project.json",
                              {**cur, "description": description})
        return {"name": name}

    def projects(self) -> list[dict[str, Any]]:
        out = []
        for name in self.project_names():
            info, doc = self.project(name), self.get(name)
            st = self.read_state(name)["steps"]
            counts = Counter(st.get(s, {"status": "pending"})["status"] for s in doc["steps"])
            out.append({"name": name, "description": info.get("description", ""),
                        "rev": doc["rev"], "counts": dict(counts)})
        return out

    # ---- the plan ----

    def get(self, project: str) -> dict[str, Any]:
        """The project's current plan, including `rev`."""
        self.project(project)
        return read_json(self.project_dir(project) / "plan.json")

    def plan(self, project: str) -> tuple[dict[str, Any], P.Plan]:
        """The current document and its parsed plan (cached per rev and fn set)."""
        doc = self.get(project)
        reg = self.registry(project)
        key = (doc["rev"], reg.key)
        hit = self._parsed.get(project)
        if hit is None or hit[0] != key:
            errs, plan = P.validate(_body(doc), reg)
            if errs:
                raise InvalidPlan(errs, f"the plan of project {project} no longer validates")
            hit = self._parsed[project] = (key, plan)
        return doc, hit[1]

    def patch(self, project: str, rev: int, ops: Any, author: str, reason: str) -> int:
        """Apply an RFC 6902 patch at `rev`. Raises Conflict, InvalidPlan or NotFound."""
        with self.lock(project):
            cur = self.get(project)
            if rev != cur["rev"]:
                raise Conflict(cur["rev"])
            reg = self.usable_registry(project)
            old = _body(cur)
            new = apply_ops(old, ops)
            errs, _ = P.validate(new, reg)
            new_steps = new.get("steps") if isinstance(new.get("steps"), dict) else {}
            for sid, e in self.read_state(project)["steps"].items():
                if e["status"] != "running":
                    continue
                if sid not in new_steps:
                    errs.append(f"steps.{sid}: cannot remove a running step")
                elif new_steps[sid] != old["steps"].get(sid):
                    errs.append(f"steps.{sid}: cannot change a running step")
            if errs:
                raise InvalidPlan(errs)
            self._log(project, rev + 1, author, reason, ops)
            atomic_write_json(self.project_dir(project) / "plan.json", {**new, "rev": rev + 1})
        self.notify()
        return rev + 1

    def _log(self, project: str, rev: int, author: str, reason: str, ops: list | None = None,
             **manual: Any) -> None:
        """One log line: an edit (`ops`), or a manual value (`action` and its args, no ops)."""
        entry = {"rev": rev, "at": now_iso(), "author": author, "reason": reason}
        entry.update({"ops": ops} if ops is not None else manual)
        append_line(self.project_dir(project) / "plan.log.jsonl", entry)

    def notify(self) -> None:
        for fn in list(self.listeners):
            fn()

    def history(self, project: str, since_rev: int | None = None) -> list[dict[str, Any]]:
        self.project(project)
        path = self.project_dir(project) / "plan.log.jsonl"
        entries = [json.loads(x) for x in path.read_text(encoding="utf-8").splitlines()
                   if x.strip()]
        return [e for e in entries if since_rev is None or e["rev"] > since_rev]

    # ---- state ----

    def read_state(self, project: str) -> dict[str, Any]:
        path = self.project_dir(project) / "state.json"
        return read_json(path) if path.exists() else {"inputs": {}, "steps": {}}

    def write_state(self, project: str, state: dict[str, Any]) -> None:
        """Callers hold the project lock."""
        atomic_write_json(self.project_dir(project) / "state.json", state)

    def status(self, project: str) -> dict[str, Any]:
        doc, plan = self.plan(project)
        state = self.read_state(project)
        outputs = {}
        for name, ref in plan.outputs.items():
            ok, v = P.value_of(ref, plan, state)
            outputs[name] = v if ok else None
        steps = []
        for sid, step in plan.steps.items():
            e = state["steps"].get(sid, {"status": "pending"})
            row = {"id": sid, "run": step.fn.name, "status": e["status"],
                   "started": e.get("started"), "finished": e.get("finished")}
            row.update({k: e[k] for k in ("outputs", "error") if e.get(k) is not None})
            steps.append({**row, "manual": bool(e.get("manual"))})
        return {"rev": doc["rev"], "inputs": {n: state["inputs"].get(n) for n in plan.inputs},
                "outputs": outputs, "steps": steps}

    # ---- manual values (SPEC §6) ----

    def _plan_for_write(self, project: str) -> tuple[dict[str, Any], P.Plan]:
        self.usable_registry(project)
        return self.plan(project)

    def set_input(self, project: str, name: str, value: Any, author: str, reason: str) -> None:
        with self.lock(project):
            doc, plan = self._plan_for_write(project)
            if name not in plan.inputs:
                raise NotFound(f"the plan of project {project} has no input {name!r}")
            errs = T.check_value(plan.inputs[name], value, f"inputs.{name}")
            if errs:
                raise InvalidPlan(errs)
            state = self.read_state(project)
            state["inputs"][name] = value
            self.write_state(project, state)
            self._log(project, doc["rev"], author, reason, action="plan_set_input", input=name,
                      value=value)
        self.notify()

    def set_step_input(self, project: str, step: str, name: str, value: Any, author: str,
                       reason: str, rev: int | None = None) -> int:
        with self.lock(project):
            cur = self.get(project)
            if step not in cur["steps"]:
                raise NotFound(f"the plan of project {project} has no step {step!r}")
            op = {"op": "add", "path": f"/steps/{step}/in/{name}", "value": {"default": value}}
            ops = [{"op": "add", "path": f"/steps/{step}/in", "value": {}}, op] \
                if "in" not in cur["steps"][step] else [op]
            return self.patch(project, cur["rev"] if rev is None else rev, ops, author, reason)

    def set_output(self, project: str, step: str, outputs: Any, author: str,
                   reason: str) -> None:
        with self.lock(project):
            doc, plan = self._plan_for_write(project)
            if step not in plan.steps:
                raise NotFound(f"the plan of project {project} has no step {step!r}")
            s = plan.steps[step]
            types = {k: s.output_type(k) for k in s.fn.outputs}
            errs = T.check_value(T.record_of(types), outputs, "outputs")
            if isinstance(outputs, dict):
                errs += [f"outputs.{k}: fn {s.fn.name} has no output {k}"
                         for k in outputs if k not in types]
            if errs:
                raise InvalidPlan(errs)
            state = self.read_state(project)
            if state["steps"].get(step, {}).get("status") == "running":
                raise BadRequest(f"step {step} is running")
            state["steps"][step] = {"status": "succeeded", "started": None,
                                    "finished": now_iso(), "outputs": outputs, "manual": True}
            self.write_state(project, state)
            self._log(project, doc["rev"], author, reason, action="step_set_output", step=step,
                      outputs=outputs)
        self.notify()

    def retry(self, project: str, step: str, author: str, reason: str) -> None:
        """step_retry: a failed (or manually set) step goes back to pending."""
        with self.lock(project):
            doc, plan = self._plan_for_write(project)
            if step not in plan.steps:
                raise NotFound(f"the plan of project {project} has no step {step!r}")
            state = self.read_state(project)
            e = state["steps"].get(step, {"status": "pending"})
            if e["status"] != "failed" and not e.get("manual"):
                raise BadRequest(f"step {step} is {e['status']}; only a failed or manually set "
                                 "step can be retried")
            state["steps"][step] = {"status": "pending"}
            self.write_state(project, state)
            self._log(project, doc["rev"], author, reason, action="step_retry", step=step)
        self.notify()


def _body(doc: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in doc.items() if k != "rev"}


def apply_ops(doc: dict[str, Any], ops: Any) -> dict[str, Any]:
    """Apply a JSON Patch, turning every failure into InvalidPlan with the op index."""
    if not isinstance(ops, list) or not all(isinstance(o, dict) for o in ops):
        raise InvalidPlan(["ops: expected a list of JSON Patch operations"])
    cur: Any = copy.deepcopy(doc)
    for i, op in enumerate(ops):
        try:
            cur = jsonpatch.JsonPatch([op]).apply(cur)
        except (jsonpatch.JsonPatchException, jsonpointer.JsonPointerException, TypeError,
                KeyError) as e:
            raise InvalidPlan([f"ops[{i}]: {e}"]) from e
    if not isinstance(cur, dict):
        raise InvalidPlan(["plan: expected an object"])
    return cur
