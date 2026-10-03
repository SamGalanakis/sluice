"""Read-only live online backup plus selected files, all writes in scratch.

Never import sluice Python code, execute a fn, use tmux, or resume an engine.
Converted scripts come from p4-06's immutable Git commit, never live fns.
"""
from __future__ import annotations

import json
import os
import shutil
import sqlite3
import subprocess
import sys
from pathlib import Path


def copy_file(source: Path, target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    shutil.copy2(source, target, follow_symlinks=False)


def copy_tree(source: Path, target: Path) -> None:
    if source.exists():
        shutil.copytree(source, target, symlinks=True,
                        ignore=shutil.ignore_patterns(".git", "__pycache__", "*.pyc"))


def main(source: Path, staging: Path) -> None:
    live = Path("/home/sam/.sluice")
    assert not source.exists() and not staging.exists()
    assert source.resolve().is_relative_to(Path("/tmp"))
    source.mkdir(mode=0o700)
    staging.mkdir(mode=0o700)
    (source / ".snapshot-origin.json").write_text(json.dumps({"home": str(live)}))
    con = sqlite3.connect(f"file:{live / 'sluice.db'}?mode=ro", uri=True)
    backup = sqlite3.connect(source / "sluice.db")
    try:
        con.backup(backup)
        plans_before = backup.execute("SELECT project,rev,doc FROM plans ORDER BY project").fetchall()
        for _, _, raw in plans_before:
            for step in json.loads(raw).get("steps", {}).values():
                for binding in step.get("in", {}).values():
                    if isinstance(binding, dict) and isinstance(binding.get("file"), str):
                        path = Path(binding["file"])
                        if path.is_relative_to(live) and path.is_file():
                            copy_file(path, source / path.relative_to(live))
        projects = backup.execute("SELECT name FROM projects ORDER BY name").fetchall()
        for name, in projects:
            for part in ["fns", "recipes"]:
                copy_tree(live / "projects" / name / part, source / "projects" / name / part)
            for part in [".env", "config.json"]:
                path = live / "projects" / name / part
                if path.is_file():
                    copy_file(path, source / "projects" / name / part)
            state_row = backup.execute("SELECT doc FROM states WHERE project=?", (name,)).fetchone()
            state = json.loads(state_row[0]) if state_row else {}
            for step in state.get("steps", {}).values():
                if step.get("status") != "running":
                    continue
                for run in step.get("run_ids", []):
                    assert Path(run).name == run
                    run_source = live / "projects" / name / "runs" / run
                    run_target = source / "projects" / name / "runs" / run
                    for filename in ["native.json", "input.json", "output.json", "exit.json"]:
                        if (run_source / filename).is_file():
                            copy_file(run_source / filename, run_target / filename)
                    native = run_target / "native.json"
                    if not native.exists():
                        continue
                    checkpoint = json.loads(native.read_text())
                    session = checkpoint.get("session")
                    if checkpoint.get("engine") != "codex" or not session:
                        continue
                    assert Path(session).name == session
                    mapping_source = live / "codex-native-sessions" / f"{session}.json"
                    mapping_target = source / "codex-native-sessions" / f"{session}.json"
                    copy_file(mapping_source, mapping_target)
                    mapping = json.loads(mapping_target.read_text())
                    private = Path(mapping["home"]).resolve()
                    assert private.is_relative_to(live / "codex-native-homes")
                    private_target = source / "codex-native-homes" / private.name
                    if not private_target.exists():
                        private_target.mkdir(parents=True, mode=0o700)
                        for item in private.iterdir():
                            required = item.name in {"sessions", "archived_sessions", "session_index.jsonl", "history.jsonl"} or item.name.startswith("state_") and ".sqlite" in item.name
                            if required:
                                if item.is_dir():
                                    copy_tree(item, private_target / item.name)
                                else:
                                    copy_file(item, private_target / item.name)
                            elif item.name == "auth.json" and item.is_symlink():
                                # Keep external credential ownership. Never read auth bytes.
                                (private_target / item.name).symlink_to(item.resolve())
                    mapping["home"] = str(private_target)
                    mapping_target.write_text(json.dumps(mapping))
        copy_tree(live / "fns", source / "fns")
        for name in ["config.json", ".env"]:
            if (live / name).is_file():
                copy_file(live / name, source / name)
        assert plans_before == con.execute("SELECT project,rev,doc FROM plans ORDER BY project").fetchall(), "live plans changed during preliminary snapshot; redo backup"
    finally:
        backup.close()
        con.close()

    root = Path(__file__).resolve().parents[4]
    ref = os.environ.get("SLUICE_IMPORT_STAGING_REF", "a882b7e")
    names = subprocess.run(["git", "ls-tree", "-r", "--name-only", ref, "cutover-staging"], cwd=root, check=True, capture_output=True, text=True).stdout.splitlines()
    assert names, "p4-06 converted staging commit is unavailable"
    for name in names:
        relative = Path(name).relative_to("cutover-staging")
        if "projects" not in relative.parts and relative.name != "plan-conversion.md":
            continue
        data = subprocess.run(["git", "show", f"{ref}:{name}"], cwd=root, check=True, capture_output=True).stdout
        target = staging / relative
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        target.write_bytes(data)
    # Config/secrets are copied from the scratch snapshot into review staging.
    for scope in [Path(), *(Path("projects") / p.name for p in (source / "projects").iterdir())]:
        for filename in ["config.json", ".env"]:
            path = source / scope / filename
            if path.exists():
                copy_file(path, staging / scope / filename)
                (staging / scope / filename).chmod(0o600)
    # Importer tests receive a stopped read-only source. The importer itself
    # opens only its private database copy for recovery.
    for directory, _, filenames in os.walk(source, followlinks=False):
        for filename in filenames:
            path = Path(directory) / filename
            if not path.is_symlink():
                path.chmod(path.stat().st_mode & 0o555)
        Path(directory).chmod(0o500)


if __name__ == "__main__":
    main(Path(sys.argv[1]), Path(sys.argv[2]))
