# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import time

from sluice.fn import run


def main(inp, ctx):
    amount = 1 if inp.get("amount") is None else inp["amount"]
    with ctx.acquire(inp["resource"], amount):
        start = time.time()
        (ctx.run_dir / "held").write_text("")
        while not (ctx.run_dir / "go").exists():
            time.sleep(0.05)
        if (ctx.run_dir / "fail").exists():
            raise RuntimeError("failed while holding")
        end = time.time()
    (ctx.run_dir / "held").unlink()
    return {"start": start, "end": end, "tag": inp.get("tag")}


if __name__ == "__main__":
    run(main)
