# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Run the pinned Rust CLI as an agent would, then submit declared fields."""
import os
from sluice_fn import run, sh


def main(inp, ctx):
    result = sh([os.environ["SLUICE_BIN"], "me"], check=False)
    if ctx.outputs:
        ctx.submit({name: f"v-{name}" for name in ctx.outputs})
    return {"me": result.stdout, "code": result.returncode, "err": result.stderr[-400:]}


if __name__ == "__main__":
    run(main)
