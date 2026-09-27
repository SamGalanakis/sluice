# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""decide.llm: ask claude (haiku by default) to choose one option with a confidence p."""

import json
import os
import sys
from pathlib import Path

from sluice.fn import ShError, Transient, run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _agents.engines import CLAUDE_TRANSIENT

PROMPT = """\
Answer the question by picking exactly one of the listed options.

Question: {question}
{context}
Options: {options}

Respond as JSON: {{"choice": <one of the options, verbatim>, "p": <probability
0..1 that this choice is right>}}."""


def main(inp, ctx):
    options = inp["options"]
    schema = {
        "type": "object",
        "properties": {
            "choice": {"type": "string", "enum": options},
            "p": {"type": "number", "minimum": 0, "maximum": 1},
        },
        "required": ["choice", "p"],
        "additionalProperties": False,
    }
    context = ""
    if inp.get("context") is not None:
        context = "Context:\n" + json.dumps(inp["context"], indent=2) + "\n"
    prompt = PROMPT.format(
        question=inp["question"], context=context, options=", ".join(options))
    argv = [
        os.environ.get("SLUICE_CLAUDE_BIN", "claude"),
        "-p", prompt,
        "--model", os.environ.get("SLUICE_DECIDE_MODEL", "haiku"),
        "--output-format", "json",
        "--json-schema", json.dumps(schema),
        "--dangerously-skip-permissions",
    ]
    p = sh(argv, check=False)
    if p.returncode != 0:
        if any(m in p.stderr for m in CLAUDE_TRANSIENT):
            raise Transient("claude hit a rate limit or capacity error")
        raise ShError(argv, p.returncode, p.stdout, p.stderr)
    data = json.loads(p.stdout)
    answer = data.get("structured_output") or json.loads(data["result"])
    choice = answer["choice"]
    if choice not in options:
        raise ValueError(f"choice {choice!r} not in options {options!r}")
    prob = float(answer["p"])
    threshold = inp.get("threshold")
    return {
        "choice": choice,
        "p": prob,
        "confident": prob >= (0.8 if threshold is None else threshold),
    }


if __name__ == "__main__":
    run(main, retries=2, backoff=30)
