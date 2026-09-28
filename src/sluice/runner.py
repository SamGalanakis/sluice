"""The runner (SPEC §6): start ready steps and pending calls as processes, record outputs and
failures."""

from __future__ import annotations

import fcntl
import json
import os
import secrets
import shutil
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
from . import db
from . import log as L
from . import state as S
from . import types as T
from .db import Busy
from .errors import BadRequest, InvalidPlan, NotFound
from .fn import HOST_VARS
from .plan import (
    Plan,
    Step,
    inputs_hash,
    is_ready,
    mark_stale,
    read_files,
    resolved_inputs,
    settle_skip,
    settle_skips,
    topo_order,
)
from .registry import NATIVE, Fn
from .store import Store
from .util import atomic_write_json, canonical, now_iso, read_dotenv, tail_text

SRC_DIR = str(Path(sluice.__file__).resolve().parent.parent)
RESTARTED = "runner restarted"
UNKNOWN = "run outcome unknown (its supervisor died)"
NOT_STARTED = "not started (the runner stopped before it started the run)"
GC_EVERY = 60.0  # seconds between the runner's passes removing unreferenced run dirs
KILL_GRACE = 5.0  # seconds between SIGTERM and SIGKILL when stopping a fn
NATIVE_PROCESSES = "native-processes.json"


# ---- one fn execution (SPEC §4 process contract) ----------------------------------------


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


def _run_json(run_dir: Path, name: str) -> dict | None:
    """A run dir's JSON file as a dict; None when it is missing, unparsable or not one."""
    try:
        data = json.loads((run_dir / name).read_text())
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def _child_pid(run_dir: Path) -> int | None:
    """The fn's pid from child.json — written by the shim right after Popen — verified
    against its recorded /proc start time so a reused pid cannot pass for it."""
    data = _run_json(run_dir, "child.json")
    if data is None:
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
    return (lock_held(run_dir / "shim.lock") or _survivor_pgid(run_dir) is not None
            or bool(_native_roots(run_dir)))


def _proc_identity(pid: int) -> tuple[int, int, str] | None:
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        fields = stat[stat.rindex(")") + 2:].split()
        return int(fields[19]), int(fields[1]), fields[0]
    except (OSError, IndexError, ValueError):
        return None


def _native_roots(run_dir: Path) -> dict[int, int]:
    """Only the recorded native processes whose /proc start time still matches."""
    data = _run_json(run_dir, NATIVE_PROCESSES) or {}
    roots = {}
    for name in ("tmux_server", "engine", "app_scope", "app_server"):
        item = data.get(name)
        if not isinstance(item, dict):
            continue
        pid, started = item.get("pid"), item.get("start_time")
        if not isinstance(pid, int) or not isinstance(started, int):
            continue
        identity = _proc_identity(pid)
        if identity and identity[0] == started and identity[2] != "Z":
            roots[pid] = started
    return roots


