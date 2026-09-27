"""The runner (SPEC §6): start ready steps and pending calls as processes, record outputs and
failures."""

from __future__ import annotations

import fcntl
import json
import os
import secrets
import signal
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
from . import log as L
from . import types as T
from .errors import BadRequest, InvalidPlan, NotFound
from .plan import Plan, Step, inputs_hash, is_ready, mark_stale, resolved_inputs, topo_order
from .registry import Fn
from .store import SUBMITTED, Store
from .util import atomic_write_json, canonical, now_iso, read_dotenv, tail_text

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
KILL_GRACE = 5.0  # seconds between SIGTERM and SIGKILL when stopping a fn


# ---- one fn execution (SPEC §4 process contract) ----------------------------------------


def fn_env(store: Store, project: str | None, fn: Fn, step: str, run_id: str,
           run_dir: Path, ports: dict[str, Any] | None = None) -> dict[str, str]:
    """os.environ, then the home .env, then the project's .env, then the SLUICE_* variables
    (with `ports`, an open fn's step: SLUICE_STEP_INPUTS and SLUICE_STEP_OUTPUTS)."""
    pythonpath = os.pathsep.join(filter(None, [SRC_DIR, os.environ.get("PYTHONPATH")]))
    env = {**os.environ, **read_dotenv(store.home / ".env")}
    if project:
        env.update(read_dotenv(store.project_dir(project) / ".env"))
    env.update({"SLUICE_HOME": str(store.home), "SLUICE_PROJECT": project or "",
                "SLUICE_STEP": step, "SLUICE_RUN_ID": run_id, "SLUICE_RUN_DIR": str(run_dir),
                "SLUICE_FN_DIR": str(fn.dir), "PYTHONPATH": pythonpath})
    for key, name in (("inputs", "SLUICE_STEP_INPUTS"), ("outputs", "SLUICE_STEP_OUTPUTS")):
        env.pop(name, None)
        if ports and ports[key]:
            env[name] = json.dumps(ports[key])
    return env


def spawn(fn: Fn, inp: dict[str, Any], run_dir: Path, env: dict[str, str]) -> subprocess.Popen:
    run_dir.mkdir(parents=True, exist_ok=True)
    atomic_write_json(run_dir / "input.json", inp)
    with open(run_dir / "input.json", "rb") as stdin, \
            open(run_dir / "output.json", "wb") as stdout, \
            open(run_dir / "stderr.log", "wb") as stderr:
        # Its own session, so kill() reaches the fn under `uv run` as well (a process group).
        return subprocess.Popen(["uv", "run", "--quiet", "--script", str(fn.dir / "main.py")],
                                stdin=stdin, stdout=stdout, stderr=stderr, cwd=run_dir, env=env,
                                start_new_session=True)


def _signal_group(proc: subprocess.Popen, sig: int) -> None:
    try:
        os.killpg(proc.pid, sig)
    except (ProcessLookupError, PermissionError):
        if proc.poll() is None:
            proc.send_signal(sig)


