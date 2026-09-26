"""Every fn.json in packs/agents and packs/git must match SPEC section 11."""

import json
from pathlib import Path

FNS = Path(__file__).resolve().parents[2] / "src" / "sluice" / "fns"

ALLOWED_KEYS = {"name", "description", "in", "out"}

AGENT_IO = ({"cwd": "string", "spec": "string", "log": "string?",
             "resume": "string?", "report_path": "string?"},
            {"log": "string", "final": "string", "report": "string?"})

DECIDE_IO = ({"question": "string", "context": "any?",
              "options": {"list": "string"}, "threshold": "float?"},
             {"choice": "string", "p": "float", "confident": "bool"})

EXPECTED = {
    "agents": {
        "agent.devin": AGENT_IO,
        "agent.codex": (
            {"cwd": "string", "spec": "string",
             "model": {"optional": ["sol", "astra"]}, "log": "string?",
             "resume": "string?", "report_path": "string?"},
            AGENT_IO[1],
        ),
        "agent.claude": (
            {"cwd": "string", "prompt": "string", "model": "string?",
             "session": "string?"},
            {"result": "string", "session": "string", "cost_usd": "float?"},
        ),
        "agent.run": (
            {"engine": ["devin", "codex", "claude"], "cwd": "string",
             "spec": "string", "model": "string?", "resume": "string?",
             "report_path": "string?"},
            {"final": "string", "report": "string?", "session": "string?"},
        ),
        "agent.review": (
            {"cwd": "string", "base": "string", "standards": "string",
             "notes": "string?"},
            {"summary": "string", "sha": "string", "commits": "int"},
        ),
        "decide.llm": DECIDE_IO,
        "decide.jev": DECIDE_IO,
    },
    "git": {
        "git.worktree": (
            {"repo": "string", "base": "string", "branch": "string",
             "path": "string?"},
            {"path": "string", "branch": "string", "sha": "string"},
        ),
        "git.worktree_rm": (
            {"repo": "string", "path": "string", "force": "bool?"},
            {"removed": "bool"},
        ),
        "git.head": (
            {"path": "string"},
            {"branch": "string", "sha": "string"},
        ),
        "git.merge": (
            {"repo": "string", "source": "string", "target": "string",
             "message": "string?", "push": "bool?"},
            {"merged": "bool", "sha": "string?",
             "conflicts": {"list": "string"}},
        ),
        "git.rebase": (
            {"path": "string", "onto": "string"},
            {"ok": "bool", "sha": "string",
             "conflicts": {"list": "string"}},
        ),
        "git.push": (
            {"path": "string", "branch": "string", "remote": "string?",
             "force_with_lease": "bool?"},
            {"sha": "string"},
        ),
        "gh.pr": (
            {"path": "string", "base": "string", "head": "string",
             "title": "string", "body": "string", "draft": "bool?"},
            {"number": "int", "url": "string"},
        ),
    },
}

def _all_fns():
    found = []
    for fn_json in sorted(FNS.glob("*/fn.json")):
        name = fn_json.parent.name
        pack = next((p for p, fns in EXPECTED.items() if name in fns), None)
        found.append((pack, name, fn_json))
    return found


def test_every_expected_fn_exists():
    found = {(pack, name) for pack, name, _ in _all_fns()}
    expected = {(pack, name) for pack, fns in EXPECTED.items() for name in fns}
    assert found == expected


def test_fn_json_matches_spec():
    for pack, name, path in _all_fns():
        doc = json.loads(path.read_text())
        assert {"name", "in", "out"} <= doc.keys() <= ALLOWED_KEYS, f"{path}: keys {sorted(doc)}"
        assert doc["name"] == name
        assert isinstance(doc.get("description", ""), str)
        exp_in, exp_out = EXPECTED[pack][name]
        assert doc["in"] == exp_in, f"{name} in: {doc['in']}"
        assert doc["out"] == exp_out, f"{name} out: {doc['out']}"
