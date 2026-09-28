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
from . import state as S
from . import types as T
from .errors import BadRequest, InvalidPlan, NotFound
from .plan import (
    Plan,
    Step,
    inputs_hash,
    is_ready,
    mark_stale,
    resolved_inputs,
    settle_skip,
    settle_skips,
    topo_order,
)
from .registry import NATIVE, Fn
from .store import SUBMITTED, Store
from .util import atomic_write_json, canonical, now_iso, read_dotenv, tail_text

SRC_DIR = str(Path(sluice.__file__).resolve().parent.parent)
RESTARTED = "runner restarted"
UNKNOWN = "run outcome unknown (its supervisor died)"
KILL_GRACE = 5.0  # seconds between SIGTERM and SIGKILL when stopping a fn


# ---- one fn execution (SPEC §4 process contract) ----------------------------------------


HOST_VARS = ("PATH", "PYTHONPATH", "VIRTUAL_ENV")  # what `uv run` and sluice change for a fn


def fn_env(store: Store, project: str | None, fn: Fn, step: str, run_id: str,
           run_dir: Path, ports: dict[str, Any] | None = None) -> dict[str, str]:
    """os.environ, then the home .env, then the project's .env, then the SLUICE_* variables
    (with `ports`, an open fn's step: SLUICE_STEP_INPUTS and SLUICE_STEP_OUTPUTS).
    SLUICE_HOST_PATH, SLUICE_HOST_PYTHONPATH and SLUICE_HOST_VIRTUAL_ENV keep those three as
    they were before `uv run` and sluice changed them for the fn's own interpreter, so the
    tools a fn starts get them back (sluice.fn.child_env; "" means unset)."""
    pythonpath = os.pathsep.join(filter(None, [SRC_DIR, os.environ.get("PYTHONPATH")]))
    env = {**os.environ, **read_dotenv(store.home / ".env")}
    if project:
        env.update(read_dotenv(store.project_dir(project) / ".env"))
    env.update({f"SLUICE_HOST_{k}": env.get(k, "") for k in HOST_VARS})
    env.update({"SLUICE_HOME": str(store.home), "SLUICE_PROJECT": project or "",
                "SLUICE_STEP": step, "SLUICE_RUN_ID": run_id, "SLUICE_RUN_DIR": str(run_dir),
                "SLUICE_FN_DIR": str(fn.dir), "PYTHONPATH": pythonpath})
    for key, name in (("inputs", "SLUICE_STEP_INPUTS"), ("outputs", "SLUICE_STEP_OUTPUTS")):
        env.pop(name, None)
        if ports and ports[key]:
            env[name] = json.dumps(ports[key])
    return env


def spawn(fn: Fn, inp: dict[str, Any], run_dir: Path, env: dict[str, str]) -> subprocess.Popen:
    """Start the run's shim (`python -m sluice.exec`): it runs the fn and records the exit
    (SPEC §4), so the outcome survives this runner."""
    run_dir.mkdir(parents=True, exist_ok=True)
    for stale in ("exit.json", "shim.json", "child.json", "output.json"):
        (run_dir / stale).unlink(missing_ok=True)  # a reused dir carries no old evidence
    atomic_write_json(run_dir / "input.json", inp)
    argv = [sys.executable, "-m", "sluice.exec", str(run_dir), "--",
            "uv", "run", "--quiet", "--script", str(fn.dir / "main.py")]
    with open(run_dir / "stderr.log", "ab") as stderr:
        # Its own session, so kill() reaches the fn under `uv run` as well (a process group).
        return subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=stderr, cwd=run_dir, env=env,
                                start_new_session=True)


def _signal_group(pid: int, sig: int, proc: subprocess.Popen | None = None) -> None:
    try:
        os.killpg(pid, sig)
    except (ProcessLookupError, PermissionError):
        if proc is not None and proc.poll() is None:
            proc.send_signal(sig)


