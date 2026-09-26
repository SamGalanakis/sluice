# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""gh.pr: create or update the open PR for a head branch via the gh CLI."""

import json

from sluice.fn import run, sh


def main(inp, ctx):
    path = inp["path"]
    listed = sh(
        ["gh", "pr", "list", "--head", inp["head"], "--json", "number,url"],
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
            "--base", inp["base"],
            "--head", inp["head"],
            "--title", inp["title"],
            "--body", inp["body"],
        ]
        if inp.get("draft"):
            argv.append("--draft")
        sh(argv, cwd=path)
    data = json.loads(
        sh(["gh", "pr", "view", inp["head"], "--json", "number,url"], cwd=path).stdout)
    return {"number": data["number"], "url": data["url"]}


if __name__ == "__main__":
    run(main)
