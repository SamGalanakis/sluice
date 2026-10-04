"""Build only synthetic temp homes with the checked-out old Python schema-v6 code.

Run from the repository: uv run python crates/sluice/tests/fixtures/build_python_home.py.
The fixture uses @SOURCE@/@STAGING@ placeholders, expanded only in scratch tests.
"""
from __future__ import annotations

import json
import os
import shutil
import sqlite3
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "legacy-python-fixture"))


def write(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")
    path.chmod(0o600)


def build(output: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="sluice-import-fixture-") as tmp:
        source = Path(tmp) / "source"
        os.environ["SLUICE_HOME"] = str(source)
        from sluice import db

        conn = db.connect(source)
        assert conn.execute("PRAGMA user_version").fetchone()[0] == 6
        at = "2026-10-03T20:00:00Z"
        default = lambda value: {"default": value}
        external = lambda **extra: {"run": "core.external", "outputs": {"value": "string"}, **extra}
        steps = {
            "done": external(outputs={"value": "string", "ok": "boolean"},
                             **{"in": {"brief": {"file": "@SOURCE@/projects/fixture/brief.md"}}}),
            "failed": external(paused="owner review"),
            "stale": external(),
            "skip": external(when="enabled"),
            "wait": external(after=["done"]),
            "work": {"run": "fixture.worker", "tags": ["unit:agent"],
                     "in": {"cwd": default("/tmp"), "spec": default("continue")}},
            "scatter": {"run": "fixture.worker", "scatter": "spec", "tags": ["unit:batch"],
                        "in": {"cwd": default("/tmp"), "spec": default(["first", "second"])}},
            "old-a": external(when="done/ok"),
            "old-b": external(after=["old-a"]),
            "tagged": external(tags=["unit:separate"],
                               **{"in": {"from_other": {"source": "old-b/value"}}}),
        }
        plan = {"inputs": {"enabled": "boolean"}, "steps": steps,
                "outputs": {"value": {"source": "tagged/value"}}}
        state = {"inputs": {"enabled": False}, "steps": {
            "done": {"status": "succeeded", "manual": True, "outputs": {"value": "kept", "ok": True}, "inputs_hash": "old-python-hash"},
            "failed": {"status": "failed", "error": "old failure", "outputs": {"value": "visible"}, "manual": True},
            "stale": {"status": "stale", "outputs": {"value": "stale visible"}},
            "skip": {"status": "skipped", "skipped": "enabled is false"},
            "wait": {"status": "pending"},
            "work": {"status": "running", "run_ids": ["scalar-run"], "outputs": {"final": "partial", "session": "fake-session"}},
            "scatter": {"status": "running", "run_ids": ["scatter-good", "scatter-busy"], "done": 1, "total": 2},
            "old-a": {"status": "succeeded", "outputs": {"value": "old group"}},
            "old-b": {"status": "pending"},
            "tagged": {"status": "pending"},
        }}
        with db.write(source) as con:
            for name, paused, archived in [("fixture", 0, 0), ("archived", 1, 1)]:
                con.execute("INSERT INTO projects(name,description,paused,archived,resources,icon_text,created) VALUES (?,?,?,?,?,?,?)",
                            (name, f"Synthetic {name}", paused, archived,
                             json.dumps({"slots": {"capacity": 2}}), "X", at))
                con.execute("INSERT INTO plans VALUES (?,1,?)", (name, json.dumps(plan if name == "fixture" else {"steps": {}})))
                con.execute("INSERT INTO states VALUES (?,?)", (name, json.dumps(state if name == "fixture" else {"inputs": {}, "steps": {}})))
            con.execute("INSERT INTO submissions VALUES ('fixture','scalar-run','work',?,?)", (json.dumps({"final": "submitted progress", "session": "fake-session"}), at))
            for n, title, input_name in [(1, "Need owner decision", None), (2, "Unsupported input", "removed-input")]:
                con.execute("INSERT INTO inbox(project,n,title,body,ui,input,sender,run,status,created) VALUES ('fixture',?,?,?,'text',?,'step:work','scalar-run','open',?)",
                            (n, title, "Choose the direction", input_name, at))
            con.execute("INSERT INTO calls(call,project,fn,status,inputs,created) VALUES ('queued','fixture','fixture.worker','pending','{}',?)", (at,))
            con.execute("INSERT INTO records(project,at,kind,data) VALUES ('fixture',?,'test.history','{}')", (at,))
            con.execute("INSERT INTO leases(project,resource,amount,step,run,created) VALUES ('fixture','slots',1,'work','scalar-run',?)", (at,))
        native = {"engine": "codex", "session": "fake-session", "cwd": "/tmp", "head_before": "abcdef",
                  "pid": 999999, "tmux": "never-import", "control": "never-import"}
        for run in ["scalar-run", "scatter-busy"]:
            write(source / "projects/fixture/runs" / run / "native.json", native)
            write(source / "projects/fixture/runs" / run / "input.json",
                  {"cwd": "/tmp", "spec": "continue" if run == "scalar-run" else "second"})
        write(source / "projects/fixture/runs/scatter-good/exit.json", {"code": 0})
        write(source / "projects/fixture/runs/scatter-good/output.json", {"final": "good item"})
        write(source / "codex-native-sessions/fake-session.json",
              {"home": "@SOURCE@/codex-native-homes/generation", "cwd": "/tmp"})
        rollout = source / "codex-native-homes/generation/sessions/2026/10/03/rollout-fixture-fake-session.jsonl"
        write(rollout, {"type": "session_meta", "payload": {"id": "fake-session", "cwd": "/tmp"}})
        rollout.write_text(json.dumps({"type": "session_meta", "payload": {"id": "fake-session", "cwd": "/tmp"}}) + "\n")
        engine_state = sqlite3.connect(source / "codex-native-homes/generation/state_5.sqlite")
        engine_state.execute("CREATE TABLE threads(id TEXT PRIMARY KEY, cwd TEXT, rollout_path TEXT)")
        engine_state.execute("INSERT INTO threads VALUES ('fake-session','/tmp',?)", ("@SOURCE@/codex-native-homes/generation/sessions/2026/10/03/rollout-fixture-fake-session.jsonl",))
        engine_state.commit()
        engine_state.close()
        (source / "projects/fixture/brief.md").write_text("A brief whose bytes never enter the hash.\n")
        staging = Path(tmp) / "staging"
        write(staging / "projects/fixture/fns/fixture.worker/fn.json", {
            "name": "fixture.worker", "inputs": {"cwd": "string", "spec": "string"},
            "outputs": {"final": "string", "session": "string?"}})
        (staging / "projects/fixture/fns/fixture.worker/main.py").write_text("raise RuntimeError('The importer must never run this')\n")
        write(staging / "fns/fixture.global/fn.json", {
            "name": "fixture.global", "inputs": {"value": "string"},
            "outputs": {"value": "string"}})
        (staging / "fns/fixture.global/main.py").write_text("raise RuntimeError('The importer must never run this')\n")
        write(staging / "projects/archived/fns/fixture.worker/fn.json", {
            "name": "fixture.worker", "inputs": {"value": "boolean"},
            "outputs": {"value": "boolean"}})
        (staging / "projects/archived/fns/fixture.worker/main.py").write_text("raise RuntimeError('The importer must never run this')\n")
        for scope, helper, value in [
            (staging, "_globallib", "global helper"),
            (staging / "projects/fixture", "_fixturelib", "project helper"),
            (staging / "projects/archived", "_archivedlib", "archived project helper"),
        ]:
            directory = scope / "fns" / helper
            directory.mkdir()
            (directory / "__init__.py").write_text(f"VALUE = '{value}'\n")
        write(staging / "projects/fixture/recipes/sample.json", {"name":"sample","steps": {"work": {"run": "fixture.worker"}}})
        write(staging / "config.json", {"http": {"port": 7420}, "fn_dirs": []})
        (staging / ".env").write_text("FIXTURE_SECRET=synthetic-only\n")
        (staging / ".env").chmod(0o600)
        write(source / "config.json", {"http": {"port": 7420}})
        (source / ".env").write_text("FIXTURE_SECRET=synthetic-only\n")
        conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        conn.close()
        readme = (output / "README.md").read_text() if (output / "README.md").exists() else None
        if output.exists():
            shutil.rmtree(output)
        shutil.copytree(Path(tmp), output)
        if readme is not None:
            (output / "README.md").write_text(readme)


if __name__ == "__main__":
    build(Path(__file__).parent / "python_home")
