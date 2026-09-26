"""The runner (SPEC §6): start ready steps and pending calls as processes, record outputs and
failures."""

from __future__ import annotations

import fcntl
import json
import os
import secrets
import subprocess
import sys
import threading
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import sluice

from . import calls as C
from . import types as T
from .errors import BadRequest, InvalidPlan, NotFound
from .plan import Plan, Source, Step, value_of
from .registry import Fn
from .store import Store
from .util import atomic_write_json, canonical, now_iso, read_dotenv, read_json, tail_text

SRC_DIR = str(Path(sluice.__file__).resolve().parent.parent)


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
          "core.format": _format}
RESTARTED = "runner restarted"


# ---- one fn execution (SPEC §4 process contract) ----------------------------------------


def fn_env(store: Store, project: str | None, fn: Fn, step: str, run_id: str,
           run_dir: Path) -> dict[str, str]:
    """os.environ, then the home .env, then the project's .env, then the SLUICE_* variables."""
    pythonpath = os.pathsep.join(filter(None, [SRC_DIR, os.environ.get("PYTHONPATH")]))
    env = {**os.environ, **read_dotenv(store.home / ".env")}
    if project:
        env.update(read_dotenv(store.project_dir(project) / ".env"))
    env.update({"SLUICE_HOME": str(store.home), "SLUICE_PROJECT": project or "",
                "SLUICE_STEP": step, "SLUICE_RUN_ID": run_id, "SLUICE_RUN_DIR": str(run_dir),
                "SLUICE_FN_DIR": str(fn.dir), "PYTHONPATH": pythonpath})
    return env


def spawn(fn: Fn, inp: dict[str, Any], run_dir: Path, env: dict[str, str]) -> subprocess.Popen:
    run_dir.mkdir(parents=True, exist_ok=True)
    atomic_write_json(run_dir / "input.json", inp)
    with open(run_dir / "input.json", "rb") as stdin, \
            open(run_dir / "output.json", "wb") as stdout, \
            open(run_dir / "stderr.log", "wb") as stderr:
        return subprocess.Popen(["uv", "run", "--quiet", "--script", str(fn.dir / "main.py")],
                                stdin=stdin, stdout=stdout, stderr=stderr, cwd=run_dir, env=env)


