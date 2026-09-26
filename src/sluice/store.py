"""The workspace on disk (SPEC §2, §5, §6): config, plans with their edit log, state, and the
edits made by hand (manual values)."""

from __future__ import annotations

import contextlib
import copy
import fcntl
import json
import os
import secrets
import threading
import time
from collections import Counter
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import jsonpatch
import jsonpointer

from . import plan as P
from . import types as T
from .errors import BadRequest, Conflict, InvalidPlan, NotFound
from .fns import BUILTIN_DIR, Registry
from .util import append_line, atomic_write_json, now_iso, read_json

DEFAULT_CONFIG: dict[str, Any] = {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420},
                                  "max_parallel": 8}
CALL_PREFIX = "call-"
CALL_STEP = "call"


def default_home() -> Path:
    return Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")


class Store:
    """All reads and writes of a SLUICE_HOME. Safe across threads and processes (flock)."""

    def __init__(self, home: Path | str | None = None):
        self.home = Path(home) if home is not None else default_home()
        self.config = copy.deepcopy(DEFAULT_CONFIG)
        if (self.home / "config.json").exists():
            self.config.update(read_json(self.home / "config.json"))
        self._registry: Registry | None = None
        self.listeners: list[Callable[[], None]] = []  # called after every accepted edit
        self._held = threading.local()
        self._parsed: dict[str, tuple[int, P.Plan]] = {}

    @property
    def registry(self) -> Registry:
        if self._registry is None:
            dirs = [BUILTIN_DIR, *(self.home / d for d in self.config["fn_dirs"])]
            self._registry = Registry.load(dirs)
        return self._registry

    def plan_dir(self, pid: str) -> Path:
        return self.home / "plans" / pid

    @property
    def runs_dir(self) -> Path:
        return self.home / "runs"

    @contextlib.contextmanager
    def lock(self, pid: str) -> Iterator[None]:
        """Exclusive flock on the plan dir's .lock; re-entrant within a thread."""
        held: set[str] = self._held.__dict__.setdefault("pids", set())
        if pid in held:
            yield
            return
        self.plan_dir(pid).mkdir(parents=True, exist_ok=True)
        fd = os.open(self.plan_dir(pid) / ".lock", os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            held.add(pid)
            try:
                yield
            finally:
                held.discard(pid)
        finally:
            os.close(fd)  # closing the descriptor releases the flock

    # ---- plans ----

    def plan_ids(self) -> list[str]:
        root = self.home / "plans"
        if not root.is_dir():
            return []
        return sorted(p.name for p in root.iterdir() if (p / "plan.json").is_file())

    def get(self, pid: str) -> dict[str, Any]:
        """The current plan, including `rev`."""
        path = self.plan_dir(pid) / "plan.json"
        if not P.ID_RE.match(pid) or not path.is_file():
            raise NotFound(f"no plan {pid!r}")
        return read_json(path)

    def plan(self, pid: str) -> tuple[dict[str, Any], P.Plan]:
        """The current document and its parsed plan (cached per rev)."""
        doc = self.get(pid)
        hit = self._parsed.get(pid)
        if hit is None or hit[0] != doc["rev"]:
            errs, plan = P.validate({k: v for k, v in doc.items() if k != "rev"}, self.registry)
            if errs:
                raise InvalidPlan(errs, f"plan {pid} no longer validates")
            hit = self._parsed[pid] = (doc["rev"], plan)
        return doc, hit[1]

    def create(self, pid: str, doc: Any, author: str, reason: str) -> int:
        if not isinstance(pid, str) or not P.ID_RE.match(pid):
            raise BadRequest(f"plan ids match {P.ID_RE.pattern}, got {pid!r}")
        if not isinstance(doc, dict):
            raise InvalidPlan(["plan: expected an object"])
        doc = {"id": pid, **{k: v for k, v in doc.items() if k != "rev"}}
        if doc["id"] != pid:
            raise InvalidPlan([f"id: the plan says {doc['id']!r} but is created as {pid!r}"])
        errs, _ = P.validate(doc, self.registry)
        if errs:
            raise InvalidPlan(errs)
        with self.lock(pid):
            if (self.plan_dir(pid) / "plan.json").exists():
                raise BadRequest(f"plan {pid!r} already exists")
            self._log(pid, 1, author, reason, [{"op": "add", "path": "", "value": doc}])
            atomic_write_json(self.plan_dir(pid) / "plan.json", {**doc, "rev": 1})
        self._notify()
        return 1

    def patch(self, pid: str, rev: int, ops: Any, author: str, reason: str) -> int:
        """Apply an RFC 6902 patch at `rev`. Raises Conflict, InvalidPlan or NotFound."""
        with self.lock(pid):
            cur = self.get(pid)
            if rev != cur["rev"]:
                raise Conflict(cur["rev"])
            old = {k: v for k, v in cur.items() if k != "rev"}
            new = apply_ops(old, ops)
            errs, _ = P.validate(new, self.registry)
            if new.get("id") != pid:
                errs.insert(0, "id: the plan id cannot change")
            new_steps = new.get("steps") if isinstance(new.get("steps"), dict) else {}
            for sid, e in self.read_state(pid)["steps"].items():
                if e["status"] != "running":
                    continue
                if sid not in new_steps:
                    errs.append(f"steps.{sid}: cannot remove a running step")
                elif new_steps[sid] != old["steps"].get(sid):
                    errs.append(f"steps.{sid}: cannot change a running step")
            if errs:
                raise InvalidPlan(errs)
            self._log(pid, rev + 1, author, reason, ops)
            atomic_write_json(self.plan_dir(pid) / "plan.json", {**new, "rev": rev + 1})
        self._notify()
        return rev + 1

    def _log(self, pid: str, rev: int, author: str, reason: str, ops: list | None = None,
             **manual: Any) -> None:
        """One log line: an edit (`ops`), or a manual value (`action` and its args, no ops)."""
        entry = {"rev": rev, "at": now_iso(), "author": author, "reason": reason}
        entry.update({"ops": ops} if ops is not None else manual)
        append_line(self.plan_dir(pid) / "plan.log.jsonl", entry)

    def _notify(self) -> None:
        for fn in list(self.listeners):
            fn()

    def history(self, pid: str, since_rev: int | None = None) -> list[dict[str, Any]]:
        self.get(pid)
        lines = (self.plan_dir(pid) / "plan.log.jsonl").read_text(encoding="utf-8").splitlines()
        entries = [json.loads(x) for x in lines if x.strip()]
        return [e for e in entries if since_rev is None or e["rev"] > since_rev]

    # ---- state ----

    def read_state(self, pid: str) -> dict[str, Any]:
        path = self.plan_dir(pid) / "state.json"
        return read_json(path) if path.exists() else {"inputs": {}, "steps": {}}

    def write_state(self, pid: str, state: dict[str, Any]) -> None:
        """Callers hold the plan lock."""
        atomic_write_json(self.plan_dir(pid) / "state.json", state)

    def status(self, pid: str) -> dict[str, Any]:
        doc, plan = self.plan(pid)
        state = self.read_state(pid)
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

    def plans(self, include_calls: bool = False) -> list[dict[str, Any]]:
        out = []
        for pid in self.plan_ids():
            if pid.startswith(CALL_PREFIX) and not include_calls:
                continue
            doc = self.get(pid)
            st = self.read_state(pid)["steps"]
            counts = Counter(st.get(s, {"status": "pending"})["status"] for s in doc["steps"])
            out.append({"id": pid, "label": doc.get("label", ""), "rev": doc["rev"],
                        "counts": dict(counts)})
        return out

    # ---- manual values (SPEC §6) ----

    def set_input(self, pid: str, name: str, value: Any, author: str, reason: str) -> None:
        with self.lock(pid):
            doc, plan = self.plan(pid)
            if name not in plan.inputs:
                raise NotFound(f"plan {pid} has no input {name!r}")
            errs = T.check_value(plan.inputs[name], value, f"inputs.{name}")
            if errs:
                raise InvalidPlan(errs)
            state = self.read_state(pid)
            if name in state["inputs"] and state["inputs"][name] != value:
                started = [s.id for s in plan.steps.values()
                           if any(r.step is None and r.name == name for r in s.reads)
                           and state["steps"].get(s.id, {}).get("status", "pending") != "pending"]
                if started:
                    raise BadRequest(f"input {name} was already read by step {started[0]}")
            state["inputs"][name] = value
            self.write_state(pid, state)
            self._log(pid, doc["rev"], author, reason, action="plan_set_input", input=name,
                      value=value)
        self._notify()

    def set_step_input(self, pid: str, step: str, name: str, value: Any, author: str,
                       reason: str, rev: int | None = None) -> int:
        with self.lock(pid):
            cur = self.get(pid)
            if step not in cur["steps"]:
                raise NotFound(f"plan {pid} has no step {step!r}")
            op = {"op": "add", "path": f"/steps/{step}/in/{name}", "value": {"default": value}}
            ops = [{"op": "add", "path": f"/steps/{step}/in", "value": {}}, op] \
                if "in" not in cur["steps"][step] else [op]
            return self.patch(pid, cur["rev"] if rev is None else rev, ops, author, reason)

    def set_output(self, pid: str, step: str, outputs: Any, author: str, reason: str) -> None:
        with self.lock(pid):
            doc, plan = self.plan(pid)
            if step not in plan.steps:
                raise NotFound(f"plan {pid} has no step {step!r}")
            s = plan.steps[step]
            types = {k: s.output_type(k) for k in s.fn.outputs}
            errs = T.check_value(T.record_of(types), outputs, "outputs")
            if isinstance(outputs, dict):
                errs += [f"outputs.{k}: fn {s.fn.name} has no output {k}"
                         for k in outputs if k not in types]
            if errs:
                raise InvalidPlan(errs)
            state = self.read_state(pid)
            if state["steps"].get(step, {}).get("status") == "running":
                raise BadRequest(f"step {step} is running")
            state["steps"][step] = {"status": "succeeded", "started": None,
                                    "finished": now_iso(), "outputs": outputs, "manual": True}
            self.write_state(pid, state)
            self._log(pid, doc["rev"], author, reason, action="step_set_output", step=step,
                      outputs=outputs)
        self._notify()

    def retry(self, pid: str, step: str, author: str, reason: str) -> None:
        """step_retry: a failed (or manually set) step goes back to pending."""
        with self.lock(pid):
            doc, plan = self.plan(pid)
            if step not in plan.steps:
                raise NotFound(f"plan {pid} has no step {step!r}")
            state = self.read_state(pid)
            e = state["steps"].get(step, {"status": "pending"})
            if e["status"] != "failed" and not e.get("manual"):
                raise BadRequest(f"step {step} is {e['status']}; only a failed or manually set "
                                 "step can be retried")
            state["steps"][step] = {"status": "pending"}
            self.write_state(pid, state)
            self._log(pid, doc["rev"], author, reason, action="step_retry", step=step)
        self._notify()

    # ---- one-off calls (fn_call) ----

    def create_call(self, name: str, inputs: Any, author: str) -> str:
        """Check `inputs` against the fn, then create a one-step plan that calls it."""
        fn = self.registry.get(name)
        if fn is None:
            raise NotFound(f"no fn {name!r}")
        if not isinstance(inputs, dict):
            raise InvalidPlan(["inputs: expected an object keyed by input name"])
        errs = T.check_value(T.record_of(fn.inputs), inputs, "inputs")
        errs += [f"inputs.{k}: fn {name} has no input {k}" for k in inputs
                 if k not in fn.inputs]
        if errs:
            raise InvalidPlan(errs, f"inputs do not match fn {name}")
        stamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
        pid = f"{CALL_PREFIX}{stamp}-{secrets.token_hex(3)}"
        doc = {"label": f"call {name}", "steps": {CALL_STEP: {
            "run": name, "in": {k: {"default": v} for k, v in inputs.items()}}}}
        self.create(pid, doc, author, f"fn_call {name}")
        return pid

    def call_result(self, pid: str) -> dict[str, Any]:
        self.get(pid)
        e = self.read_state(pid)["steps"].get(CALL_STEP, {"status": "pending"})
        out = {"plan": pid, "status": e["status"]}
        out.update({k: e[k] for k in ("outputs", "error") if e.get(k) is not None})
        return out


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
