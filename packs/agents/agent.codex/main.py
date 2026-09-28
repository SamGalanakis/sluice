# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""agent.codex: run a supervised Codex app-server thread in a working directory."""

import sys
from pathlib import Path

from sluice.fn import run

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _agents.native import run_codex


def main(inp, ctx):
    out = run_codex(inp["spec"], inp, ctx, inp["cwd"])
    log = Path(inp["log"]) if inp.get("log") else ctx.run_dir / "codex.log"
    if inp.get("log"):
        log.write_text((ctx.run_dir / "codex.log").read_text())
    report_path = inp.get("report_path")
    report = Path(report_path).read_text() if report_path and Path(report_path).exists() else None
    return {"log": str(log), "final": out["final"], "report": report,
            "session": out["session"]}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
