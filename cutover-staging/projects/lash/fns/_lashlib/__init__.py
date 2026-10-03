"""Helpers shared by the lash project's fns. Each fn puts this dir's parent on sys.path."""

import re
import time
from pathlib import Path

from sluice_fn import ShError, sh

NETWORK_ERRORS = ("unable to access", "Connection reset", "Could not resolve host", "early EOF",
                  "The remote end hung up", "Connection timed out")


def net_sh(argv, attempts=5, **kw):
    """sh for an idempotent command that talks to GitHub, retried on a transient network error
    (a reset connection must not fail a fork or a land)."""
    for attempt in range(attempts):
        try:
            return sh(argv, **kw)
        except ShError as e:
            if attempt == attempts - 1 or not any(n in str(e) for n in NETWORK_ERRORS):
                raise
            time.sleep(5 * (attempt + 1))


def on_main(repo: str | Path, sha: str, fetch: bool = True) -> bool:
    """Whether `sha` is on origin/main of `repo` (after fetching it)."""
    if fetch:
        net_sh(["git", "-C", str(repo), "fetch", "-q", "origin", "main"])
    r = sh(["git", "-C", str(repo), "merge-base", "--is-ancestor", sha, "origin/main"],
           check=False)
    return r.returncode == 0


def text_file(text: str, run_dir: Path, name: str) -> str:
    """Write text to a file in the run dir (for tools that read markdown from a file)."""
    run_dir.mkdir(parents=True, exist_ok=True)
    path = run_dir / name
    path.write_text(text)
    return str(path)


ISSUE_RE = re.compile(r"\b([A-Z][A-Z0-9]+-\d+)\b")
URL_RE = re.compile(r"https://linear\.app/\S+")


def linear_bin() -> str:
    """The linear CLI, found even when the runner's PATH lacks the npm global bin."""
    import shutil
    found = shutil.which("linear")
    if found:
        return found
    for p in (Path.home() / ".npm-global/bin/linear", Path("/usr/local/bin/linear")):
        if p.exists():
            return str(p)
    raise RuntimeError("the linear CLI is not on PATH or in ~/.npm-global/bin")
