"""Tests for how the native supervisor keeps a session on track and what it reports about it.
The supervisor is driven by test_native's scripted Model (its pane runs `sleep`, or `sh` where
the pane must start processes)."""

import sys
from pathlib import Path

AGENTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(AGENTS))
sys.path.insert(0, str(Path(__file__).parent))

from _agents.native.claude import Claude
from _agents.native.processes import engine_env

# ---- the git environment -----------------------------------------------------------------------

GIT_QUIET = {"GIT_TERMINAL_PROMPT": "0", "GIT_EDITOR": "true", "GIT_MERGE_AUTOEDIT": "no"}


def test_git_never_prompts_in_either_env_builder(monkeypatch):
    monkeypatch.setenv("CLAUDECODE", "1")
    for env in (engine_env(), Claude().env()):
        assert GIT_QUIET.items() <= env.items() and "CLAUDECODE" not in env


def test_claude_env_is_the_engine_env():
    assert Claude().env() == engine_env()
