# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.dev_test: scripts/dev-test.py in a fork, read back from its receipt."""

import json
import shlex
import time
from pathlib import Path

from sluice.fn import run, sh, sh_stream


def receipt_path(fork):
    out = sh(["git", "-C", fork, "rev-parse", "--path-format=absolute", "--git-path",
              "lash-validation"]).stdout.strip()
    return Path(out) / "latest.json"


def main(inp, ctx):
    fork = inp["fork"]
    args = ["--dependents"] if inp.get("dependents") else []
    args += ["--base", inp["base"]] if inp.get("base") else []
    args += ["--dry-run"] if inp.get("dry_run") else []
    started = time.time_ns()
    script = ". ./env.sh && python3 scripts/dev-test.py " + " ".join(map(shlex.quote, args))
    res = sh_stream(["bash", "-c", script.rstrip()], cwd=fork, check=False)
    tail = "\n".join((res.stdout + res.stderr).strip().splitlines()[-60:])
    if inp.get("dry_run"):
        if res.returncode != 0:
            raise RuntimeError(f"dev-test.py --dry-run exited {res.returncode}\n{tail}")
        planned, ok, code = json.loads(res.stdout), True, 0
    else:
        path = receipt_path(fork)
        receipt = json.loads(path.read_text()) if path.exists() else None
        if receipt is None or receipt["started_ns"] < started:
            raise RuntimeError(f"dev-test.py wrote no receipt for this run (exit "
                               f"{res.returncode})\n{tail}")
        planned, code = receipt["plan"], receipt["exit_code"]
        ok = code == 0 and receipt["inputs_unchanged"]
    commands = [shlex.join(c) for c in planned["commands"]]
    tested = any(c[:2] == ["kiln", "test"] for c in planned["commands"])
    if inp.get("require_ok") and not ok:
        why = f"exit {code}" if code else "the checkout changed during the run"
        raise RuntimeError(f"dev-test.py is red ({why}): {commands}\n{tail}")
    if inp.get("require_tests") and not tested:
        raise RuntimeError(f"dev-test.py selected no tests ({planned['selection']}): "
                           f"{commands or 'no commands'}")
    return {"ok": ok, "code": code, "selection": planned["selection"], "commands": commands,
            "tested": tested, "changed_files": planned["changed_files"], "tail": tail}


if __name__ == "__main__":
    run(main)
