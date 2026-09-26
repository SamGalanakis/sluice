"""The check ladder's building blocks: L0 hygiene, receipted test runs and named gates."""

from __future__ import annotations

import datetime as dt
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from lashkit import just, kiln, python, receipts, tail
from lashkit import ledger as ledger_mod
from sluice.fn import sh


@dataclass
class Stage:
    name: str
    ok: bool
    summary: str
    log: str
    tests_run: int = 0
    failures: list[str] = field(default_factory=list)
    required: int = 0  # test cases the receipt had to show


def _out(p) -> str:
    return (p.stdout or "") + ("\n" if p.stdout and p.stderr else "") + (p.stderr or "")


def _merge_base(path: str, base: str) -> str:
    p = sh(["git", "merge-base", base, "HEAD"], cwd=path, check=False)
    return p.stdout.strip() if p.returncode == 0 and p.stdout.strip() else base


def l0(path: str, base: str | None) -> Stage:
    """`kiln fmt -- --check` and `git diff --check` (since the merge-base with `base`, else
    the uncommitted changes)."""
    failures, logs = [], []
    fmt = sh([kiln(), "fmt", "--", "--check"], cwd=path, check=False)
    logs.append(f"$ kiln fmt -- --check  (exit {fmt.returncode})\n{tail(_out(fmt), 4000)}")
    if fmt.returncode != 0:
        failures.append("l0: kiln fmt -- --check")
    since = _merge_base(path, base) if base else "HEAD"
    diff = sh(["git", "diff", "--check", since], cwd=path, check=False)
    logs.append(f"$ git diff --check {since}  (exit {diff.returncode})\n{tail(_out(diff), 4000)}")
    if diff.returncode != 0:
        failures.append("l0: git diff --check")
    ok = not failures
    summary = "L0 green" if ok else "L0 red: " + ", ".join(f.removeprefix("l0: ") for f in failures)
    return Stage("l0", ok, summary, "\n".join(logs), 0, failures)


def receipted(name: str, argv: list[str], path: str, min_tests: int | Callable[[str], int],
              per_target: bool = False) -> Stage:
    """Run a test command and judge it by its receipt, not only its exit code.

    `min_tests` may be a function of the command's output (for commands that plan their own
    test selection and may legitimately select none).

    `per_target`: every PASSED target whose log was found must itself have run a case (for
    explicitly named labels, where a filter matching nothing is the classic silent green).
    """
    p = sh(argv, cwd=path, check=False)
    out = _out(p)
    r = receipts.parse(out, Path(path))
    if callable(min_tests):
        min_tests = min_tests(out)
    failures = r.failures()
    if p.returncode != 0 and not failures:
        failures.append(f"{name}: exit {p.returncode}")
    if r.tests_run < min_tests:
        failures.append(f"{name}: {r.tests_run} test cases executed (receipt needs >= {min_tests})")
    if per_target:
        failures += [f"{t.label}: PASSED with 0 test cases executed" for t in r.targets
                     if t.status == "PASSED" and t.evidence != "none" and t.cases_run == 0]
    ok = p.returncode == 0 and not failures
    summary = f"{name}: {'green' if ok else 'red'}, {r.describe()}"
    log = f"$ {' '.join(argv)}  (exit {p.returncode}; {r.describe()})\n{out}"
    return Stage(name, ok, summary, log, r.tests_run, failures, min_tests)


# ---- named gates (L2/L3) -------------------------------------------------------------------

def gate_id(g: dict[str, Any]) -> str:
    return f"{g['kind']}:{g['target']}"


def gate_argv(g: dict[str, Any]) -> list[str]:
    kind, target, args = g["kind"], g["target"], list(g.get("args") or [])
    if kind == "just":
        return [just(), target, *args]
    if kind == "kiln-test":
        return [kiln(), "test", target, *args]
    if kind == "script":
        if target.endswith(".py"):
            return [python(), target, *args]
        if target.endswith(".sh"):
            return ["bash", target, *args]
        return [target, *args]
    raise ValueError(f"unknown gate kind {kind!r} (expected just, kiln-test or script)")


def run_gate(path: str, g: dict[str, Any], entries: list[ledger_mod.Entry],
             today: dt.date) -> dict[str, Any]:
    gid = gate_id(g)
    min_tests = g.get("min_tests")
    if min_tests is None:
        min_tests = 1 if g["kind"] == "kiln-test" else 0
    argv = gate_argv(g)
    p = sh(argv, cwd=path, check=False)
    out = _out(p)
    r = receipts.parse(out, Path(path))
    items = r.failures()
    if p.returncode != 0 and not items:
        items = [gid]
    if r.tests_run < min_tests and gid not in items:
        items.append(gid)
    verdicts = [ledger_mod.classify(i, gid, entries, today) for i in items]
    new = [v for v in verdicts if v.kind != "known"]
    known = [v for v in verdicts if v.kind == "known"]
    status = "pass" if not items else "red" if new else "known_red"
    notes = []
    if r.tests_run < min_tests:
        notes.append(f"receipt: {r.tests_run} test cases executed, needs >= {min_tests}")
    summary = f"{gid}: {status} (exit {p.returncode}; {r.describe()})"
    if notes:
        summary += "; " + "; ".join(notes)
    passing = [gid] if status == "pass" else []
    passing += [t.label for t in r.targets if t.passed]
    return {
        "gate": gid,
        "status": status,
        "summary": summary,
        "tests_run": r.tests_run,
        "failures": [v.describe() for v in new],
        "known": [v.describe() for v in known],
        "_passing": passing,
        "_failing": items,
        "_log": f"==== {gid}: $ {' '.join(argv)} (exit {p.returncode})\n{tail(out, 6000)}",
    }


def run_gates(path: str, gates: list[dict[str, Any]], ledger_path: str | None,
              allow_known: bool, level: str) -> dict[str, Any]:
    entries = ledger_mod.load(ledger_path)
    today = dt.datetime.now(dt.UTC).date()
    results = [run_gate(path, g, entries, today) for g in gates]
    new_reds = [f for res in results for f in res["failures"]]
    known_reds = [k for res in results for k in res["known"]]
    passing = [i for res in results for i in res.pop("_passing")]
    failing = [i for res in results for i in res.pop("_failing")]
    logs = [res.pop("_log") for res in results]
    # A live entry that matches something passing and nothing failing is ready to delete.
    stale = sorted({
        e.test for e in entries
        if e.live(today) and any(e.matches(i) for i in passing)
        and not any(e.matches(i) for i in failing)
    })
    ok = not new_reds and (allow_known or not known_reds)
    counts = {s: sum(1 for res in results if res["status"] == s)
              for s in ("pass", "known_red", "red")}
    head = f"{level} {'green' if ok else 'red'}"
    summary = (f"{head}: {counts['pass']} pass, {counts['known_red']} known red, "
               f"{counts['red']} red of {len(results)} gates; "
               f"{sum(res['tests_run'] for res in results)} test cases executed")
    if new_reds:
        summary += "\nnew reds:\n" + "\n".join(f"- {x}" for x in new_reds)
    if known_reds:
        summary += "\nknown reds:\n" + "\n".join(f"- {x}" for x in known_reds)
    if known_reds and not allow_known:
        summary += f"\n{level} does not accept known reds."
    if stale:
        summary += "\nknown-red entries now passing (remove them):\n" + "\n".join(
            f"- {x}" for x in stale)
    return {
        "ok": ok,
        "results": results,
        "new_reds": new_reds,
        "known_reds": known_reds,
        "stale": stale,
        "summary": summary,
        "log": tail("\n".join(logs), 20000),
    }
