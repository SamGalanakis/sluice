# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""jev.choice: pick one option for a state, with probabilities and confidence."""

from sluice.fn import run
from sluice.fns._lib.jev import one


def main(inp, ctx):
    options = inp["options"]
    criteria = {o: None for o in options} if isinstance(options, list) else options
    if not isinstance(criteria, dict) or not criteria:
        raise ValueError("options must be a non-empty list of names or map of name to description")
    answer, model = one(inp["state"], {"type": "choice", "instructions": inp["instructions"],
                                       "criteria": criteria}, inp.get("model"))
    threshold = inp.get("min_confidence")
    threshold = 0.8 if threshold is None else threshold
    return {"choice": answer["choice"], "probabilities": answer["probabilities"],
            "confidence": answer["confidence"], "confident": answer["confidence"] >= threshold,
            "model": model}


if __name__ == "__main__":
    run(main, retries=3, backoff=5)
