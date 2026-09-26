# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""check.l1: L0 + narrow tests + dev-test, judged by receipts.

Receipt rule: if any test was requested (labels given, or dev-test planned a `kiln test`), at
least one test case must have executed, and every named label must itself have run a case.
If nothing was requested at all (a docs-only diff), ok only with allow_no_tests.
"""

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "_lib"))

from lashkit import checks, kiln, python, tail

from sluice.fn import run

_PLANNED_TEST = re.compile(r"^\+ \S*kiln test\b", re.MULTILINE)


def main(inp, ctx):
    path, base = inp["path"], inp.get("base")
    labels = list(inp.get("labels") or [])
    stages = [checks.l0(path, base)]
    requested = False
    if stages[-1].ok and labels:
        requested = True
        argv = [kiln(), "test", *labels, *(inp.get("args") or [])]
        stages.append(checks.receipted("narrow", argv, path, min_tests=1, per_target=True))
    if stages[-1].ok:
        argv = [python(), "scripts/dev-test.py"] + (["--base", base] if base else [])
        # dev-test prints `+ <command>` before each planned command; no `kiln test` means it
        # selected no tests (e.g. docs-only), which is not a receipt lie.
        dev = checks.receipted("dev-test", argv, path,
                               min_tests=lambda out: 1 if _PLANNED_TEST.search(out) else 0)
        requested = requested or dev.required > 0
        stages.append(dev)
    tests_run = sum(s.tests_run for s in stages)
    failures = [f for s in stages for f in s.failures]
    ok = all(s.ok for s in stages) and len(stages) >= 2
    if ok and not requested and not inp.get("allow_no_tests"):
        ok = False
        failures.append("l1: no tests were requested or executed (set allow_no_tests for a "
                        "docs-only change)")
    head = f"L1 {'green' if ok else 'red'}: {tests_run} test cases executed"
    summary = "\n".join([head, *(f"- {s.summary}" for s in stages),
                         *(f"- FAILED {f}" for f in failures)])
    log = "\n\n".join(s.log for s in stages)
    if ctx.run_dir.is_dir():
        (ctx.run_dir / "l1.log").write_text(log)
    return {"ok": ok, "summary": summary, "log": tail(log), "tests_run": tests_run,
            "failures": failures}


if __name__ == "__main__":
    run(main)
