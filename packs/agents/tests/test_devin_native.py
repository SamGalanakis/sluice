"""Devin's per-run hook stream and native session contract."""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest

from sluice.runner import Runner
from sluice.store import Store

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from _agents.native.devin import FUSION, GUARDRAIL, Composer, Devin


def test_devin_model_allowlist_maps_to_cli_ids():
    assert Devin().model == "swe-2-high"
    for name in ("swe-2-high", "high"):
        assert Devin(name).model == "swe-2-high"
    for name in ("fusion", FUSION):
        assert Devin(name).model == FUSION
    with pytest.raises(ValueError) as err:
        Devin("swe-2-medium")
    message = str(err.value)
    assert "swe-2-medium" in message and "swe-2-high" in message and FUSION in message


def test_devin_config_pins_the_chosen_model(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "no-config"))
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    devin = Devin("fusion")
    devin.prepare(run_dir, str(tmp_path), None)
    cfg = json.loads(devin.config_file.read_text())
    assert cfg["agent"]["model"] == FUSION
    assert devin.argv()[devin.argv().index("--model") + 1] == FUSION


def test_devin_config_keeps_user_settings_and_registers_hooks(tmp_path, monkeypatch):
    home = tmp_path / "config"
    home.joinpath("devin").mkdir(parents=True)
    home.joinpath("devin/config.json").write_text(
        '// comment\n{"theme_mode": "dark", "agent": {"model": "swe-1-6-fast"}, '
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
    assert cfg["agent"]["model"] == "swe-2-high"  # the launch's model, not the user's
    assert len(cfg["hooks"]["SessionStart"]) == 2
    assert devin.argv()[-4:] == ["--permission-mode", "dangerous",
                                 "--respect-workspace-trust", "false"]
    assert devin.config_file.stat().st_mode & 0o777 == 0o600
    assert "/bin/sh -c 'cat >>" in cfg["hooks"]["Stop"][-1]["hooks"][0]["command"]
    assert "push only when the task says so" in GUARDRAIL and "push to main" not in GUARDRAIL
    command = cfg["hooks"]["Stop"][-1]["hooks"][0]["command"]
    for _ in range(2):
        subprocess.run(["sh", "-c", command], input='{"hook_event_name":"Stop"}',
                       text=True, check=True)
    assert len(devin.hooks.read()) == 2


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


def test_transient_comes_from_error_channel_and_clears_each_turn(tmp_path):
    devin = Devin()
    devin.prepare(tmp_path, str(tmp_path), None)
    class Pane:
        def dead(self):
            return None
    with devin.hooks_file.open("a") as f:
        f.write(json.dumps({"hook_event_name": "Stop",
                            "last_assistant_message": "Landed a529f1c; 1529 tests pass"}) + "\n")
    assert devin.poll(Pane()).error == ""
    with devin.hooks_file.open("a") as f:
        f.write(json.dumps({"hook_event_name": "UserPromptSubmit"}) + "\n")
        f.write(json.dumps({"hook_event_name": "Stop", "error": "HTTP status 529"}) + "\n")
    assert devin.poll(Pane()).error == "HTTP status 529"
    with devin.hooks_file.open("a") as f:
        f.write(json.dumps({"hook_event_name": "UserPromptSubmit"}) + "\n")
    assert devin.poll(Pane()).error == ""


def test_devin_close_before_prepare_is_safe():
    Devin().close()


def test_devin_uses_host_environment(monkeypatch):
    monkeypatch.setenv("SLUICE_HOST_PATH", "/usr/bin")
    monkeypatch.setenv("SLUICE_HOST_PYTHONPATH", "")
    monkeypatch.setenv("SLUICE_HOST_VIRTUAL_ENV", "")
    monkeypatch.setenv("PATH", "/fn/bin:/usr/bin")
    monkeypatch.setenv("PYTHONPATH", "/fn/src")
    monkeypatch.setenv("VIRTUAL_ENV", "/fn")
    monkeypatch.setenv("CLAUDECODE", "1")
    env = Devin().env()
    assert env["PATH"] == "/usr/bin"
    assert "PYTHONPATH" not in env and "VIRTUAL_ENV" not in env
    assert "CLAUDECODE" not in env


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
    scratch = Path(os.environ.get("SLUICE_LIVE_DIR", tempfile.gettempdir())) / "sluice-live"
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


@pytest.mark.skipif(os.environ.get("SLUICE_LIVE") != "1", reason="set SLUICE_LIVE=1")
@pytest.mark.live
def test_devin_live_fusion_model():
    """model "fusion" launches the Fusion pairing: the session Devin records for the step
    carries the fusion model id."""
    scratch = Path(os.environ.get("SLUICE_LIVE_DIR", tempfile.gettempdir())) / "sluice-live"
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
                            "model": {"default": "fusion"},
                            "spec": {"default": "Submit the literal word blue as `word` "
                                                "with the command in your task. Then finish."}}},
        }
        store.patch("p", 1, [{"op": "replace", "path": "/steps", "value": steps}], "t", "t")
        runner = Runner(store)
        deadline = time.monotonic() + 240
        while time.monotonic() < deadline:
            runner.tick()
            state = store.read_state("p")["steps"]
            if state.get("pick", {}).get("status") in ("succeeded", "failed"):
                break
            time.sleep(0.2)
        state = store.read_state("p")["steps"]
        run_dir = store.runs_dir("p") / state["pick"]["run_ids"][-1]
        assert state["pick"]["status"] == "succeeded", \
            (state["pick"].get("error"), (run_dir / "stderr.log").read_text())
        assert not (run_dir / "tmux.sock").exists()
        assert state["pick"]["outputs"]["word"] == "blue"
        session = state["pick"]["outputs"]["session"]
        db = Path.home() / ".local/share/devin/cli/sessions.db"
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as con:
            row = con.execute("SELECT model FROM sessions WHERE id = ?",
                              (session,)).fetchone()
        assert row and row[0] == FUSION
        exported = json.loads((run_dir / "devin.log.json").read_text())
        assert "fusion" in exported["agent"]["model_name"].lower()
