# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.review: review `git diff <base>...HEAD` against the standards file, fix issues."""

import json
import os

from sluice.fn import ShError, Transient, run, sh

TRANSIENT_MARKERS = ("rate limit", "overloaded", "529")

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


def claude(prompt, model, cwd, session=None):
    """Run claude -p and return the parsed JSON object it prints on stdout."""
    argv = [
        os.environ.get("SLUICE_CLAUDE_BIN", "claude"),
        "-p", prompt,
        "--model", model,
        "--output-format", "json",
        "--dangerously-skip-permissions",
    ]
    if session:
        argv += ["--resume", session]
    p = sh(argv, cwd=cwd, check=False)
    if p.returncode != 0:
        if any(m in p.stderr for m in TRANSIENT_MARKERS):
            raise Transient("claude hit a rate limit or capacity error")
        raise ShError(argv, p.returncode, p.stdout, p.stderr)
    return json.loads(p.stdout)


def main(inp, ctx):
    cwd = inp["cwd"]
    before = sh(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
    notes = ""
    if inp.get("notes"):
        notes = "\n\nAdditional notes from the caller:\n" + inp["notes"]
    prompt = PROMPT.format(base=inp["base"], standards=inp["standards"], notes=notes)
    data = claude(prompt, "opus", cwd)
    sha = sh(["git", "rev-parse", "HEAD"], cwd=cwd).stdout.strip()
    commits = int(
        sh(["git", "rev-list", "--count", f"{before}..{sha}"], cwd=cwd).stdout.strip())
    return {"summary": data["result"], "sha": sha, "commits": commits}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
