# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""linear.close: comment the evidence, then move the issue to its completed state."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _figlib import text_file
from sluice_fn import run, sh


def main(inp, ctx):
    linear = "linear"
    if inp.get("evidence"):
        body = text_file(inp["evidence"], ctx.run_dir, "evidence.md")
        sh([linear, "issue", "comment", "add", inp["issue"], "--body-file", body],
           env={"NO_COLOR": "1"})
    sh([linear, "issue", "update", inp["issue"], "--state", inp.get("state") or "completed"],
       env={"NO_COLOR": "1"})
    return {"ok": True}


if __name__ == "__main__":
    run(main)
