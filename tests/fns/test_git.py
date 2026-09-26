"""Tests for packs/git.

All git fns run real git against throwaway repos in tmp_path with a bare
`origin`. `gh` is faked by a script on PATH that records argv NUL-separated
(one call per line) and answers `pr list`/`pr view`.
"""

import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

GIT = Path(__file__).resolve().parents[2] / "src" / "sluice" / "fns"


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
