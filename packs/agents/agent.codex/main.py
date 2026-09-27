# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.codex: run codex-harness-run on a spec in a working directory."""

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
)


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(with_step_notes(inp["spec"], inp, ctx, inp.get("listen")))
    log = Path(inp["log"]) if inp.get("log") else ctx.run_dir / "codex.log"
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
    final = _codex_final(log, since)
    report = None
    report_path = inp.get("report_path")
    if report_path and Path(report_path).exists():
        report = Path(report_path).read_text()
    return {"log": str(log), "final": final, "report": report, "session": _session(log)}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
