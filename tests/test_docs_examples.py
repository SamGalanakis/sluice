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
    for page in ("plans", "examples", "threads", "inbox", "composing"):
        for i, block in enumerate(BLOCK.findall((DOCS / f"{page}.md").read_text())):
            doc = json.loads(block)
            if isinstance(doc, dict) and "steps" in doc and "params" not in doc:
                yield pytest.param(doc, id=f"{page}-{i}")


def test_doc_recipes_expand_and_validate(tmp_path):
    """A recipe in the docs expands with every param given and validates."""
    from sluice import recipe as RC

    found = 0
    for block in BLOCK.findall((DOCS / "plans.md").read_text()):
        doc = json.loads(block)
        if not (isinstance(doc, dict) and "params" in doc):
            continue
        path = tmp_path / f"{doc['name']}.json"
        path.write_text(block)
        r = RC.load(path, "global")
        assert r.errors == []
        sample = {"string": "/x/y", "int": 1, "boolean": True}
        params = {k: sample.get(str(t), "devin") for k, t in r.params.items() if k != "unit"}
        steps, errs = RC.expand(r, {"unit": "u", **params})
        assert errs == []
        errs, _ = P.validate({"steps": steps}, load({
            "builtin": [BUILTIN_DIR], "global": [PACKS / p for p in ("agents", "git")]}))
        assert errs == []
        found += 1
    assert found == 1


@pytest.mark.parametrize("doc", list(plans()))
def test_doc_plans_validate(doc):
    errs, _ = P.validate(doc, load({"builtin": [BUILTIN_DIR],
                                    "global": [PACKS / p for p in ("agents", "git", "jev")]}))
    assert errs == []


def test_there_are_plans_to_check():
    assert len(list(plans())) >= 4
    assert len([p for p in plans() if p.id.startswith("composing-")]) == 3
