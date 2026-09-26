"""Minimal stdlib client for TypeSafe's System One API (Jev), shared by the jev.* fns.

POST {TYPESAFE_BASE_URL or https://api.typesafe.ai}/v1/systemone with a bearer key from
TYPESAFE_API_KEY. See https://docs.typesafe.ai/api.md.
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from typing import Any

from sluice.fn import Transient

DEFAULT_BASE = "https://api.typesafe.ai"
DEFAULT_MODEL = "jev-latest"


def ask(state: Any, questions: dict[str, Any], model: str | None = None,
        timeout: float = 120) -> dict[str, Any]:
    """Evaluate `state` against named questions; return the response body."""
    key = os.environ.get("TYPESAFE_API_KEY")
    if not key:
        raise RuntimeError("TYPESAFE_API_KEY is not set: put it in the environment of sluice "
                           "(e.g. $SLUICE_HOME/.env)")
    base = os.environ.get("TYPESAFE_BASE_URL", DEFAULT_BASE).rstrip("/")
    body = {"state": state, "model": model or os.environ.get("TYPESAFE_MODEL", DEFAULT_MODEL),
            "questions": questions}
    req = urllib.request.Request(
        f"{base}/v1/systemone", data=json.dumps(body).encode(), method="POST",
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")[:2000]
        if e.code in (429, 529) or e.code >= 500:
            raise Transient(f"typesafe {e.code}: {detail}") from None
        raise RuntimeError(f"typesafe {e.code}: {detail}") from None
    except urllib.error.URLError as e:
        raise Transient(f"typesafe unreachable: {e.reason}") from None


def one(state: Any, question: dict[str, Any], model: str | None = None) -> tuple[dict, str]:
    """Ask a single question; return (answer, model that answered)."""
    resp = ask(state, {"q": question}, model)
    return resp["answers"]["q"], resp.get("model", "")
