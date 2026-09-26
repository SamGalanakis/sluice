# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""decide.jev: POST the decision to the Jev service (SLUICE_JEV_URL / SLUICE_JEV_KEY)."""

import json
import os
import urllib.error
import urllib.request

from sluice.fn import Transient, run


def _call_jev(question, context, options):
    """POST the decision request to Jev and return (choice, p).

    PENDING: mapping not yet checked against the real Jev API.
    """
    url = os.environ.get("SLUICE_JEV_URL")
    key = os.environ.get("SLUICE_JEV_KEY")
    if not url or not key:
        raise RuntimeError("Jev is not configured: set SLUICE_JEV_URL and SLUICE_JEV_KEY")
    body = json.dumps(
        {"question": question, "context": context, "options": options}).encode()
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Authorization": f"Bearer {key}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            data = json.loads(resp.read())
    except urllib.error.HTTPError as e:
        if e.code == 429 or e.code >= 500:
            raise Transient(f"jev responded http {e.code}") from e
        raise
    return data["choice"], float(data["p"])


def main(inp, ctx):
    choice, prob = _call_jev(inp["question"], inp.get("context"), inp["options"])
    if choice not in inp["options"]:
        raise ValueError(f"choice {choice!r} not in options {inp['options']!r}")
    return {
        "choice": choice,
        "p": prob,
        "confident": prob >= (inp.get("threshold") or 0.8),
    }


if __name__ == "__main__":
    run(main)
