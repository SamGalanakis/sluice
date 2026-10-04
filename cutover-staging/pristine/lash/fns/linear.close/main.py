# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""linear.close: comment the evidence, then move the issue to its completed state."""

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from sluice.fn import run, sh

from _lashlib import text_file, linear_bin


def main(inp, ctx):
    linear = linear_bin()
    message = inp.get("message")
    issue = re.escape(inp["issue"])
    title = (message or "").strip().splitlines()[0] if (message or "").strip() else ""
    completes = bool(re.search(rf"\bCloses {issue}\b", message or "")
                     or re.search(rf"\({issue}\)\s*$", title))
    if message is not None and not completes:
        # a partial change ("Part of <issue>"): comment the evidence, leave the issue open
        if inp.get("evidence"):
            body = text_file(inp["evidence"], ctx.run_dir, "evidence.md")
            sh([linear, "issue", "comment", "add", inp["issue"], "--body-file", body],
               env={"NO_COLOR": "1"})
        return {"ok": True, "closed": False}
    if inp.get("evidence"):
        body = text_file(inp["evidence"], ctx.run_dir, "evidence.md")
        sh([linear, "issue", "comment", "add", inp["issue"], "--body-file", body],
           env={"NO_COLOR": "1"})
    sh([linear, "issue", "update", inp["issue"], "--state", inp.get("state") or "completed"],
       env={"NO_COLOR": "1"})
    return {"ok": True, "closed": True}


if __name__ == "__main__":
    run(main)
