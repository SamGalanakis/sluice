# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""gh.pr: create or update the open PR for a head branch via the gh CLI."""

import json
import sys
from pathlib import Path

from sluice.fn import run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _git.refs import ref


def main(inp, ctx):
    path = inp["path"]
    head = ref("head", inp["head"])
    base = ref("base", inp["base"])
    listed = sh(
        ["gh", "pr", "list", "--head", head, "--json", "number,url"],
        cwd=path,
    ).stdout
    prs = json.loads(listed)
    if prs:
        sh(
            ["gh", "pr", "edit", str(prs[0]["number"]),
             "--title", inp["title"], "--body", inp["body"]],
            cwd=path,
        )
    else:
        argv = [
            "gh", "pr", "create",
            "--base", base,
            "--head", head,
            "--title", inp["title"],
            "--body", inp["body"],
        ]
        if inp.get("draft"):
            argv.append("--draft")
        sh(argv, cwd=path)
    data = json.loads(
        sh(["gh", "pr", "view", head, "--json", "number,url"], cwd=path).stdout)
    return {"number": data["number"], "url": data["url"]}


if __name__ == "__main__":
    run(main)
