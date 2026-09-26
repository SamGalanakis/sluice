"""Test receipts: how many test cases a Bazel/kiln run actually executed, and which failed.

Bazel's summary counts test *targets*. A target whose filter matched nothing (a bare name in a
nested-module target) still reports PASSED, so a receipt counts *cases* from the libtest (or
pytest) summaries in each target's test.log. Logs are read from `<root>/bazel-testlogs`; when a
target has no log there, its section of the console output (`--test_output=errors|all`) is used.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path

PASS_STATUSES = {"PASSED", "FLAKY"}
NOT_RUN_STATUSES = {"SKIPPED"}

_STATUS = re.compile(
    r"^(?P<label>@{0,2}[\w.~+-]*//\S*)\s+(?:\((?P<cached>cached)\)\s+)?"
    r"(?P<status>FAILED TO BUILD|PASSED|FAILED|FLAKY|TIMEOUT|NO STATUS|INCOMPLETE|"
    r"REMOTE FAILURE|SKIPPED)\b",
    re.MULTILINE,
)
_SECTION = re.compile(r"^={10,} Test output for (?P<label>\S+?):?\s*$", re.MULTILINE)
_SECTION_END = re.compile(r"^={40,}\s*$", re.MULTILINE)
_LIBTEST_RESULT = re.compile(r"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;", re.MULTILINE)
_LIBTEST_FAIL = re.compile(r"^test (\S+) \.\.\. FAILED\s*$", re.MULTILINE)
_PYTEST_SUMMARY = re.compile(r"^=+ (?P<body>[^=\n]*\b(?:passed|failed)\b[^=\n]*) in [\d.]+s", re.MULTILINE)
_PYTEST_FAIL = re.compile(r"^FAILED (\S+)", re.MULTILINE)
_EXECUTED = re.compile(r"Executed (\d+) out of (\d+) tests?")


@dataclass
class Target:
    label: str
    status: str
    cached: bool
    cases_run: int = 0
    failed_cases: list[str] = field(default_factory=list)
    evidence: str = "none"  # "testlog", "console" or "none"

    @property
    def passed(self) -> bool:
        return self.status in PASS_STATUSES or self.status in NOT_RUN_STATUSES


@dataclass
class Receipt:
    targets: list[Target]
    tests_run: int
    loose_failed_cases: list[str]  # failing cases not attributable to a target
    executed_line: str | None
    no_targets: bool

    @property
    def failing_targets(self) -> list[Target]:
        return [t for t in self.targets if not t.passed]

    def failures(self) -> list[str]:
        """Failing items: `<label> <case>` per failing case, or `<label>` if no case is known."""
        items: list[str] = []
        for t in self.targets:
            if t.passed:
                continue
            if t.failed_cases:
                items.extend(f"{t.label} {case}" for case in t.failed_cases)
            else:
                items.append(t.label)
        items.extend(self.loose_failed_cases)
        return items

    def describe(self) -> str:
        cached = sum(1 for t in self.targets if t.cached)
        parts = [f"{self.tests_run} test cases executed", f"{len(self.targets)} targets"]
        if cached:
            parts.append(f"{cached} cached")
        blind = [t.label for t in self.targets if t.evidence == "none" and t.status == "PASSED"]
        if blind:
            parts.append(f"{len(blind)} without a case receipt")
        if self.no_targets:
            parts.append("no test targets found")
        return ", ".join(parts)


def count_cases(text: str) -> tuple[int, list[str]]:
    """Cases executed (passed + failed) and failing case names, from libtest/pytest output."""
    run = 0
    for passed, failed in _LIBTEST_RESULT.findall(text):
        run += int(passed) + int(failed)
    failed_cases = _LIBTEST_FAIL.findall(text)
    for m in _PYTEST_SUMMARY.finditer(text):
        body = m.group("body")
        for n, _word in re.findall(r"(\d+) (passed|failed)", body):
            run += int(n)
    failed_cases += [c for c in _PYTEST_FAIL.findall(text) if c not in failed_cases]
    return run, list(dict.fromkeys(failed_cases))


def testlog_dir(root: Path, label: str) -> Path:
    """`//a/b:t` -> bazel-testlogs/a/b/t; `@repo//a:t` -> bazel-testlogs/external/repo/a/t."""
    if label.startswith("@"):
        repo, _, rest = label.lstrip("@").partition("//")
    else:
        repo, rest = "", label.removeprefix("//")
    pkg, _, name = rest.partition(":")
    if not name:
        name = pkg.rsplit("/", 1)[-1]
    base = root / "bazel-testlogs"
    if repo:
        base = base / "external" / repo
    return base / pkg / name if pkg else base / name


def read_testlogs(root: Path, label: str) -> str | None:
    d = testlog_dir(root, label)
    # Shards/runs live in `shard_k_of_n/`, `run_k_of_n/` (never recurse: a subpackage's logs
    # can live below a target dir of the same name).
    parts = sorted(d.glob("*_of_*/test.log"))
    if not parts and (d / "test.log").is_file():
        parts = [d / "test.log"]
    if not parts:
        return None
    return "\n".join(p.read_text(errors="replace") for p in parts)


def console_sections(output: str) -> dict[str, str]:
    sections: dict[str, str] = {}
    for m in _SECTION.finditer(output):
        start = m.end()
        end_m = _SECTION_END.search(output, start)
        end = end_m.start() if end_m else len(output)
        sections[m.group("label")] = sections.get(m.group("label"), "") + output[start:end]
    return sections


def parse(output: str, root: Path | None) -> Receipt:
    """Build a receipt from a run's console output and the fork's bazel-testlogs."""
    targets: dict[str, Target] = {}
    for m in _STATUS.finditer(output):
        label = m.group("label")
        targets[label] = Target(label, m.group("status"), bool(m.group("cached")))
    sections = console_sections(output)
    for t in targets.values():
        text = read_testlogs(root, t.label) if root is not None else None
        if text is not None:
            t.evidence = "testlog"
        elif t.label in sections:
            text, t.evidence = sections[t.label], "console"
        if text is not None:
            t.cases_run, t.failed_cases = count_cases(text)
    tests_run = sum(t.cases_run for t in targets.values())
    loose: list[str] = []
    if not any(t.evidence != "none" for t in targets.values()):
        # No per-target evidence (e.g. a script or just recipe running tests directly):
        # count whatever case summaries the console shows.
        run, failed = count_cases(output)
        tests_run += run
        loose = failed
    executed = _EXECUTED.search(output)
    return Receipt(
        targets=list(targets.values()),
        tests_run=tests_run,
        loose_failed_cases=loose,
        executed_line=executed.group(0) if executed else None,
        no_targets="No test targets were found" in output,
    )