def _group_alive(pid: int, proc: subprocess.Popen | None = None) -> bool:
    if proc is not None:
        proc.poll()  # reap the leader, so only live members keep the group
    try:
        os.killpg(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return proc is None or proc.returncode is None
    return True


def lock_held(path: Path) -> bool:
    """Whether some process holds an exclusive flock on `path` — the shim's liveness, immune
    to pid reuse (a dead shim's lock is free, whatever process now has its pid)."""
    try:
        fd = os.open(path, os.O_RDWR)
    except OSError:
        return False
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        held = False
    except BlockingIOError:
        held = True
    os.close(fd)  # closing the descriptor releases a lock we took
    return held


def _live_leader(pid: int) -> bool:
    """Whether `pid` names a live process that leads its own process group — the shape the
    shim has (it runs `start_new_session`). A fn can overwrite shim.json in its run dir,
    so a recorded pid is never signalled without this check."""
    try:
        return pid > 1 and os.getpgid(pid) == pid
    except OSError:
        return False


def _child_pid(run_dir: Path) -> int | None:
    """The fn's pid from child.json — written by the shim right after Popen — verified
    against its recorded /proc start time so a reused pid cannot pass for it."""
    try:
        data = json.loads((run_dir / "child.json").read_text())
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    pid, started = data.get("pid"), data.get("pid_start")
    if not isinstance(pid, int) or not isinstance(started, str) or pid <= 1:
        return None
    return pid if C.pid_start(pid) == started else None


def _survivor_pgid(run_dir: Path) -> int | None:
    """The process group of a fn that outlived its shim (the shim killed alone, the fn's
    group lives on): the recorded child still runs and its group is still the recorded
    shim's pid — both files sit in a dir the fn can write, so they must agree before
    anything is signalled."""
    pid, child = _shim_pid(run_dir), _child_pid(run_dir)
    if pid is None or child is None:
        return None
    try:
        return pid if pid > 1 and os.getpgid(child) == pid else None
    except OSError:
        return None


def _target_pgid(run: Run) -> int | None:
    """The process group it is safe to signal for a run, or None. A run this runner
    spawned is trusted by its proc's pid; a proc-less run (adopted, or found on disk)
    takes the recorded shim pid only while the shim lock is held — and, with the shim
    already dead, the group of a fn child that verifiably outlived it."""
    if run.proc is not None:
        return run.proc.pid
    if run.run_dir is None:
        return None
    pid = run.pid
    if pid is None:  # lazily re-read: shim.json may have landed since we last looked
        pid = run.pid = _shim_pid(run.run_dir)
    if pid is not None and _live_leader(pid) and lock_held(run.run_dir / "shim.lock"):
        return pid
    return _survivor_pgid(run.run_dir)


def _run_alive(run_dir: Path) -> bool:
    """Whether a run dir still has processes to stop: a held shim.lock, or a recorded fn
    child that outlived its shim."""
    return lock_held(run_dir / "shim.lock") or _survivor_pgid(run_dir) is not None


def kill(*runs: Run, grace: float = KILL_GRACE) -> list[Run]:
    """Stop runs' processes and everything in their process groups, then reap them.

    SIGTERM first, so an agent CLI can stop the tool processes it started in sessions of their
    own (Claude Code runs each Bash command in a new session); SIGKILL whatever is left in a
    group after `grace` seconds. A run with no proc (adopted) is killed by its recorded pgid
    — the shim leads the group — and only while its shim lock is held or its recorded child
    verifiably lives on: otherwise the pid may already name an unrelated process group.
    Returns the runs that were actually signalled.
    """
    targets = [(pgid, run) for run in runs if (pgid := _target_pgid(run)) is not None]
    for pgid, run in targets:
        _signal_group(pgid, signal.SIGTERM, run.proc)
    deadline = time.monotonic() + grace
    left = targets
    while left and time.monotonic() < deadline:
        left = [t for t in left if _group_alive(t[0], t[1].proc)]
        if left:
            time.sleep(0.05)
    for pgid, run in left:
        _signal_group(pgid, signal.SIGKILL, run.proc)
    for _, run in targets:
        if run.proc is not None:
            run.proc.wait()
    return [run for _, run in targets]


def _read_exit(run_dir: Path) -> int | None:
    """A finished run's wait code from exit.json — its `code`, or `-signal` when it died by a
    signal (SPEC §4: exit.json is the only evidence a run is done). None while unfinished, or
    for a corrupt record."""
    try:
        data = json.loads((run_dir / "exit.json").read_text())
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict):
        return None
    code, sig = data.get("code"), data.get("signal")
    return code if isinstance(code, int) else (-sig if isinstance(sig, int) else None)


