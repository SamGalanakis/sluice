# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""inbox.ask: post an item to the project's inbox and wait for a person to answer it."""

import time

from sluice import db, inbox
from sluice import log as L
from sluice.fn import run
from sluice.fns._lib.threads import project_log


def ask(ctx, title, body=None, ui=None, interval=0.5):
    """Post this step's item and poll it, or take up the step's own earlier one with the same
    title (a failed, cancelled or restarted run's, or one answered while nobody waited)."""
    home, project = project_log()
    sender, run = (ctx.step, ctx.run_id or None) if ctx.step else (f"call {ctx.run_id}", None)
    with db.write(home) as conn:
        item = inbox.ask(conn, project, L.cap_of(home), title, body, ui, sender, run)
    ctx.log(f"waiting for an answer to inbox item {item['id']}")
    while True:
        with db.read(home) as conn:
            item = inbox.find(conn, project, item["id"])
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
