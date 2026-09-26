# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import os

from sluice.fn import run


def main(inp, ctx):
    env = {k: v for k, v in os.environ.items() if k.startswith("SLUICE_")}
    return {"env": env, "cwd": os.getcwd(), "input": inp, "step": ctx.step}


if __name__ == "__main__":
    run(main)
