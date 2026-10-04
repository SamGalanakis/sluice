"""Focused contracts for the lash project's local functions."""

import contextlib
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace

import pytest


FNS = Path(__file__).resolve().parents[1]


def load_fn(name):
    spec = importlib.util.spec_from_file_location(name.replace(".", "_"), FNS / name / "main.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize("mode", ["plan", "lands", "queued", "read_only", "branch"])
def test_worker_header_is_the_one_rule_source_chosen_by_inputs(tmp_path, mode):
    worker = load_fn("lash.worker")
    fork = str(tmp_path / "fork")
    run_dir = tmp_path / "run"
    inp = {"cwd": fork, "ticket": "FIG-1"}
    inp.update({"plan": {}, "lands": {"lands": True}, "queued": {"queued": True},
                "read_only": {"read_only": True}, "branch": {"push_branch": "lanes/x"}}[mode])
    header = worker.worker_header(inp, SimpleNamespace(run_dir=run_dir))

    assert f"cd {fork} && . ./env.sh" in header and "Never git stash" in header
    assert "foreground" in header and "do not spawn subagents" in header
    assert "No AI attribution anywhere" in header
    assert "linear issue view FIG-1" in header
    assert "/workspace/notes/lash/tasks/lanes/fig-1.report.md" in header
    assert f"{run_dir}/summary.txt" in header and "120 words or fewer" in header
    assert "$TASK" not in header and header.rstrip().endswith("Unit notes:")
    proves = mode in ("lands", "queued")
    assert ("Proof before landing" in header) == proves
    assert ("runs_per_test" in header) == proves  # only to forbid it
    assert ("Design rules" in header) == (mode != "read_only")
    assert ("git push origin HEAD:main" in header) == (mode == "lands")
    assert ("land step lands your commit" in header) == (mode == "queued")
    assert ("Read-only" in header) == (mode == "read_only")
    assert ("refs/heads/lanes/x" in header) == (mode == "branch")
    if mode != "read_only":
        assert '"Closes FIG-1"' in header and '"Part of FIG-1"' in header


def test_worker_summary_output_allows_historical_steps():
    definition = json.loads((FNS / "lash.worker" / "fn.json").read_text())
    assert definition["outputs"]["summary"] == "string?"
    assert definition["outputs"]["final"] == "string"


def test_worker_submitted_summary_becomes_final(tmp_path, monkeypatch):
    worker = load_fn("lash.worker")
    report = "Landed abc123. Focused tests: 4 passed; format: 1 passed. Open items: none."
    calls = []

    def fake_agent_run(ctx):
        def fake_main(inp, _ctx):
            calls.append(inp)
            assert inp["report_path"] == str(tmp_path / "summary.txt")
            assert "summary.txt" in inp["spec"]
            return {"report": report, "final": "raw transcript tail", "session": "session-1"}
        return SimpleNamespace(main=fake_main)

    monkeypatch.setattr(worker, "agent_run", fake_agent_run)
    out = worker.main({"engine": "codex", "cwd": str(tmp_path), "spec": "Do the task."},
                      SimpleNamespace(run_dir=tmp_path))
    assert len(calls) == 1
    assert out == {"summary": report, "final": report, "session": "session-1"}


def test_worker_fallback_uses_final_message_with_1500_character_cap(tmp_path, monkeypatch):
    worker = load_fn("lash.worker")
    last_message = "x" * 1700
    monkeypatch.setattr(worker, "agent_run", lambda _ctx: SimpleNamespace(
        main=lambda _inp, _ctx: {"report": None, "final": last_message, "session": "s"}))
    out = worker.main({"engine": "codex", "cwd": str(tmp_path), "spec": "Do the task."},
                      SimpleNamespace(run_dir=tmp_path))
    assert out == {"summary": last_message[:1500], "final": last_message[:1500],
                   "session": "s"}


def test_worker_fallback_uses_harness_final_without_codex_message(tmp_path, monkeypatch):
    worker = load_fn("lash.worker")
    (tmp_path / "codex.log.final").write_text("y" * 1600)
    monkeypatch.setattr(worker, "agent_run", lambda _ctx: SimpleNamespace(
        main=lambda _inp, _ctx: {"report": None, "final": "", "session": "s"}))
    out = worker.main({"engine": "codex", "cwd": str(tmp_path), "spec": "Do the task."},
                      SimpleNamespace(run_dir=tmp_path))
    assert out["final"] == "y" * 1500
    assert out["summary"] == out["final"]


def test_worker_submitted_summary_stays_within_120_words():
    worker = load_fn("lash.worker")
    assert worker.bounded_summary(" ".join(str(n) for n in range(125))) == (
        " ".join(str(n) for n in range(120)))


RUNS = [
    {"databaseId": 105, "workflowName": "CI", "event": "workflow_dispatch",
     "status": "completed"},
    {"databaseId": 100, "workflowName": "CI", "event": "push", "status": "completed"},
    {"databaseId": 103, "workflowName": "Confidence", "event": "workflow_dispatch",
     "status": "completed"},
    {"databaseId": 102, "workflowName": "CI", "event": "workflow_dispatch",
     "status": "in_progress"},
    {"databaseId": 101, "workflowName": "CI", "event": "workflow_dispatch",
     "status": "completed"},
]


def test_main_red_filters_and_orders_canned_runs(monkeypatch):
    main_red = load_fn("lash.main_red")
    calls = []

    def fake_sh(argv, cwd):
        calls.append((argv, cwd))
        return SimpleNamespace(stdout=json.dumps(RUNS))

    monkeypatch.setattr(main_red, "sh", fake_sh)
    repo = "/tmp/lash"
    assert main_red.next_run({"repo": repo})["databaseId"] == 101
    assert main_red.next_run({"repo": repo, "after_run": 101})["databaseId"] == 105
    assert main_red.next_run({"repo": repo, "after_run": 105}) is None
    assert main_red.next_run({"repo": repo, "workflow": "Confidence"})["databaseId"] == 103
    assert main_red.next_run({"repo": repo, "event": "push"})["databaseId"] == 100
    assert all(cwd == repo for _, cwd in calls)
    assert calls[0][0][calls[0][0].index("--workflow") + 1] == "CI"
    assert calls[0][0][calls[0][0].index("--event") + 1] == "workflow_dispatch"


LOG = """Test tail\tRun tail\t2026-09-27T12:00:00Z //crates/lash:a__test FAILED in 1.2s
Test tail\tRun tail\t2026-09-27T12:00:01Z //crates/lash:b__test TIMEOUT in 60.0s
Test tail\tRun tail\t2026-09-27T12:00:02Z test tests::restart::case ... FAILED
Effect group\tRun E2E\t2026-09-27T12:00:03Z [1/2] FAILED  0.12s  tests::effect::one
Effect group\tRun E2E\t2026-09-27T12:00:04Z [2/2] TIMED OUT  240.00s  tests::effect::two
Gates\tRun gates\t2026-09-27T12:00:05Z 2 of 5 gate commands failed:
Gates\tRun gates\t2026-09-27T12:00:06Z - bash scripts/check-one.sh
Gates\tRun gates\t2026-09-27T12:00:07Z - cargo shear
Gates\tRun gates\t2026-09-27T12:00:08Z done
Gates\tRun gates\t2026-09-27T12:00:09Z - unrelated later list item
Test tail\tRun tail\t2026-09-27T12:00:10Z test tests::restart::case ... FAILED
"""


def test_main_red_extracts_all_log_formats_once():
    main_red = load_fn("lash.main_red")
    assert main_red.parse_failed_tests(LOG) == [
        "//crates/lash:a__test", "//crates/lash:b__test", "tests::restart::case",
        "tests::effect::one", "tests::effect::two", "bash scripts/check-one.sh",
        "cargo shear",
    ]


def test_main_red_polls_then_returns_failed_job_ids_and_tests(monkeypatch):
    main_red = load_fn("lash.main_red")
    run = {"databaseId": 201, "workflowName": "CI", "event": "workflow_dispatch",
           "status": "completed", "conclusion": "failure", "headSha": "abc123", "url": "https://example/run"}
    jobs = {"jobs": [{"databaseId": 11, "name": "tail", "conclusion": "failure"},
                     {"databaseId": 12, "name": "gates", "conclusion": "failure"},
                     {"databaseId": 13, "name": "lint", "conclusion": "success"}]}
    calls = []

    def fake_sh(argv, cwd):
        calls.append(argv)
        if argv[1:3] == ["run", "list"]:
            body = [] if sum(call[1:3] == ["run", "list"] for call in calls) == 1 else [run]
            return SimpleNamespace(stdout=json.dumps(body))
        if argv[1:3] == ["run", "view"] and "--json" in argv:
            return SimpleNamespace(stdout=json.dumps(jobs))
        if "--job" in argv:
            return SimpleNamespace(stdout=LOG if argv[argv.index("--job") + 1] == "11"
                                   else "test tests::restart::case ... FAILED\n")
        raise AssertionError(argv)

    sleeps, messages = [], []
    monkeypatch.setattr(main_red, "sh", fake_sh)
    monkeypatch.setattr(main_red.time, "sleep", sleeps.append)
    out = main_red.main({"repo": "/tmp/lash", "interval": 7},
                        SimpleNamespace(log=messages.append))
    assert sleeps == [7] and "checking again in 7s" in messages[0]
    assert out["run_id"] == 201 and out["red"] is True
    assert out["failed_jobs"] == ["tail", "gates"]
    assert out["failed_job_ids"] == [11, 12]
    assert out["failed_tests"] == main_red.parse_failed_tests(LOG)
    assert [call[call.index("--job") + 1] for call in calls if "--job" in call] == ["11", "12"]


@pytest.mark.parametrize(("inp", "expected"), [
    ({"title": "Improve retry path"}, ("Improve retry path", "")),
    ({"title": "Complete retry path", "ticket": "FIG-123", "completes": True},
     ("Complete retry path (FIG-123)", "")),
    ({"title": "Prepare retry path", "ticket": "FIG-123", "completes": False},
     ("Prepare retry path", "Part of FIG-123")),
])
def test_land_message_ticket_cases(inp, expected):
    assert load_fn("lash.land").message(inp) == expected


def test_worker_passes_fusion_to_devin_and_refuses_it_elsewhere(tmp_path, monkeypatch):
    worker = load_fn("lash.worker")
    seen = {}

    def run(inp, _ctx):
        seen.update(inp)
        return {"report": "Landed abc. Open items: none.", "final": "", "session": "s"}

    monkeypatch.setattr(worker, "agent_run", lambda _ctx: SimpleNamespace(main=run))
    worker.main({"engine": "devin", "model": "fusion", "cwd": str(tmp_path), "spec": "Do it."},
                SimpleNamespace(run_dir=tmp_path))
    assert seen["engine"] == "devin" and seen["model"] == "fusion"
    for engine, model in (("opus", "fusion"), ("codex", "fusion"), ("devin", "sol")):
        try:
            worker.main({"engine": engine, "model": model, "cwd": str(tmp_path), "spec": "x"},
                        SimpleNamespace(run_dir=tmp_path))
        except ValueError:
            continue
        raise AssertionError(f"{engine}/{model} was accepted")


def _git(cwd, *args):
    import subprocess
    return subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True,
                          text=True).stdout.strip()


