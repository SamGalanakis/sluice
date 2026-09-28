# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import time

from sluice.fn import run


def main(inp, ctx):
    while not (ctx.run_dir / "go").exists():
        time.sleep(0.05)
    return {"value": inp.get("value")}


if __name__ == "__main__":
    run(main)
