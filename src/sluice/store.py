"""The workspace on disk (SPEC §2, §5, §6): config, function scopes, projects with their plan,
edit log and state, and the edits made by hand (manual values)."""

from __future__ import annotations

import contextlib
import copy
import os
import shutil
import threading
from collections import Counter
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import jsonpatch
import jsonpointer

from . import inbox as I
from . import log as L
from . import plan as P
from . import registry as R
from . import types as T
from .errors import BadRequest, Conflict, InvalidPlan, NotFound, NotOpen
from .util import atomic_write_json, atomic_write_text, now_iso, read_json

DEFAULT_CONFIG: dict[str, Any] = {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420},
                                  "log_max": L.DEFAULT_MAX}
PROJECT_KEYS = {"name", "description"}
SUBMITTED = "submitted.json"  # in a run dir: the outputs its agent submitted (step_submit)
ANSWER_KEYS = {"action": str, "params": dict, "values": dict, "text": str}


def default_home() -> Path:
    return Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")



BRIEF = 200  # characters of a string value `status(brief=True)` keeps


def _brief(value: Any) -> Any:
    """A value with every string over BRIEF characters cut to its start and a note of how
    much more there is."""
    if isinstance(value, str) and len(value) > BRIEF:
        return f"{value[:BRIEF]}… [{len(value) - BRIEF} more characters]"
    if isinstance(value, dict):
        return {k: _brief(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_brief(v) for v in value]
    return value

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

    def log_dir(self, project: str | None) -> Path:
        """Where a log lives: the project's dir, or SLUICE_HOME for calls without a project."""
        return self.project_dir(project) if project else self.home

    def runs_dir(self, project: str | None) -> Path:
        return self.log_dir(project) / "runs"

    def global_fn_dirs(self) -> list[Path]:
        return [self.home / "fns", *(self.home / d for d in self.config["fn_dirs"])]

    @contextlib.contextmanager
    def lock(self, project: str | None) -> Iterator[None]:
        """Exclusive flock on the project's .lock (SLUICE_HOME/.lock without a project), the
        lock of its state and its log; re-entrant within a thread."""
        held: set[str] = self._held.__dict__.setdefault("projects", set())
        key = project or ""
        if key in held:
            yield
            return
        if project and not self.project_dir(project).is_dir():  # deleted: never recreate it
            raise NotFound(f"no project {project!r}")
        with L.flock(self.log_dir(project) / L.LOCK):
            held.add(key)
            try:
                yield
            finally:
                held.discard(key)

    # ---- the log (SPEC §6b) ----

    def append(self, project: str | None, *records: dict[str, Any]) -> list[int]:
        """Append records to the project's (or the home's) log; returns their seqs."""
        with self.lock(project):
            return L.append(self.log_dir(project), list(records),
                            int(self.config.get("log_max") or L.DEFAULT_MAX))

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
        d.mkdir(parents=True, exist_ok=True)
        with self.lock(name):
            if (d / "project.json").exists():
                raise BadRequest(f"project {name!r} already exists")
            doc = copy.deepcopy(P.EMPTY)
            (d / L.FILE).unlink(missing_ok=True)
            self._log(name, 1, author, reason or "project created",
                      [{"op": "add", "path": "", "value": doc}])
            atomic_write_json(d / "plan.json", {**doc, "rev": 1})
            atomic_write_json(d / "project.json", {"name": name, "description": description})
        self.notify()
        return {"name": name}

    def update_project(self, name: str, description: str | None = None,
                       archived: bool | None = None, paused: bool | None = None
                       ) -> dict[str, str]:
        """Replace the description and/or set `archived` (an archived project stays whole and
        keeps running; the dashboard lists it apart) and/or
        `paused` (no step of it starts until unpaused; running ones finish)."""
        if description is not None and not isinstance(description, str):
            raise BadRequest("description: expected a string")
        for key, value in (("archived", archived), ("paused", paused)):
            if value is not None and not isinstance(value, bool):
                raise BadRequest(f"{key}: expected true or false")
        with self.lock(name):
            new = dict(self.project(name))
            if description is not None:
                new["description"] = description
            if archived is not None:
                new["archived"] = archived
            if paused is not None:
                new["paused"] = paused
            atomic_write_json(self.project_dir(name) / "project.json", new)
        self.notify()
        return {"name": name}

    def delete_project(self, name: str) -> dict[str, Any]:
        """Delete a project and everything it holds (plan, state, log, inbox, runs). Refused
        unless it is archived first and none of its steps is running."""
        with self.lock(name):
            if not self.archived(name):
                raise BadRequest(f"archive project {name!r} before deleting it")
            running = [s for s, e in self.read_state(name)["steps"].items()
                       if e.get("status") == "running"]
            if running:
                raise BadRequest(f"project {name!r} has running steps: {', '.join(running)}")
            d = self.project_dir(name)
            (d / "project.json").unlink()  # first: from here on it is not a project
            shutil.rmtree(d)
        self._parsed.pop(name, None)
        self.notify()
        return {"deleted": name}

    def archived(self, name: str) -> bool:
        try:
            return self.project(name).get("archived") is True
        except (NotFound, OSError, ValueError):
            return False

    def paused(self, name: str) -> bool:
        try:
            return self.project(name).get("paused") is True
        except (NotFound, OSError, ValueError):
            return False

    def projects(self) -> list[dict[str, Any]]:
        out = []
        for name in self.project_names():
            info, doc = self.project(name), self.get(name)
            st = self.read_state(name)["steps"]
            counts = Counter(st.get(s, {"status": "pending"})["status"] for s in doc["steps"])
            out.append({"name": name, "description": info.get("description", ""),
                        "rev": doc["rev"], "counts": dict(counts),
                        "archived": info.get("archived") is True,
                        "paused": info.get("paused") is True})
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

    def patch(self, project: str, rev: int, ops: Any, author: str, reason: str,
              start: bool = True) -> int:
        """Apply an RFC 6902 patch at `rev`. Unless `start`, a step it adds comes in paused
        (unless the step itself says `paused`); that pause is one more op in the history.
        Raises Conflict, InvalidPlan or NotFound."""
        with self.lock(project):
            cur = self.get(project)
            if rev != cur["rev"]:
                raise Conflict(cur["rev"])
            reg = self.usable_registry(project)
            old = _body(cur)
            new = apply_ops(old, ops)
            if not start and isinstance(new.get("steps"), dict):
                held = [{"op": "add", "path": f"/steps/{sid}/paused", "value": True}
                        for sid, s in new["steps"].items()
                        if sid not in old["steps"] and isinstance(s, dict) and "paused" not in s]
                if held:
                    ops = [*ops, *held]
                    new = apply_ops(new, held)
            errs, _ = P.validate(new, reg)
            new_steps = new.get("steps") if isinstance(new.get("steps"), dict) else {}
            for sid, e in self.read_state(project)["steps"].items():
                if e["status"] != "running":
                    continue
                if sid not in new_steps:
                    errs.append(f"steps.{sid}: cannot remove a running step")
                elif _unpaused(new_steps[sid]) != _unpaused(old["steps"].get(sid)):
                    errs.append(f"steps.{sid}: cannot change a running step (only pause it)")
            if errs:
                raise InvalidPlan(errs)
            self._log(project, rev + 1, author, reason, ops)
            atomic_write_json(self.project_dir(project) / "plan.json", {**new, "rev": rev + 1})
        self.notify()
        return rev + 1

    # ---- one step of the plan: plan_patch for a single step, at the current rev ----

    def add_step(self, project: str, sid: str, step: Any, author: str, reason: str,
                 start: bool = False) -> int:
        if not isinstance(sid, str) or not P.ID_RE.match(sid):
            raise BadRequest(f"step ids match {P.ID_RE.pattern}, got {sid!r}")
        with self.lock(project):
            cur = self.get(project)
            if sid in cur["steps"]:
                raise BadRequest(f"step {sid!r} already exists (step_update changes it)")
            return self.patch(project, cur["rev"],
                              [{"op": "add", "path": f"/steps/{sid}", "value": step}],
                              author, reason or f"add step {sid}", start)

    def update_step(self, project: str, sid: str, changes: Any, author: str,
                    reason: str) -> int:
        """Merge `changes` into a step: each key replaces that field of it, null removes it."""
        if not isinstance(changes, dict) or not changes:
            raise BadRequest("changes: expected an object of step field -> new value")
        with self.lock(project):
            cur = self.get(project)
            if sid not in cur["steps"]:
                raise NotFound(f"the plan of project {project} has no step {sid!r}")
            new = copy.deepcopy(cur["steps"][sid])
            for key, value in changes.items():
                if value is None:
                    new.pop(key, None)
                else:
                    new[key] = value
            return self.patch(project, cur["rev"],
                              [{"op": "replace", "path": f"/steps/{sid}", "value": new}],
                              author, reason or f"update step {sid}")

    def remove_steps(self, project: str, steps: Any = None, tags: Any = None,
                     author: str = "", reason: str = "") -> dict[str, Any]:
        """Remove the selected steps in one edit. Returns {rev, steps}."""
        with self.lock(project):
            chosen = self.select_steps(project, steps, tags)
            cur = self.get(project)
            rev = self.patch(project, cur["rev"],
                             [{"op": "remove", "path": f"/steps/{sid}"} for sid in chosen],
                             author, reason or f"remove {', '.join(chosen)}")
        return {"rev": rev, "steps": chosen}

    def select_steps(self, project: str, steps: Any = None, tags: Any = None,
                     subtree: bool = False) -> list[str]:
        """Step ids by id and/or tag, with everything downstream of them when `subtree` (the
        steps that read from or run after them, transitively), in plan order. A single id or
        tag counts as a list of one."""
        steps = [steps] if isinstance(steps, str) else steps
        tags = [tags] if isinstance(tags, str) else tags
        for name, v in (("steps", steps), ("tags", tags)):
            if v is not None and not (isinstance(v, list) and all(isinstance(x, str) for x in v)):
                raise BadRequest(f"{name}: expected an array of strings")
        if not steps and not tags:
            raise BadRequest("select steps by `steps` (ids) and/or `tags`")
        _, plan = self.plan(project)
        missing = [s for s in steps or [] if s not in plan.steps]
        if missing:
            raise NotFound(f"the plan of project {project} has no step {', '.join(missing)}")
        chosen = set(steps or []) | {s.id for s in plan.steps.values()
                                     if set(s.tags) & set(tags or [])}
        if subtree:
            below: dict[str, list[str]] = {}
            for s in plan.steps.values():
                for w in s.waits:
                    below.setdefault(w, []).append(s.id)
            todo = list(chosen)
            while todo:
                for d in below.get(todo.pop(), []):
                    if d not in chosen:
                        chosen.add(d)
                        todo.append(d)
        return [sid for sid in plan.steps if sid in chosen]

    def pause_steps(self, project: str, steps: Any = None, tags: Any = None,
                    subtree: bool = False, paused: bool = True, author: str = "",
                    reason: str = "") -> dict[str, Any]:
        """Pause (or unpause) the selected steps in one edit: `paused` becomes the reason when
        one is given, else true; unpausing removes it. Returns {rev, steps}."""
        if not isinstance(paused, bool):
            raise BadRequest("paused: expected true or false")
        with self.lock(project):
            chosen = self.select_steps(project, steps, tags, subtree)
            cur = self.get(project)
            mark: Any = (reason.strip() or True) if paused else None
            ops = []
            for sid in chosen:
                if cur["steps"][sid].get("paused", False) not in (False, None) and paused and \
                        not reason.strip():
                    continue  # already paused: keep its reason
                if mark is None:
                    if "paused" in cur["steps"][sid]:
                        ops.append({"op": "remove", "path": f"/steps/{sid}/paused"})
                elif cur["steps"][sid].get("paused") != mark:
                    ops.append({"op": "add", "path": f"/steps/{sid}/paused", "value": mark})
            if not ops:
                return {"rev": cur["rev"], "steps": chosen}
            what = "pause" if paused else "unpause"
            rev = self.patch(project, cur["rev"], ops, author,
                             reason or f"{what} {', '.join(chosen)}")
        return {"rev": rev, "steps": chosen}

    def cancel_steps(self, project: str, steps: Any = None, tags: Any = None,
                     author: str = "", reason: str = "") -> list[str]:
        """Ask the runner to stop the selected running steps: it kills their processes and
        fails them with `cancelled` (and the reason). Refused unless every one is running."""
        with self.lock(project):
            chosen = self.select_steps(project, steps, tags)
            state = self.read_state(project)
            idle = [f"{s} is {state['steps'].get(s, {'status': 'pending'})['status']}"
                    for s in chosen
                    if state["steps"].get(s, {"status": "pending"})["status"] != "running"]
            if idle:
                raise BadRequest(f"only a running step can be cancelled: {', '.join(idle)}")
            for sid in chosen:
                state["steps"][sid]["cancel"] = reason or "cancelled"
            self.write_state(project, state)
            self.append(project, *({"kind": "step.cancel", "step": sid, "author": author,
                                    "reason": reason} for sid in chosen))
        self.notify()
        return chosen

    def _log(self, project: str, rev: int, author: str, reason: str, ops: list | None = None,
             kind: str = "plan.edit", **fields: Any) -> None:
        """A history record: an edit (`plan.edit` with `ops`), or a manual value (its kind and
        arguments). Callers hold the project lock."""
        rec = {"kind": kind, "rev": rev, "author": author, "reason": reason, **fields}
        if ops is not None:
            rec["ops"] = ops
        self.append(project, rec)

    def notify(self) -> None:
        for fn in list(self.listeners):
            fn()

    def history(self, project: str, since_rev: int | None = None) -> list[dict[str, Any]]:
        """plan_history: the plan edits and manual values still in the log."""
        self.project(project)
        recs = L.read(self.log_dir(project), kinds=L.HISTORY_KINDS)["records"]
        return [e for e in recs if since_rev is None or e["rev"] > since_rev]

    # ---- state ----

    def read_state(self, project: str) -> dict[str, Any]:
        path = self.project_dir(project) / "state.json"
        return read_json(path) if path.exists() else {"inputs": {}, "steps": {}}

    def write_state(self, project: str, state: dict[str, Any]) -> None:
        """Callers hold the project lock."""
        atomic_write_json(self.project_dir(project) / "state.json", state)

    def status(self, project: str, steps: Any = None, tags: Any = None,
               brief: bool = False) -> dict[str, Any]:
        """The plan's inputs, outputs and steps (with `steps` and/or `tags`, only those); with
        `brief`, their long strings cut (`_brief`)."""
        only = set(self.select_steps(project, steps, tags)) if steps or tags else None
        doc, plan = self.plan(project)
        state = self.read_state(project)
        project_paused = self.paused(project)
        outputs = {}
        for name, ref in plan.outputs.items():
            ok, v = P.value_of(ref, plan, state)
            outputs[name] = v if ok else None
        rows = []
        for sid, step in plan.steps.items():
            if only is not None and sid not in only:
                continue
            e = state["steps"].get(sid, {"status": "pending"})
            row = {"id": sid, "run": step.fn.name, "status": e["status"],
                   "started": e.get("started"), "finished": e.get("finished")}
            row.update({k: e[k] for k in ("outputs", "error") if e.get(k) is not None})
            row.update({"doc": step.doc} if step.doc else {})
            row.update({"paused": step.pause_reason or True} if step.paused else {})
            row.update({"tags": step.tags} if step.tags else {})
            row.update({"after": step.after} if step.after else {})
            row.update({"when": str(step.when)} if step.when else {})
            row.update({"skipped": e.get("skipped")} if e["status"] == "skipped" else {})
            if e["status"] == "pending":  # why it has not started
                held = ([f"paused: {step.pause_reason}" if step.pause_reason else "paused"]
                        if step.paused else [])
                held += ["the project is paused"] if project_paused else []
                row["waiting"] = held + P.not_ready(step, plan, state)
            rows.append({**row, "manual": bool(e.get("manual"))})
        out = {"rev": doc["rev"], "paused": project_paused,
               "inputs": {n: state["inputs"].get(n) for n in plan.inputs}}
        if plan.input_docs:
            out["input_docs"] = dict(plan.input_docs)
        if brief:
            out["inputs"], outputs = _brief(out["inputs"]), _brief(outputs)
            for row in rows:
                if "outputs" in row:
                    row["outputs"] = _brief(row["outputs"])
        return {**out, "outputs": outputs, "steps": rows}

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
            self._log(project, doc["rev"], author, reason, kind="plan.input", name=name,
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

    def _status_change(self, project: str, step: str, before: str | None, after: str) -> None:
        if before != after:
            self.append(project, {"kind": "step.status", "step": step, "from": before,
                                  "to": after})

    def set_output(self, project: str, step: str, outputs: Any, author: str, reason: str,
                   force: bool = False) -> None:
        """step_set_output: the step succeeds with these outputs (manual). Refused while what
        it reads is not ready, unless `force` (then its inputs are unknown: it turns stale once
        they are all there)."""
        with self.lock(project):
            doc, plan = self._plan_for_write(project)
            if step not in plan.steps:
                raise NotFound(f"the plan of project {project} has no step {step!r}")
            s = plan.steps[step]
            types = {k: s.output_type(k) for k in s.outputs}
            errs = T.check_value(T.record_of(types), outputs, "outputs")
            if isinstance(outputs, dict):
                errs += [f"outputs.{k}: step {step} (fn {s.fn.name}) has no output {k}"
                         for k in outputs if k not in types]
            if errs:
                raise InvalidPlan(errs)
            state = self.read_state(project)
            before = state["steps"].get(step, {}).get("status")
            if before == "running":
                raise BadRequest(f"step {step} is running")
            waiting = P.not_ready(s, plan, state)
            if waiting and not force:
                raise InvalidPlan(waiting, f"step {step} reads values that are not ready; "
                                  "pass force: true to set its outputs anyway (it turns stale "
                                  "once they are)")
            h = None if waiting else P.inputs_hash(P.resolved_inputs(s, plan, state))
            state["steps"][step] = {"status": "succeeded", "started": None,
                                    "finished": now_iso(), "outputs": outputs, "manual": True,
                                    "inputs_hash": h}
            self.write_state(project, state)
            extra = {"force": True} if force else {}
            self._log(project, doc["rev"], author, reason, kind="step.output", step=step,
                      outputs=outputs, **extra)
            self._status_change(project, step, before, "succeeded")
        self.notify()

    def retry(self, project: str, steps: Any = None, tags: Any = None, author: str = "",
              reason: str = "") -> list[str]:
        """step_retry: the selected steps, each failed, stale or manually set, go back to
        pending (refused, changing nothing, unless every one of them is)."""
        with self.lock(project):
            doc, _ = self._plan_for_write(project)
            chosen = self.select_steps(project, steps, tags)
            state = self.read_state(project)
            was = {s: state["steps"].get(s, {"status": "pending"}) for s in chosen}
            bad = [f"step {s} is {e['status']}" for s, e in was.items()
                   if e["status"] not in ("failed", "stale") and not e.get("manual")]
            if bad:
                raise BadRequest(f"{'; '.join(bad)}; only a failed, stale or manually set step "
                                 "can be retried")
            for sid in chosen:
                state["steps"][sid] = {"status": "pending"}
            self.write_state(project, state)
            for sid, e in was.items():
                self._log(project, doc["rev"], author, reason, kind="step.retry", step=sid)
                self._status_change(project, sid, e["status"], "pending")
        self.notify()
        return chosen

    def submit(self, project: str, step: str, outputs: Any,
               run: str | None = None) -> dict[str, Any]:
        """step_submit: the agent of a running step hands over the outputs the step declares
        (SPEC §5). Checked against them: every required one, fitting types, no others. Written
        to the run's submitted.json (a resubmit replaces it) and logged as `step.submit`; the
        runner merges them into the step's outputs when the fn exits."""
        with self.lock(project):
            _, plan = self.plan(project)
            s = plan.steps.get(step)
            if s is None:
                raise NotFound(f"the plan of project {project} has no step {step!r}")
            if not s.declared:
                raise BadRequest(f"step {step} declares no outputs to submit")
            e = self.read_state(project)["steps"].get(step, {"status": "pending"})
            if e["status"] != "running":
                raise BadRequest(f"step {step} is {e['status']}; outputs are submitted while "
                                 "it runs")
            runs = e.get("run_ids") or []
            if run is None:
                if len(runs) != 1:
                    raise BadRequest(f"step {step} has {len(runs)} runs; pass run, the run id "
                                     "(SLUICE_RUN_ID) of yours")
                run = runs[0]
            elif run not in runs:
                raise NotFound(f"step {step} has no run {run!r}")
            errs = T.check_value(T.record_of(s.declared), outputs, "outputs")
            if isinstance(outputs, dict):
                errs += [f"outputs.{k}: step {step} declares no output {k}"
                         + (" (the fn returns that one itself)" if k in s.fn.outputs else "")
                         for k in outputs if k not in s.declared]
            if errs:
                raise InvalidPlan(errs, f"outputs do not match what step {step} declares")
            atomic_write_json(self.runs_dir(project) / run / SUBMITTED, outputs)
            self.append(project, {"kind": "step.submit", "step": step, "run": run,
                                  "outputs": outputs})
        return {"ok": True, "run": run}

    # ---- the inbox (SPEC §8) ----

    def log_cap(self) -> int:
        return int(self.config.get("log_max") or L.DEFAULT_MAX)

    def inbox(self, project: str | None = None, status: str = "open") -> list[dict[str, Any]]:
        """inbox_list: the items with this status ("all" for every one) of the project, or of
        every project, oldest first; each carries its `project`."""
        if status not in (*I.STATUSES, "all"):
            raise BadRequest(f"status: expected one of {', '.join(I.STATUSES)} or all, "
                             f"got {status!r}")
        if project is not None:
            self.project(project)
        out = [{"project": name, **item}
               for name in ([project] if project else self.project_names())
               for item in I.items(self.project_dir(name))
               if status in ("all", item["status"])]
        return sorted(out, key=lambda item: item["created"])

    def inbox_post(self, project: str, title: str, body: str | None = None,
                   ui: str | None = None, input: str | None = None,
                   sender: str | None = None) -> dict[str, Any]:
        """Post an open item. With `input`, answering it sets that plan input, so the plan
        must declare it; without a body, the item's body is that input's doc."""
        if not isinstance(title, str) or not title.strip():
            raise BadRequest("title: expected a non-empty string")
        with self.lock(project):
            self.project(project)
            if input is not None:
                plan = self.plan(project)[1]
                if input not in plan.inputs:
                    raise NotFound(f"the plan of project {project} has no input {input!r}")
                body = plan.input_docs.get(input) if body is None else body
            item = I.post(self.project_dir(project), self.log_cap(), title, body, ui, input,
                          sender)
        self.notify()
        return item

    def _open_item(self, project: str, item_id: str) -> dict[str, Any]:
        """The item, refusing an unknown one (NotFound) or one that is not open (NotOpen).
        Callers hold the project lock."""
        self.project(project)
        item = I.find(self.project_dir(project), item_id)
        if item is None:
            raise NotFound(f"project {project} has no inbox item {item_id!r}")
        if item["status"] != "open":
            raise NotOpen(item_id, item["status"])
        return item

    def inbox_answer(self, project: str, item_id: str, answer: Any,
                     author: str) -> dict[str, Any]:
        """Answer an open item. When it names a plan input, the answer's value (answer_value)
        goes through set_input first; a value that does not fit refuses the answer and the item
        stays open. The one write path for MCP and the dashboard."""
        errs = check_answer(answer)
        if errs:
            raise InvalidPlan(errs, "not a valid answer")
        with self.lock(project):
            item = self._open_item(project, item_id)
            if item.get("input"):
                name = item["input"]
                value = answer_value(answer)
                if value is None:
                    need = "give values.value, params.value or text"
                    raise InvalidPlan([f"answer: inbox item {item_id} sets plan input {name}; "
                                       + need])
                try:
                    self.set_input(project, name, value, author,
                                   f"inbox item {item_id}: {item['title']}")
                except InvalidPlan as e:
                    raise InvalidPlan(e.errors, f"inbox item {item_id}: the answer does not "
                                      f"fit plan input {name}") from e
            item = I.finish(self.project_dir(project), self.log_cap(), item_id,
                            {"status": "answered", "answer": answer, "answered": now_iso()},
                            {"kind": "inbox.answer", "answer": answer, "by": author})
        self.notify()
        return item

    def inbox_close(self, project: str, item_id: str, reason: str | None,
                    author: str) -> dict[str, Any]:
        """Withdraw an open item (the poster no longer needs it)."""
        with self.lock(project):
            self._open_item(project, item_id)
            extra = {"reason": reason} if reason else {}
            item = I.finish(self.project_dir(project), self.log_cap(), item_id,
                            {"status": "closed", "closed": now_iso(), **extra},
                            {"kind": "inbox.close", **extra, "by": author})
        self.notify()
        return item


def check_answer(answer: Any) -> list[str]:
    """An answer is {action: string, params?: object, values?: object, text?: string}."""
    if not isinstance(answer, dict):
        return ["answer: expected an object {action, params?, values?, text?}"]
    errs = [f"answer.{k}: unknown key (answers have action, params, values, text)"
            for k in answer if k not in ANSWER_KEYS]
    if "action" not in answer:
        errs.append("answer.action: missing required field")
    errs += [f"answer.{k}: expected {'a string' if t is str else 'an object'}"
             for k, t in ANSWER_KEYS.items() if k in answer and not isinstance(answer[k], t)]
    return errs


def answer_value(answer: dict[str, Any]) -> Any:
    """The value an answer gives a plan input: the first of `values.value` (a form field named
    value), `params.value` (a button's value) and `text` that is there; None when none is."""
    for where in (answer.get("values") or {}, answer.get("params") or {}):
        if "value" in where:
            return where["value"]
    return answer.get("text")


def _body(doc: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in doc.items() if k != "rev"}


def _unpaused(step: Any) -> Any:
    """A step without its `paused` flag: the one change a running step takes."""
    return {k: v for k, v in step.items() if k != "paused"} if isinstance(step, dict) else step


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
