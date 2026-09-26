"""Helper for writing sluice functions (SPEC §8). Standard library only.

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
import traceback
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

TRANSIENT_EXIT = 75
_TAIL = 2000


class Transient(Exception):
    """Raise for failures worth retrying (capacity, rate limits, flaky infra). Exits 75."""


class ShError(Exception):
    def __init__(self, argv: list[str], code: int, stdout: str, stderr: str):
        self.argv, self.code, self.stdout, self.stderr = argv, code, stdout, stderr
        super().__init__(f"{argv[0]} exited {code}: {stderr.strip()[-_TAIL:]}")


@dataclass
class Context:
    plan: str
    node: str
    run_id: str
    run_dir: Path
    attempt: int
    idempotency_key: str
    home: Path
    fn_dir: Path
    _spawn_nodes: dict[str, Any] = field(default_factory=dict)
    _spawn_reasons: list[str] = field(default_factory=list)
    _forward: str | None = None

    def log(self, msg: str) -> None:
        print(msg, file=sys.stderr, flush=True)

    def spawn(self, nodes: dict[str, Any], reason: str, forward: str | None = None) -> None:
        """Ask the runner to add these nodes to the plan when this fn succeeds.

        With `forward`, this node is answered by that spawned node: dependents wait for it
        and read its outputs (SPEC §4.1).
        """
        clash = set(nodes) & set(self._spawn_nodes)
        if clash:
            raise ValueError(f"spawn ids used twice: {sorted(clash)}")
        if forward is not None:
            if forward not in nodes:
                raise ValueError(f"forward target {forward!r} is not among the spawned nodes")
            if self._forward is not None:
                raise ValueError("a node forwards to at most one spawned node")
            self._forward = forward
        self._spawn_nodes.update(nodes)
        self._spawn_reasons.append(reason)


def _context() -> Context:
    env = os.environ
    run_dir = Path(env.get("SLUICE_RUN_DIR", "."))
    return Context(
        plan=env.get("SLUICE_PLAN", ""),
        node=env.get("SLUICE_NODE", ""),
        run_id=env.get("SLUICE_RUN_ID", ""),
        run_dir=run_dir,
        attempt=int(env.get("SLUICE_ATTEMPT", "1")),
        idempotency_key=env.get("SLUICE_IDEMPOTENCY_KEY", ""),
        home=Path(env.get("SLUICE_HOME", str(Path.home() / ".sluice"))),
        fn_dir=Path(env.get("SLUICE_FN_DIR", ".")),
    )


def _write_json(path: Path, obj: Any) -> None:
    with contextlib.suppress(OSError):
        tmp = path.with_suffix(path.suffix + ".tmp")
        tmp.write_text(json.dumps(obj))
        os.replace(tmp, path)


def run(main: Callable[[dict[str, Any], Context], dict[str, Any]]) -> None:
    """Read the input from stdin, call main(inp, ctx), write the output, exit."""
    ctx = _context()
    raw = sys.stdin.read()
    inp = json.loads(raw) if raw.strip() else {}
    real_stdout = sys.stdout
    try:
        with contextlib.redirect_stdout(sys.stderr):
            out = main(inp, ctx)
        if out is None:
            out = {}
        if not isinstance(out, dict):
            raise TypeError(f"main must return a dict, got {type(out).__name__}")
        if ctx._spawn_nodes:
            spawn: dict[str, Any] = {"reason": "; ".join(ctx._spawn_reasons),
                                     "nodes": ctx._spawn_nodes}
            if ctx._forward is not None:
                spawn["forward"] = ctx._forward
            out = {**out, "_spawn": spawn}
    except Transient as e:
        print(f"transient: {e}", file=sys.stderr, flush=True)
        _write_json(ctx.run_dir / "error.json", {"type": "Transient", "message": str(e)})
        sys.exit(TRANSIENT_EXIT)
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
