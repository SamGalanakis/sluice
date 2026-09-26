"""Every built-in fn.json declares exactly the agreed inputs and outputs (CWL type spellings)."""

import json
from pathlib import Path

FNS = Path(__file__).resolve().parents[2] / "src" / "sluice" / "fns"

ALLOWED_KEYS = {"name", "doc", "inputs", "outputs"}

EXPECTED = {
    "agents": {
        "agent.devin": [
            {
                "cwd": "string",
                "spec": "string",
                "log": "string?",
                "resume": "string?",
                "report_path": "string?"
            },
            {
                "log": "string",
                "final": "string",
                "report": "string?"
            }
        ],
        "agent.codex": [
            {
                "cwd": "string",
                "spec": "string",
                "model": [
                    "null",
                    {
                        "type": "enum",
                        "symbols": [
                            "sol",
                            "astra"
                        ]
                    }
                ],
                "log": "string?",
                "resume": "string?",
                "report_path": "string?"
            },
            {
                "log": "string",
                "final": "string",
                "report": "string?"
            }
        ],
        "agent.claude": [
            {
                "cwd": "string",
                "prompt": "string",
                "model": "string?",
                "session": "string?"
            },
            {
                "result": "string",
                "session": "string",
                "cost_usd": "float?"
            }
        ],
        "agent.run": [
            {
                "engine": {
                    "type": "enum",
                    "symbols": [
                        "devin",
                        "codex",
                        "claude"
                    ]
                },
                "cwd": "string",
                "spec": "string",
                "model": "string?",
                "resume": "string?",
                "report_path": "string?"
            },
            {
                "final": "string",
                "report": "string?",
                "session": "string?"
            }
        ],
        "agent.review": [
            {
                "cwd": "string",
                "base": "string",
                "standards": "string",
                "notes": "string?"
            },
            {
                "summary": "string",
                "sha": "string",
                "commits": "int"
            }
        ],
        "decide.llm": [
            {
                "question": "string",
                "context": "Any?",
                "options": "string[]",
                "threshold": "float?"
            },
            {
                "choice": "string",
                "p": "float",
                "confident": "boolean"
            }
        ],
        "jev.ask": [{"state": "Any", "questions": "Any", "model": "string?"},
                    {"answers": "Any", "model": "string", "usage": "Any"}],
        "jev.choice": [{"state": "Any", "instructions": "Any", "options": "Any",
                        "min_confidence": "float?", "model": "string?"},
                       {"choice": "string", "probabilities": "Any", "confidence": "float",
                        "confident": "boolean", "model": "string"}],
        "jev.score": [{"state": "Any", "instructions": "Any", "levels": "Any[]", "model": "string?"},
                      {"score": "float", "probabilities": "Any", "confidence": "float",
                       "legend": "Any", "model": "string"}],
        "jev.noul": [{"state": "Any", "instructions": "Any", "yes": "Any?", "no": "Any?",
                      "model": "string?"},
                     {"noul": "float", "model": "string"}],
    },
    "git": {
        "git.worktree": [
            {
                "repo": "string",
                "base": "string",
                "branch": "string",
                "path": "string?"
            },
            {
                "path": "string",
                "branch": "string",
                "sha": "string"
            }
        ],
        "git.worktree_rm": [
            {
                "repo": "string",
                "path": "string",
                "force": "boolean?"
            },
            {
                "removed": "boolean"
            }
        ],
        "git.head": [
            {
                "path": "string"
            },
            {
                "branch": "string",
                "sha": "string"
            }
        ],
        "git.merge": [
            {
                "repo": "string",
                "source": "string",
                "target": "string",
                "message": "string?",
                "push": "boolean?"
            },
            {
                "merged": "boolean",
                "sha": "string?",
                "conflicts": "string[]"
            }
        ],
        "git.rebase": [
            {
                "path": "string",
                "onto": "string"
            },
            {
                "ok": "boolean",
                "sha": "string",
                "conflicts": "string[]"
            }
        ],
        "git.push": [
            {
                "path": "string",
                "branch": "string",
                "remote": "string?",
                "force_with_lease": "boolean?"
            },
            {
                "sha": "string"
            }
        ],
        "gh.pr": [
            {
                "path": "string",
                "base": "string",
                "head": "string",
                "title": "string",
                "body": "string",
                "draft": "boolean?"
            },
            {
                "number": "int",
                "url": "string"
            }
        ]
    }
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
        keys = doc.keys()
        assert {"name", "inputs", "outputs"} <= keys <= ALLOWED_KEYS, f"{path}: keys {sorted(doc)}"
        assert doc["name"] == name
        assert isinstance(doc.get("doc", ""), str)
        exp_in, exp_out = EXPECTED[pack][name]
        assert doc["inputs"] == exp_in, f"{name} inputs: {doc['inputs']}"
        assert doc["outputs"] == exp_out, f"{name} outputs: {doc['outputs']}"
