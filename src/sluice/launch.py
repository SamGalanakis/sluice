"""The launcher (SPEC §7): `python -m sluice.launch <run_dir>`. Standard library only.

Reads cmd.json (argv, env, cwd), runs the child with stdin from input.json, stdout to
stdout.log and stderr to stderr.log, then writes exit.json ({"code", "at"}) atomically.
The runner starts it in a new session, so the fn keeps running if the runner restarts.
"""

from __future__ import annotations

import datetime
import json
import os
import subprocess
import sys
from pathlib import Path


def _write_exit(run_dir: Path, code: int) -> None:
    at = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ")
    tmp = run_dir / "exit.json.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump({"code": code, "at": at}, f)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, run_dir / "exit.json")


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: python -m sluice.launch <run_dir>", file=sys.stderr)
        return 2
    run_dir = Path(argv[1]).resolve()
    cmd = json.loads((run_dir / "cmd.json").read_text(encoding="utf-8"))
    env = {**os.environ, **cmd.get("env", {})}
    input_path = run_dir / "input.json"
    with (open(input_path, "rb") if input_path.exists() else open(os.devnull, "rb")) as stdin, \
            open(run_dir / "stdout.log", "wb") as out, open(run_dir / "stderr.log", "wb") as err:
        try:
            proc = subprocess.Popen(cmd["argv"], stdin=stdin, stdout=out, stderr=err, env=env,
                                    cwd=cmd.get("cwd") or run_dir)
            code = proc.wait()
        except OSError as e:
            err.write(f"sluice.launch: could not start {cmd['argv'][:1]}: {e}\n".encode())
            code = 127
    if code < 0:
        code = 128 - code  # killed by a signal
    _write_exit(run_dir, code)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
