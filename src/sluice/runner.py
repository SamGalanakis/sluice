"""The runner (SPEC §6): start ready steps as processes, record outputs and failures."""

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

from . import types as T
from .errors import BadRequest, InvalidPlan
from .plan import Plan, Source, Step, value_of
from .store import Store
from .util import atomic_write_json, canonical, now_iso, tail_text

SRC_DIR = str(Path(sluice.__file__).resolve().parent.parent)
NATIVE = {"core.echo": lambda inp: {"value": inp["value"]},
          "core.collect": lambda inp: {"items": inp["items"]}}
RESTARTED = "runner restarted"


def source_value(src: Source, plan: Plan, state: dict[str, Any]) -> Any:
    if not src.refs and not src.fan_in:
        return src.default
    values = [value_of(r, plan, state)[1] for r in src.refs]
    return values if src.fan_in else values[0]


def is_ready(step: Step, plan: Plan, state: dict[str, Any]) -> bool:
    return all(value_of(r, plan, state)[0] for r in step.reads)


@dataclass
class Active:
    """A running step: one input object per run (several when scattered)."""

    step: Step
    inputs: list[dict[str, Any]]
    procs: dict[int, subprocess.Popen] = field(default_factory=dict)
    dirs: dict[int, Path] = field(default_factory=dict)
    results: dict[int, dict[str, Any]] = field(default_factory=dict)
    launched: int = 0

    def outputs(self) -> dict[str, Any]:
        if not self.step.scatter:
            return self.results[0]
        n = len(self.inputs)
        return {o: [self.results[i].get(o) for i in range(n)] for o in self.step.fn.outputs}

    def kill(self) -> None:
        for p in self.procs.values():
            p.kill()
            p.wait()


class Runner:
    def __init__(self, store: Store):
        self.store = store
        self.active: dict[tuple[str, str], Active] = {}
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

    def tick(self) -> bool:
        """One pass over every plan. Returns whether any state changed."""
        changed = False
        for pid in self.store.plan_ids():
            try:
                changed |= self._tick_plan(pid)
            except InvalidPlan as e:  # e.g. a fn dir went away; the other plans still run
                print(f"sluice runner: {pid}: {e.errors}", file=sys.stderr, flush=True)
        return changed

    def _tick_plan(self, pid: str) -> bool:
        with self.store.lock(pid):
            state = self.store.read_state(pid)
            before = canonical(state)
            _, plan = self.store.plan(pid)
            st = state["steps"]
            for sid in [s for s in st if s not in plan.steps]:  # removed from the plan
                if (a := self.active.pop((pid, sid), None)) is not None:
                    a.kill()
                del st[sid]
            for sid in plan.steps:
                st.setdefault(sid, {"status": "pending"})
            for sid, e in st.items():
                if e["status"] == "running":
                    self._poll(pid, sid, e)
            self._launch(pid, st)  # queued scatter runs first
            progress = True
            while progress:  # built-ins finish inline and can make more steps ready
                progress = False
                for sid, step in plan.steps.items():
                    if st[sid]["status"] != "pending" or not is_ready(step, plan, state):
                        continue
                    if not step.fn.native and self._procs() >= self.store.config["max_parallel"]:
                        continue  # stays pending until a process slot frees up
                    self._begin(pid, step, plan, state)
                    self._launch(pid, st)
                    progress = True
            if canonical(state) == before:
                return False
            self.store.write_state(pid, state)
            return True

    # ---- one step ----

    def _begin(self, pid: str, step: Step, plan: Plan, state: dict[str, Any]) -> None:
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
        a = Active(step, runs)
        if step.fn.native:
            a.results = {i: NATIVE[step.fn.name](run) for i, run in enumerate(runs)}
        if len(a.results) == len(runs):
            return _finish(e, outputs=a.outputs())
        self.active[(pid, step.id)] = a

    def _procs(self) -> int:
        return sum(len(a.procs) for a in self.active.values())

    def _launch(self, pid: str, st: dict[str, Any]) -> None:
        """Start queued runs of this plan's steps while under max_parallel processes."""
        limit = self.store.config["max_parallel"]
        for (p, sid), a in list(self.active.items()):
            while p == pid and a.launched < len(a.inputs) and self._procs() < limit:
                i, a.launched = a.launched, a.launched + 1
                try:
                    st[sid]["run_ids"].append(self._spawn(pid, a, i))
                except OSError as ex:
                    a.kill()
                    del self.active[(p, sid)]
                    _finish(st[sid], error=f"could not start the fn: {ex}")
                    break

    def _spawn(self, pid: str, a: Active, i: int) -> str:
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime())
        run_id = f"{stamp}-{pid}-{a.step.id}-{i}-{secrets.token_hex(2)}"
        run_dir = a.dirs[i] = self.store.runs_dir / run_id
        run_dir.mkdir(parents=True)
        atomic_write_json(run_dir / "input.json", a.inputs[i])
        pythonpath = os.pathsep.join(filter(None, [SRC_DIR, os.environ.get("PYTHONPATH")]))
        env = {**os.environ, "SLUICE_HOME": str(self.store.home), "SLUICE_PLAN": pid,
               "SLUICE_STEP": a.step.id, "SLUICE_RUN_ID": run_id,
               "SLUICE_RUN_DIR": str(run_dir), "SLUICE_FN_DIR": str(a.step.fn.dir),
               "PYTHONPATH": pythonpath}
        with open(run_dir / "input.json", "rb") as stdin, \
                open(run_dir / "output.json", "wb") as stdout, \
                open(run_dir / "stderr.log", "wb") as stderr:
            a.procs[i] = subprocess.Popen(
                ["uv", "run", "--quiet", "--script", str(a.step.fn.dir / "main.py")],
                stdin=stdin, stdout=stdout, stderr=stderr, cwd=run_dir, env=env)
        return run_id

    def _poll(self, pid: str, sid: str, e: dict[str, Any]) -> None:
        a = self.active.get((pid, sid))
        if a is None:  # started by a runner that is gone
            return _finish(e, error=RESTARTED)
        for i, proc in list(a.procs.items()):
            code = proc.poll()
            if code is None:
                continue
            del a.procs[i]
            outputs, err = _read_run(a.step, a.dirs[i], code)
            if err:
                a.kill()
                del self.active[(pid, sid)]
                return _finish(e, error=f"run {i}: {err}" if a.step.scatter else err)
            a.results[i] = outputs
        if len(a.results) == len(a.inputs):
            del self.active[(pid, sid)]
            _finish(e, outputs=a.outputs())


def _read_run(step: Step, run_dir: Path, code: int) -> tuple[dict[str, Any], str]:
    tail = tail_text(run_dir / "stderr.log", 2000).strip()
    if code != 0:
        return {}, f"exit code {code}" + (f"\n{tail}" if tail else "")
    try:
        out = json.loads((run_dir / "output.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as ex:
        return {}, f"the output is not one JSON object: {ex}"
    errs = T.check_value(T.record_of(step.fn.outputs), out)
    return (out, "") if not errs else ({}, "outputs do not match the fn: " + "; ".join(errs))


def _finish(e: dict[str, Any], outputs: Any = None, error: str | None = None) -> None:
    e.update(status="failed" if error else "succeeded", finished=now_iso(), outputs=outputs,
             error=error)
