# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.devin: run a supervised interactive Devin session."""

import sys
from pathlib import Path

from sluice.fn import run

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _agents.native import git_output, run_devin


def main(inp, ctx):
    ctx.run_dir.mkdir(parents=True, exist_ok=True)
    log = Path(inp["log"]) if inp.get("log") else ctx.run_dir / "devin.log"
    out = run_devin(inp["spec"], {**inp, "log": str(log)}, ctx, inp["cwd"])
    report = None
    report_path = inp.get("report_path")
    if report_path and Path(report_path).exists():
        report = Path(report_path).read_text()
    return {"log": str(log), "final": out["final"], "report": report,
            "session": out["session"], **git_output(out)}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
