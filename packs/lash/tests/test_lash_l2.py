"""check.l2 / check.l3: named gates, receipts, and the known-red ledger."""

import json

import pytest
from helpers import bazel_line, libtest, make_fakes, make_repo, run_fn

LIVE, PAST = "2999-01-01", "2020-01-01"


@pytest.fixture
def arc(tmp_path):
    repo = make_repo(tmp_path)
    return repo, make_fakes(tmp_path, repo)


def ledger(tmp_path, *entries):
    p = tmp_path / "known-reds.json"
    p.write_text(json.dumps({"version": 1, "entries": list(entries)}))
    return str(p)


def entry(test, expires=LIVE, owner="FIG-1", reason="flaky under load"):
    return {"test": test, "owner": owner, "reason": reason, "expires": expires}


def failing_target(fakes, label, case):
    pkg, _, name = label.removeprefix("//").partition(":")
    fakes.respond("kiln", ["test", label], exit=3, stdout=bazel_line(label, "FAILED"),
                  testlogs={f"{pkg}/{name}/test.log": libtest(4, 1, (case,))})


def by_gate(out):
    return {r["gate"]: r for r in out["results"]}


def test_known_red_vs_new_red_vs_expired(tmp_path, arc):
    repo, fakes = arc
    failing_target(fakes, "//crates/a:known", "m::known_case")
    failing_target(fakes, "//crates/a:old", "m::old_case")
    fakes.respond("kiln", ["test", "//crates/a:ok"], stdout=bazel_line("//crates/a:ok"),
                  testlogs={"crates/a/ok/test.log": libtest(9)})
    fakes.respond("just", ["agent-service-restate-e2e"], exit=1, stderr="e2e: turn stalled")
    path = ledger(tmp_path, entry("//crates/a:known", owner="node:arc/s5b"),
                  entry("//crates/a:old", expires=PAST, owner="FIG-2"))
    gates = [{"kind": "kiln-test", "target": t} for t in
             ("//crates/a:known", "//crates/a:old", "//crates/a:ok")]
    gates.append({"kind": "just", "target": "agent-service-restate-e2e"})
    r = run_fn("check.l2", {"path": str(repo), "gates": gates, "known_reds": path}, tmp_path,
               fakes.env)
    assert r.code == 0, r.stderr
    out = r.out
    res = by_gate(out)
    assert res["kiln-test://crates/a:known"]["status"] == "known_red"
    assert res["kiln-test://crates/a:old"]["status"] == "red"
    assert res["kiln-test://crates/a:ok"]["status"] == "pass"
    assert res["kiln-test://crates/a:ok"]["tests_run"] == 9
    assert res["just:agent-service-restate-e2e"]["status"] == "red"
    assert out["ok"] is False
    assert out["new_reds"] == [
        "//crates/a:old m::old_case (known-red entry expired 2020-01-01, owner FIG-2)",
        "just:agent-service-restate-e2e",
    ]
    assert len(out["known_reds"]) == 1 and "node:arc/s5b" in out["known_reds"][0]
    assert [c["argv"] for c in fakes.calls("just")] == [["agent-service-restate-e2e"]]


def test_only_known_reds_is_green(tmp_path, arc):
    repo, fakes = arc
    failing_target(fakes, "//crates/a:known", "m::known_case")
    fakes.respond("just", ["workers-e2e"], exit=1)
    path = ledger(tmp_path, entry("//crates/a:known m::known_case"), entry("just:workers-e2e"))
    gates = [{"kind": "kiln-test", "target": "//crates/a:known"},
             {"kind": "just", "target": "workers-e2e"}]
    r = run_fn("check.l2", {"path": str(repo), "gates": gates, "known_reds": path}, tmp_path,
               fakes.env)
    assert r.out["ok"] is True, r.out["summary"]
    assert {x["status"] for x in r.out["results"]} == {"known_red"}
    assert r.out["new_reds"] == []


def test_an_entry_for_one_case_does_not_excuse_another(tmp_path, arc):
    repo, fakes = arc
    failing_target(fakes, "//crates/a:t", "m::other_case")
    path = ledger(tmp_path, entry("//crates/a:t m::known_case"))
    r = run_fn("check.l2", {"path": str(repo), "known_reds": path,
                            "gates": [{"kind": "kiln-test", "target": "//crates/a:t"}]},
               tmp_path, fakes.env)
    assert r.out["ok"] is False
    assert r.out["new_reds"] == ["//crates/a:t m::other_case"]


