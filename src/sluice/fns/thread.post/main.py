# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""thread.post: append a message to a thread of the project."""

from sluice.fn import run
from sluice.fns._lib.threads import post


def main(inp, ctx):
    return {"seq": post(inp["thread"], inp["body"], inp["from"], inp.get("to"),
                        inp.get("data"), inp.get("needs_reply") is not False)}


if __name__ == "__main__":
    run(main)
