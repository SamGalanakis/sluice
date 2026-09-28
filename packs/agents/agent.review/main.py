# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.review: review `git diff <base>...HEAD` against the standards file, fix issues."""

import sys
from pathlib import Path

from sluice.fn import run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _agents.native import run_claude

PROMPT = """\
You are reviewing a branch in the git repository at your working directory.

Review the diff `git diff {base}...HEAD` against the coding standards file at
{standards} (read it first, then the diff).{notes}

Rules:
- Fix every problem you find by editing files and committing. Use
  plain-sentence commit messages. Never add AI attribution of any kind: no
  Co-Authored-By trailers, no "Generated with" lines, no mention of any AI
  tool — not in commits, comments, docs, or tickets.
- Hunt for tautological tests and tests that cannot fail; fix or delete them.
- Do not rewrite history, do not force-push, do not push.
- Your final message is a short prose report of only what you could not fix."""


def main(inp, ctx):
    cwd = inp["cwd"]
    before = sh(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
    notes = ""
    if inp.get("notes"):
        notes = "\n\nAdditional notes from the caller:\n" + inp["notes"]
    prompt = PROMPT.format(base=inp["base"], standards=inp["standards"], notes=notes)
    out = run_claude(prompt, inp, ctx, cwd)
    sha = sh(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
    commits = int(
        sh(["git", "rev-list", "--count", f"{before}..{sha}"], cwd=cwd).stdout.strip())
    return {"summary": out["final"], "sha": sha, "commits": commits,
            "session": out["session"]}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
