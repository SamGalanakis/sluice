# /// script
# requires-python = ">=3.12"
# dependencies = ["tomli-w>=1.2,<2"]
# ///
"""agent.run: dispatch a spec to the engine named in the input (devin/codex/claude)."""

import sys
from pathlib import Path

from sluice.fn import run

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _agents.native import git_output, run_claude, run_codex, run_devin


def _devin(inp, ctx):
    out = run_devin(inp["spec"], inp, ctx, inp["cwd"])
    return {"final": out["final"], "session": out["session"], **git_output(out)}


def _codex(inp, ctx):
    out = run_codex(inp["spec"], inp, ctx, inp["cwd"])
    return {"final": out["final"], "session": out["session"], **git_output(out)}


def _claude(inp, ctx):
    if inp.get("model"):
        raise ValueError("the claude engine always runs Opus; model is for codex and devin")
    out = run_claude(inp["spec"], inp, ctx, inp["cwd"])
    return {"final": out["final"], "session": out["session"], **git_output(out)}


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    engine = inp["engine"]
    if inp.get("effort") and engine != "codex":
        raise ValueError("effort is for the codex engine")
    if engine == "claude":  # its session builds the task itself
        out = _claude(inp, ctx)
    elif engine == "codex":
        out = _codex(inp, ctx)
    elif engine == "devin":
        out = _devin(inp, ctx)
    else:
        raise ValueError(f"unknown engine {engine!r}")
    report_path = inp.get("report_path")
    out["report"] = (
        Path(report_path).read_text()
        if report_path and Path(report_path).exists()
        else None
    )
    return out


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
