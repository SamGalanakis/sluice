"""check.l0 and check.l1 against fake kiln / dev-test output and real git."""

import pytest
from helpers import bazel_line, commit, git, libtest, log_rel, make_fakes, make_repo, run_fn


@pytest.fixture
def fork(tmp_path):
    repo = make_repo(tmp_path)
    git(repo, "switch", "-q", "-c", "task")
    fakes = make_fakes(tmp_path, repo)
    return repo, fakes


def devtest(fakes, stdout, exit=0, testlogs=None):
    fakes.respond("python3", ["scripts/dev-test.py"], stdout=stdout, exit=exit, testlogs=testlogs)


# ---- L0 --------------------------------------------------------------------------------------

def test_l0_green(tmp_path, fork):
    repo, fakes = fork
    commit(repo, "a.rs", "fn a() {}\n")
    r = run_fn("check.l0", {"path": str(repo), "base": "origin/main"}, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert r.out["ok"] is True and r.out["failures"] == []
    assert fakes.calls("kiln")[0]["argv"] == ["fmt", "--", "--check"]


def test_l0_red_on_fmt(tmp_path, fork):
    repo, fakes = fork
    fakes.respond("kiln", ["fmt"], stdout="Diff in a.rs", exit=1)
    r = run_fn("check.l0", {"path": str(repo)}, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert r.out["ok"] is False
    assert r.out["failures"] == ["l0: kiln fmt -- --check"]
    assert "Diff in a.rs" in r.out["log"]


def test_l0_red_on_whitespace_committed_since_base(tmp_path, fork):
    repo, fakes = fork
    commit(repo, "a.rs", "fn a() {}   \n")
    r = run_fn("check.l0", {"path": str(repo), "base": "origin/main"}, tmp_path, fakes.env)
    assert r.out["ok"] is False
    assert r.out["failures"] == ["l0: git diff --check"]
    # Without a base only uncommitted changes count, and there are none.
    r = run_fn("check.l0", {"path": str(repo)}, tmp_path, fakes.env)
    assert r.out["ok"] is True


# ---- L1 receipts -------------------------------------------------------------------------------

def test_l1_green_counts_cases_from_testlogs(tmp_path, fork):
    repo, fakes = fork
    fakes.respond("kiln", ["test", "//crates/a:tests"], stdout=bazel_line("//crates/a:tests"),
                  testlogs={log_rel("//crates/a:tests"): libtest(5)})
    devtest(fakes, "+ kiln test //crates/a:batch //:schema_checks\n"
            + bazel_line("//crates/a:batch") + bazel_line("//:schema_checks", cached=True)
            + "Executed 1 out of 2 tests: 2 tests pass.\n",
            testlogs={log_rel("//crates/a:batch"): libtest(12),
                      "schema_checks/test.log": libtest(3)})
    r = run_fn("check.l1", {"path": str(repo), "base": "origin/main",
                            "labels": ["//crates/a:tests"]}, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert r.out["ok"] is True, r.out["summary"]
    assert r.out["tests_run"] == 20
    assert r.out["failures"] == []
    py = fakes.calls("python3")
    assert py[0]["argv"] == ["scripts/dev-test.py", "--base", "origin/main"]


def test_l1_zero_cases_is_red_even_on_exit_zero(tmp_path, fork):
    repo, fakes = fork
    # A bare test name in a nested-module target: the target PASSES but runs nothing.
    fakes.respond("kiln", ["test"], stdout=bazel_line("//crates/a:tests"),
                  testlogs={log_rel("//crates/a:tests"): libtest(0, filtered=40)})
    r = run_fn("check.l1", {"path": str(repo), "labels": ["//crates/a:tests"],
                            "args": ["--test_arg=send_batch"]}, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert r.out["ok"] is False
    assert r.out["tests_run"] == 0
    assert any("0 test cases executed" in f for f in r.out["failures"])
    assert fakes.calls("kiln")[-1]["argv"] == ["test", "//crates/a:tests",
                                                "--test_arg=send_batch"]
    assert fakes.calls("python3") == [], "a red narrow stage stops before dev-test"


def test_l1_one_empty_label_is_red_even_if_another_ran(tmp_path, fork):
    repo, fakes = fork
    fakes.respond("kiln", ["test"], stdout=bazel_line("//a:x") + bazel_line("//a:y"),
                  testlogs={"a/x/test.log": libtest(7), "a/y/test.log": libtest(0, filtered=3)})
    r = run_fn("check.l1", {"path": str(repo), "labels": ["//a:x", "//a:y"]}, tmp_path,
               fakes.env)
    assert r.out["ok"] is False
    assert "//a:y: PASSED with 0 test cases executed" in r.out["failures"]


def test_l1_dev_test_planned_tests_but_no_receipt_is_red(tmp_path, fork):
    repo, fakes = fork
    devtest(fakes, "+ kiln test //crates/a:batch\n" + bazel_line("//crates/a:batch", cached=True))
    r = run_fn("check.l1", {"path": str(repo)}, tmp_path, fakes.env)
    assert r.out["ok"] is False
    assert any("dev-test: 0 test cases executed" in f for f in r.out["failures"])


def test_l1_failures_are_parsed_per_case(tmp_path, fork):
    repo, fakes = fork
    fakes.respond("kiln", ["test"], exit=3,
                  stdout=bazel_line("//crates/a:tests", "FAILED") + bazel_line("//crates/b:t"),
                  testlogs={log_rel("//crates/a:tests"):
                            libtest(3, 1, failing=("store::send_batch_is_atomic",)),
                            log_rel("//crates/b:t"): libtest(2)})
    r = run_fn("check.l1", {"path": str(repo), "labels": ["//crates/a:tests", "//crates/b:t"]},
               tmp_path, fakes.env)
    assert r.out["ok"] is False
    assert r.out["tests_run"] == 6
    assert r.out["failures"] == ["//crates/a:tests store::send_batch_is_atomic"]
    assert "L1 red" in r.out["summary"]


def test_l1_timeout_without_cases_names_the_target(tmp_path, fork):
    repo, fakes = fork
    devtest(fakes, "+ kiln test //a:slow\n" + bazel_line("//a:slow", "TIMEOUT")
            + bazel_line("//a:ok"), exit=3, testlogs={"a/ok/test.log": libtest(4)})
    r = run_fn("check.l1", {"path": str(repo)}, tmp_path, fakes.env)
    assert r.out["ok"] is False
    assert r.out["failures"] == ["//a:slow"]


def test_l1_console_sections_are_a_receipt_when_testlogs_are_missing(tmp_path, fork):
    repo, fakes = fork
    section = ("==================== Test output for //a:t:\n" + libtest(2, 1, ("m::bad",))
               + "=" * 80 + "\n")
    devtest(fakes, "+ kiln test //a:t\n" + section + bazel_line("//a:t", "FAILED"), exit=3)
    r = run_fn("check.l1", {"path": str(repo)}, tmp_path, fakes.env)
    assert r.out["tests_run"] == 3
    assert r.out["failures"] == ["//a:t m::bad"]


def test_l1_nothing_requested_needs_allow_no_tests(tmp_path, fork):
    repo, fakes = fork
    devtest(fakes, "dev-test: checkout/config snapshot abc, packages\n")
    r = run_fn("check.l1", {"path": str(repo)}, tmp_path, fakes.env)
    assert r.out["ok"] is False
    assert any("allow_no_tests" in f for f in r.out["failures"])
    r = run_fn("check.l1", {"path": str(repo), "allow_no_tests": True}, tmp_path, fakes.env)
    assert r.out["ok"] is True, r.out["summary"]


def test_l1_stops_at_a_red_l0(tmp_path, fork):
    repo, fakes = fork
    fakes.respond("kiln", ["fmt"], exit=1)
    r = run_fn("check.l1", {"path": str(repo), "labels": ["//a:t"]}, tmp_path, fakes.env)
    assert r.out["ok"] is False
    assert r.out["failures"] == ["l0: kiln fmt -- --check"]
    assert [c["argv"][0] for c in fakes.calls("kiln")] == ["fmt"]
    assert fakes.calls("python3") == []