def _commit(repo, files, msg):
    for name, text in files.items():
        (Path(repo) / name).parent.mkdir(parents=True, exist_ok=True)
        (Path(repo) / name).write_text(text)
    _git(repo, "add", "-A")
    _git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", msg)


@contextlib.contextmanager
def _no_lease(_resource, _amount=1):
    yield


@pytest.fixture
def land_repos(tmp_path, monkeypatch):
    """An origin, a fork with one change on it, and a second clone that moves main."""
    origin, fork, other = tmp_path / "origin.git", tmp_path / "fork", tmp_path / "other"
    _git(tmp_path, "init", "-q", "--bare", "-b", "main", str(origin))
    _git(tmp_path, "clone", "-q", str(origin), str(other))
    _commit(other, {"a.rs": "a\n", "b.rs": "b\n", "BUCK": "x\n", "docs/a.md": "a\n", "ci.yml": "a\n"}, "base")
    _git(other, "push", "-q", "origin", "HEAD:main")
    _git(tmp_path, "clone", "-q", str(origin), str(fork))
    land = load_fn("lash.land")
    calls = []

    def kiln(path, args):
        calls.append(args)
        if args == "sync":
            (Path(path) / "BUCK").write_text("regenerated\n")
        return SimpleNamespace(returncode=0, stdout="BUILD SUCCEEDED\n")

    monkeypatch.setattr(land, "kiln", kiln)
    monkeypatch.setattr(land, "SEND_BACK", tmp_path / "absent.sh")
    return SimpleNamespace(land=land, fork=fork, other=other, calls=calls,
                           ctx=SimpleNamespace(log=lambda _m: None, acquire=_no_lease))


