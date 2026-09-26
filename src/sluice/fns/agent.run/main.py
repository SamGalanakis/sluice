# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.run: dispatch a spec to the engine named in the input (devin/codex/claude)."""

import json
import os
from pathlib import Path

from sluice.fn import ShError, Transient, run, sh

CODEX_BIN = str(Path.home() / ".codex" / "bin" / "codex-harness-run")
CLAUDE_TRANSIENT = ("rate limit", "overloaded", "529")
CODEX_TRANSIENT = ("rate limit", "429", "capacity")


def _devin(inp, ctx):
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(inp["spec"])
    log = ctx.run_dir / "devin.log"
    argv = [
        os.environ.get("SLUICE_DEVIN_BIN", "devin-harness-run"),
        "--cd", inp["cwd"],
        "--spec", str(spec_file),
        "--log", str(log),
    ]
    if inp.get("model"):
        argv += ["--model", inp["model"]]
    if inp.get("resume"):
        argv += ["--resume", inp["resume"]]
    try:
        sh(argv)
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if "capacity issues" in tail:
            raise Transient("devin-harness-run reported capacity issues") from e
        raise
    final_file = Path(str(log) + ".final")
    session_file = Path(str(log) + ".session")
    return {
        "final": final_file.read_text() if final_file.exists() else "",
        "session": session_file.read_text().strip() if session_file.exists() else None,
    }


def _codex(inp, ctx):
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(inp["spec"])
    log = ctx.run_dir / "codex.log"
    argv = [
        os.environ.get("SLUICE_CODEX_BIN", CODEX_BIN),
        "--cd", inp["cwd"],
        "--spec", str(spec_file),
        "--log", str(log),
    ]
    if inp.get("model"):
        argv += ["--model", inp["model"]]
    if inp.get("resume"):
        argv += ["--resume", inp["resume"]]
    try:
        sh(argv)
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if any(m in tail for m in CODEX_TRANSIENT):
            raise Transient("codex-harness-run hit a rate limit or capacity error") from e
        raise
    # codex writes <log>.session but no <log>.final: the last chunk of the log is the report.
    session_file = Path(str(log) + ".session")
    return {
        "final": log.read_text()[-4000:] if log.exists() else "",
        "session": session_file.read_text().strip() if session_file.exists() else None,
    }


def _claude(inp):
    argv = [
        os.environ.get("SLUICE_CLAUDE_BIN", "claude"),
        "-p", inp["spec"],
        "--model", inp.get("model") or "opus",
        "--output-format", "json",
        "--dangerously-skip-permissions",
    ]
    if inp.get("resume"):
        argv += ["--resume", inp["resume"]]
    p = sh(argv, cwd=inp["cwd"], check=False)
    if p.returncode != 0:
        if any(m in p.stderr for m in CLAUDE_TRANSIENT):
            raise Transient("claude hit a rate limit or capacity error")
        raise ShError(argv, p.returncode, p.stdout, p.stderr)
    data = json.loads(p.stdout)
    return {"final": data["result"], "session": data["session_id"]}


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    engine = inp["engine"]
    if engine == "claude":
        out = _claude(inp)
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
