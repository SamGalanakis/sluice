"""Tests for how the native supervisor keeps a session on track and what it reports about it.
The supervisor is driven by test_native's scripted Model (its pane runs `sleep`, or `sh` where
the pane must start processes)."""

import json
import sys
from pathlib import Path

import pytest

from sluice.fn import Transient

AGENTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(AGENTS))
sys.path.insert(0, str(Path(__file__).parent))

from _agents.native.claude import Claude
from _agents.native.processes import engine_env
from test_agents import init_repo, make_claude
from test_native import Model, run


def repo_at(tmp_path):
    """A git repo at tmp_path/work, the cwd `run` gives the model."""
    return init_repo(tmp_path / "work")


def commit(repo, name, text="x\n"):
    (repo.path / name).write_text(text)
    repo.git("add", name)
    repo.git("commit", "-q", "-m", f"add {name}")


# ---- git facts ---------------------------------------------------------------------------------

def test_git_facts_keep_the_first_attempts_head_across_a_transient_retry(tmp_path):
    repo = repo_at(tmp_path)
    start = repo.git("rev-parse", "HEAD")

    def first(m, n, text):
        commit(repo, "a.txt")
        m.error = "rate limit exceeded"

    with pytest.raises(Transient):
        run(Model(first), tmp_path)
    assert json.loads((tmp_path / "run" / "native.json").read_text())["head_before"] == start

    def second(m, n, text):
        commit(repo, "b.txt")
        m.submit(word="w")

    out, _ = run(Model(second), tmp_path, attempt=2)
    assert out["git"] == {"head_before": start, "head_after": repo.git("rev-parse", "HEAD"),
                          "commits": 2, "dirty": False}


def test_outside_a_git_worktree_there_are_no_git_facts(tmp_path):
    out, _ = run(Model(lambda m, n, text: m.submit(word="w")), tmp_path)
    assert out["git"] is None
    assert "head_before" not in json.loads((tmp_path / "run" / "native.json").read_text())


def test_an_agent_fn_reports_git_facts_across_its_own_retry(call_fn, tmp_path):
    repo = init_repo(tmp_path / "repo")
    start = repo.git("rev-parse", "HEAD")
    work = "echo {0} > {0}.txt && git add {0}.txt && git -c user.email=a@b -c user.name=A " \
           "commit -q -m {0}"
    env, _ = make_claude(tmp_path, [{"run": work.format("one"), "error": "API Error: 529"},
                                    {"run": work.format("two"), "reply": "done"}])
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(repo.path), "prompt": "p"},
                             env=env)
    assert code == 0, err
    assert "transient (attempt 1)" in err
    assert out["git"] == {"head_before": start, "head_after": repo.git("rev-parse", "HEAD"),
                          "commits": 2, "dirty": False}


def test_agent_review_counts_commits_across_a_retry(call_fn, tmp_path):
    repo = init_repo(tmp_path / "repo")
    repo.git("branch", "base")
    work = "echo {0} > {0}.txt && git add {0}.txt && git -c user.email=a@b -c user.name=A " \
           "commit -q -m {0}"
    env, _ = make_claude(tmp_path, [{"run": work.format("one"), "error": "API Error: 529"},
                                    {"run": work.format("two"), "reply": "fixed"}])
    code, out, err = call_fn(AGENTS / "agent.review",
                             {"cwd": str(repo.path), "base": "base", "standards": "S.md"},
                             env=env)
    assert code == 0, err
    assert out["commits"] == 2 and out["sha"] == repo.git("rev-parse", "HEAD")
    assert out["git"]["commits"] == 2


# ---- the git environment -----------------------------------------------------------------------

GIT_QUIET = {"GIT_TERMINAL_PROMPT": "0", "GIT_EDITOR": "true", "GIT_MERGE_AUTOEDIT": "no"}


def test_git_never_prompts_in_either_env_builder(monkeypatch):
    monkeypatch.setenv("CLAUDECODE", "1")
    for env in (engine_env(), Claude().env()):
        assert GIT_QUIET.items() <= env.items() and "CLAUDECODE" not in env


def test_claude_env_is_the_engine_env():
    assert Claude().env() == engine_env()