def read_run(fn: Fn, run_dir: Path, code: int) -> tuple[dict[str, Any], str]:
    """A finished process's outputs, or the error: exit code or type errors plus stderr tail."""
    tail = tail_text(run_dir / "stderr.log", 2000).strip()
    if code != 0:
        return {}, f"exit code {code}" + (f"\n{tail}" if tail else "")
    try:
        out = json.loads((run_dir / "output.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as ex:
        return {}, f"the output is not one JSON object: {ex}"
    errs = T.check_value(T.record_of(fn.outputs), out)
    return (out, "") if not errs else ({}, "outputs do not match the fn: " + "; ".join(errs))


def run_native(fn: Fn, inp: dict[str, Any]) -> tuple[dict[str, Any], str]:
    try:
        return NATIVE[fn.name](inp), ""
    except (KeyError, IndexError, ValueError, TypeError) as ex:
        return {}, f"{fn.name}: {type(ex).__name__}: {ex}"


def run_call_direct(store: Store, call: str, project: str | None) -> dict[str, Any]:
    """fn_call with direct: run a call created with `direct` in this process, to the end."""
    d = C.call_dir(store, call, project)
    rec = C.read(d)
    fn = store.fn(rec["fn"], project)
    inp = read_json(d / "input.json")
    if fn.native:
        outputs, err = run_native(fn, inp)
    else:
        proc = spawn(fn, inp, d, fn_env(store, project, fn, "", call, d))
        outputs, err = read_run(fn, d, proc.wait())
    _finish(rec, outputs=outputs, error=err or None)
    C.write(d, rec)
    return C.result(rec)


# ---- the loop ----------------------------------------------------------------------------


def source_value(src: Source, plan: Plan, state: dict[str, Any]) -> Any:
    if not src.refs and not src.fan_in:
        return src.default
    values = [value_of(r, plan, state)[1] for r in src.refs]
    return values if src.fan_in else values[0]


def is_ready(step: Step, plan: Plan, state: dict[str, Any]) -> bool:
    return all(value_of(r, plan, state)[0] for r in step.reads)


@dataclass
class Active:
    """A running step or call: one input object per run (several when scattered)."""

    fn: Fn
    project: str | None
    step: str  # the step id; "" for a call
    inputs: list[dict[str, Any]]
    scatter: bool = False
    run_dirs: list[Path] = field(default_factory=list)  # one per run (calls: the call dir)
    procs: dict[int, subprocess.Popen] = field(default_factory=dict)
    results: dict[int, dict[str, Any]] = field(default_factory=dict)
    launched: int = 0

    def outputs(self) -> dict[str, Any]:
        if not self.scatter:
            return self.results[0]
        n = len(self.inputs)
        return {o: [self.results[i].get(o) for i in range(n)] for o in self.fn.outputs}

    def kill(self) -> None:
        for p in self.procs.values():
            p.kill()
            p.wait()


class Runner:
    def __init__(self, store: Store):
        self.store = store
        self.active: dict[tuple[str, ...], Active] = {}
        self._finished_calls: set[Path] = set()
        self._reported: dict[str, str] = {}
        self._wake = threading.Event()
        self._stop = threading.Event()

    def wake(self) -> None:
        self._wake.set()

    def stop(self) -> None:
        self._stop.set()
        self._wake.set()

    def run_forever(self, interval: float = 1.0) -> None:
        """Tick until stop(), waking early after in-process edits; then end running fns.

        Holds SLUICE_HOME/runner.lock: a second runner on the same home would mark this one's
        running steps as failed.
        """
        self.store.home.mkdir(parents=True, exist_ok=True)
        fd = os.open(self.store.home / "runner.lock", os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(fd)
            raise BadRequest(f"another runner is active in {self.store.home}") from None
        try:
            while not self._stop.is_set():
                try:
                    self.tick()
                except Exception:  # noqa: BLE001 - the loop must survive any one tick
                    traceback.print_exc()
                self._wake.wait(interval)
                self._wake.clear()
        finally:
            for a in self.active.values():
                a.kill()
            os.close(fd)

    def _report(self, who: str, message: str) -> None:
        """Print a project's blocking problem once (until it changes)."""
        if self._reported.get(who) != message:
            self._reported[who] = message
            print(f"sluice runner: {who}: {message}", file=sys.stderr, flush=True)

    def tick(self) -> bool:
        """One pass over every project and every call. Returns whether any state changed."""
        changed = False
        for project in self.store.project_names():
            try:
                changed |= self._tick_project(project)
            except (InvalidPlan, NotFound) as e:  # e.g. a fn dir went away; others still run
                errs = getattr(e, "errors", None)
                self._report(project, e.message + (f": {errs}" if errs else ""))
            self._tick_calls(project)
        self._tick_calls(None)
        return changed

    def _tick_project(self, project: str) -> bool:
        with self.store.lock(project):
            state = self.store.read_state(project)
            before = canonical(state)
            _, plan = self.store.plan(project)
            problems = self.store.registry(project).problems
            if problems:  # SPEC §2: no new runs until the functions are fixed
                self._report(project, f"function problems block runs: {problems[0]['where']}: "
                                      f"{problems[0]['message']}")
            else:
                self._reported.pop(project, None)
            st = state["steps"]
            for sid in [s for s in st if s not in plan.steps]:  # removed from the plan
                if (a := self.active.pop(("step", project, sid), None)) is not None:
                    a.kill()
                del st[sid]
            for name in [n for n in state["inputs"] if n not in plan.inputs]:
                del state["inputs"][name]
            for sid in plan.steps:
                st.setdefault(sid, {"status": "pending"})
            for sid, e in st.items():
                if e["status"] == "running":
                    self._poll(("step", project, sid), e)
            self._launch(project, st)  # queued scatter runs first
            progress = not problems
            while progress:  # built-ins finish inline and can make more steps ready
                progress = False
                for sid, step in plan.steps.items():
                    if st[sid]["status"] != "pending" or not is_ready(step, plan, state):
                        continue
                    if not step.fn.native and self._procs() >= self.store.config["max_parallel"]:
                        continue  # stays pending until a process slot frees up
                    self._begin(project, step, plan, state)
                    self._launch(project, st)
                    progress = True
            if canonical(state) == before:
                return False
            self.store.write_state(project, state)
            return True

    def _tick_calls(self, project: str | None) -> None:
        root = self.store.calls_dir(project)
        if not root.is_dir():
            return
        for d in sorted(root.iterdir()):
            if d in self._finished_calls or not (d / "call.json").is_file():
                continue
            rec = C.read(d)
            before = dict(rec)
            if rec["status"] == "running" and not rec.get("direct"):
                self._poll(("call", project or "", rec["call"]), rec)
            elif rec["status"] == "pending" and \
                    self._procs() < self.store.config["max_parallel"]:
                self._start_call(("call", project or "", rec["call"]), d, rec, project)
            if rec["status"] in C.DONE:
                self._finished_calls.add(d)
            if rec != before:
                C.write(d, rec)

    def _start_call(self, key: tuple[str, ...], d: Path, rec: dict[str, Any],
                    project: str | None) -> None:
        reg = self.store.registry(project)
        if reg.problems:
            return  # stays pending until the functions are fixed
        fn = reg.get(rec["fn"])
        if fn is None:
            return _finish(rec, error=f"no fn {rec['fn']!r}")
        inp = read_json(d / "input.json")
        rec.update(status="running", started=now_iso())
        if fn.native:
            outputs, err = run_native(fn, inp)
            return _finish(rec, outputs=outputs, error=err or None)
        a = self.active[key] = Active(fn, project, "", [inp], run_dirs=[d])
        try:
            a.procs[0] = spawn(fn, inp, d, fn_env(self.store, project, fn, "", rec["call"], d))
            a.launched = 1
        except OSError as ex:
            del self.active[key]
            _finish(rec, error=f"could not start the fn: {ex}")

    # ---- one step ----

    def _begin(self, project: str, step: Step, plan: Plan, state: dict[str, Any]) -> None:
        e = state["steps"][step.id] = {"status": "running", "started": now_iso(),
                                       "run_ids": []}
        inp = {k: None for k in step.fn.inputs}
        inp.update({k: source_value(s, plan, state) for k, s in step.sources.items()})
        runs = [inp]
        if step.scatter:
            items = inp[step.scatter]
            if not isinstance(items, list):
                return _finish(e, error=f"scatter input {step.scatter} is not an array")
            runs = [{**inp, step.scatter: item} for item in items]
        for i, run in enumerate(runs):
            errs = T.check_value(T.record_of(step.fn.inputs), run, "inputs")
            if errs:
                where = f"run {i}: " if step.scatter else ""
                return _finish(e, error=f"{where}inputs do not match the fn: " + "; ".join(errs))
        a = Active(step.fn, project, step.id, runs, scatter=bool(step.scatter))
        if step.scatter:
            e.update(done=0, total=len(runs))
        if step.fn.native:
            for i, run in enumerate(runs):
                a.results[i], err = run_native(step.fn, run)
                if err:
                    return _finish(e, error=err)
            if step.scatter:
                e["done"] = len(runs)
        if len(a.results) == len(runs):
            return _finish(e, outputs=a.outputs())
        self.active[("step", project, step.id)] = a

    def _procs(self) -> int:
        return sum(len(a.procs) for a in self.active.values())

    def _launch(self, project: str, st: dict[str, Any]) -> None:
        """Start queued runs of this project's steps while under max_parallel processes."""
        limit = self.store.config["max_parallel"]
        for key, a in list(self.active.items()):
            if key[0] != "step" or key[1] != project:
                continue
            while a.launched < len(a.inputs) and self._procs() < limit:
                i, a.launched = a.launched, a.launched + 1
                try:
                    st[a.step]["run_ids"].append(self._spawn_run(a, i))
                except OSError as ex:
                    a.kill()
                    del self.active[key]
                    _finish(st[a.step], error=f"could not start the fn: {ex}")
                    break

    def _spawn_run(self, a: Active, i: int) -> str:
        assert a.project is not None
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime())
        run_id = f"{stamp}-{a.step}-{i}-{secrets.token_hex(2)}"
        run_dir = self.store.runs_dir(a.project) / run_id
        a.run_dirs.append(run_dir)
        env = fn_env(self.store, a.project, a.fn, a.step, run_id, run_dir)
        a.procs[i] = spawn(a.fn, a.inputs[i], run_dir, env)
        return run_id

    def _poll(self, key: tuple[str, ...], e: dict[str, Any]) -> None:
        a = self.active.get(key)
        if a is None:  # started by a runner that is gone
            return _finish(e, error=RESTARTED)
        for i, proc in list(a.procs.items()):
            code = proc.poll()
            if code is None:
                continue
            del a.procs[i]
            outputs, err = read_run(a.fn, a.run_dirs[i], code)
            if err:
                a.kill()
                del self.active[key]
                return _finish(e, error=f"run {i}: {err}" if a.scatter else err)
            a.results[i] = outputs
            if a.scatter:
                e["done"] = len(a.results)
        if len(a.results) == len(a.inputs):
            del self.active[key]
            _finish(e, outputs=a.outputs())


def _finish(e: dict[str, Any], outputs: Any = None, error: str | None = None) -> None:
    e.update(status="failed" if error else "succeeded", finished=now_iso(),
             outputs=None if error else outputs, error=error)