def test_zero_case_kiln_gate_is_a_new_red(tmp_path, arc):
    repo, fakes = arc
    fakes.respond("kiln", ["test"], stdout=bazel_line("//a:t"),
                  testlogs={"a/t/test.log": libtest(0, filtered=12)})
    r = run_fn("check.l2", {"path": str(repo), "known_reds": ledger(tmp_path),
                            "gates": [{"kind": "kiln-test", "target": "//a:t"}]},
               tmp_path, fakes.env)
    assert r.out["ok"] is False
    assert r.out["results"][0]["status"] == "red"
    assert "needs >= 1" in r.out["results"][0]["summary"]


def test_passing_known_red_is_reported_stale(tmp_path, arc):
    repo, fakes = arc
    fakes.respond("kiln", ["test"], stdout=bazel_line("//a:t"), testlogs={"a/t/test.log":
                                                                         libtest(3)})
    path = ledger(tmp_path, entry("//a:t"))
    r = run_fn("check.l2", {"path": str(repo), "known_reds": path,
                            "gates": [{"kind": "kiln-test", "target": "//a:t"}]},
               tmp_path, fakes.env)
    assert r.out["ok"] is True
    assert r.out["stale"] == ["//a:t"]


def test_script_gate_runs_through_python(tmp_path, arc):
    repo, fakes = arc
    fakes.respond("python3", ["scripts/check_versions.py"], stdout="ok\n")
    r = run_fn("check.l2", {"path": str(repo), "known_reds": ledger(tmp_path),
                            "gates": [{"kind": "script", "target": "scripts/check_versions.py",
                                       "args": ["--strict"]}]},
               tmp_path, fakes.env)
    assert r.out["ok"] is True
    assert fakes.calls("python3")[0]["argv"] == ["scripts/check_versions.py", "--strict"]


def test_invalid_ledger_fails_the_node(tmp_path, arc):
    repo, fakes = arc
    path = ledger(tmp_path, {"test": "//a:t", "reason": "x", "expires": LIVE})
    r = run_fn("check.l2", {"path": str(repo), "known_reds": path,
                            "gates": [{"kind": "just", "target": "x"}]}, tmp_path, fakes.env)
    assert r.code == 1
    assert "missing owner" in r.error["message"]


def test_shipped_ledger_is_valid_and_default(tmp_path, arc):
    repo, fakes = arc
    fakes.respond("just", ["e2e"], exit=1)
    r = run_fn("check.l2", {"path": str(repo), "gates": [{"kind": "just", "target": "e2e"}]},
               tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert r.out["new_reds"] == ["just:e2e"]


def test_l3_runs_workspace_tests_and_refuses_known_reds_by_default(tmp_path, arc):
    repo, fakes = arc
    fakes.respond("kiln", ["test", "//:workspace_tests"], stdout=bazel_line("//a:t"),
                  testlogs={"a/t/test.log": libtest(100)})
    fakes.respond("just", ["workers-e2e"], exit=1)
    path = ledger(tmp_path, entry("just:workers-e2e"))
    inp = {"path": str(repo), "known_reds": path,
           "extra": [{"kind": "just", "target": "workers-e2e"}]}
    r = run_fn("check.l3", inp, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert fakes.calls("kiln")[0]["argv"] == ["test", "//:workspace_tests"]
    assert by_gate(r.out)["kiln-test://:workspace_tests"]["tests_run"] == 100
    assert r.out["ok"] is False and r.out["new_reds"] == []
    assert "does not accept known reds" in r.out["summary"]
    r = run_fn("check.l3", {**inp, "allow_known_reds": True}, tmp_path, fakes.env)
    assert r.out["ok"] is True


def test_l3_zero_receipt_is_red(tmp_path, arc):
    repo, fakes = arc
    fakes.respond("kiln", ["test"], stdout="ERROR: No test targets were found, yet testing "
                  "was requested\n")
    r = run_fn("check.l3", {"path": str(repo), "known_reds": ledger(tmp_path)}, tmp_path,
               fakes.env)
    assert r.out["ok"] is False
    assert r.out["new_reds"] == ["kiln-test://:workspace_tests"]
