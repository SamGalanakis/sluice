"""Every fn.json in packs/agents and packs/git must match SPEC section 11."""

import json
from pathlib import Path

PACKS = Path(__file__).resolve().parents[1]

REQUIRED_KEYS = {"name", "version", "description", "in", "out",
                 "effects", "timeout", "slots", "retry"}

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

META = {
    "agent.devin": ("4h", {"agent": 1}, {"transient": 3, "backoff": "10m"}, True),
    "agent.codex": ("4h", {"agent": 1}, {"transient": 3, "backoff": "10m"}, True),
    "agent.claude": ("4h", {"agent": 1}, {"transient": 3, "backoff": "10m"}, True),
    "agent.review": ("4h", {"agent": 1}, {"transient": 3, "backoff": "10m"}, True),
    "agent.run": ("4h", {"agent": 1}, {"transient": 3, "backoff": "10m"}, True),
    "decide.llm": ("5m", {"default": 1}, {"transient": 2, "backoff": "30s"}, True),
    "decide.jev": ("5m", {"default": 1}, {"transient": 2, "backoff": "30s"}, True),
    "git.worktree": ("10m", {"default": 1}, {"transient": 2, "backoff": "30s"}, True),
    "git.worktree_rm": ("10m", {"default": 1}, {"transient": 2, "backoff": "30s"},
                      True),
    "git.head": ("10m", {"default": 1}, {"transient": 2, "backoff": "30s"}, False),
    "git.merge": ("10m", {"default": 1}, {"transient": 2, "backoff": "30s"}, True),
    "git.rebase": ("10m", {"default": 1}, {"transient": 2, "backoff": "30s"}, True),
    "git.push": ("10m", {"default": 1}, {"transient": 2, "backoff": "30s"}, True),
    "gh.pr": ("10m", {"default": 1}, {"transient": 2, "backoff": "30s"}, True),
}


def _all_fns():
    found = []
    for pack in ("agents", "git"):
        for fn_json in sorted((PACKS / pack).glob("*/fn.json")):
            found.append((pack, fn_json.parent.name, fn_json))
    return found


def test_every_expected_fn_exists():
    found = {(pack, name) for pack, name, _ in _all_fns()}
    expected = {(pack, name) for pack, fns in EXPECTED.items() for name in fns}
    assert found == expected


def test_fn_json_matches_spec():
    for pack, name, path in _all_fns():
        doc = json.loads(path.read_text())
        assert REQUIRED_KEYS <= doc.keys(), f"{path}: missing keys"
        assert doc["name"] == name
        assert doc["version"] == 1
        assert isinstance(doc["description"], str) and doc["description"]
        exp_in, exp_out = EXPECTED[pack][name]
        assert doc["in"] == exp_in, f"{name} in: {doc['in']}"
        assert doc["out"] == exp_out, f"{name} out: {doc['out']}"
        timeout, slots, retry, effects = META[name]
        assert doc["timeout"] == timeout
        assert doc["slots"] == slots
        assert doc["retry"] == retry
        assert doc["effects"] is effects
