# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import time

from sluice.fn import run


def main(inp, ctx):
    start = time.time()
    time.sleep(0.3)
    n = inp["round"]
    if n < inp["green_at"]:
        nxt = f"{inp['prefix']}-{n + 1}"
        ctx.spawn({nxt: {"fn": "test.gate", "claims": inp["claims"], "in": {
            "round": {"value": n + 1}, "green_at": {"value": inp["green_at"]},
            "prefix": {"value": inp["prefix"]}, "claims": {"value": inp["claims"]}}}},
            reason=f"round {n} red", forward=nxt)
        return {"round": n, "start": start, "end": time.time()}
    return {"round": n, "start": start, "end": time.time()}


if __name__ == "__main__":
    run(main)
