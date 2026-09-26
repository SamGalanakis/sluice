# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.standards: where the reviewer's standards live (a composite cannot compute a path)."""

from pathlib import Path

from sluice.fn import run

STANDARDS = Path(__file__).resolve().parents[1] / "standards" / "coding-standards.md"


def main(inp, ctx):
    if not STANDARDS.is_file():
        raise FileNotFoundError(STANDARDS)
    return {"path": str(STANDARDS)}


if __name__ == "__main__":
    run(main)
