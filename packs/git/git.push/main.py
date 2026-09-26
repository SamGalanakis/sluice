# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""git.push: push HEAD:<branch> to a remote (default origin)."""

from sluice.fn import run, sh


def main(inp, ctx):
    path = inp["path"]
    argv = ["git", "-C", path, "push"]
    if inp.get("force_with_lease"):
        argv.append("--force-with-lease")
    argv += [inp.get("remote") or "origin", f"HEAD:{inp['branch']}"]
    sh(argv)
    sha = sh(["git", "-C", path, "rev-parse", "HEAD"]).stdout.strip()
    return {"sha": sha}


if __name__ == "__main__":
    run(main)
