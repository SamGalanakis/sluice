# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.decide: log a non-blocking orchestrator decision to REVISIT.md and the inbox."""

import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from sluice.fn import run, sh

REVISIT = "/workspace/notes/lash/tasks/takeover-66c735c3/REVISIT.md"
UI = """root = Stack([note, form])
note = Callout("Already decided and in effect. Nothing waits on this; override only if you disagree.", "info")
form = Form("o", [why], [accept, override])
why = Textarea("why", "Override with (only for Override)", null, null, null, 3)
accept = Button("Accept", "accept", {}, "secondary")
override = Button("Override", "override")"""


def main(inp, ctx):
    path = Path(inp.get("revisit") or REVISIT)
    text = path.read_text() if path.exists() else "# For Sam to revisit\n"
    n = max((int(m) for m in re.findall(r"^(\d+)\. ", text, re.M)), default=0) + 1
    entry = f"{n}. **{inp['title']}:** {inp['decision']}"
    if inp.get("alternative"):
        entry += f" *Alternative:* {inp['alternative']}"
    if inp.get("reversible"):
        entry += f" *Reversible:* {inp['reversible']}"
    if inp.get("refs"):
        entry += f" ({inp['refs']})"
    path.write_text(text.rstrip("\n") + "\n" + entry + "\n")
    body = "\n\n".join(x for x in [
        inp["decision"],
        inp.get("alternative") and f"**Alternative:** {inp['alternative']}",
        inp.get("reversible") and f"**Reversible:** {inp['reversible']}",
        inp.get("refs") and f"**Refs:** {inp['refs']}",
        f"Logged as #{n} in `{path}`."] if x)
    out = sh(["sluice", "tool", "inbox_post", json.dumps({
        "project": ctx.project or "lash", "title": f"Decided #{n}: {inp['title']}"[:200],
        "body": body, "ui": UI, "from": "orchestrator"})]).stdout
    return {"item": json.loads(out)["id"], "n": n}


if __name__ == "__main__":
    run(main)
