# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""thread.wait: block until a message arrives on a thread, or the timeout passes."""

from sluice.fn import run
from sluice.fns._lib.threads import wait


def main(inp, ctx):
    timeout = inp.get("timeout")
    return wait(inp["thread"], inp.get("since_seq"), inp.get("to"),
                300 if timeout is None else timeout)


if __name__ == "__main__":
    run(main)
