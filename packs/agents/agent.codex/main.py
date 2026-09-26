# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.codex: run codex-harness-run on a spec in a working directory."""

import os
from pathlib import Path

from sluice.fn import ShError, Transient, run, sh

DEFAULT_BIN = str(Path.home() / ".codex" / "bin" / "codex-harness-run")
TRANSIENT_MARKERS = ("rate limit", "429", "capacity")


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(inp["spec"])
    log = Path(inp["log"]) if inp.get("log") else ctx.run_dir / "codex.log"
    argv = [
        os.environ.get("SLUICE_CODEX_BIN", DEFAULT_BIN),
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
        if any(m in tail for m in TRANSIENT_MARKERS):
            raise Transient("codex-harness-run hit a rate limit or capacity error") from e
        raise
    # codex writes <log>.session but no <log>.final: the last chunk of the log is the report.
    final = log.read_text()[-4000:] if log.exists() else ""
    report = None
    report_path = inp.get("report_path")
    if report_path and Path(report_path).exists():
        report = Path(report_path).read_text()
    return {"log": str(log), "final": final, "report": report}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
