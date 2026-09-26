# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""jev.score: rate a state on ordered levels."""

import sys
from pathlib import Path

from sluice.fn import run

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _jev.client import one


def main(inp, ctx):
    levels = inp["levels"]
    if not 2 <= len(levels) <= 10:
        raise ValueError(f"levels must have 2 to 10 entries, got {len(levels)}")
    answer, model = one(inp["state"], {"type": "score", "instructions": inp["instructions"],
                                       "criteria": levels}, inp.get("model"))
    return {"score": answer["score"], "probabilities": answer["probabilities"],
            "confidence": answer["confidence"], "legend": answer["legend"], "model": model}


if __name__ == "__main__":
    run(main, retries=3, backoff=5)