def _landed(r):
    out = r.land.main({"fork": str(r.fork)}, r.ctx)
    assert out["landed_sha"] == _git(r.other, "ls-remote", "origin", "main").split()[0]
    return out


def test_land_pushes_without_a_build_when_main_moved_elsewhere(land_repos):
    r = land_repos
    _commit(r.fork, {"a.rs": "mine\n"}, "change")
    _commit(r.other, {"b.rs": "theirs\n"}, "elsewhere")
    _git(r.other, "push", "-q", "origin", "HEAD:main")
    _landed(r)
    assert r.calls == []


def test_land_builds_when_main_touched_the_changes_files(land_repos):
    r = land_repos
    _commit(r.fork, {"a.rs": "a\nmine\n"}, "change")
    _commit(r.other, {"a.rs": "theirs\na\n"}, "same file, no conflict")
    _git(r.other, "push", "-q", "origin", "HEAD:main")
    _landed(r)
    assert r.calls == ["check //..."]


def test_land_regenerates_a_conflicted_generated_file_then_builds(land_repos):
    r = land_repos
    _commit(r.fork, {"BUCK": "mine\n", "a.rs": "mine\n"}, "change")
    _commit(r.other, {"BUCK": "theirs\n"}, "generated conflict")
    _git(r.other, "push", "-q", "origin", "HEAD:main")
    _landed(r)
    assert r.calls == ["sync", "check //..."]
    assert (r.fork / "BUCK").read_text() == "regenerated\n"


