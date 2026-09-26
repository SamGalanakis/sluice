# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""jev.noul: probability that the answer to a yes/no question is yes."""

import sys
from pathlib import Path

from sluice.fn import run

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _jev.client import one


def main(inp, ctx):
    question = {"type": "noul", "instructions": inp["instructions"]}
    criteria = {k: inp[v] for k, v in (("true", "yes"), ("false", "no")) if inp.get(v) is not None}
    if criteria:
        question["criteria"] = criteria
    answer, model = one(inp["state"], question, inp.get("model"))
    return {"noul": answer["noul"], "model": model}


if __name__ == "__main__":
    run(main, retries=3, backoff=5)