def _exit_error(run_dir: Path) -> str | None:
    """The start failure a shim recorded in exit.json (code 127: the fn never ran)."""
    try:
        data = json.loads((run_dir / "exit.json").read_text())
    except (OSError, ValueError):
        return None
    err = data.get("error") if isinstance(data, dict) else None
    return err if isinstance(err, str) else None


def _shim_pid(run_dir: Path) -> int | None:
    """The supervising shim's pid — also the run's process-group id — from shim.json."""
    try:
        data = json.loads((run_dir / "shim.json").read_text())
    except (OSError, ValueError):
        return None
    pid = data.get("pid") if isinstance(data, dict) else None
    return pid if isinstance(pid, int) else None


def _run_code(run: Run) -> int | str | None:
    """An exit code if the run is done, UNKNOWN if its shim died without exit.json, else
    None (still running). For a run this runner didn't spawn, the shim's lock is the
    liveness check; exit.json is checked again last — it can land while we look."""
    if run.result is not None:
        return None
    if run.proc is not None:
        proc, run.proc = run.proc, None
        if proc.poll() is None:
            run.proc = proc
            return None  # running
        code = _read_exit(run.run_dir)
        return code if code is not None else UNKNOWN
    code = _read_exit(run.run_dir)
    if code is not None:
        return code
    if lock_held(run.run_dir / "shim.lock"):
        return None  # running under another runner's shim
    code = _read_exit(run.run_dir)
    return code if code is not None else UNKNOWN


def _probe(run_dir: Path) -> tuple[str, int | None]:
    """Classify a run dir a `running` entry references: finished / watching / unknown /
    restarted — the last meaning a pre-shim run dir (SPEC §6). exit.json first and last; the
    shim may still be starting, so a bare dir gets one recheck before `restarted`."""
    if (code := _read_exit(run_dir)) is not None:
        return "finished", code
    for attempt in range(2):
        has_shim = (run_dir / "shim.json").exists()
        if lock_held(run_dir / "shim.lock"):
            return "watching", None
        if (code := _read_exit(run_dir)) is not None:
            return "finished", code
        if has_shim:
            return "unknown", None
        if attempt == 0:
            time.sleep(0.3)
    return "restarted", None


def read_run(fn: Fn, run_dir: Path, code: int,
             declared: dict[str, T.Type] | None = None) -> tuple[dict[str, Any], str]:
    """A finished process's outputs, or the error: exit code or type errors plus stderr tail.
    With `declared` (a step's own outputs), those come from what was submitted (SPEC §5)."""
    tail = tail_text(run_dir / "stderr.log", 2000).strip()
    if code != 0:
        if (why := _exit_error(run_dir)) is not None:
            return {}, f"could not start the fn: {why}"
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
    """The step's outputs: the fn's own plus the declared ones, which the fn returns itself
    (an inline fn) or its agent submitted with step_submit (the run dir's submitted.json; the
    fn's own values win on a name they share). A required declared output given neither way
    fails the step."""
    try:
        sent = json.loads((run_dir / SUBMITTED).read_text(encoding="utf-8"))
    except FileNotFoundError:
        sent = {}
    except (OSError, json.JSONDecodeError) as ex:
        return {}, f"the submitted outputs are not readable: {ex}"
    merged = {k: sent.get(k) for k in declared} | out
    missing = [k for k, t in declared.items()
               if k not in sent and k not in out and not isinstance(t, T.Optional)]
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
class Run:
    """One fn execution: its input; its run dir, its shim's pid and (when this runner started
    it) its Popen once spawned; its outputs once collected (None until then), or its error
    once it failed (a scattered step's failed item does not stop the others)."""

    inp: dict[str, Any]
    run_dir: Path | None = None
    proc: subprocess.Popen | None = None
    pid: int | None = None  # the shim's pid — the run's process group id
    result: dict[str, Any] | None = None
    error: str | None = None


@dataclass
class Active:
    """A running step or call: its runs (several when the step scatters). `fatal` is the error
    it fails with once its live runs are killed (an adoption-time decision, SPEC §6)."""

    fn: Fn
    project: str | None
    step: str  # the step id; "" for a call
    runs: list[Run]
    scatter: bool = False
    declared: dict[str, T.Type] = field(default_factory=dict)  # a step's own outputs
    ports: dict[str, Any] | None = None  # what an open fn is told about its step
    fatal: str | None = None

    def outputs(self) -> dict[str, Any]:
        if not self.scatter:
            return self.runs[0].result
        return {o: [run.result.get(o) for run in self.runs]
                for o in {**self.fn.outputs, **self.declared}}

    def kill(self) -> None:
        kill(*self.runs)


