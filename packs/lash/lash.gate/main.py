# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.gate: turn a red L1 into the next attempt (a spawned node), or into an inbox item.

A plan is a DAG, so the retry loop is unrolled at run time: each red gate spawns one
`lash.attempt` node (agent -> L1 -> gate -> finish), which carries its own gate. The ladder
lists the engine per attempt; its length is the attempt budget.
"""

import json
import re
from pathlib import Path

from sluice.fn import run

ESCALATE = {"devin": "claude", "codex": "claude", "claude": "claude"}
LOG_TAIL = 6000
_ID = re.compile(r"[^a-z0-9_-]+")

PASS_THROUGH = ("name", "ladder", "brief", "path", "branch", "base", "arc", "labels",
                "allow_no_tests", "report_path")


def ladder_of(inp) -> list[str]:
    return list(inp.get("ladder") or [inp["engine"], inp["engine"], ESCALATE[inp["engine"]]])


def failure_text(inp) -> str:
    fails = "\n".join(f"- {f}" for f in inp["failures"]) or "- (none parsed; see the log)"
    log = inp["log"][-LOG_TAIL:]
    return (f"{inp['summary'].strip()}\n\nFailures:\n{fails}\n\nLog tail:\n```\n{log}\n```")


def retry_spec(inp, resume: bool) -> str:
    n = inp["attempt"]
    if resume:
        return (f"The plan's L1 check is red on your branch `{inp['branch']}` after attempt {n}. "
                "Fix these failures in the same fork, run the gates again, commit, and report. "
                "Do not weaken, narrow, exempt or re-pin a gate to make it pass.\n\n"
                + failure_text(inp))
    return (inp["brief"].rstrip() + "\n\n## Previous attempt\n"
            f"Attempt {n} ({inp['engine']}) left commits on branch `{inp['branch']}` in "
            f"`{inp['path']}`, and the plan's L1 check is red on them. Start from that branch: "
            "keep what is right, fix or replace what is not.\n\n" + failure_text(inp))


def plan_doc(ctx) -> dict:
    p = Path(ctx.home) / "plans" / ctx.plan / "plan.json"
    try:
        return json.loads(p.read_text())
    except (OSError, ValueError):
        return {}


def main(inp, ctx):
    n = inp["attempt"]
    if inp["ok"]:
        return {"ok": True, "attempt": n, "next": None,
                "summary": f"L1 green on attempt {n}"}
    ladder = ladder_of(inp)
    if n >= len(ladder):
        raise RuntimeError(
            f"task {inp['name']}: L1 red after {n} attempts (ladder {ladder}). The fork "
            f"{inp['path']} is kept on branch {inp['branch']}.\n" + failure_text(inp))
    engine = ladder[n]
    resume = engine == inp["engine"] and bool(inp.get("session"))
    doc = plan_doc(ctx)
    nodes = doc.get("nodes", {})
    top = ctx.node.split("/", 1)[0]
    # Spawned attempts are top-level nodes, so they inherit the task's claims explicitly
    # (a hot file such as pg-schema stays claimed across retries).
    claims = list(nodes.get(top, {}).get("claims", []))
    base_id = _ID.sub("-", f"{inp['name']}-a{n + 1}".lower()).strip("-")
    node_id, k = base_id, 1
    while node_id in nodes:
        k += 1
        node_id = f"{base_id}-{k}"
    ins = {key: inp.get(key) for key in PASS_THROUGH}
    ins.update({
        "attempt": n + 1,
        "engine": engine,
        "ladder": ladder,
        "model": inp.get("model") if engine == inp["engine"] else None,
        "spec": retry_spec(inp, resume),
        "resume": inp["session"] if resume else None,
    })
    node = {
        "fn": "lash.attempt",
        "in": {key: {"value": v} for key, v in ins.items() if v is not None},
        "note": (f"attempt {n + 1} of {len(ladder)} for task {inp['name']} "
                 f"({engine}, {'resumed' if resume else 'fresh'}); spawned by {ctx.node}"),
    }
    if claims:
        node["claims"] = claims
    how = "resumes the same session" if resume else f"starts a fresh {engine} session"
    ctx.spawn({node_id: node},
              reason=f"L1 red on attempt {n} of task {inp['name']}; {node_id} {how}")
    return {"ok": False, "attempt": n, "next": node_id,
            "summary": f"L1 red on attempt {n}; spawned {node_id} ({engine}, {how})"}


if __name__ == "__main__":
    run(main)
