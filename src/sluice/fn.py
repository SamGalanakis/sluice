"""Helper for writing sluice functions (SPEC §7). Standard library only.

A fn's main.py looks like:

    from sluice.fn import run, Transient, sh

    def main(inp, ctx):
        return {"sha": sh(["git", "rev-parse", "HEAD"], cwd=inp["path"]).stdout.strip()}

    if __name__ == "__main__":
        run(main)
"""

from __future__ import annotations

import contextlib
import json
import os
import subprocess
import sys
import threading
import time
import traceback
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

_TAIL = 2000


class Transient(Exception):
    """Raise for failures worth retrying (capacity, rate limits, flaky infra).

    run(main, retries=N) calls main again up to N times after a Transient; past that the fn fails.
    """


class ShError(Exception):
    def __init__(self, argv: list[str], code: int, stdout: str, stderr: str):
        self.argv, self.code, self.stdout, self.stderr = argv, code, stdout, stderr
        super().__init__(f"{argv[0]} exited {code}: {stderr.strip()[-_TAIL:]}")


@dataclass
class Context:
    project: str
    step: str
    run_id: str
    run_dir: Path
    home: Path
    fn_dir: Path
    attempt: int = 1
    # An open fn's step (SPEC §5): the extra inputs it binds, {name: {"type"}} (their values
    # are in inp), and the outputs it declares, {name: {"type", "doc"}}, which the agent
    # submits with step_submit. Empty otherwise.
    extra_inputs: dict[str, dict[str, Any]] = field(default_factory=dict)
    outputs: dict[str, dict[str, Any]] = field(default_factory=dict)

    def log(self, msg: str) -> None:
        print(msg, file=sys.stderr, flush=True)


def _ports(name: str) -> dict[str, dict[str, Any]]:
    try:
        value = json.loads(os.environ.get(name) or "{}")
    except json.JSONDecodeError:
        return {}
    return value if isinstance(value, dict) else {}


def _context() -> Context:
    env = os.environ
    run_dir = Path(env.get("SLUICE_RUN_DIR", "."))
    return Context(
        project=env.get("SLUICE_PROJECT", ""),
        step=env.get("SLUICE_STEP", ""),
        run_id=env.get("SLUICE_RUN_ID", ""),
        run_dir=run_dir,
        home=Path(env.get("SLUICE_HOME", str(Path.home() / ".sluice"))),
        fn_dir=Path(env.get("SLUICE_FN_DIR", ".")),
        extra_inputs=_ports("SLUICE_STEP_INPUTS"),
        outputs=_ports("SLUICE_STEP_OUTPUTS"),
    )


def _write_json(path: Path, obj: Any) -> None:
    with contextlib.suppress(OSError):
        tmp = path.with_suffix(path.suffix + ".tmp")
        tmp.write_text(json.dumps(obj))
        os.replace(tmp, path)


def run(
    main: Callable[[dict[str, Any], Context], dict[str, Any]],
    retries: int = 0,
    backoff: float = 30.0,
) -> None:
    """Read the input from stdin, call main(inp, ctx), print the output as JSON, exit.

    On Transient, sleep `backoff` seconds (env SLUICE_BACKOFF overrides) and call main again,
    up to `retries` more times (ctx.attempt counts from 1). Any other exception, or running out
    of retries, exits 1.
    """
    ctx = _context()
    backoff = float(os.environ.get("SLUICE_BACKOFF", backoff))  # tests set 0
    raw = sys.stdin.read()
    inp = json.loads(raw) if raw.strip() else {}
    real_stdout = sys.stdout
    try:
        while True:
            try:
                with contextlib.redirect_stdout(sys.stderr):
                    out = main(inp, ctx)
                break
            except Transient as e:
                if ctx.attempt > retries:
                    raise
                print(f"transient (attempt {ctx.attempt}): {e}; retrying in {backoff}s",
                      file=sys.stderr, flush=True)
                time.sleep(backoff)
                ctx.attempt += 1
        if out is None:
            out = {}
        if not isinstance(out, dict):
            raise TypeError(f"main must return a dict, got {type(out).__name__}")
    except Exception as e:  # noqa: BLE001 - every failure is reported, then exits 1
        traceback.print_exc(file=sys.stderr)
        _write_json(ctx.run_dir / "error.json", {"type": type(e).__name__, "message": str(e)})
        sys.exit(1)
    _write_json(ctx.run_dir / "output.json", out)
    real_stdout.write(json.dumps(out))
    real_stdout.flush()


