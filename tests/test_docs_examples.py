"""Every JSON plan in the agent docs validates against the built-in fns, so the docs cannot rot."""

import json
import re

import pytest

from sluice import plan as P
from sluice.fns import BUILTIN_DIR, Registry
from sluice.mcp_server import DOCS

BLOCK = re.compile(r"```json\n(.*?)```", re.DOTALL)


def plans():
    for page in ("plans", "examples"):
        for i, block in enumerate(BLOCK.findall((DOCS / f"{page}.md").read_text())):
            doc = json.loads(block)
            if isinstance(doc, dict) and "steps" in doc:
                yield pytest.param(doc, id=f"{page}-{i}")


@pytest.mark.parametrize("doc", list(plans()))
def test_doc_plans_validate(doc):
    errs, _ = P.validate({"id": "doc", **doc}, Registry.load([BUILTIN_DIR]))
    assert errs == []


def test_there_are_plans_to_check():
    assert len(list(plans())) >= 3