def _group_alive(proc: subprocess.Popen) -> bool:
    proc.poll()  # reap the leader, so only live members keep the group
    try:
        os.killpg(proc.pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return proc.returncode is None
    return True


def kill(*procs: subprocess.Popen, grace: float = KILL_GRACE) -> None:
    """Stop fn processes and everything in their process groups, then reap them.

    SIGTERM first, so an agent CLI can stop the tool processes it started in sessions of their
    own (Claude Code runs each Bash command in a new session); SIGKILL whatever is left in a
    group after `grace` seconds.
    """
    for p in procs:
        _signal_group(p, signal.SIGTERM)
    deadline = time.monotonic() + grace
    left = list(procs)
    while left and time.monotonic() < deadline:
        left = [p for p in left if _group_alive(p)]
        if left:
            time.sleep(0.05)
    for p in left:
        _signal_group(p, signal.SIGKILL)
    for p in procs:
        p.wait()


def read_run(fn: Fn, run_dir: Path, code: int,
             declared: dict[str, T.Type] | None = None) -> tuple[dict[str, Any], str]:
    """A finished process's outputs, or the error: exit code or type errors plus stderr tail.
    With `declared` (a step's own outputs), those come from what was submitted (SPEC §5)."""
    tail = tail_text(run_dir / "stderr.log", 2000).strip()
    if code != 0:
        return {}, f"exit code {code}" + (f"\n{tail}" if tail else "")
    try:
        out = json.loads((run_dir / "output.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as ex:
        return {}, f"the output is not one JSON object: {ex}"
    errs = T.check_value(T.record_of(fn.outputs), out)
    if errs:
        return {}, "outputs do not match the fn: " + "; ".join(errs)
    return with_submitted(out, declared, run_dir) if declared else (out, "")


def with_submitted(out: dict[str, Any], declared: dict[str, T.Type],
                   run_dir: Path) -> tuple[dict[str, Any], str]:
    """The step's outputs: the fn's own plus the declared ones its agent submitted with
    step_submit (the run dir's submitted.json; the fn's own values win on a name they share).
    A required declared output that was not submitted fails the step."""
    try:
        sent = json.loads((run_dir / SUBMITTED).read_text(encoding="utf-8"))
    except FileNotFoundError:
        sent = {}
    except (OSError, json.JSONDecodeError) as ex:
        return {}, f"the submitted outputs are not readable: {ex}"
    merged = {k: sent.get(k) for k in declared} | out
    missing = [k for k, t in declared.items() if k not in sent and not isinstance(t, T.Optional)]
    if missing:
        return {}, (f"declared outputs not submitted: {', '.join(missing)} (the agent must call "
                    "step_submit with them before it finishes)")
    errs = T.check_value(T.record_of(declared), merged)
    return (merged, "") if not errs else ({}, "submitted outputs do not match: "
                                          + "; ".join(errs))


def run_native(fn: Fn, inp: dict[str, Any]) -> tuple[dict[str, Any], str]:
    try:
        return NATIVE[fn.name](inp), ""
    except (KeyError, IndexError, ValueError, TypeError) as ex:
        return {}, f"{fn.name}: {type(ex).__name__}: {ex}"


def run_call_direct(store: Store, call: str, project: str | None) -> dict[str, Any]:
    """fn_call with direct: run a call created with `direct` in this process, to the end."""
    rec = C.latest(store, call, project)
    fn = store.fn(rec["fn"], project)
    inp = rec["inputs"]
    if fn.native:
        outputs, err = run_native(fn, inp)
    else:
        d = store.runs_dir(project) / call
        proc = spawn(fn, inp, d, fn_env(store, project, fn, "", call, d))
        outputs, err = read_run(fn, d, proc.wait())
    _finish(rec, outputs=outputs, error=err or None)
    C.record(store, project, rec)
    return C.result(rec)


# ---- the loop ----------------------------------------------------------------------------


@dataclass
class Active:
    """A running step or call: one input object per run (several when scattered)."""

    fn: Fn
    project: str | None
    step: str  # the step id; "" for a call
    inputs: list[dict[str, Any]]
    scatter: bool = False
    declared: dict[str, T.Type] = field(default_factory=dict)  # a step's own outputs
    ports: dict[str, Any] | None = None  # what an open fn is told about its step
    run_dirs: list[Path] = field(default_factory=list)  # one per run
    procs: dict[int, subprocess.Popen] = field(default_factory=dict)
    results: dict[int, dict[str, Any]] = field(default_factory=dict)
    launched: int = 0

    def outputs(self) -> dict[str, Any]:
        if not self.scatter:
            return self.results[0]
        n = len(self.inputs)
        return {o: [self.results[i].get(o) for i in range(n)]
                for o in {**self.fn.outputs, **self.declared}}

    def kill(self) -> None:
        kill(*self.procs.values())


class Runner:
    def __init__(self, store: Store):
        self.store = store
        self.active: dict[tuple[str, ...], Active] = {}
        self._calls: dict[str, tuple[int, dict[str, dict[str, Any]]]] = {}  # log -> seq, live
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
            kill(*(p for a in self.active.values() for p in a.procs.values()))
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
            was = {sid: e.get("status") for sid, e in state["steps"].items()}
            _, plan = self.store.plan(project)
            problems = self.store.registry(project).blocking
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
            held = self.store.paused(project)  # a paused project starts nothing
            self._launch(project, st)  # queued scatter runs first
            order = topo_order(plan)
            progress = True
            while progress:  # built-ins finish inline and can make more steps ready
                mark_stale(plan, state)  # before anything reads a result that no longer holds
                progress = False
                if problems:
                    break
                for sid in order:
                    step = plan.steps[sid]
                    if st[sid]["status"] != "pending" or not is_ready(step, plan, state):
                        continue
                    if held or step.paused:
                        continue  # stays pending, its inputs held, until unpaused
                    self._begin(project, step, plan, state)
                    self._launch(project, st)
                    progress = True
            if canonical(state) == before:
                return False
            self.store.write_state(project, state)
            records = []
            for sid, e in st.items():
                if e["status"] != was.get(sid):
                    rec = {"kind": "step.status", "step": sid, "from": was.get(sid),
                           "to": e["status"]}
                    if e["status"] == "failed":
                        rec["error"] = e.get("error")
                    if e["status"] in ("succeeded", "failed") and e.get("run_ids"):
                        rec["run_ids"] = e["run_ids"]
                    records.append(rec)
            if records:
                self.store.append(project, *records)
            return True

    def _tick_calls(self, project: str | None) -> None:
        """Start pending calls and collect finished ones. Follows the log's `call` records from
        the last seq seen, keeping the calls that are still pending or running."""
        d = self.store.log_dir(project)
        key = project or ""
        seq, live = self._calls.get(key, (0, {}))
        res = L.read(d, since_seq=seq, kinds=["call"])
        if res["last_seq"] < seq:  # the log was reset: start over
            seq, live = 0, {}
            res = L.read(d, since_seq=0, kinds=["call"])
        for rec in res["records"]:
            if rec["status"] in C.DONE:
                live.pop(rec["call"], None)
            else:
                live[rec["call"]] = rec
        self._calls[key] = (res["last_seq"], live)
        for call, rec in list(live.items()):
            rec = dict(rec)
            before = rec["status"]
            akey = ("call", key, call)
            if rec["status"] == "running" and rec.get("direct"):
                if not C.alive(rec.get("pid")):  # its process died before logging the end
                    _finish(rec, error=C.GONE)
            elif rec["status"] == "running":
                self._poll(akey, rec)
            elif rec["status"] == "pending":
                self._start_call(akey, rec, project)
            if rec["status"] != before:
                C.record(self.store, project, rec)
                live[call] = rec  # the record itself is picked up on the next pass

    def _start_call(self, key: tuple[str, ...], rec: dict[str, Any],
                    project: str | None) -> None:
        reg = self.store.registry(project)
        if reg.blocking:
            return  # stays pending until the functions are fixed
        fn = reg.get(rec["fn"])
        if fn is None:
            return _finish(rec, error=f"no fn {rec['fn']!r}")
        inp = rec.get("inputs") or {}
        rec.update(status="running")
        if fn.native:
            outputs, err = run_native(fn, inp)
            return _finish(rec, outputs=outputs, error=err or None)
        d = self.store.runs_dir(project) / rec["call"]
        a = self.active[key] = Active(fn, project, "", [inp], run_dirs=[d])
        try:
            a.procs[0] = spawn(fn, inp, d, fn_env(self.store, project, fn, "", rec["call"], d))
            a.launched = 1
        except OSError as ex:
            del self.active[key]
            _finish(rec, error=f"could not start the fn: {ex}")

    # ---- one step ----

    def _begin(self, project: str, step: Step, plan: Plan, state: dict[str, Any]) -> None:
        inp = resolved_inputs(step, plan, state)
        e = state["steps"][step.id] = {"status": "running", "started": now_iso(),
                                       "run_ids": [], "inputs_hash": inputs_hash(inp)}
        runs = [inp]
        if step.scatter:
            items = inp[step.scatter]
            if not isinstance(items, list):
                return _finish(e, error=f"scatter input {step.scatter} is not an array")
            runs = [{**inp, step.scatter: item} for item in items]
        for i, run in enumerate(runs):
            errs = T.check_value(T.record_of(step.inputs), run, "inputs")
            if errs:
                where = f"run {i}: " if step.scatter else ""
                return _finish(e, error=f"{where}inputs do not match the fn: " + "; ".join(errs))
        a = Active(step.fn, project, step.id, runs, scatter=bool(step.scatter),
                   declared=step.declared, ports=step.ports() if step.fn.open else None)
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

    def _launch(self, project: str, st: dict[str, Any]) -> None:
        """Start the queued runs of this project's scattered steps."""
        for key, a in list(self.active.items()):
            if key[0] != "step" or key[1] != project:
                continue
            while a.launched < len(a.inputs):
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
        env = fn_env(self.store, a.project, a.fn, a.step, run_id, run_dir, a.ports)
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
            outputs, err = read_run(a.fn, a.run_dirs[i], code, a.declared)
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
