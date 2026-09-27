"""Tests for packs/git.

All git fns run real git against throwaway repos in tmp_path with a bare
`origin`. `gh` is faked by a script on PATH that records argv NUL-separated
(one call per line) and answers `pr list`/`pr view`.
"""

import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

GIT = Path(__file__).resolve().parents[1]


def read_calls(path):
    """Decode one line of NUL-separated argv per recorded call."""
    return [
        [a.decode() for a in line.split(b"\0") if a]
        for line in path.read_bytes().split(b"\n")
        if line
    ]


def gdir(path):
    def g(*args, check=True):
        return subprocess.run(
            ["git", "-C", str(path), *args],
            check=check, capture_output=True, text=True).stdout.strip()
    return g


@pytest.fixture
def repo(tmp_path):
    """A repo on `main` with one commit, wired to a bare `origin` remote."""
    path = tmp_path / "repo"
    path.mkdir()
    g = gdir(path)
    g("init", "-b", "main")
    g("config", "user.email", "t@example.com")
    g("config", "user.name", "T")
    (path / "f.txt").write_text("one\n")
    g("add", ".")
    g("commit", "-m", "init")
    bare = tmp_path / "origin.git"
    subprocess.run(["git", "init", "--bare", str(bare)],
                   check=True, capture_output=True)
    g("remote", "add", "origin", str(bare))
    g("push", "-u", "origin", "main")
    return SimpleNamespace(path=path, git=g, origin=bare)


def test_worktree_new_branch_default_path(call_fn, repo):
    code, out, err = call_fn(
        GIT / "git.worktree",
        {"repo": str(repo.path), "base": "main", "branch": "feat"})
    assert code == 0, err
    expected = repo.path.parent / "repo-wt" / "feat"
    assert out == {
        "path": str(expected.resolve()),
        "branch": "feat",
        "sha": repo.git("rev-parse", "main"),
    }
    assert gdir(expected)("rev-parse", "--abbrev-ref", "HEAD") == "feat"


def test_worktree_explicit_path(call_fn, repo, tmp_path):
    wt = tmp_path / "wt-here"
    code, out, err = call_fn(
        GIT / "git.worktree",
        {"repo": str(repo.path), "base": "main", "branch": "feat",
         "path": str(wt)})
    assert code == 0, err
    assert out["path"] == str(wt.resolve())
    assert (wt / "f.txt").exists()


def test_worktree_existing_branch(call_fn, repo, tmp_path):
    repo.git("branch", "exists")
    sha = repo.git("rev-parse", "exists")
    wt = tmp_path / "wt2"
    code, out, err = call_fn(
        GIT / "git.worktree",
        {"repo": str(repo.path), "base": "main", "branch": "exists",
         "path": str(wt)})
    assert code == 0, err
    assert out["sha"] == sha
    assert out["branch"] == "exists"


def test_worktree_rm(call_fn, repo, tmp_path):
    wt = tmp_path / "wt-gone"
    code, _, err = call_fn(
        GIT / "git.worktree",
        {"repo": str(repo.path), "base": "main", "branch": "feat",
         "path": str(wt)})
    assert code == 0, err
    code, out, err = call_fn(
        GIT / "git.worktree_rm", {"repo": str(repo.path), "path": str(wt)})
    assert code == 0, err
    assert out == {"removed": True}
    assert not wt.exists()
    code, out, err = call_fn(
        GIT / "git.worktree_rm", {"repo": str(repo.path), "path": str(wt)})
    assert code == 0, err
    assert out == {"removed": False}


def test_worktree_rm_needs_force(call_fn, repo, tmp_path):
    wt = tmp_path / "wt-dirty"
    code, _, err = call_fn(
        GIT / "git.worktree",
        {"repo": str(repo.path), "base": "main", "branch": "feat",
         "path": str(wt)})
    assert code == 0, err
    (wt / "f.txt").write_text("dirty\n")
    code, out, err = call_fn(
        GIT / "git.worktree_rm", {"repo": str(repo.path), "path": str(wt)})
    assert code == 1  # git refuses to remove a dirty worktree
    code, out, err = call_fn(
        GIT / "git.worktree_rm",
        {"repo": str(repo.path), "path": str(wt), "force": True})
    assert code == 0, err
    assert out == {"removed": True}