class Runner:
    def __init__(self, store: Store, kill_runs: bool = False):
        self.store = store
        self.kill_runs = kill_runs
        self.active: dict[tuple[str, ...], Active] = {}
        self._calls: dict[str, tuple[int, dict[str, dict[str, Any]]]] = {}  # log -> seq, live
        self._reported: dict[str, str] = {}
        self._wake = threading.Event()
        self._stop = threading.Event()
        self._adopted = False  # startup adoption happens once (SPEC §6)
        self._started = ""  # runner.json: when this runner started beating
        self._beat_at = 0.0

    def wake(self) -> None:
        self._wake.set()

    def stop(self) -> None:
        self._stop.set()
        self._wake.set()

    def run_forever(self, interval: float = 1.0) -> None:
        """Tick until stop(), waking early after in-process edits. Runs are left running —
        the next runner adopts them — unless this runner was made with `kill_runs`.

        Holds SLUICE_HOME/runner.lock: a second runner on the same home is refused, so one
        runner owns adoption at a time. Writes runner.json (pid, started, beat) each beat:
        its staleness marks a runner as down.
        """
        self.store.home.mkdir(parents=True, exist_ok=True)
        fd = os.open(self.store.home / "runner.lock", os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(fd)
            raise BadRequest(f"another runner is active in {self.store.home}") from None
        self._started = now_iso()
        try:
            while not self._stop.is_set():
                try:
                    self._beat()
                    self.tick()
                except Exception:  # noqa: BLE001 - the loop must survive any one tick
                    traceback.print_exc()
                self._wake.wait(interval)
                self._wake.clear()
        finally:
            if self.kill_runs:
                kill(*(run for a in self.active.values() for run in a.runs))
            os.close(fd)

    def _beat(self) -> None:
        """Refresh the runner heartbeat (runner.json), at most once a second."""
        if (t := time.monotonic()) - self._beat_at < 1.0:
            return
        self._beat_at = t
        atomic_write_json(self.store.home / "runner.json",
                          {"pid": os.getpid(), "started": self._started, "beat": now_iso()})

    def _report(self, who: str, message: str) -> None:
        """Print a project's blocking problem once (until it changes) — with the traceback
        on the first report when it came out of an except, so a repeated one-line report
        cannot hide a real bug."""
        if self._reported.get(who) != message:
            self._reported[who] = message
            print(f"sluice runner: {who}: {message}", file=sys.stderr, flush=True)
            if sys.exc_info()[0] is not None:
                traceback.print_exc()

    def tick(self) -> bool:
        """One pass over every project and every call. Returns whether any state changed."""
        self._adopt()
        changed = False
        for project in self.store.project_names():
            try:
                changed |= self._tick_project(project)
            except (InvalidPlan, NotFound) as e:  # e.g. a fn dir went away; others still run
                errs = getattr(e, "errors", None)
                self._report(project, e.message + (f": {errs}" if errs else ""))
            except (OSError, ValueError, LookupError, TypeError, AttributeError) as e:
                # corrupt project files must not stop the others
                self._report(project, f"cannot read its files: {e}")
            try:
                self._tick_calls(project)
            except (OSError, ValueError, LookupError, TypeError, AttributeError) as e:
                self._report(project, f"cannot read its calls: {e}")
        self._tick_calls(None)
        return changed

    # ---- adoption (SPEC §6) ----

    def _adopt(self) -> None:
        """The startup pass, once: rebuild an Active for every entry the previous runner left
        `running`, then kill runs nothing references. Also what makes a bare `tick()` see
        leftovers — _poll re-does it lazily for anything missed."""
        if self._adopted:
            return
        self._adopted = True
        for project in self.store.project_names():
            try:
                with self.store.lock(project):
                    state = self.store.read_state(project)
                    dirty = False
                    for sid, e in (state.get("steps") or {}).items():
                        if isinstance(e, dict) and e.get("status") == "running":
                            done = e.get("done")
                            self._adopt_entry(("step", project, sid), e)
                            dirty |= e.get("done") != done
                    if dirty:
                        self.store.write_state(project, state)
            except Exception as ex:  # noqa: BLE001 - one bad project must not stop the rest
                self._report(project, f"adoption failed: {ex}")
        for phase in (self._adopt_calls, self._orphans):  # a failure must not skip a phase
            try:
                phase()
            except Exception:  # noqa: BLE001 - the startup pass must finish
                traceback.print_exc()

    def _adopt_calls(self) -> None:
        """Rebuild the Active of every non-direct call a previous runner left `running`
        (a direct call's run belongs to its caller — it dies or finishes with it)."""
        for project in (*self.store.project_names(), None):
            try:
                latest: dict[str, dict[str, Any]] = {}
                for rec in L.read(self.store.log_dir(project), kinds=["call"])["records"]:
                    latest[rec["call"]] = rec
                for call, rec in latest.items():
                    if rec.get("status") == "running" and not rec.get("direct"):
                        self._adopt_entry(("call", project or "", call), rec)
                        if rec.get("status") != "running":  # it was finished right there
                            C.record(self.store, project, rec)
            except Exception as ex:  # noqa: BLE001 - one log must not stop the others
                self._report(project or "home", f"call adoption failed: {ex}")

    def _adopt_entry(self, key: tuple[str, ...], e: dict[str, Any]) -> None:
        """Rebuild an Active for a `running` entry: runs with an exit.json collect their
        outputs, live shims get watched, and what neither shows decides the entry's `fatal`.
        One `run.adopt` record per run."""
        if key in self.active:
            return
        kind, pkey, name = key
        project = pkey or None
        if kind == "call":
            fn = self.store.registry(project).get(e.get("fn"))
            if fn is None:
                if L.RUN_ID_RE.match(name):
                    d = self.store.runs_dir(project) / name
                    kill(Run({}, run_dir=d, pid=_shim_pid(d)))
                return _finish(e, error=f"no fn {e.get('fn')!r}")
            a = Active(fn, project, "", [])
            run_ids = [name]
        else:
            step = self.store.plan(project)[1].steps.get(name)
            if step is None:
                return  # not in the plan any more: the tick drops it
            a = Active(step.fn, project, name, [], scatter=bool(step.scatter),
                       declared=step.declared)
            raw = e.get("run_ids")
            run_ids = raw if isinstance(raw, list) else []
            if not run_ids:  # `running` with nothing recorded: pre-change state
                a.fatal = RESTARTED
        records = []
        for i, rid in enumerate(run_ids):
            if not isinstance(rid, str) or not L.RUN_ID_RE.match(rid):
                # a malformed id: don't build a path from it
                d, outcome, code = None, "unknown", None
            else:
                d = self.store.runs_dir(project) / rid
                outcome, code = _probe(d)
            records.append({"kind": "run.adopt",
                            "step" if kind == "step" else "call": name,
                            "run": rid, "outcome": outcome})
            run = Run({}, run_dir=d)
            if outcome == "finished":
                run.result, err = read_run(a.fn, d, code, a.declared)
                if err:
                    if a.scatter:  # the item's failure; the other runs still stand
                        run.result, run.error = None, err
                    elif a.fatal is None:
                        a.fatal = err
            elif outcome == "watching":
                run.pid = _shim_pid(d)
            elif outcome == "unknown" and a.scatter:
                run.error = UNKNOWN
                kill(run)  # a fn child that outlived the shim still dies (SPEC §6)
            elif a.fatal is None:
                a.fatal = UNKNOWN if outcome == "unknown" else RESTARTED
            a.runs.append(run)
        if a.scatter:
            e["done"] = sum(_ended(r) for r in a.runs)
        self.active[key] = a
        if records:
            self.store.append(project, *records)

    def _orphans(self) -> None:
        """Kill runs whose shim (or a recorded fn child that outlived it) lives but which no
        `running` step entry or call references: the runner that spawned them died before
        recording them (SPEC §6). Candidates are collected before the references are read —
        a run's record always lands before its lock — so a run started while the sweep runs
        is either referenced then, or left for the next runner, never killed live."""
        for project in (*self.store.project_names(), None):
            try:
                root = self.store.runs_dir(project)
                cands = [d for d in sorted(root.iterdir())
                         if d.is_dir() and _run_alive(d)] if root.is_dir() else []
                live = self._referenced_runs(project)
                runs = [Run({}, run_dir=d) for d in cands if d.name not in live]
                for run in kill(*runs):  # log only what was really signalled
                    self.store.append(project,
                                      {"kind": "run.orphan", "run": run.run_dir.name})
            except Exception as ex:  # noqa: BLE001 - a failed sweep must not stop the runner
                self._report(project or "home", f"orphan sweep failed: {ex}")

    def _referenced_runs(self, project: str | None) -> set[str]:
        """Run ids (dirs under runs/) that a `running` step entry or `running` call claims."""
        refs: set[str] = set()
        if project is not None:
            steps = self.store.read_state(project).get("steps") or {}
            refs |= {r for e in steps.values()
                     if isinstance(e, dict) and e.get("status") == "running"
                     for r in e.get("run_ids") or [] if isinstance(r, str)}
        latest: dict[str, dict[str, Any]] = {}
        for rec in L.read(self.store.log_dir(project), kinds=["call"])["records"]:
            latest[rec["call"]] = rec
        refs |= {c for c, r in latest.items() if r.get("status") == "running"}
        return refs

    def _kill_step_runs(self, project: str, e: dict[str, Any]) -> None:
        """Kill the runs a `running` entry names when no Active tracks them — by the
        recorded shim pids while their locks are held, or by the recorded fn child when
        the shim is already dead."""
        kill(*[Run({}, run_dir=self.store.runs_dir(project) / rid)
               for rid in e.get("run_ids") or []
               if isinstance(rid, str) and L.RUN_ID_RE.match(rid)])

    def _persist(self, project: str, state: dict[str, Any], was: dict[str, Any],
                 before: str) -> str:
        """Append a step.status record for every status that changed since the last write,
        then write state.json — records first, so a crash between them leaves a record for
        a state that did not land (which re-runs), never a state change with no record.
        `was` advances past what was logged; returns the canonical the caller diffs."""
        if (now := canonical(state)) == before:
            return before
        st = state["steps"]
        records = []
        for sid, e in st.items():
            if e["status"] == was.get(sid):
                continue
            rec = {"kind": "step.status", "step": sid, "from": was.get(sid),
                   "to": e["status"]}
            if e["status"] == "failed":
                rec["error"] = e.get("error")
            if e["status"] == "skipped":
                rec["reason"] = e.get("skipped")
            if e["status"] in ("succeeded", "failed") and e.get("run_ids"):
                rec["run_ids"] = e["run_ids"]
            records.append(rec)
        if records:
            self.store.append(project, *records)
        self.store.write_state(project, state)
        was.clear()
        was.update({sid: e.get("status") for sid, e in st.items()})
        return now

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
                elif st[sid].get("status") == "running":
                    # a leftover entry adoption could not take (no such step): kill its runs
                    self._kill_step_runs(project, st[sid])
                del st[sid]
            for name in [n for n in state["inputs"] if n not in plan.inputs]:
                del state["inputs"][name]
            for sid in plan.steps:
                st.setdefault(sid, S.pending())
            for sid, e in st.items():
                if e["status"] == "running" and "cancel" in e:  # step_cancel asked to stop it
                    if (a := self.active.pop(("step", project, sid), None)) is not None:
                        a.kill()
                    else:  # adoption never took it: kill what its run_ids name anyway
                        self._kill_step_runs(project, e)
                    why = e.pop("cancel")
                    _finish(e, error="cancelled" + (f": {why}" if why != "cancelled" else ""))
                elif e["status"] == "running":
                    self._poll(("step", project, sid), e)
            held = self.store.paused(project)  # a paused project starts nothing
            holding = set(plan.steps) if held else {s for s, x in plan.steps.items() if x.paused}
            order = topo_order(plan)
            progress = True
            while progress:  # built-ins finish inline and can make more steps ready
                mark_stale(plan, state)  # before anything reads a result that no longer holds
                settle_skips(plan, state, holding)  # `when` false, or reads a skipped step
                progress = False
                if problems:
                    break
                for sid in order:
                    step = plan.steps[sid]
                    if st[sid]["status"] != "pending" or not is_ready(step, plan, state):
                        continue
                    if held or step.paused:
                        continue  # stays pending, its inputs held, until unpaused
                    if settle_skip(step, plan, state):  # its `when` says no, or it reads a skip
                        progress = True
                        continue
                    before = self._begin(project, step, plan, state, was, before)
                    progress = True
            return self._persist(project, state, was, before) != before

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
                if not C.alive(rec.get("pid"), rec.get("pid_start")):  # its process is gone
                    _finish(rec, error=C.GONE)
            elif rec["status"] == "running":
                self._poll(akey, rec)
            elif rec["status"] == "pending":
                logged = self._start_call(akey, rec, project)
                before = logged or before  # what _start_call already logged, if anything
            if rec["status"] != before:
                C.record(self.store, project, rec)
                live[call] = rec  # the record itself is picked up on the next pass

    def _start_call(self, key: tuple[str, ...], rec: dict[str, Any],
                    project: str | None) -> str | None:
        """Start a pending call; returns the status it already logged itself, else None."""
        reg = self.store.registry(project)
        if reg.blocking:
            return None  # stays pending until the functions are fixed
        fn = reg.get(rec["fn"])
        if fn is None:
            _finish(rec, error=f"no fn {rec['fn']!r}")
            return None
        inp = rec.get("inputs") or {}
        rec.update(status="running")
        if fn.native:
            outputs, err = run_native(fn, inp)
            _finish(rec, outputs=outputs, error=err or None)
            return None
        d = self.store.runs_dir(project) / rec["call"]
        a = Active(fn, project, "", [Run(inp, d)])
        try:
            a.runs[0].proc = spawn(fn, inp, d,
                                   fn_env(self.store, project, fn, "", rec["call"], d))
            a.runs[0].pid = a.runs[0].proc.pid
        except Exception as ex:  # noqa: BLE001 - a failed start fails the call
            _finish(rec, error=f"could not start the fn: {ex}")
            return None
        # the running record lands before the run is tracked: a crash here leaves the run
        # referenced by the log (adoptable), not an orphan the next runner would kill
        C.record(self.store, project, rec)
        self.active[key] = a
        return "running"

    # ---- one step ----

    def _begin(self, project: str, step: Step, plan: Plan, state: dict[str, Any],
               was: dict[str, Any], before: str) -> str:
        """Start a ready step's runs; returns the canonical the caller diffs, advanced when
        the spawn persist happens here."""
        inp = resolved_inputs(step, plan, state)
        h = inputs_hash(inp)
        kept = state["steps"][step.id].get("kept")  # a retried scatter's finished items
        e = state["steps"][step.id] = S.running(h)
        runs = [inp]
        if step.scatter:
            items = inp[step.scatter]
            if not isinstance(items, list):
                _finish(e, error=f"scatter input {step.scatter} is not an array")
                return before
            runs = [{**inp, step.scatter: item} for item in items]
        for i, run in enumerate(runs):
            errs = T.check_value(T.record_of(step.inputs), run, "inputs")
            if errs:
                where = f"run {i}: " if step.scatter else ""
                _finish(e, error=f"{where}inputs do not match the fn: " + "; ".join(errs))
                return before
        a = Active(step.fn, project, step.id, [Run(run) for run in runs],
                   scatter=bool(step.scatter), declared=step.declared,
                   ports=step.ports() if step.fn.open else None)
        # `kept` that still fits (same inputs, one run id and result per item) stands:
        # those items are not re-run; anything else re-runs every item as usual
        kept_runs = _kept(kept, h, len(runs)) if step.scatter else {}
        for i, (_, result) in kept_runs.items():
            a.runs[i].result = result
        if step.scatter:
            e.update(done=len(kept_runs), total=len(runs))
        if step.fn.native:
            for run in a.runs:
                if run.result is not None:
                    continue
                run.result, err = run_native(step.fn, run.inp)
                if err:
                    run.result = None
                    if not a.scatter:
                        _finish(e, error=err)
                        return before
                    run.error = err
            if step.scatter:
                e["done"] = sum(_ended(run) for run in a.runs)
        else:
            try:
                for i in range(len(a.runs)):
                    # run_ids stays index-aligned: a kept item keeps its old run id
                    e["run_ids"].append(kept_runs[i][0] if i in kept_runs
                                        else self._spawn_run(a, i))
            except Exception as ex:  # noqa: BLE001 - a failed start fails the step
                a.kill()
                _finish(e, error=f"could not start the fn: {ex}")
                return before
            # the spawn→persist window stays live: a crash here must not leave runs the
            # state doesn't know (the orphan sweep kills those), nor a state change
            # without its step.status record (SPEC §6)
            before = self._persist(project, state, was, before)
        if all(run.result is not None for run in a.runs):
            _finish(e, outputs=a.outputs())
            return before
        if a.scatter and all(_ended(run) for run in a.runs):
            _finish_scatter(a, e)
            return before
        self.active[("step", project, step.id)] = a
        return before

    def _spawn_run(self, a: Active, i: int) -> str:
        """Start run i of a scattered (or single-run) step; returns its run id."""
        assert a.project is not None
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime())
        run_id = f"{stamp}-{a.step}-{i}-{secrets.token_hex(2)}"
        run = a.runs[i]
        run.run_dir = run_dir = self.store.runs_dir(a.project) / run_id
        env = fn_env(self.store, a.project, a.fn, a.step, run_id, run_dir, a.ports)
        run.proc = spawn(a.fn, run.inp, run_dir, env)
        run.pid = run.proc.pid
        return run_id

    def _poll(self, key: tuple[str, ...], e: dict[str, Any]) -> None:
        a = self.active.get(key)
        if a is None:  # started by a runner that is gone: adopt it (SPEC §6)
            self._adopt_entry(key, e)
            a = self.active.get(key)
            if a is None:
                if e.get("status") == "running":
                    _finish(e, error=RESTARTED)
                return
        if a.fatal is not None:
            a.kill()
            del self.active[key]
            return _finish(e, error=a.fatal)
        for run in a.runs:
            if _ended(run):
                continue
            code = _run_code(run)
            if code is None:
                continue
            if isinstance(code, str):  # UNKNOWN: the shim died without writing exit.json
                if not a.scatter:
                    a.kill()
                    del self.active[key]
                    return _finish(e, error=code)
                run.error = code
                kill(run)  # a fn child that outlived the shim still dies (SPEC §6)
                continue
            outputs, err = read_run(a.fn, run.run_dir, code, a.declared)
            if err:
                if not a.scatter:
                    a.kill()
                    del self.active[key]
                    return _finish(e, error=err)
                run.error = err  # a scattered item's failure does not stop the others
                continue
            run.result = outputs
        if a.scatter:
            e["done"] = sum(_ended(run) for run in a.runs)
        if all(run.result is not None for run in a.runs):
            del self.active[key]
            _finish(e, outputs=a.outputs())
        elif a.scatter and all(_ended(run) for run in a.runs):
            del self.active[key]
            _finish_scatter(a, e)


