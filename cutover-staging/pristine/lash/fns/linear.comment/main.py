# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""linear.comment: linear issue comment add."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from sluice.fn import run, sh

from _lashlib import text_file, linear_bin


def main(inp, ctx):
    body = text_file(inp["body"], ctx.run_dir, "comment.md")
    sh([linear_bin(), "issue", "comment", "add", inp["issue"],
        "--body-file", body], env={"NO_COLOR": "1"})
    return {"ok": True}


if __name__ == "__main__":
    run(main)