def test_worktree_rm_plain_dir_is_not_a_worktree(call_fn, repo, tmp_path):
    plain = tmp_path / "plain"
    plain.mkdir()
    code, out, err = call_fn(
        GIT / "git.worktree_rm", {"repo": str(repo.path), "path": str(plain)})
    assert code == 0, err
    assert out == {"removed": False}
    assert plain.exists()


def test_head(call_fn, repo):
    code, out, err = call_fn(GIT / "git.head", {"path": str(repo.path)})
    assert code == 0, err
    assert out == {"branch": "main", "sha": repo.git("rev-parse", "HEAD")}


def test_merge_detached_target(call_fn, repo):
    """target is not checked out anywhere: plain `worktree add <wt> target`."""
    repo.git("checkout", "-b", "feature")
    (repo.path / "g.txt").write_text("feature\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "feature work")
    repo.git("checkout", "main")
    repo.git("branch", "release")  # target exists, not checked out

    code, out, err = call_fn(
        GIT / "git.merge",
        {"repo": str(repo.path), "source": "feature", "target": "release",
         "message": "merge feature into release"})
    assert code == 0, err
    assert out["merged"] is True
    assert out["conflicts"] == []
    assert out["sha"] == repo.git("rev-parse", "release")
    assert "g.txt" in repo.git("ls-tree", "--name-only", "release")
    # merge commit has two parents
    assert len(repo.git("rev-list", "--parents", "-n", "1", "release").split()) == 3
    # temporary worktree is gone
    assert len(repo.git("worktree", "list").splitlines()) == 1


def test_merge_checked_out_target_and_push(call_fn, repo):
    """target is checked out in the main worktree: merge detached, then move ref."""
    repo.git("checkout", "-b", "feature")
    (repo.path / "g.txt").write_text("feature\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "feature work")
    repo.git("checkout", "main")
    old_main = repo.git("rev-parse", "main")

    code, out, err = call_fn(
        GIT / "git.merge",
        {"repo": str(repo.path), "source": "feature", "target": "main",
         "push": True})
    assert code == 0, err
    assert out["merged"] is True
    assert out["sha"] != old_main
    assert repo.git("rev-parse", "main") == out["sha"]
    origin_main = subprocess.run(
        ["git", "--git-dir", str(repo.origin), "rev-parse", "main"],
        check=True, capture_output=True, text=True).stdout.strip()
    assert origin_main == out["sha"]
    assert len(repo.git("worktree", "list").splitlines()) == 1


def test_merge_conflict(call_fn, repo):
    repo.git("checkout", "-b", "feature")
    (repo.path / "f.txt").write_text("feature version\n")
    repo.git("commit", "-am", "feature edit")
    repo.git("checkout", "main")
    (repo.path / "f.txt").write_text("main version\n")
    repo.git("commit", "-am", "main edit")
    old_main = repo.git("rev-parse", "main")

    code, out, err = call_fn(
        GIT / "git.merge",
        {"repo": str(repo.path), "source": "feature", "target": "main"})
    assert code == 0, err
    assert out == {"merged": False, "sha": None, "conflicts": ["f.txt"]}
    assert repo.git("rev-parse", "main") == old_main
    assert len(repo.git("worktree", "list").splitlines()) == 1


def test_rebase_ok(call_fn, repo):
    repo.git("checkout", "-b", "topic")
    (repo.path / "t.txt").write_text("topic\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "topic work")
    repo.git("checkout", "main")
    (repo.path / "m.txt").write_text("main\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "main work")
    repo.git("checkout", "topic")

    code, out, err = call_fn(
        GIT / "git.rebase", {"path": str(repo.path), "onto": "main"})
    assert code == 0, err
    assert out == {"ok": True, "sha": repo.git("rev-parse", "HEAD"),
                   "conflicts": []}
    repo.git("merge-base", "--is-ancestor", "main", "topic")
    assert (repo.path / "m.txt").exists()
    assert (repo.path / "t.txt").exists()


def test_rebase_conflict(call_fn, repo):
    repo.git("checkout", "-b", "topic")
    (repo.path / "f.txt").write_text("topic version\n")
    repo.git("commit", "-am", "topic edit")
    topic_sha = repo.git("rev-parse", "HEAD")
    repo.git("checkout", "main")
    (repo.path / "f.txt").write_text("main version\n")
    repo.git("commit", "-am", "main edit")
    repo.git("checkout", "topic")

    code, out, err = call_fn(
        GIT / "git.rebase", {"path": str(repo.path), "onto": "main"})
    assert code == 0, err
    assert out == {"ok": False, "sha": topic_sha, "conflicts": ["f.txt"]}
    # rebase fully aborted
    assert repo.git("rev-parse", "HEAD") == topic_sha
    assert repo.git("rev-parse", "-q", "--verify", "REBASE_HEAD",
                    check=False) == ""
    assert repo.git("status", "--porcelain") == ""


def test_push(call_fn, repo):
    (repo.path / "n.txt").write_text("new\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "new work")
    head = repo.git("rev-parse", "HEAD")
    code, out, err = call_fn(
        GIT / "git.push", {"path": str(repo.path), "branch": "main"})
    assert code == 0, err
    assert out == {"sha": head}
    origin_main = subprocess.run(
        ["git", "--git-dir", str(repo.origin), "rev-parse", "main"],
        check=True, capture_output=True, text=True).stdout.strip()
    assert origin_main == head


def test_push_force_with_lease(call_fn, repo):
    (repo.path / "f.txt").write_text("amended\n")
    repo.git("commit", "-am", "amended", "--amend", "--no-edit")
    head = repo.git("rev-parse", "HEAD")
    code, out, err = call_fn(
        GIT / "git.push",
        {"path": str(repo.path), "branch": "main", "force_with_lease": True})
    assert code == 0, err
    assert out == {"sha": head}
    origin_main = subprocess.run(
        ["git", "--git-dir", str(repo.origin), "rev-parse", "main"],
        check=True, capture_output=True, text=True).stdout.strip()
    assert origin_main == head


def make_gh(tmp_path, fake_bin, *, list_json="[]",
            view_json='{"number": 12, "url": "https://example.test/pr/12"}'):
    argv_file = tmp_path / "gh.argv"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" >> \"{argv_file}\"\n"
        f"printf '\\n' >> \"{argv_file}\"\n"
        'if [ "$1" = "pr" ] && [ "$2" = "list" ]; then\n'
        f"  printf '%s\\n' '{list_json}'\n"
        'elif [ "$1" = "pr" ] && [ "$2" = "view" ]; then\n'
        f"  printf '%s\\n' '{view_json}'\n"
        "fi\n"
        "exit 0\n"
    )
    bin_dir = fake_bin("gh", script)
    return bin_dir, argv_file


def test_gh_pr_create(call_fn, fake_bin, repo, tmp_path):
    bin_dir, argv_file = make_gh(tmp_path, fake_bin)
    code, out, err = call_fn(
        GIT / "gh.pr",
        {"path": str(repo.path), "base": "main", "head": "feature",
         "title": "My title", "body": "My body", "draft": True},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"number": 12, "url": "https://example.test/pr/12"}
    calls = read_calls(argv_file)
    assert calls[0] == ["pr", "list", "--head", "feature",
                        "--json", "number,url"]
    assert calls[1] == ["pr", "create", "--base", "main", "--head", "feature",
                        "--title", "My title", "--body", "My body", "--draft"]
    assert calls[2] == ["pr", "view", "feature", "--json", "number,url"]
    assert len(calls) == 3


def test_gh_pr_edit_existing(call_fn, fake_bin, repo, tmp_path):
    bin_dir, argv_file = make_gh(
        tmp_path, fake_bin,
        list_json='[{"number": 7, "url": "https://example.test/pr/7"}]',
        view_json='{"number": 7, "url": "https://example.test/pr/7"}')
    code, out, err = call_fn(
        GIT / "gh.pr",
        {"path": str(repo.path), "base": "main", "head": "feature",
         "title": "New title", "body": "New body"},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"number": 7, "url": "https://example.test/pr/7"}
    calls = read_calls(argv_file)
    assert calls[1] == ["pr", "edit", "7", "--title", "New title",
                        "--body", "New body"]
    assert not any(c[1] == "create" for c in calls)
    assert "--draft" not in calls[1]


def test_gh_pr_create_not_draft(call_fn, fake_bin, repo, tmp_path):
    bin_dir, argv_file = make_gh(tmp_path, fake_bin)
    code, _out, err = call_fn(
        GIT / "gh.pr",
        {"path": str(repo.path), "base": "main", "head": "feature",
         "title": "T", "body": "B"},
        path=bin_dir,
    )
    assert code == 0, err
    calls = read_calls(argv_file)
    assert "--draft" not in calls[1]


def pr_json(state="OPEN", mergeable="MERGEABLE", sha="abc123",
            url="https://example.test/pr/3", rollup=None):
    return json.dumps({"state": state, "mergeable": mergeable, "headRefOid": sha,
                       "url": url, "statusCheckRollup": rollup or []})


def check(name, status="COMPLETED", conclusion="SUCCESS"):
    """A CheckRun rollup entry."""
    return {"name": name, "status": status, "conclusion": conclusion}


def context(name, state="SUCCESS"):
    """A StatusContext rollup entry."""
    return {"context": name, "state": state}


def make_gh_seq(tmp_path, fake_bin, responses):
    """A fake gh answering each `pr view` with the next canned JSON, then the last."""
    argv_file = tmp_path / "gh.argv"
    resp = tmp_path / "gh-resp"
    resp.mkdir(exist_ok=True)
    for f in resp.glob("*.json"):
        f.unlink()
    for i, body in enumerate(responses):
        (resp / f"{i}.json").write_text(body)
    (resp / "n").write_text("0")
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" >> \"{argv_file}\"\n"
        f"printf '\\n' >> \"{argv_file}\"\n"
        "n=0\n"
        f'[ -f "{resp}/n" ] && n=$(cat "{resp}/n")\n'
        f"echo $((n + 1)) > \"{resp}/n\"\n"
        f'f="{resp}/$n.json"\n'
        'while [ ! -f "$f" ]; do n=$((n - 1)); '
        f'f="{resp}/$n.json"; done\n'
        'cat "$f"\n'
    )
    return fake_bin("gh", script), argv_file


def make_gh_failing(tmp_path, fake_bin, msg="gh: network unreachable"):
    argv_file = tmp_path / "gh.argv"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" >> \"{argv_file}\"\n"
        f"printf '\\n' >> \"{argv_file}\"\n"
        f'echo "{msg}" >&2\n'
        "exit 1\n"
    )
    return fake_bin("gh", script), argv_file


def make_gh_runs(tmp_path, fake_bin, *, list_body="[]", jobs_body="{}",
                 cancel_code=0, cancel_err=""):
    """A fake gh answering `run list`/`run view`/`run cancel` from canned bodies."""
    argv_file = tmp_path / "gh.argv"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" >> \"{argv_file}\"\n"
        f"printf '\\n' >> \"{argv_file}\"\n"
        'if [ "$1" = "run" ] && [ "$2" = "list" ]; then\n'
        f"  printf '%s\\n' '{list_body}'\n"
        'elif [ "$1" = "run" ] && [ "$2" = "view" ]; then\n'
        f"  printf '%s\\n' '{jobs_body}'\n"
        'elif [ "$1" = "run" ] && [ "$2" = "cancel" ]; then\n'
        f"  printf '%s' '{cancel_err}' >&2\n"
        f"  exit {cancel_code}\n"
        "fi\n"
        "exit 0\n"
    )
    return fake_bin("gh", script), argv_file


def pr_wait(call_fn, bin_dir, tmp_path, until, **kw):
    return call_fn(
        GIT / "gh.pr_wait",
        {"path": str(tmp_path), "pr": "3", "until": until, "interval": 0, **kw},
        path=bin_dir)


def test_pr_wait_green(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_gh_seq(tmp_path, fake_bin, [
        pr_json(rollup=[check("build"), context("ci/ok")])])
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, "checks")
    assert code == 0, err
    assert out == {"state": "green", "sha": "abc123",
                   "url": "https://example.test/pr/3", "failed": []}
    calls = read_calls(argv_file)
    assert calls == [["pr", "view", "3", "--json",
                      "state,mergeable,headRefOid,url,statusCheckRollup"]]


def test_pr_wait_pending_then_green(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_gh_seq(tmp_path, fake_bin, [
        pr_json(rollup=[check("build", "IN_PROGRESS", None),
                        context("ci/ok", "PENDING")]),
        pr_json(rollup=[check("build"), context("ci/ok")]),
    ])
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, "checks")
    assert code == 0, err
    assert out["state"] == "green"
    assert len(read_calls(argv_file)) == 2


def test_pr_wait_red(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_gh_seq(tmp_path, fake_bin, [
        pr_json(rollup=[check("build", conclusion="FAILURE"),
                        context("ci/x", "ERROR"),
                        check("test")])])
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, "checks")
    assert code == 0, err
    assert out["state"] == "red"
    assert out["failed"] == ["build", "ci/x"]


def test_pr_wait_failure_while_pending_keeps_polling(call_fn, fake_bin, tmp_path):
    """A failed check while others still run is not yet red."""
    bin_dir, argv_file = make_gh_seq(tmp_path, fake_bin, [
        pr_json(rollup=[check("a", conclusion="FAILURE"),
                        check("b", "IN_PROGRESS", None)]),
        pr_json(rollup=[check("a", conclusion="TIMED_OUT"),
                        check("b")]),
    ])
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, "checks")
    assert code == 0, err
    assert out["state"] == "red"
    assert out["failed"] == ["a"]
    assert len(read_calls(argv_file)) == 2


