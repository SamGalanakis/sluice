"""The runner loop (SPEC §7): reconcile, poll, apply spawns, start ready nodes, persist."""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import secrets
import subprocess
import sys
import threading
import time
import traceback
from collections import Counter
from pathlib import Path
from typing import Any

import sluice

from . import lifecycle as L
from . import types as T
from .errors import BadRequest, InvalidPlan
from .plan import ID_RE, ENode, Expanded
from .store import Store
from .util import (
    atomic_write_json,
    canonical,
    now_iso,
    parse_duration,
    read_json,
    sha256_json,
    tail_text,
)

SRC_DIR = str(Path(sluice.__file__).resolve().parent.parent)


def fn_env(store: Store, pid: str, nid: str, run_id: str, run_dir: Path, attempt: int,
           fn_dir: Path | None) -> dict[str, str]:
    """The SLUICE_* environment of a fn process (SPEC §4.1). Merged over the runner's env."""
    pythonpath = os.pathsep.join(p for p in (SRC_DIR, os.environ.get("PYTHONPATH")) if p)
    return {"SLUICE_HOME": str(store.home), "SLUICE_PLAN": pid, "SLUICE_NODE": nid,
            "SLUICE_RUN_ID": run_id, "SLUICE_RUN_DIR": str(run_dir),
            "SLUICE_ATTEMPT": str(attempt), "SLUICE_IDEMPOTENCY_KEY": f"{pid}/{nid}/{attempt}",
            "SLUICE_FN_DIR": str(fn_dir or ""), "PYTHONPATH": pythonpath}


def prepare_run(run_dir: Path, fn_dir: Path, inp: dict[str, Any], env: dict[str, str]) -> None:
    run_dir.mkdir(parents=True, exist_ok=True)
    atomic_write_json(run_dir / "cmd.json", {
        "argv": ["uv", "run", "--quiet", "--script", str(fn_dir / "main.py")],
        "env": env, "cwd": str(run_dir)})
    atomic_write_json(run_dir / "input.json", inp)


def launch(run_dir: Path) -> subprocess.Popen:
    """Start the launcher in its own session so fn processes survive a runner restart."""
    env = {**os.environ, "PYTHONPATH": os.pathsep.join(
        p for p in (SRC_DIR, os.environ.get("PYTHONPATH")) if p)}
    return subprocess.Popen([sys.executable, "-m", "sluice.launch", str(run_dir)],
                            start_new_session=True, stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            cwd=run_dir, env=env)


def read_exit(run_dir: Path) -> int | None:
    try:
        return int(read_json(run_dir / "exit.json")["code"])
    except (OSError, ValueError, KeyError, TypeError):
        return None


def read_output(run_dir: Path) -> tuple[dict[str, Any] | None, str]:
    """The fn's output object: output.json if present, else stdout. Returns (output, error)."""
    for name in ("output.json", "stdout.log"):
        path = run_dir / name
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8", errors="replace").strip()
        if not text and name == "output.json":
            continue
        try:
            out = json.loads(text)
        except json.JSONDecodeError:
            return None, f"{name} is not one JSON object: {text[:200]!r}"
        if not isinstance(out, dict):
            return None, f"{name} is not a JSON object"
        return out, ""
    return None, "the fn wrote no output"


def run_error(run_dir: Path, code: int) -> str:
    try:
        err = read_json(run_dir / "error.json")
        return f"{err.get('type', 'Error')}: {err.get('message', '')} (exit {code})"
    except (OSError, ValueError):
        return f"exit code {code}"


