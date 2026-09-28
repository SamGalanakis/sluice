# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import time

from sluice.fn import run


def main(inp, ctx):
    while not (ctx.run_dir / "go").exists():
        time.sleep(0.05)
    if (ctx.run_dir / "fail").exists():
        raise RuntimeError("gate says no")
    return {"tag": inp["tag"]}


if __name__ == "__main__":
    run(main)
