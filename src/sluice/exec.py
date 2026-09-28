"""The run shim (SPEC §4): `python -m sluice.exec <run_dir> -- <argv>` supervises one fn so
its fate survives the runner that started it.

The shim holds an exclusive flock on <run_dir>/shim.lock for its whole life — a later runner
takes liveness from the lock, never the pid (pid reuse, zombies). It writes shim.json
{pid, started, argv} before spawning and child.json {pid, pid_start} right after, so a fn
that outlives its shim can still be found and stopped. Once the fn exits it writes
exit.json {code, signal, finished} — the only evidence a run is done. It ignores
SIGINT/SIGTERM/SIGHUP itself — a stop reaches the fn through the process group (the shim
leads it) and the shim stays to record how it went — then exits as the fn did.
"""

from __future__ import annotations

import fcntl
import os
import resource
import signal
import subprocess
import sys
import time
from pathlib import Path

from .calls import pid_start
from .util import atomic_write_json, now_iso

IGNORE = (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
LOCK_RETRY = 2.0  # s: a runner probing the lock must not make a starting shim refuse


def _defaults() -> None:
    """The child's signal setup: the shim's ignores must not survive the exec."""
    for sig in IGNORE:
        signal.signal(sig, signal.SIG_DFL)


def main(argv: list[str]) -> int:
    if "--" not in argv or argv.index("--") == 0 or argv.index("--") == len(argv) - 1:
        print("usage: python -m sluice.exec <run_dir> -- <argv>", file=sys.stderr)
        return 2
    run_dir = Path(argv[0])
    cmd = argv[argv.index("--") + 1:]
    lock = os.open(run_dir / "shim.lock", os.O_RDWR | os.O_CREAT, 0o644)
    deadline = time.monotonic() + LOCK_RETRY
    while True:  # runner-side probes take and drop the lock: wait them out briefly
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            break
        except BlockingIOError:
            if time.monotonic() >= deadline:
                print("sluice: another shim holds this run's lock", file=sys.stderr)
                return 2
            time.sleep(0.05)
    for sig in IGNORE:
        signal.signal(sig, signal.SIG_IGN)
    atomic_write_json(run_dir / "shim.json",
                      {"pid": os.getpid(), "started": now_iso(), "argv": cmd})
    try:
        with open(run_dir / "input.json", "rb") as stdin, \
                open(run_dir / "output.json", "wb") as stdout, \
                open(run_dir / "stderr.log", "wb") as stderr:
            proc = subprocess.Popen(cmd, stdin=stdin, stdout=stdout, stderr=stderr,
                                    cwd=run_dir,
                                    # single-threaded process: a preexec_fn is safe here
                                    preexec_fn=_defaults)  # noqa: PLW1509
            try:
                atomic_write_json(run_dir / "child.json",
                                  {"pid": proc.pid, "pid_start": pid_start(proc.pid)})
            except OSError:
                proc.kill()  # a child it cannot record must not run untracked
                raise
            code = proc.wait()
    except OSError as ex:  # the fn never ran: record it like exit 127 did
        print(f"sluice: could not start the fn: {ex}", file=sys.stderr)
        atomic_write_json(run_dir / "exit.json",
                          {"code": 127, "signal": None, "error": str(ex),
                           "finished": now_iso()})
        os.close(lock)
        return 127
    atomic_write_json(run_dir / "exit.json", {
        "code": code if code >= 0 else None,
        "signal": -code if code < 0 else None,
        "finished": now_iso()})
    os.close(lock)
    if code < 0:  # the fn died by a signal: die the same way so wait() tells the truth
        sig = -code
        try:
            resource.setrlimit(resource.RLIMIT_CORE, (0, 0))  # no core of the shim itself
        except (OSError, ValueError):
            pass
        if sig in IGNORE:
            signal.signal(sig, signal.SIG_DFL)
        os.kill(os.getpid(), sig)
        os._exit(128 + sig)
    return code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