class Runner:
    def __init__(self, store: Store):
        self.store = store
        self.procs: dict[int, subprocess.Popen] = {}
        self._wake = threading.Event()
        self._stop = threading.Event()
        self._reported: dict[str, str] = {}
        self._home_fd: int | None = None

    # ---- loop control ----

    def wake(self, *_: Any) -> None:
        self._wake.set()

    def stop(self) -> None:
        self._stop.set()
        self._wake.set()

    def lock_home(self) -> None:
        """Take SLUICE_HOME/runner.lock so only one runner loop runs per home."""
        if self._home_fd is not None:
            return
        self.store.home.mkdir(parents=True, exist_ok=True)
        fd = os.open(self.store.home / "runner.lock", os.O_RDWR | os.O_CREAT, 0o644)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(fd)
            raise BadRequest(f"another runner is active in {self.store.home}") from None
        self._home_fd = fd

    def unlock_home(self) -> None:
        if self._home_fd is not None:
            os.close(self._home_fd)
            self._home_fd = None

    def run_forever(self) -> None:
        """Tick until stop(), waking early after in-process edits. Holds the home lock."""
        self.lock_home()
        interval = parse_duration(self.store.config["tick"])
        try:
            for pid in self.store.plan_ids():
                self.store.append_event(pid, "runner_started", data={"pid": os.getpid()})
            while not self._stop.is_set():
                try:
                    self.tick()
                except Exception:  # noqa: BLE001 - the loop must survive any one tick
                    traceback.print_exc()
                self._wake.wait(interval)
                self._wake.clear()
        finally:
            for pid in self.store.plan_ids():
                self.store.append_event(pid, "runner_stopped", data={"pid": os.getpid()})
            self.unlock_home()

    async def arun_forever(self) -> None:
        import anyio.to_thread

        try:
            await anyio.to_thread.run_sync(self.run_forever, abandon_on_cancel=True)
        finally:
            self.stop()

    # ---- one tick ----

    def tick(self) -> bool:
        """One pass over every plan. Returns whether any state changed."""
        self._reap()
        usage: dict[str, Counter[str]] = {}
        for pid in self.store.plan_ids():
            with contextlib.suppress(Exception):
                usage[pid] = L.slot_usage(self.store.read_state(pid)["nodes"])
        changed = False
        for pid in self.store.plan_ids():
            try:
                changed |= self._tick_plan(pid, usage)
            except Exception:  # noqa: BLE001 - one broken plan must not stop the others
                msg = traceback.format_exc()
                if self._reported.get(pid) != msg:
                    self._reported[pid] = msg
                    print(f"sluice runner: plan {pid}: {msg}", file=sys.stderr, flush=True)
        return changed

    def _reap(self) -> None:
        for pid, p in list(self.procs.items()):
            if p.poll() is not None:
                del self.procs[pid]

    def _alive(self, pid: int | None) -> bool:
        if pid in self.procs:
            return self.procs[pid].poll() is None
        return L.launcher_alive(pid)

    def _tick_plan(self, pid: str, usage: dict[str, Counter[str]]) -> bool:
        store = self.store
        with store.lock(pid):
            state = store.read_state(pid)
            nodes = state["nodes"]
            before = canonical(state)
            doc, exp = store.expanded(pid)
            others: Counter[str] = Counter()
            for p, c in usage.items():
                if p != pid:
                    others.update(c)
            self._reconcile(pid, exp, nodes)
            self._poll(pid, exp, state)
            rev = doc["rev"]
            for _ in range(len(exp.nodes) + 2):
                doc, exp = store.expanded(pid)
                if doc["rev"] != rev:
                    self._reconcile(pid, exp, nodes)
                    rev = doc["rev"]
                decisions = L.decide(store, pid, doc, exp, nodes, now=time.time(),
                                     slots_used=others + L.slot_usage(nodes))
                acted = False
                for d in decisions:
                    if d.action == "skip":
                        e = nodes[d.id]
                        e.update(status="skipped", skipped_by=d.skipped_by, finished=now_iso(),
                                 def_sha=exp.nodes[d.id].def_sha, claims_held=[])
                        store.append_event(pid, "node_skipped", d.id, {"reason": d.reason})
                        acted = True
                    elif d.action == "start":
                        self._start(pid, exp, state, exp.nodes[d.id], d)
                        acted = True
                self._update_composites(pid, exp, nodes)
                if not acted:
                    break
            state["plan_rev"] = doc["rev"]
            usage[pid] = L.slot_usage(nodes)
            if canonical(state) == before:
                return False
            store.write_state(pid, state)
            return True

    # ---- reconcile ----

    def _reconcile(self, pid: str, exp: Expanded, nodes: dict[str, Any]) -> None:
        store = self.store
        L.ensure_entries(exp, nodes)
        for nid in list(nodes):
            if nid in exp.nodes:
                continue
            e = nodes[nid]
            if not e.get("composite") and e["status"] in ("running", "waiting"):
                if e["status"] == "running":
                    L.kill_group(e.get("pid"), self.procs.pop(e.get("pid"), None))
                e.update(status="cancelled", finished=now_iso(), pid=None, claims_held=[])
                L.close_items(store, pid, nid, {"action": "cancel"}, "runner")
                store.append_event(pid, "node_cancelled", nid, {"reason": "removed from the plan"})
            else:
                del nodes[nid]
        for nid in exp.leaves():
            e = nodes[nid]
            sha = e.get("def_sha")
            if e["status"] in L.TERMINAL and sha and sha != exp.nodes[nid].def_sha:
                L.close_items(store, pid, nid, {"action": "retry"}, "runner")
                L.reset(e, bump=e["status"] != "skipped")
                e["def_sha"] = None
                store.append_event(pid, "node_retrying", nid, {"reason": "definition changed",
                                                              "attempt": e["attempt"]})

    # ---- poll ----

    def _poll(self, pid: str, exp: Expanded, state: dict[str, Any]) -> None:
        nodes = state["nodes"]
        for nid in exp.leaves():
            e = nodes[nid]
            if e["status"] != "running":
                continue
            en = exp.nodes[nid]
            run_dir = self.store.runs_dir / e["run_id"]
            code = read_exit(run_dir)
            if code is None:
                if time.time() > e.get("deadline", float("inf")):
                    L.kill_group(e.get("pid"), self.procs.pop(e.get("pid"), None))
                    self._fail(pid, en, e, f"timed out after {e.get('timeout_s')}s", run_dir)
                    continue
                if self._alive(e.get("pid")):
                    continue
                code = read_exit(run_dir)
                if code is None:
                    self._transient(pid, en, e, "lost: the launcher exited without exit.json",
                                    run_dir)
                    continue
            proc = self.procs.pop(e.get("pid"), None)
            if proc is not None:
                with contextlib.suppress(subprocess.TimeoutExpired):
                    proc.wait(timeout=5)
            self._finish(pid, exp, state, en, e, code, run_dir)

    def _finish(self, pid: str, exp: Expanded, state: dict[str, Any], en: ENode,
                e: dict[str, Any], code: int, run_dir: Path) -> None:
        if code == 75:
            self._transient(pid, en, e, run_error(run_dir, code), run_dir)
            return
        if code != 0:
            self._fail(pid, en, e, run_error(run_dir, code), run_dir)
            return
        out, err = read_output(run_dir)
        if out is None:
            self._fail(pid, en, e, err, run_dir)
            return
        spawn = out.pop("_spawn", None)
        errs = T.check_value(en.fn.out_record, out)
        if errs:
            self._fail(pid, en, e, "output does not match the fn's out type: " + "; ".join(errs),
                       run_dir)
            return
        if spawn is not None:
            errs = self._apply_spawn(pid, en.id, e, spawn, state)
            if errs:
                self._fail(pid, en, e, "invalid spawn: " + "; ".join(errs), run_dir)
                return
        if not en.fn.effects:
            key = cache_key(en.fn.name, en.fn.version, read_json(run_dir / "input.json"))
            atomic_write_json(self.store.cache_dir / f"{key}.json", out)
        L.succeed(self.store, pid, en.id, e, out)

    def _apply_spawn(self, pid: str, nid: str, e: dict[str, Any], spawn: Any,
                     state: dict[str, Any]) -> list[str]:
        if (not isinstance(spawn, dict) or set(spawn) - {"reason", "nodes"}
                or not isinstance(spawn.get("nodes"), dict)):
            return ['_spawn must be {"reason": str, "nodes": {id: node}}']
        new = spawn["nodes"]
        bad = [i for i in new if not isinstance(i, str) or not ID_RE.match(i)]
        if bad:
            return [f"nodes.{i}: node ids match {ID_RE.pattern}" for i in bad]
        doc = self.store.get(pid)
        run_id = e.get("run_id")
        reason = f"{spawn.get('reason') or 'spawn'} (run {run_id})"
        existing = [i for i in new if i in doc["nodes"]]
        if existing:
            done = any(h["author"] == f"node:{nid}" and f"(run {run_id})" in h["reason"]
                       for h in self.store.history(pid))
            if done and len(existing) == len(new):
                return []  # applied before a restart
            return [f"nodes.{i}: a node with this id already exists" for i in existing]
        ops = [{"op": "add", "path": f"/nodes/{i}", "value": node} for i, node in new.items()]
        try:
            rev = self.store.patch(pid, doc["rev"], ops, f"node:{nid}", reason, state=state)
        except InvalidPlan as ex:
            return ex.errors
        self.store.append_event(pid, "spawn_applied", nid, {"rev": rev, "nodes": list(new)})
        return []

    def _fail(self, pid: str, en: ENode, e: dict[str, Any], error: str,
              run_dir: Path | None, inp: Any = None) -> None:
        e.update(status="failed", finished=now_iso(), error=error, pid=None, claims_held=[])
        self.store.append_event(pid, "node_failed", en.id, {"error": error,
                                                           "attempt": e["attempt"]})
        if inp is None and run_dir is not None:
            with contextlib.suppress(OSError, ValueError):
                inp = read_json(run_dir / "input.json")
        item = self.store.inbox_open(pid, "failure", en.id, {
            "fn": en.fn.name, "attempt": e["attempt"], "error": error,
            "stderr_tail": tail_text(run_dir / "stderr.log") if run_dir else "",
            "input": inp, "run_dir": str(run_dir) if run_dir else None})
        e["item"] = item["id"]

    def _transient(self, pid: str, en: ENode, e: dict[str, Any], reason: str,
                   run_dir: Path) -> None:
        if e.get("retries", 0) >= en.fn.retry_transient:
            self._fail(pid, en, e, f"{reason} (transient; {e.get('retries', 0)} retries used)",
                       run_dir)
            return
        e["retries"] = e.get("retries", 0) + 1
        e["attempt"] += 1
        e.update(status="pending", pid=None, not_before=time.time() + en.fn.retry_backoff)
        self.store.append_event(pid, "node_retrying", en.id, {
            "attempt": e["attempt"], "reason": reason, "backoff_s": en.fn.retry_backoff})

    # ---- start ----

    def _start(self, pid: str, exp: Expanded, state: dict[str, Any], en: ENode,
               d: L.Decision) -> None:
        store, nodes = self.store, state["nodes"]
        e = nodes[en.id]
        now = time.time()
        for cid, claims in d.claims.items():
            nodes[cid]["claims_held"] = list(claims)
        e.update(def_sha=en.def_sha, started=now_iso(now), finished=None, error=None,
                 output=None, cache_hit=False, not_before=None, skipped_by=None, pid=None)
        vals = L.Values(store, pid, exp, nodes)
        try:
            inp = L.make_input(exp, en.id, vals)
        except (L.Unresolvable, OSError) as ex:
            self._fail(pid, en, e, f"input: {ex}", None, inp={})
            return
        errs = T.check_value(en.fn.in_record, inp)
        if errs:
            self._fail(pid, en, e, f"input does not match fn {en.fn.name}: " + "; ".join(errs),
                       None, inp=inp)
            return
        fn = en.fn
        if fn.native:
            self._native(pid, en, e, inp)
            return
        if not fn.effects:
            hit = store.cache_dir / f"{cache_key(fn.name, fn.version, inp)}.json"
            if hit.exists():
                with contextlib.suppress(OSError, ValueError):
                    L.succeed(store, pid, en.id, e, read_json(hit), cache_hit=True)
                    return
        stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime(now))
        run_id = f"{stamp}-{en.id.replace('/', '-')}-{e['attempt']}-{secrets.token_hex(2)}"
        run_dir = store.runs_dir / run_id
        assert fn.dir is not None
        prepare_run(run_dir, fn.dir, inp, fn_env(store, pid, en.id, run_id, run_dir,
                                                  e["attempt"], fn.dir))
        timeout = en.timeout if en.timeout is not None else fn.timeout
        try:
            proc = launch(run_dir)
        except OSError as ex:
            self._fail(pid, en, e, f"could not start the launcher: {ex}", run_dir)
            return
        self.procs[proc.pid] = proc
        e.update(status="running", run_id=run_id, pid=proc.pid, timeout_s=timeout,
                 deadline=now + timeout, slots=dict(fn.slots))
        store.append_event(pid, "node_started", en.id, {"run_id": run_id,
                                                        "attempt": e["attempt"]})

    def _native(self, pid: str, en: ENode, e: dict[str, Any], inp: dict[str, Any]) -> None:
        name = en.fn.name
        if name == "core.echo":
            L.succeed(self.store, pid, en.id, e, {"value": inp["value"]})
        elif name == "core.fail":
            self._fail(pid, en, e, str(inp["message"]), None, inp=inp)
        elif name == "core.ask":
            e.update(status="waiting")
            item = self.store.inbox_open(pid, "ask", en.id, {
                "question": inp["question"], "context": inp.get("context"),
                "to": inp.get("to") or "orchestrator"})
            e["item"] = item["id"]
            self.store.append_event(pid, "node_waiting", en.id, {"item": item["id"]})
        else:  # pragma: no cover - the registry defines exactly these natives
            self._fail(pid, en, e, f"unknown native fn {name}", None, inp=inp)

    # ---- composites ----

    def _update_composites(self, pid: str, exp: Expanded, nodes: dict[str, Any]) -> None:
        vals = L.Values(self.store, pid, exp, nodes)
        for cid in exp.composites_bottom_up():
            e = nodes[cid]
            st = vals.derive(cid)
            done = all(nodes[x]["status"] in L.TERMINAL for x in exp.leaves_under(cid))
            if done and e.get("claims_held"):
                e["claims_held"] = []
            if st == e["status"]:
                continue
            e["status"] = st
            if st != "pending" and not e.get("started"):
                e["started"] = now_iso()
                self.store.append_event(pid, "node_started", cid, {"composite": True})
            e["finished"] = now_iso() if st in L.TERMINAL else None
            if st in L.TERMINAL:
                self.store.append_event(pid, f"node_{st}", cid, {"composite": True})


def cache_key(name: str, version: int, inp: Any) -> str:
    return sha256_json([name, version, inp])
