# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.devin: run devin-harness-run on a spec in a working directory."""

import os
from pathlib import Path

from sluice.fn import ShError, Transient, run, sh


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    spec_file = ctx.run_dir / "spec.md"
    spec_file.write_text(inp["spec"])
    log = Path(inp["log"]) if inp.get("log") else ctx.run_dir / "devin.log"
    argv = [
        os.environ.get("SLUICE_DEVIN_BIN", "devin-harness-run"),
        "--cd", inp["cwd"],
        "--spec", str(spec_file),
        "--log", str(log),
    ]
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
    final = final_file.read_text() if final_file.exists() else ""
    report = None
    report_path = inp.get("report_path")
    if report_path and Path(report_path).exists():
        report = Path(report_path).read_text()
    return {"log": str(log), "final": final, "report": report}


if __name__ == "__main__":
    run(main)
