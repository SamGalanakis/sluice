# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Post an owner decision note. An owner reply can override the decision."""

from sluice_fn import run


def main(inp, ctx):
    body = "\n\n".join(text for text in [
        inp["decision"],
        inp.get("alternative") and f"Alternative: {inp['alternative']}",
        inp.get("reversible") and f"Reversible: {inp['reversible']}",
        inp.get("refs") and f"Evidence: {inp['refs']}",
    ] if text)
    note = ctx.tool("message_post", {
        "thread": inp.get("thread"), "to": "owner", "needs_reply": False,
        "title": inp["title"][:200], "body": body,
    })
    return {"id": note["id"]}


if __name__ == "__main__":
    run(main)
