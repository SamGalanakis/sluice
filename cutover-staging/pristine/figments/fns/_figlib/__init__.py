"""Helpers shared by the figments project's fns. Each fn puts this dir's parent on sys.path."""

import re
from pathlib import Path

FORKS = Path("/workspace/kiln/figments/forks")
ISSUE_RE = re.compile(r"\b([A-Z][A-Z0-9]+-\d+)\b")
URL_RE = re.compile(r"https://linear\.app/\S+")


def text_file(text: str, run_dir: Path, name: str) -> str:
    """Write text to a file in the run dir (for tools that read markdown from a file)."""
    run_dir.mkdir(parents=True, exist_ok=True)
    path = run_dir / name
    path.write_text(text)
    return str(path)
