# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Submits like an agent would: the sluice CLI, run by TEST_PYTHON (a Python with sluice's
dependencies), with the run id from the environment."""
import json
import os
import subprocess

from sluice.fn import run

CLI = "import sys; from sluice.cli import main; sys.exit(main(sys.argv[1:]))"


def main(inp, ctx):
    results = []
    for outputs in inp["attempts"] or []:
        args = {"project": ctx.project, "step": ctx.step, "run": ctx.run_id, "outputs": outputs}
        p = subprocess.run([os.environ["TEST_PYTHON"], "-c", CLI, "tool", "step_submit",
                            json.dumps(args)], capture_output=True, text=True, check=False)
        results.append({"code": p.returncode, "out": json.loads(p.stdout or p.stderr)})
    return {"ports": {"inputs": ctx.extra_inputs, "outputs": ctx.outputs},
            "extra": {k: inp[k] for k in ctx.extra_inputs}, "results": results}


if __name__ == "__main__":
    run(main)
