# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""linear.create: linear issue create."""

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _figlib import ISSUE_RE, URL_RE, text_file
from sluice_fn import run, sh

DEFAULT_TEAM = "FIG"  # figments Linear team


def team_of(inp):
    """The team: given, else the parent issue's (FIG-12 -> FIG), else LINEAR_TEAM, else FIG."""
    if inp.get("team"):
        return inp["team"]
    parent = ISSUE_RE.search(inp.get("parent") or "")
    if parent:
        return parent.group(1).rsplit("-", 1)[0]
    return os.environ.get("LINEAR_TEAM") or DEFAULT_TEAM


def main(inp, ctx):
    linear = "linear"
    argv = [linear, "issue", "create", "--no-interactive", "--title", inp["title"],
            "--team", team_of(inp),
            "--description-file", text_file(inp.get("description") or "", ctx.run_dir,
                                            "description.md")]
    for flag in ("parent", "project", "state"):
        if inp.get(flag):
            argv += [f"--{flag}", inp[flag]]
    for label in inp.get("labels") or []:
        argv += ["--label", label]
    out = sh(argv, env={"NO_COLOR": "1"}).stdout
    ids = ISSUE_RE.findall(out)
    if not ids:
        raise RuntimeError(f"no issue id in the linear output: {out.strip()[:500]!r}")
    urls = URL_RE.findall(out)
    url = urls[0] if urls else sh([linear, "issue", "url", ids[0]],
                                  env={"NO_COLOR": "1"}).stdout.strip()
    return {"id": ids[0], "url": url}


if __name__ == "__main__":
    run(main)
