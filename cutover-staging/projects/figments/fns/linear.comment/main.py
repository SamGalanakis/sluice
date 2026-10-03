# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""linear.comment: linear issue comment add."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _figlib import text_file
from sluice_fn import run, sh


def main(inp, ctx):
    body = text_file(inp["body"], ctx.run_dir, "comment.md")
    sh(["linear", "issue", "comment", "add", inp["issue"],
        "--body-file", body], env={"NO_COLOR": "1"})
    return {"ok": True}


if __name__ == "__main__":
    run(main)