def sh(
    argv: list[str],
    cwd: str | Path | None = None,
    check: bool = True,
    env: dict[str, str] | None = None,
    timeout: float | None = None,
    input: str | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run a command, echo it and a tail of its output to stderr, raise ShError on failure."""
    print(f"$ {' '.join(argv)}" + (f"  (in {cwd})" if cwd else ""), file=sys.stderr, flush=True)
    full_env = {**os.environ, **env} if env else None
    p = subprocess.run(argv, cwd=cwd, env=full_env, timeout=timeout, input=input,
                       text=True, capture_output=True, check=False)
    for stream in (p.stdout, p.stderr):
        if stream.strip():
            print(stream[-_TAIL:].rstrip(), file=sys.stderr, flush=True)
    if check and p.returncode != 0:
        raise ShError(argv, p.returncode, p.stdout, p.stderr)
    return p


def echo_line(line: str, source: str) -> None:
    """sh_stream's default on_line: the line on stderr, on one line, cut to 200 chars."""
    line = " ".join(line.split())
    if line:
        print(line[:200], file=sys.stderr, flush=True)


def _follow(path: Path, pos: int, done: threading.Event,
            on_line: Callable[[str], None]) -> None:
    """Pass each line appended to `path` after byte `pos` to on_line until `done`, then the
    rest. Starts over when the file is truncated or replaced."""
    buf = b""
    while True:
        finished = done.is_set()
        with contextlib.suppress(OSError):
            size = path.stat().st_size
            if size < pos:
                pos, buf = 0, b""
            if size > pos:
                with open(path, "rb") as f:
                    f.seek(pos)
                    data = f.read(size - pos)
                pos += len(data)
                *lines, buf = (buf + data).split(b"\n")
                for line in lines:
                    on_line(line.decode(errors="replace"))
        if finished:
            if buf:
                on_line(buf.decode(errors="replace"))
            return
        done.wait(0.2)


def sh_stream(
    argv: list[str],
    on_line: Callable[[str, str], None] = echo_line,
    cwd: str | Path | None = None,
    check: bool = True,
    env: dict[str, str] | None = None,
    follow: str | Path | None = None,
) -> subprocess.CompletedProcess[str]:
    """Run a command like sh(), but call on_line(line, source) for each line as it arrives:
    source "stdout" or "stderr", or "follow" for a line appended to the file `follow` (a tool
    that logs to a file). The default echoes each line to stderr. stdin is /dev/null. Returns
    the full stdout and stderr; raises ShError on a non-zero exit when `check`."""
    print(f"$ {' '.join(argv)}" + (f"  (in {cwd})" if cwd else ""), file=sys.stderr, flush=True)
    full_env = {**os.environ, **env} if env else None
    lock = threading.Lock()
    out: dict[str, list[str]] = {"stdout": [], "stderr": []}

    def emit(line: str, source: str) -> None:
        with lock:
            on_line(line.rstrip("\r\n"), source)

    def pump(stream: Any, source: str) -> None:
        for line in stream:
            out[source].append(line)
            emit(line, source)

    if follow is not None:  # where the file ends before the command can write to it
        follow = Path(follow)
        start = follow.stat().st_size if follow.exists() else 0
    p = subprocess.Popen(argv, cwd=cwd, env=full_env, stdin=subprocess.DEVNULL,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                         errors="replace", bufsize=1)
    done = threading.Event()
    threads = [threading.Thread(target=pump, args=(p.stderr, "stderr"), daemon=True)]
    if follow is not None:
        threads.append(threading.Thread(
            target=_follow, args=(follow, start, done, lambda s: emit(s, "follow")),
            daemon=True))
    for t in threads:
        t.start()
    try:
        pump(p.stdout, "stdout")
        code = p.wait()
    except BaseException:
        p.kill()
        p.wait()
        raise
    finally:
        done.set()
        for t in threads:
            t.join()
    res = subprocess.CompletedProcess(argv, code, "".join(out["stdout"]), "".join(out["stderr"]))
    if check and code != 0:
        raise ShError(argv, code, res.stdout, res.stderr)
    return res
