# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import time

from sluice.fn import run


def main(inp, ctx):
    (ctx.run_dir / "started").write_text(str(time.time()))
    time.sleep(inp["seconds"])
    return {"slept": inp["seconds"], "tag": inp.get("tag")}


if __name__ == "__main__":
    run(main)
