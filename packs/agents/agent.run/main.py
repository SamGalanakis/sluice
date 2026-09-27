# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.run: dispatch a spec to the engine named in the input (devin/codex/claude)."""

import datetime as dt
import os
import sys
from pathlib import Path

from sluice.fn import ShError, Transient, run, sh_stream, with_step_notes

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _agents.engines import (
    CODEX_BIN,
    CODEX_EFFORT,
    CODEX_TRANSIENT,
    DiffFold,
    _codex_final,
    _session,
    claude,
)


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
    if inp.get("session"):
        argv += ["--resume", inp["session"]]
    try:
        sh_stream(argv, follow=log)  # the harness writes its progress to the log
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if "capacity issues" in tail:
            raise Transient("devin-harness-run reported capacity issues") from e
        raise
    final_file = Path(str(log) + ".final")
    return {"final": final_file.read_text() if final_file.exists() else "",
            "session": _session(log)}


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
    model = inp.get("model") or "sol"
    if model not in CODEX_EFFORT:
        raise ValueError(f"codex models are {', '.join(CODEX_EFFORT)}, got {model!r}")
    argv += ["--model", model, "--effort", inp.get("effort") or CODEX_EFFORT[model]]
    if inp.get("session"):
        argv += ["--resume", inp["session"]]
    try:
        since = dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H:%M:%S")
        fold = DiffFold()
        sh_stream(argv, on_line=fold, follow=log)  # the harness writes its progress to the log
        fold.flush()
    except ShError as e:
        tail = log.read_text()[-3000:] if log.exists() else (e.stdout + e.stderr)[-3000:]
        if any(m in tail for m in CODEX_TRANSIENT):
            raise Transient("codex-harness-run hit a rate limit or capacity error") from e
        raise
    return {"final": _codex_final(log, since), "session": _session(log)}


def _claude(inp):
    if inp.get("model"):
        raise ValueError("the claude engine always runs Opus; model is for codex and devin")
    data = claude(inp["spec"], "opus", inp["cwd"], inp.get("session"))
    return {"final": data["result"], "session": data["session_id"]}


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    inp = {**inp, "spec": with_step_notes(inp["spec"], inp, ctx, inp.get("listen"))}
    engine = inp["engine"]
    if inp.get("effort") and engine != "codex":
        raise ValueError("effort is for the codex engine")
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