def test_land_refuses_a_logic_conflict_and_leaves_the_fork_unrebased(land_repos):
    r = land_repos
    _commit(r.fork, {"a.rs": "mine\n"}, "change")
    head = _git(r.fork, "rev-parse", "HEAD")
    _commit(r.other, {"a.rs": "theirs\n"}, "conflict")
    _git(r.other, "push", "-q", "origin", "HEAD:main")
    with pytest.raises(RuntimeError, match="conflicts in a.rs"):
        r.land.main({"fork": str(r.fork)}, r.ctx)
    assert _git(r.fork, "rev-parse", "HEAD") == head and r.calls == []


def test_land_skips_the_check_when_main_touched_only_the_changes_docs(land_repos):
    r = land_repos
    _commit(r.fork, {"docs/a.md": "a\nmine\n", "a.rs": "mine\n"}, "change")
    _commit(r.other, {"docs/a.md": "theirs\na\n"}, "same doc, no conflict")
    _git(r.other, "push", "-q", "origin", "HEAD:main")
    _landed(r)
    assert r.calls == []


def test_land_skips_the_check_when_the_overlap_is_not_a_build_input(land_repos):
    r = land_repos
    _commit(r.fork, {"ci.yml": "a\nmine\n", "a.rs": "mine\n"}, "change")
    _commit(r.other, {"ci.yml": "theirs\na\n"}, "workflow overlap")
    _git(r.other, "push", "-q", "origin", "HEAD:main")
    _landed(r)
    assert r.calls == []


def test_land_reports_a_hook_refusal_instead_of_retrying_it_as_a_race(land_repos):
    r = land_repos
    _commit(r.fork, {"a.rs": "mine\n"}, "change")
    hook = r.fork / ".git" / "hooks" / "pre-push"
    hook.write_text("#!/bin/sh\necho 'production file-size budget: Failed' >&2\nexit 1\n")
    hook.chmod(0o755)
    with pytest.raises(RuntimeError, match="not a lost race"):
        r.land.main({"fork": str(r.fork)}, r.ctx)


def test_close_leaves_a_partial_change_open(monkeypatch, tmp_path):
    close = load_fn("linear.close")
    calls = []
    monkeypatch.setattr(close, "sh", lambda argv, **kw: calls.append(argv))
    ctx = SimpleNamespace(run_dir=tmp_path)
    out = close.main({"issue": "FIG-1", "message": "Slice one\n\nPart of FIG-1"}, ctx)
    assert out["closed"] is False and not any("update" in a for a in calls)
    out = close.main({"issue": "FIG-1", "message": "Done\n\nCloses FIG-1"}, ctx)
    assert out["closed"] is True and any("update" in a for a in calls)