def _finish(e: dict[str, Any], outputs: Any = None, error: str | None = None) -> None:
    e.update(S.failed(error) if error else S.succeeded(outputs))


def _ended(run: Run) -> bool:
    """Whether the run has ended: its outputs collected, or its failure recorded."""
    return run.result is not None or run.error is not None


def _scatter_error(runs: list[Run]) -> str:
    """The error a scattered step fails with once every run has ended: `run <i>: <err>`
    for one failed run, `<n> of <N> runs failed: ...` for several, in index order; each
    <err> is cut to one line of at most 200 characters."""
    bad = [(i, r.error.split("\n", 1)[0][:200])
           for i, r in enumerate(runs) if r.error is not None]
    if len(bad) == 1:
        return f"run {bad[0][0]}: {bad[0][1]}"
    return f"{len(bad)} of {len(runs)} runs failed: " + \
        "; ".join(f"run {i}: {err}" for i, err in bad)


def _finish_scatter(a: Active, e: dict[str, Any]) -> None:
    """Fail a scattered step whose every run has ended: the entry keeps `run_ids` and
    gains `results` — each item's outputs, null where it failed — for a retry to keep."""
    e["results"] = [run.result for run in a.runs]
    _finish(e, error=_scatter_error(a.runs))


def _kept(kept: Any, h: str, n: int) -> dict[int, tuple[str, dict[str, Any]]]:
    """The items a retried scattered step does not re-run — index -> (old run id, its
    outputs) — when the pending entry's `kept` still fits: the inputs hash as before and
    there is one run id and one result per item."""
    if not (isinstance(kept, dict) and kept.get("inputs_hash") == h
            and isinstance(kept.get("run_ids"), list) and len(kept["run_ids"]) == n
            and isinstance(kept.get("results"), list) and len(kept["results"]) == n):
        return {}
    return {i: (rid, res) for i, (rid, res)
            in enumerate(zip(kept["run_ids"], kept["results"], strict=True))
            if res is not None and isinstance(rid, str) and L.RUN_ID_RE.match(rid)}
