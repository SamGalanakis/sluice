"""Every JSON plan in the agent docs validates against the built-in fns and the first-party
packs the docs reference, so the docs cannot rot."""

import json
import re
from pathlib import Path

import pytest

from sluice import plan as P
from sluice.mcp_server import DOCS
from sluice.registry import BUILTIN_DIR, load

PACKS = Path(__file__).resolve().parents[1] / "packs"
BLOCK = re.compile(r"```json\n(.*?)```", re.DOTALL)


def plans():
    for page in ("plans", "examples", "threads"):
        for i, block in enumerate(BLOCK.findall((DOCS / f"{page}.md").read_text())):
            doc = json.loads(block)
            if isinstance(doc, dict) and "steps" in doc:
                yield pytest.param(doc, id=f"{page}-{i}")


@pytest.mark.parametrize("doc", list(plans()))
def test_doc_plans_validate(doc):
    errs, _ = P.validate(doc, load({"builtin": [BUILTIN_DIR],
                                    "global": [PACKS / p for p in ("agents", "git", "jev")]}))
    assert errs == []


def test_there_are_plans_to_check():
    assert len(list(plans())) >= 4
