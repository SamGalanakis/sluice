"""lash.fork / lash.fork_rm against a fake kiln that manages real git worktrees."""

import pytest
from helpers import commit, git, make_fakes, make_repo, run_fn


@pytest.fixture
def env(tmp_path):
    repo = make_repo(tmp_path)
    fakes = make_fakes(tmp_path, repo)
    return repo, fakes


def test_fork_cuts_a_kiln_fork_and_branches_from_origin_main(tmp_path, env):
    repo, fakes = env
    main_sha = git(repo, "rev-parse", "origin/main")
    r = run_fn("lash.fork", {"name": "fig-1-store"}, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert fakes.calls("kiln")[0]["argv"] == ["fork", "lash", "fig-1-store"]
    out = r.out
    assert out["path"] == str((tmp_path / "forks" / "fig-1-store").resolve())
    assert out["branch"] == "fig-1-store" and out["base"] == "origin/main"
    assert out["sha"] == main_sha
    assert git(tmp_path / "forks" / "fig-1-store", "branch", "--show-current") == "fig-1-store"


def test_fork_prefers_the_remote_arc_branch_over_a_stale_local_one(tmp_path, env):
    repo, fakes = env
    git(repo, "branch", "arc-x")  # local arc-x stays at main
    git(repo, "switch", "-q", "-c", "tmp")
    arc_sha = commit(repo, "a.txt", "arc\n")
    git(repo, "push", "-q", "origin", "tmp:arc-x")
    r = run_fn("lash.fork", {"name": "t1", "base": "arc-x"}, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert r.out["base"] == "origin/arc-x"
    assert r.out["sha"] == arc_sha


def test_fork_detached_creates_no_branch(tmp_path, env):
    repo, fakes = env
    r = run_fn("lash.fork", {"name": "land", "detach": True}, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert r.out["branch"] == ""
    assert git(tmp_path / "forks" / "land", "branch", "--show-current") == ""
    assert "land" not in git(repo, "branch", "--list", "land")


def test_fork_fails_on_an_unknown_base(tmp_path, env):
    _, fakes = env
    r = run_fn("lash.fork", {"name": "t2", "base": "no-such-branch"}, tmp_path, fakes.env)
    assert r.code == 1
    assert "does not resolve" in r.error["message"]


def test_fork_rm_calls_kiln_rm(tmp_path, env):
    _, fakes = env
    assert run_fn("lash.fork", {"name": "t3"}, tmp_path, fakes.env).code == 0
    r = run_fn("lash.fork_rm", {"name": "t3"}, tmp_path, fakes.env)
    assert r.code == 0, r.stderr
    assert r.out == {"removed": True}
    assert fakes.calls("kiln")[-1]["argv"] == ["rm", "lash", "t3"]
    assert not (tmp_path / "forks" / "t3").exists()