def test_pr_wait_no_checks_is_green(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_gh_seq(tmp_path, fake_bin, [pr_json()])
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, "checks")
    assert code == 0, err
    assert out["state"] == "green"


@pytest.mark.parametrize("gh_state,mergeable,state", [
    ("MERGED", "MERGEABLE", "merged"),
    ("CLOSED", "MERGEABLE", "closed"),
    ("OPEN", "CONFLICTING", "conflicting"),
])
def test_pr_wait_terminal_states(call_fn, fake_bin, tmp_path,
                                 gh_state, mergeable, state):
    for until in ("checks", "merged"):
        bin_dir, _ = make_gh_seq(
            tmp_path, fake_bin,
            [pr_json(state=gh_state, mergeable=mergeable,
                     rollup=[check("build")])])
        code, out, err = pr_wait(call_fn, bin_dir, tmp_path, until)
        assert code == 0, err
        assert out["state"] == state


def test_pr_wait_until_merged_waits_past_green(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_gh_seq(tmp_path, fake_bin, [
        pr_json(rollup=[check("build")]),
        pr_json(state="MERGED", rollup=[check("build")]),
    ])
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, "merged")
    assert code == 0, err
    assert out["state"] == "merged"
    assert len(read_calls(argv_file)) == 2


def test_pr_wait_until_merged_red_stops(call_fn, fake_bin, tmp_path):
    """Red checks end a merged wait too, so the caller can act."""
    bin_dir, _ = make_gh_seq(tmp_path, fake_bin, [
        pr_json(rollup=[check("build", conclusion="FAILURE")])])
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, "merged")
    assert code == 0, err
    assert out["state"] == "red"
    assert out["failed"] == ["build"]


