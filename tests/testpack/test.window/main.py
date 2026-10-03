# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import time

from sluice_fn import run


def main(inp, ctx):
    start = time.time()
    time.sleep(inp["seconds"])
    return {"start": start, "end": time.time(), "tag": inp["tag"]}


if __name__ == "__main__":
    run(main)
