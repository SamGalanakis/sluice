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
import time
import traceback
from collections.abc import Callable
from dataclasses import dataclass
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

    def log(self, msg: str) -> None:
        print(msg, file=sys.stderr, flush=True)


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
