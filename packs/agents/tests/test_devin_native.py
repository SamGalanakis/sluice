"""Devin's per-run hook stream and native session contract."""

import json
import os
import sqlite3
import sys
import tempfile
import time
from pathlib import Path

import pytest

from sluice.runner import Runner
from sluice.store import Store

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from _agents.native.devin import Composer, Devin


def test_devin_config_keeps_user_settings_and_registers_hooks(tmp_path, monkeypatch):
    home = tmp_path / "config"
    home.joinpath("devin").mkdir(parents=True)
    home.joinpath("devin/config.json").write_text(
        '// comment\n{"theme_mode": "dark", "agent": {"model": "swe-2-high"}, '
        '"permissions": {"allow": ["read"]}, '
        '"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "true"}]}]}, '
        '"link": "https://example.org/a//b"} /* end */')
    monkeypatch.setenv("XDG_CONFIG_HOME", str(home))
    devin = Devin()
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    devin.prepare(run_dir, str(tmp_path), None)
    cfg = json.loads(devin.config_file.read_text())
    assert cfg["theme_mode"] == "dark"
    assert cfg["permissions"] == {"allow": ["read"]}
    assert cfg["link"] == "https://example.org/a//b"
    assert set(cfg["hooks"]) >= {"SessionStart", "UserPromptSubmit", "Stop"}
    assert len(cfg["hooks"]["SessionStart"]) == 2
    assert devin.argv()[-4:] == ["--permission-mode", "dangerous",
                                 "--respect-workspace-trust", "false"]
    assert devin.config_file.stat().st_mode & 0o777 == 0o600


def test_hook_stream_tracks_turns_progress_and_final(tmp_path):
    devin = Devin(log=tmp_path / "log")
    devin.prepare(tmp_path, str(tmp_path), None)
    class Pane:
        def capture(self):
            return ""
        def dead(self):
            return None
    pane = Pane()
    with devin.hooks_file.open("a") as f:
        for event in ({"hook_event_name": "SessionStart", "session_id": "sess-1"},
                      {"hook_event_name": "UserPromptSubmit", "prompt_id": "p1"},
                      {"hook_event_name": "PreToolUse", "tool_name": "exec",
                       "tool_input": {"command": "echo done"}}):
            f.write(json.dumps(event) + "\n")
    snap = devin.poll(pane)
    assert snap.state == "busy" and snap.turns == 0
    assert devin.session_id() == "sess-1"
    assert devin.progress() == ["tool exec echo done"]
    with devin.hooks_file.open("a") as f:
        f.write(json.dumps({"hook_event_name": "Stop",
                            "last_assistant_message": "Finished."}) + "\n")
    snap = devin.poll(pane)
    assert snap.state == "idle" and snap.turns == 1
    assert devin.final() == "Finished."
    assert devin.progress() == ["Finished."]
    devin.close()
    assert Path(str(devin.log) + ".final").read_text() == "Finished."
    assert Path(str(devin.log) + ".session").read_text() == "sess-1\n"


def test_devin_composer_reads_only_current_draft():
    pane = ("old answer\n" + "─" * 90 + "\n❯ Your task is in /tmp/task.md; read it fully\n"
            + "─" * 90 + "\nAsk Devin to build features, fix bugs, or work on your code")
    assert Composer.ready(pane)
    assert Composer.draft_visible(pane, "Your task is in")
    assert not Composer.draft_visible(pane, "old answer")


def test_resume_cwd_comes_from_devin_session_store(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    db = tmp_path / ".local/share/devin/cli/sessions.db"
    db.parent.mkdir(parents=True)
    with sqlite3.connect(db) as con:
        con.execute("CREATE TABLE sessions (id TEXT, working_directory TEXT)")
        con.execute("INSERT INTO sessions VALUES (?, ?)", ("s1", "/a/work"))
    assert Devin().session_cwd("s1") == "/a/work"


@pytest.mark.skipif(os.environ.get("SLUICE_LIVE") != "1", reason="set SLUICE_LIVE=1")
@pytest.mark.live
def test_devin_live_declared_output_and_resume():
    scratch = Path("/workspace/tmp/claude-1000/-workspace-code-lash/"
                   "8dfa931c-0520-4166-a225-16dc65dc37d8/scratchpad/native")
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="devin-live-", dir=scratch) as root:
        root = Path(root)
        home, work = root / "home", root / "work"
        home.mkdir()
        work.mkdir()
        (home / "config.json").write_text(json.dumps({
            "fn_dirs": [str(Path(__file__).parents[1])]}))
        (home / ".env").write_text("SLUICE_AGENT_GRACE_MIN=0.02\n"
                                   "SLUICE_AGENT_SETTLE_S=0.5\n")
        store = Store(home)
        store.create_project("p", "", "t", "t")
        steps = {
            "pick": {"run": "agent.devin", "outputs": {"word": "string"},
                     "in": {"cwd": {"default": str(work)},
                            "spec": {"default": "Submit the literal word blue as `word` "
                                                "with the command in your task. Then finish."}}},
            "again": {"run": "agent.run", "outputs": {"again": "string"},
                      "in": {"engine": {"default": "devin"},
                             "cwd": {"default": str(work)},
                             "session": {"source": "pick/session"},
                             "spec": {"default": "Submit the same word as `again` using the "
                                                 "command in your task. Then finish."}}},
        }
        store.patch("p", 1, [{"op": "replace", "path": "/steps", "value": steps}], "t", "t")
        runner = Runner(store)
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            runner.tick()
            state = store.read_state("p")["steps"]
            if len(state) == 2 and all(e["status"] in ("succeeded", "failed")
                                       for e in state.values()):
                break
            time.sleep(0.2)
        state = store.read_state("p")["steps"]
        for name in steps:
            run_dir = store.runs_dir("p") / state[name]["run_ids"][-1]
            assert state[name]["status"] == "succeeded", (name, state[name].get("error"),
                                                          (run_dir / "stderr.log").read_text())
            assert not (run_dir / "tmux.sock").exists()
        assert state["pick"]["outputs"]["word"] == "blue"
        assert state["again"]["outputs"]["again"] == "blue"
        assert state["again"]["outputs"]["session"] == state["pick"]["outputs"]["session"]