@pytest.mark.parametrize("until", ["checks", "merged"])
def test_pr_wait_timeout(call_fn, fake_bin, tmp_path, until):
    responses = [pr_json(rollup=[check("build", "IN_PROGRESS", None)])]
    if until == "merged":  # green-but-unmerged also keeps polling
        responses = [pr_json(rollup=[check("build")])]
    bin_dir, argv_file = make_gh_seq(tmp_path, fake_bin, responses)
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, until, timeout=0)
    assert code == 0, err
    assert out["state"] == "timeout"
    assert out["sha"] == "abc123"  # the last sha seen
    assert len(read_calls(argv_file)) == 1


def test_pr_wait_gh_failure_is_transient(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_gh_failing(tmp_path, fake_bin)
    code, out, err = pr_wait(call_fn, bin_dir, tmp_path, "checks")
    assert code == 1
    assert "transient (attempt 1)" in err  # the helper retried before giving up
    assert out is None


def run_json(**kw):
    r = {"databaseId": 991, "headSha": "feed42", "status": "completed",
         "conclusion": "success", "url": "https://example.test/run/991",
         "workflowName": "CI"}
    r.update(kw)
    return json.dumps([r])


def test_run_latest_success(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_gh_runs(tmp_path, fake_bin, list_body=run_json())
    code, out, err = call_fn(
        GIT / "gh.run_latest", {"path": str(tmp_path)}, path=bin_dir)
    assert code == 0, err
    assert out == {"run_id": 991, "sha": "feed42", "status": "completed",
                   "conclusion": "success", "url": "https://example.test/run/991",
                   "workflow": "CI", "failed_jobs": []}
    calls = read_calls(argv_file)
    fields = "databaseId,headSha,status,conclusion,url,workflowName"
    assert calls == [["run", "list", "--branch", "main", "--limit", "1",
                      "--json", fields]]


def test_run_latest_branch_and_workflow(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_gh_runs(tmp_path, fake_bin, list_body=run_json())
    code, _out, err = call_fn(
        GIT / "gh.run_latest",
        {"path": str(tmp_path), "branch": "dev", "workflow": "ci.yml"},
        path=bin_dir)
    assert code == 0, err
    calls = read_calls(argv_file)
    assert calls[0][:4] == ["run", "list", "--branch", "dev"]
    assert calls[0][4:6] == ["--workflow", "ci.yml"]


def test_run_latest_failed_jobs(call_fn, fake_bin, tmp_path):
    jobs = {"jobs": [
        {"name": "build", "conclusion": "failure"},
        {"name": "lint", "conclusion": "success"},
        {"name": "docs", "conclusion": "skipped"},
        {"name": "mac", "conclusion": "cancelled"},
        {"name": "slow", "conclusion": "timed_out"},
    ]}
    bin_dir, argv_file = make_gh_runs(
        tmp_path, fake_bin, list_body=run_json(conclusion="failure"),
        jobs_body=json.dumps(jobs))
    code, out, err = call_fn(
        GIT / "gh.run_latest", {"path": str(tmp_path)}, path=bin_dir)
    assert code == 0, err
    assert out["conclusion"] == "failure"
    assert out["failed_jobs"] == ["build", "mac", "slow"]
    calls = read_calls(argv_file)
    assert calls[1] == ["run", "view", "991", "--json", "jobs"]


def test_run_latest_no_runs(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_gh_runs(tmp_path, fake_bin, list_body="[]")
    code, out, err = call_fn(
        GIT / "gh.run_latest",
        {"path": str(tmp_path), "branch": "dev"}, path=bin_dir)
    assert code == 1
    assert "no runs on dev" in err
    assert out is None


def test_run_cancel(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_gh_runs(tmp_path, fake_bin)
    code, out, err = call_fn(
        GIT / "gh.run_cancel", {"path": str(tmp_path), "run_id": 991},
        path=bin_dir)
    assert code == 0, err
    assert out == {"cancelled": True}
    assert read_calls(argv_file) == [["run", "cancel", "991"]]


def test_run_cancel_already_completed(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_gh_runs(
        tmp_path, fake_bin, cancel_code=1,
        cancel_err="cannot cancel a workflow run that is completed")
    code, out, err = call_fn(
        GIT / "gh.run_cancel", {"path": str(tmp_path), "run_id": 991},
        path=bin_dir)
    assert code == 0, err
    assert out == {"cancelled": False}


def test_run_cancel_other_failure(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_gh_runs(
        tmp_path, fake_bin, cancel_code=1, cancel_err="HTTP 404: not found")
    code, out, _err = call_fn(
        GIT / "gh.run_cancel", {"path": str(tmp_path), "run_id": 991},
        path=bin_dir)
    assert code == 1
    assert out is None


# ---- a plan-supplied ref that looks like an option is refused before the tool runs --------


def make_git(tmp_path, fake_bin):
    """A fake git that records argv (one call per line); it must never be invoked."""
    argv_file = tmp_path / "git.argv"
    script = ("#!/bin/sh\n"
              f"printf '%s\\0' \"$@\" >> \"{argv_file}\"\n"
              f"printf '\\n' >> \"{argv_file}\"\n")
    return fake_bin("git", script), argv_file


@pytest.mark.parametrize("fn,inputs", [
    ("git.push", {"path": ".", "branch": "--upload-pack=touch-pwned"}),
    ("git.push", {"path": ".", "branch": "main", "remote": "--all"}),
    ("git.rebase", {"path": ".", "onto": "--exec=touch-pwned"}),
    ("git.merge", {"repo": ".", "source": "-m pwned", "target": "main"}),
    ("git.merge", {"repo": ".", "source": "feat", "target": "--detach"}),
    ("git.worktree", {"repo": ".", "base": "main", "branch": "-b"}),
    ("git.worktree", {"repo": ".", "base": "--orphan", "branch": "feat"}),
])
def test_git_fns_refuse_refs_that_look_like_options(call_fn, fake_bin, tmp_path,
                                                    fn, inputs):
    bin_dir, argv_file = make_git(tmp_path, fake_bin)
    code, out, err = call_fn(GIT / fn, inputs, path=bin_dir)
    assert code == 1
    assert "may not start with '-'" in err
    assert out is None
    assert not argv_file.exists()  # git never ran


@pytest.mark.parametrize("fn,inputs", [
    ("gh.pr", {"path": ".", "base": "main", "head": "--repo=o/r",
               "title": "t", "body": "b"}),
    ("gh.pr", {"path": ".", "base": "--repo=o/r", "head": "feat",
               "title": "t", "body": "b"}),
    ("gh.pr_wait", {"path": ".", "pr": "--repo=o/r", "until": "checks",
                    "timeout": 0}),
    ("gh.run_latest", {"path": ".", "branch": "--limit"}),
])
def test_gh_fns_refuse_refs_that_look_like_options(call_fn, fake_bin, tmp_path,
                                                   fn, inputs):
    bin_dir, argv_file = make_gh(tmp_path, fake_bin)
    code, out, err = call_fn(GIT / fn, inputs, path=bin_dir)
    assert code == 1
    assert "may not start with '-'" in err
    assert out is None
    assert not argv_file.exists()  # gh never ran


def test_run_cancel_refuses_a_negative_run_id(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_gh_runs(tmp_path, fake_bin)
    code, out, err = call_fn(
        GIT / "gh.run_cancel", {"path": str(tmp_path), "run_id": -1},
        path=bin_dir)
    assert code == 1
    assert "may not start with '-'" in err
    assert out is None
    assert not argv_file.exists()  # gh never ran
