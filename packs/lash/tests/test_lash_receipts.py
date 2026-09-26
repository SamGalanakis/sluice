"""Receipt parsing details the process tests do not reach: shards, runs, external repos."""

import sys

from helpers import PACK, REPO, bazel_line, libtest

sys.path[:0] = [str(PACK / "_lib"), str(REPO / "src")]
from lashkit import receipts


def write(root, rel, text):
    p = root / "bazel-testlogs" / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)


def test_shards_and_runs_are_summed_and_subpackages_are_not(tmp_path):
    write(tmp_path, "crates/a/tests/shard_1_of_2/test.log", libtest(3))
    write(tmp_path, "crates/a/tests/shard_2_of_2/test.log", libtest(4, 1, ("m::x",)))
    # A subpackage `//crates/a/tests:inner` lives below the same directory; never counted.
    write(tmp_path, "crates/a/tests/inner/test.log", libtest(50))
    write(tmp_path, "external/rules_x/pkg/t/run_1_of_2/test.log", libtest(1))
    write(tmp_path, "external/rules_x/pkg/t/run_2_of_2/test.log", libtest(1))
    out = bazel_line("//crates/a:tests", "FAILED") + bazel_line("@rules_x//pkg:t")
    r = receipts.parse(out, tmp_path)
    assert r.tests_run == 10
    assert r.failures() == ["//crates/a:tests m::x"]


def test_flaky_counts_as_passed_and_skipped_is_not_a_failure(tmp_path):
    write(tmp_path, "a/f/test.log", libtest(2, 1, ("m::first_try",)) + libtest(3))
    out = bazel_line("//a:f", "FLAKY") + bazel_line("//a:s", "SKIPPED")
    r = receipts.parse(out, tmp_path)
    assert r.failures() == []
    assert r.tests_run == 6


def test_pytest_summaries_count_when_no_target_evidence_exists():
    out = "collected 7 items\n...\nFAILED tests/test_x.py::test_y - assert 1\n" \
          "===== 1 failed, 6 passed in 0.52s =====\n"
    r = receipts.parse(out, None)
    assert r.tests_run == 7
    assert r.failures() == ["tests/test_x.py::test_y"]
