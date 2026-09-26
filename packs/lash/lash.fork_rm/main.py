# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.fork_rm: `kiln rm lash <name>`."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "_lib"))

from lashkit import kiln

from sluice.fn import run, sh


def main(inp, ctx):
    sh([kiln(), "rm", "lash", inp["name"]])
    return {"removed": True}


if __name__ == "__main__":
    run(main)
