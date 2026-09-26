# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""git.head: current branch and sha of a worktree."""

from sluice.fn import run, sh


def main(inp, ctx):
    path = inp["path"]
    branch = sh(["git", "-C", path, "rev-parse", "--abbrev-ref", "HEAD"]).stdout.strip()
    sha = sh(["git", "-C", path, "rev-parse", "HEAD"]).stdout.strip()
    return {"branch": branch, "sha": sha}


if __name__ == "__main__":
    run(main)
