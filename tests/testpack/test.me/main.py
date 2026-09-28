# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Runs `sluice me` like an agent at a checkpoint would (the sluice CLI under TEST_PYTHON),
then submits each output the step declares so the run can finish."""
import json
import os
import subprocess

from sluice.fn import run

CLI = "import sys; from sluice.cli import main; sys.exit(main(sys.argv[1:]))"


def main(inp, ctx):
    p = subprocess.run([os.environ["TEST_PYTHON"], "-c", CLI, "me"],
                       capture_output=True, text=True, check=False)
    if ctx.outputs:
        args = {"project": ctx.project, "step": ctx.step, "run": ctx.run_id,
                "outputs": {name: f"v-{name}" for name in ctx.outputs}}
        subprocess.run([os.environ["TEST_PYTHON"], "-c", CLI, "tool", "step_submit",
                        json.dumps(args)], capture_output=True, text=True, check=False)
    return {"me": p.stdout, "code": p.returncode, "err": p.stderr[-400:]}


if __name__ == "__main__":
    run(main)
