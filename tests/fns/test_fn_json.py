"""Every built-in fn.json declares exactly the agreed inputs and outputs (CWL type
spellings), and nothing else is built in (the other fns live in packs/, see
packs/tests/test_packs_fn_json.py)."""

import json
from pathlib import Path

FNS = Path(__file__).resolve().parents[2] / "src" / "sluice" / "fns"

ALLOWED_KEYS = {"name", "doc", "inputs", "outputs"}

EXPECTED = {
    "core.echo": [{"value": "Any"}, {"value": "Any"}],
    "core.collect": [{"items": "Any[]"}, {"items": "Any[]"}],
    "core.format": [{"template": "string", "values": "Any"}, {"text": "string"}],
    "thread.post": [{"thread": "string", "body": "string", "from": "string",
                     "to": "string?", "data": "Any?"},
                    {"seq": "int"}],
    "thread.wait": [{"thread": "string", "since_seq": "int?", "to": "string?",
                     "timeout": "int?"},
                    {"messages": "Any[]", "last_seq": "int"}],
    "inbox.ask": [{"title": "string", "body": "string?", "ui": "string?"},
                  {"answer": {"type": "record", "fields": {
                      "action": "string", "params": "Any?", "values": "Any?",
                      "text": "string?"}}}],
}


def _all_fns():
    return [(fn_json.parent.name, fn_json) for fn_json in sorted(FNS.glob("*/fn.json"))]


def test_only_the_expected_fns_are_built_in():
    assert {name for name, _ in _all_fns()} == set(EXPECTED)


def test_fn_json_matches_spec():
    for name, path in _all_fns():
        doc = json.loads(path.read_text())
        keys = doc.keys()
        assert {"name", "inputs", "outputs"} <= keys <= ALLOWED_KEYS, \
            f"{path}: keys {sorted(doc)}"
        assert doc["name"] == name
        assert isinstance(doc.get("doc", ""), str)
        exp_in, exp_out = EXPECTED[name]
        assert doc["inputs"] == exp_in, f"{name} inputs: {doc['inputs']}"
        assert doc["outputs"] == exp_out, f"{name} outputs: {doc['outputs']}"