def _reap_native(run_dir: Path | None) -> None:
    """Reap a native session even if its fn was SIGKILLed before its finally block."""
    if run_dir is None or not (run_dir / NATIVE_PROCESSES).exists():
        return
    roots = _native_roots(run_dir)
    data = _run_json(run_dir, NATIVE_PROCESSES) or {}
    tmux_rec = data.get("tmux_server")
    tmux_pid = tmux_rec.get("pid") if isinstance(tmux_rec, dict) else None
    tree = dict(roots)
    children: dict[int, list[tuple[int, int]]] = {}
    for d in Path("/proc").iterdir():
        if not d.name.isdigit():
            continue
        pid = int(d.name)
        if identity := _proc_identity(pid):
            started, parent, state = identity
            if state != "Z":
                children.setdefault(parent, []).append((pid, started))
    todo = list(roots)
    while todo:
        for pid, started in children.get(todo.pop(), []):
            if pid not in tree:
                tree[pid] = started
                todo.append(pid)
    if tmux_pid in roots:
        try:
            subprocess.run(["tmux", "-S", "tmux.sock", "kill-server"], cwd=run_dir,
                           stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL, timeout=0.75, check=False)
        except (OSError, subprocess.TimeoutExpired):
            pass
    for pid, started in tree.items():
        identity = _proc_identity(pid)
        if identity and identity[0] == started and identity[2] != "Z":
            try:
                os.kill(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
    if tmux_pid in roots:
        (run_dir / "tmux.sock").unlink(missing_ok=True)


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
    native = [run for run in runs if run.run_dir and _native_roots(run.run_dir)]
    for run in runs:
        _reap_native(run.run_dir)
    return list({id(run): run for run in [*(r for _, r in targets), *native]}.values())


def _read_exit(run_dir: Path) -> int | None:
    """A finished run's wait code from exit.json — its `code`, or `-signal` when it died by a
    signal (SPEC §4: exit.json is the only evidence a run is done). None while unfinished, or
    for a corrupt record."""
    data = _run_json(run_dir, "exit.json")
    if data is None:
        return None
    code, sig = data.get("code"), data.get("signal")
    return code if isinstance(code, int) else (-sig if isinstance(sig, int) else None)


def _exit_error(run_dir: Path) -> str | None:
    """The start failure a shim recorded in exit.json (code 127: the fn never ran)."""
    data = _run_json(run_dir, "exit.json")
    err = data.get("error") if data is not None else None
    return err if isinstance(err, str) else None


def _shim_pid(run_dir: Path) -> int | None:
    """The supervising shim's pid — also the run's process-group id — from shim.json."""
    data = _run_json(run_dir, "shim.json")
    pid = data.get("pid") if data is not None else None
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
        _reap_native(run.run_dir)
        return code if code is not None else UNKNOWN
    code = _read_exit(run.run_dir)
    if code is not None:
        _reap_native(run.run_dir)
        return code
    if lock_held(run.run_dir / "shim.lock"):
        return None  # running under another runner's shim
    code = _read_exit(run.run_dir)
    _reap_native(run.run_dir)
    return code if code is not None else UNKNOWN


def _probe(run_dir: Path) -> tuple[str, int | None]:
    """Classify a run dir a `running` entry references: finished / watching / unknown /
    restarted — the last meaning a pre-shim run dir (SPEC §6) — or `not started` when there
    is no dir at all (its dir is made before its process is started, so it never was).
    exit.json first and last; the shim may still be starting, so a bare dir gets one recheck
    before `restarted`."""
    if not run_dir.is_dir():
        return "not started", None
    if (code := _read_exit(run_dir)) is not None:
        _reap_native(run_dir)
        return "finished", code
    for attempt in range(2):
        has_shim = (run_dir / "shim.json").exists()
        if lock_held(run_dir / "shim.lock"):
            return "watching", None
        if (code := _read_exit(run_dir)) is not None:
            _reap_native(run_dir)
            return "finished", code
        if has_shim:
            _reap_native(run_dir)
            return "unknown", None
        if attempt == 0:
            time.sleep(0.3)
    _reap_native(run_dir)
    return "restarted", None


def read_run(fn: Fn, run_dir: Path, code: int, declared: dict[str, T.Type] | None = None,
             sent: dict[str, Any] | None = None) -> tuple[dict[str, Any], str]:
    """A finished process's outputs, or the error: exit code or type errors plus stderr tail.
    With `declared` (a step's own outputs), those come from `sent`, what was submitted
    (SPEC §5)."""
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
    return with_submitted(out, declared, sent or {}) if declared else (out, "")


def with_submitted(out: dict[str, Any], declared: dict[str, T.Type],
                   sent: dict[str, Any]) -> tuple[dict[str, Any], str]:
    """The step's outputs: the fn's own plus the declared ones, which the fn returns itself
    (an inline fn) or its agent submitted with step_submit (`sent`, the run's submission; the
    fn's own values win on a name they share). A required declared output given neither way
    fails the step."""
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
        code = proc.wait()
        _reap_native(d)
        outputs, err = read_run(fn, d, code)
    _finish(rec, outputs=outputs, error=err or None)
    C.record(store, project, rec, ("running",))
    return C.result(rec)


# ---- the loop ----------------------------------------------------------------------------


@dataclass
class Run:
    """One fn execution: its input; its run id and dir, its shim's pid and (when this runner
    started it) its Popen once spawned; its outputs once collected (None until then), or its
    error once it failed (a scattered step's failed item does not stop the others)."""

    inp: dict[str, Any]
    run_dir: Path | None = None
    proc: subprocess.Popen | None = None
    pid: int | None = None  # the shim's pid — the run's process group id
    result: dict[str, Any] | None = None
    error: str | None = None
    rid: Any = None  # its run id (as recorded: an adopted one may be malformed)


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

    def run_ids(self) -> list[Any]:
        return [run.rid for run in self.runs]

    def kill(self) -> None:
        kill(*self.runs)


class Runner:
    def __init__(self, store: Store, kill_runs: bool = False):
        self.store = store
        self.kill_runs = kill_runs
        self.active: dict[tuple[str, ...], Active] = {}
        self._reported: dict[str, str] = {}
        self._wake = threading.Event()
        self._stop = threading.Event()
        self._started_up = False  # the startup pass happens once (SPEC §6)
        self._started = ""  # runner.json: when this runner started beating
        self._beat_at = 0.0
        self._gc_at: float | None = None

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
            if sys.exc_info()[0] is not None and not isinstance(sys.exc_info()[1], Busy):
                traceback.print_exc()

    def tick(self) -> bool:
        """One pass over every project and every call. Returns whether any state changed."""
        if not self._started_up:
            self._started_up = True
            self._orphans()
        changed = False
        for project in self.store.project_names():
            try:
                changed |= self._tick_project(project)
            except Busy as e:  # retried on the next tick
                self._report(project, e.message)
            except (InvalidPlan, NotFound) as e:  # e.g. a fn dir went away; others still run
                errs = getattr(e, "errors", None)
                self._report(project, e.message + (f": {errs}" if errs else ""))
            except (OSError, ValueError, LookupError, TypeError, AttributeError) as e:
                # a broken project must not stop the others
                self._report(project, f"cannot read it: {e}")
            self._calls(project)
        self._calls(None)
        if self._gc_at is None or time.monotonic() - self._gc_at >= GC_EVERY:
            self._gc_at = time.monotonic()
            self._gc()
        return changed

    def _calls(self, project: str | None) -> None:
        try:
            self._tick_calls(project)
        except Busy as e:
            self._report(project or "home", e.message)
        except (NotFound, OSError, ValueError, LookupError, TypeError, AttributeError) as e:
            self._report(project or "home", f"cannot read its calls: {e}")

    # ---- adoption (SPEC §6) ----

    def _adopt_entry(self, key: tuple[str, ...], e: dict[str, Any],
                     plan: Plan | None = None) -> Active | None:
        """Rebuild an Active for a `running` step entry or call a previous runner left: runs
        with an exit.json collect their outputs, live shims get watched, a run with no dir was
        never started, and what none of these shows decides the entry's `fatal`. One
        `run.adopt` record per run; the Active counts once they are committed. Outside any
        transaction (it probes and kills). None when there is nothing to adopt with (the step
        left the plan, the call's fn is gone)."""
        kind, pkey, name = key
        project = pkey or None
        if kind == "call":
            fn = self.store.registry(project).get(e.get("fn"))
            if fn is None:
                if L.RUN_ID_RE.match(name):
                    d = self.store.runs_dir(project) / name
                    kill(Run({}, run_dir=d, pid=_shim_pid(d)))
                return None
            a = Active(fn, project, "", [])
            run_ids = [name]
        else:
            step = plan.steps.get(name) if plan is not None else None
            if step is None:
                return None  # not in the plan any more: the tick drops it
            a = Active(step.fn, project, name, [], scatter=bool(step.scatter),
                       declared=step.declared)
            raw = e.get("run_ids")
            run_ids = raw if isinstance(raw, list) else []
            if not run_ids:  # `running` with nothing recorded: pre-change state
                a.fatal = RESTARTED
        records = []
        for rid in run_ids:
            if not isinstance(rid, str) or not L.RUN_ID_RE.match(rid):
                # a malformed id: don't build a path from it
                d, outcome, code = None, "unknown", None
            else:
                d = self.store.runs_dir(project) / rid
                outcome, code = _probe(d)
            records.append({"kind": "run.adopt",
                            "step" if kind == "step" else "call": name,
                            "run": rid, "outcome": outcome})
            run = Run({}, run_dir=d, rid=rid)
            if outcome == "finished":
                run.result, err = read_run(a.fn, d, code, a.declared, self._sent(a, rid))
                if err:
                    if a.scatter:  # the item's failure; the other runs still stand
                        run.result, run.error = None, err
                    elif a.fatal is None:
                        a.fatal = err
            elif outcome == "watching":
                run.pid = _shim_pid(d)
            elif outcome in ("unknown", "not started") and a.scatter:
                run.error = UNKNOWN if outcome == "unknown" else NOT_STARTED
                if outcome == "unknown":
                    kill(run)  # a fn child that outlived the shim still dies (SPEC §6)
            elif a.fatal is None:
                a.fatal = {"unknown": UNKNOWN, "not started": NOT_STARTED}.get(outcome,
                                                                              RESTARTED)
            a.runs.append(run)
        if records:
            self.store.append(project, *records)
        self.active[key] = a
        return a

    def _sent(self, a: Active, rid: Any) -> dict[str, Any] | None:
        """What the agent of a step's run submitted, when the step declares outputs."""
        if not a.declared or not isinstance(rid, str):
            return None
        return self.store.submission(a.project, rid)

    def _orphans(self) -> None:
        """Kill runs whose shim (or a recorded fn child that outlived it) lives but which no
        `running` step entry or call references: the runner that spawned them died between
        starting them and a reference to them (SPEC §6) — only possible for runs from before
        reservations. Candidates are collected before the references are read — a run's
        reference always lands before its dir — so a run started while the sweep runs is
        either referenced then, or left for the next runner, never killed live."""
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
        """Run ids (dirs under runs/) that a `running` step entry or `running` call claims,
        from one snapshot."""
        refs: set[str] = set()
        with self.store.rx() as conn:
            if project is not None:
                steps = self.store.read_state(project).get("steps") or {}
                refs |= {r for e in steps.values()
                         if isinstance(e, dict) and e.get("status") == "running"
                         for r in e.get("run_ids") or [] if isinstance(r, str)}
            refs |= {r[0] for r in db.all_rows(
                conn, "SELECT call FROM calls WHERE project IS ? AND status = 'running'",
                (project,))}
        return refs

    def _gc(self) -> None:
        """At startup and about once a minute: finish the deleted projects' directory removals
        still pending (store.finish_deletions), remove whatever SLUICE_HOME/trash still holds,
        and remove every run dir nothing references (a retained record, a state entry, a call,
        a submission: log.refs) whose shim and fn are gone. Each log's dirs are listed before
        its references are read: a run's reference is committed before its dir is made, so a
        listed dir nothing references can never gain a reference again."""
        try:
            for name in self.store.finish_deletions():
                self._report(name, "deleted, but its directory could not be removed yet "
                                   "(retried)")
        except Exception as ex:  # noqa: BLE001 - retried on the next pass
            self._report("home", f"deleted projects' cleanup failed: {ex}")
        trash = self.store.home / "trash"
        for d in sorted(trash.iterdir()) if trash.is_dir() else []:
            shutil.rmtree(d, ignore_errors=True)
        ours = {run.rid for a in self.active.values() for run in a.runs}
        for project in (None, *self.store.project_names()):
            try:
                root = self.store.runs_dir(project)
                dirs = sorted(d for d in root.iterdir() if d.is_dir()) if root.is_dir() else []
                if not dirs:
                    continue
                with self.store.rx() as conn:
                    refs = L.refs(conn, project)
                for d in dirs:
                    if d.name not in refs and d.name not in ours and not _run_alive(d):
                        shutil.rmtree(d, ignore_errors=True)
            except Exception as ex:  # noqa: BLE001 - a failed pass is retried later
                self._report(project or "home", f"run dir cleanup failed: {ex}")

    def _kill_step_runs(self, project: str, e: dict[str, Any]) -> list[Run]:
        """The runs a `running` entry names, to kill when no Active tracks them — by the
        recorded shim pids while their locks are held, or by the recorded fn child when
        the shim is already dead."""
        return [Run({}, run_dir=self.store.runs_dir(project) / rid)
                for rid in e.get("run_ids") or []
                if isinstance(rid, str) and L.RUN_ID_RE.match(rid)]

    # ---- one project's steps ----

    def _persist(self, project: str, state: dict[str, Any], was: dict[str, Any],
                 before: str) -> bool:
        """Write the state and a step.status record for every status that changed since `was`,
        in the caller's transaction. Returns whether anything changed."""
        if canonical(state) == before:
            return False
        records = []
        for sid, e in state["steps"].items():
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
        self.store.write_state(project, state)
        if records:
            self.store.append(project, *records)
        return True

    def _tick_project(self, project: str) -> bool:
        """(1) collect what finished, outside any transaction; (2) one write transaction:
        apply it, settle, and reserve every ready step's launch (running, with its run ids);
        (3) outside it, stop cancelled steps and start the reserved runs; (4) one short write
        transaction records what (3) did. `active` changes only once the transaction that
        justifies it has committed."""
        self._collect(project)
        stop, launch, changed = self._settle(project)
        if not stop and not launch:
            return changed
        kill(*(run for runs in stop.values() for run in runs))
        self._launch(project, launch)
        return self._record(project, stop, launch) or changed

    def _collect(self, project: str) -> None:
        """Adopt the running entries no Active tracks, then read what finished into the
        Actives' runs, killing what a failed run leaves behind."""
        state = self.store.read_state(project)
        plan = None
        for sid, e in state["steps"].items():
            key = ("step", project, sid)
            if e.get("status") != "running" or "cancel" in e:
                continue
            if key not in self.active:
                plan = plan or self.store.plan(project)[1]
                self._adopt_entry(key, e, plan)
            if (a := self.active.get(key)) is not None:
                self._collect_runs(a)

    def _collect_runs(self, a: Active) -> None:
        for run in a.runs:
            if _ended(run) or run.run_dir is None:
                continue
            code = _run_code(run)
            if code is None:
                continue
            if isinstance(code, str):  # UNKNOWN: the shim died without writing exit.json
                run.error = code
                kill(run)  # a fn child that outlived the shim still dies (SPEC §6)
                continue
            outputs, err = read_run(a.fn, run.run_dir, code, a.declared, self._sent(a, run.rid))
            if err:
                run.error = err  # a scattered item's failure does not stop the others
            else:
                run.result = outputs
        if a.fatal is not None or (not a.scatter and a.runs and a.runs[0].error is not None):
            a.kill()

    def _apply(self, a: Active, e: dict[str, Any]) -> bool:
        """Carry the Active's collected runs into its state entry; returns whether the step
        (or call) is done with."""
        if a.fatal is not None:
            _finish(e, error=a.fatal)
            return True
        if a.scatter:
            e["done"] = sum(_ended(run) for run in a.runs)
        if all(run.result is not None for run in a.runs):
            _finish(e, outputs=a.outputs())
        elif not a.scatter and a.runs[0].error is not None:
            _finish(e, error=a.runs[0].error)
        elif a.scatter and all(_ended(run) for run in a.runs):
            _finish_scatter(a, e)
        else:
            return False
        return True

    def _settle(self, project: str) -> tuple[dict[str, list[Run]], dict[str, Active], bool]:
        """The tick's write transaction: returns the cancelled steps to stop (their runs), the
        reserved launches and whether the state changed."""
        done, stop, launch = [], {}, {}
        drop: list[Run] = []  # runs of steps that left the plan, killed after the commit
        with self.store.tx():
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
                if (a := self.active.get(("step", project, sid))) is not None:
                    drop += a.runs
                    done.append(("step", project, sid))
                elif st[sid].get("status") == "running":
                    # a leftover entry adoption could not take (no such step): kill its runs
                    drop += self._kill_step_runs(project, st[sid])
                del st[sid]
            for name in [n for n in state["inputs"] if n not in plan.inputs]:
                del state["inputs"][name]
            for sid in plan.steps:
                st.setdefault(sid, S.pending())
            for sid, e in st.items():
                if e["status"] != "running":
                    continue
                key = ("step", project, sid)
                a = self.active.get(key)
                if "cancel" in e:  # step_cancel asked to stop it: the flag stays until it is
                    stop[sid] = a.runs if a is not None else self._kill_step_runs(project, e)
                elif a is None:  # adoption could not take it
                    _finish(e, error=RESTARTED)
                elif a.run_ids() != (e.get("run_ids") or []):  # not the runs we track
                    drop += a.runs
                    done.append(key)
                elif self._apply(a, e):
                    done.append(key)
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
                    if step.fn.external:
                        continue  # done outside sluice: it waits to be settled by hand
                    if (a := self._begin(project, step, plan, state)) is not None:
                        launch[sid] = a
                    progress = True
            changed = self._persist(project, state, was, before)
        for key in done:
            self.active.pop(key, None)
        for sid, a in launch.items():
            self.active[("step", project, sid)] = a
        kill(*drop)
        return stop, launch, changed

    def _launch(self, project: str, launch: dict[str, Active]) -> None:
        """Start the reserved runs, outside any transaction: make each run's dir and spawn its
        shim. A start that raises is that run's failure. Each run's entry is read again just
        before its spawn: a run whose step was cancelled since, or that its entry no longer
        lists, is not started. A cancel committed between that read and the spawn is not
        seen here; the next tick stops that run (the flag stays until the stop is done)."""
        for sid, a in launch.items():
            for run in a.runs:
                if run.result is not None:
                    continue  # a kept item of a retried scatter
                e = self.store.read_state(project)["steps"].get(sid) or {}
                if "cancel" in e or run.rid not in (e.get("run_ids") or []):
                    break
                try:
                    env = fn_env(self.store, project, a.fn, sid, run.rid, run.run_dir, a.ports)
                    run.proc = spawn(a.fn, run.inp, run.run_dir, env)
                    run.pid = run.proc.pid
                except Exception as ex:  # noqa: BLE001 - a failed start fails the run
                    run.error = f"could not start the fn: {ex}"

    def _record(self, project: str, stop: dict[str, list[Run]],
                launch: dict[str, Active]) -> bool:
        """The tick's second write transaction: the stopped steps fail `cancelled`, and a
        launch that could not start (all of it, or a scattered item) is recorded."""
        done = []
        with self.store.tx():
            state = self.store.read_state(project)
            before = canonical(state)
            st = state["steps"]
            was = {sid: e.get("status") for sid, e in st.items()}
            for sid in stop:
                e = st.get(sid)
                if e is not None and e["status"] == "running" and "cancel" in e:
                    why = e.pop("cancel")
                    _finish(e, error="cancelled" + (f": {why}" if why != "cancelled" else ""))
                    done.append(sid)
            for sid, a in launch.items():
                e = st.get(sid)
                if e is not None and e["status"] == "running" and "cancel" not in e \
                        and a.run_ids() == e.get("run_ids") and self._apply(a, e):
                    done.append(sid)
            changed = self._persist(project, state, was, before)
        for sid in done:
            self.active.pop(("step", project, sid), None)
        return changed

    def _begin(self, project: str, step: Step, plan: Plan,
               state: dict[str, Any]) -> Active | None:
        """A ready step, inside the tick's transaction: a built-in runs inline and finishes;
        anything else is reserved — running, with a fresh run id per run (index-aligned, a
        kept item of a retried scatter keeping its own) — and returned, to be started once
        the reservation is committed. Bad inputs, or a file binding it cannot read, fail it
        at once."""
        inp = resolved_inputs(step, plan, state)
        h = inputs_hash(step, plan, state)  # a file binding hashes as its path, not its content
        kept = state["steps"][step.id].get("kept")  # a retried scatter's finished items
        e = state["steps"][step.id] = S.running(h)
        inp, err = read_files(step, inp)  # each start reads the files afresh
        if err:
            _finish(e, error=err)
            return None
        runs = [inp]
        if step.scatter:
            items = inp[step.scatter]
            if not isinstance(items, list):
                _finish(e, error=f"scatter input {step.scatter} is not an array")
                return None
            runs = [{**inp, step.scatter: item} for item in items]
        for i, run in enumerate(runs):
            errs = T.check_value(T.record_of(step.inputs), run, "inputs")
            if errs:
                where = f"run {i}: " if step.scatter else ""
                _finish(e, error=f"{where}inputs do not match the fn: " + "; ".join(errs))
                return None
        a = Active(step.fn, project, step.id, [Run(run) for run in runs],
                   scatter=bool(step.scatter), declared=step.declared,
                   ports=step.ports() if step.fn.open else None)
        # `kept` that still fits (same inputs, one run id and result per item) stands:
        # those items are not re-run; anything else re-runs every item as usual
        kept_runs = _kept(kept, h, len(runs)) if step.scatter else {}
        for i, (rid, result) in kept_runs.items():
            a.runs[i].rid, a.runs[i].result = rid, result
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
                        return None
                    run.error = err
        else:
            stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime())
            for i, run in enumerate(a.runs):
                if i not in kept_runs:
                    run.rid = f"{stamp}-{step.id}-{i}-{secrets.token_hex(2)}"
                run.run_dir = self.store.runs_dir(project) / run.rid
            e["run_ids"] = a.run_ids()
        if not self._apply(a, e):
            return a
        return None

    # ---- calls ----

    def _tick_calls(self, project: str | None) -> None:
        """Start pending calls and collect finished ones, from the `calls` rows still pending
        or running."""
        for rec in C.live(self.store, project):
            key = ("call", project or "", rec["call"])
            if rec["status"] == "pending":
                self._start_call(key, rec, project)
            elif rec.get("direct"):
                if not C.alive(rec.get("pid"), rec.get("pid_start")):  # its process is gone
                    _reap_native(self.store.runs_dir(project) / rec["call"])
                    _finish(rec, error=C.GONE)
                    C.record(self.store, project, rec, ("running",))
            else:
                a = self.active.get(key) or self._adopt_entry(key, rec)
                if a is None:  # its fn is gone
                    _finish(rec, error=f"no fn {rec['fn']!r}")
                else:
                    self._collect_runs(a)
                    if not self._apply(a, rec):
                        continue
                C.record(self.store, project, rec, ("running",))
                self.active.pop(key, None)

    def _start_call(self, key: tuple[str, ...], rec: dict[str, Any],
                    project: str | None) -> None:
        """Start a pending call: a built-in runs inline; anything else is reserved (its row
        running, committed), then spawned, and a start that raises fails it."""
        reg = self.store.registry(project)
        if reg.blocking:
            return  # stays pending until the functions are fixed
        fn = reg.get(rec["fn"])
        if fn is None:
            _finish(rec, error=f"no fn {rec['fn']!r}")
            C.record(self.store, project, rec, ("pending",))
            return
        inp = rec["inputs"]
        if fn.native:
            outputs, err = run_native(fn, inp)
            _finish(rec, outputs=outputs, error=err or None)
            C.record(self.store, project, rec, ("pending",))
            return
        call = rec["call"]
        d = self.store.runs_dir(project) / call
        a = Active(fn, project, "", [Run(inp, d, rid=call)])
        rec["status"] = "running"
        if not C.record(self.store, project, rec, ("pending",)):
            return
        self.active[key] = a
        try:
            a.runs[0].proc = spawn(fn, inp, d, fn_env(self.store, project, fn, "", call, d))
            a.runs[0].pid = a.runs[0].proc.pid
        except Exception as ex:  # noqa: BLE001 - a failed start fails the call
            # kept on the run too: a Busy result write is retried from it on the next tick
            a.runs[0].error = f"could not start the fn: {ex}"
            _finish(rec, error=a.runs[0].error)
            if C.record(self.store, project, rec, ("running",)):
                self.active.pop(key, None)


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
