# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""inbox.ask: post an item to the project's inbox and wait for a person to answer it."""

import time

from sluice import inbox
from sluice import log as L
from sluice.fn import run
from sluice.fns._lib.threads import project_log


def ask(ctx, title, body=None, ui=None, interval=0.5):
    """Post (or, after a runner restart, find again) this step's open item and poll it."""
    d, home = project_log()
    sender = ctx.step or f"call {ctx.run_id}"
    fields = {"title": title, "body": body, "ui": ui, "sender": sender}
    with L.flock(d / L.LOCK):
        same = [i for i in inbox.items(d) if i["status"] == "open" and i.get("from") == sender
                and (i["title"], i.get("body"), i.get("ui")) == (title, body, ui)]
        item = same[-1] if same else inbox.post(d, L.cap_of(home), **fields)
    ctx.log(f"waiting for an answer to inbox item {item['id']}")
    while True:
        item = inbox.find(d, item["id"])
        if item["status"] == "answered":
            return {"answer": item["answer"]}
        if item["status"] == "closed":
            why = f": {item['reason']}" if item.get("reason") else ""
            raise RuntimeError(f"inbox item {item['id']} was closed without an answer{why}")
        time.sleep(interval)


def main(inp, ctx):
    return ask(ctx, inp["title"], inp.get("body"), inp.get("ui"))


if __name__ == "__main__":
    run(main)
