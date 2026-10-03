"""Labelled scratch homes built with the pinned old Python source, never the live home."""
import json
import os
from pathlib import Path
import sqlite3
import sys

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src"))
from sluice import db


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.write_text(json.dumps(value) + "\n")
    path.chmod(0o600)


def build(root):
    assert "sluice-test-" in str(root.resolve())
    source = root / "python-home"
    source.mkdir(mode=0o700)
    owner = root / "owner"
    cwd = root / "work"
    cwd.mkdir(mode=0o700, exist_ok=True)
    os.environ["SLUICE_HOME"] = str(source)
    os.environ["HOME"] = str(owner)
    staging = root / "staging"
    staging.mkdir(mode=0o700)
    write(staging / "config.json", {"fn_dirs": []})
    write(source / "config.json", {"fn_dirs": [str(REPO / "packs/agents")]})
    default = lambda x: {"default": x}
    engines = ("codex", "claude", "devin")
    steps = {e: {"run": "agent.run", "in": {"engine": default(e), "cwd": default(str(cwd)),
            "spec": default("Labelled imported-session acceptance fixture")}} for e in engines}
    at = "2026-10-04T00:00:00Z"
    with db.write(source) as con:
        con.execute("INSERT INTO projects(name,description,created) VALUES ('import-resume','Labelled scratch import-resume fixture',?)", (at,))
        con.execute("INSERT INTO plans VALUES ('import-resume',1,?)", (json.dumps({"inputs": {}, "outputs": {}, "steps": steps}),))
        state = {"inputs": {}, "steps": {e: {"status": "running", "run_ids": [f"old-{e}"]} for e in engines}}
        con.execute("INSERT INTO states VALUES ('import-resume',?)", (json.dumps(state),))
    for e in engines:
        write(source / f"projects/import-resume/runs/old-{e}/native.json",
              {"engine": e, "session": f"fixture-{e}", "cwd": str(cwd), "head_before": "fixture-baseline"})
        write(source / f"projects/import-resume/runs/old-{e}/input.json",
              {"engine": e, "cwd": str(cwd), "spec": "Labelled imported-session acceptance fixture"})
    private = source / "codex-native-homes/generation"
    rollout = private / "sessions/2026/10/04/rollout-fixture-codex.jsonl"
    write(rollout, {"type": "session_meta", "payload": {"id": "fixture-codex", "cwd": str(cwd)}})
    write(source / "codex-native-sessions/fixture-codex.json", {"cwd": str(cwd), "home": str(private)})
    con = sqlite3.connect(private / "state_5.sqlite")
    con.execute("CREATE TABLE threads(id TEXT PRIMARY KEY, cwd TEXT, rollout_path TEXT)")
    con.execute("INSERT INTO threads VALUES (?,?,?)", ("fixture-codex", str(cwd), str(rollout)))
    con.commit()
    con.close()
    write(owner / ".claude/projects/fixture/fixture-claude.jsonl", {"sessionId": "fixture-claude", "cwd": str(cwd), "type": "user"})
    data = owner / ".local/share/devin/cli"
    data.mkdir(parents=True, mode=0o700)
    con = sqlite3.connect(data / "sessions.db")
    con.execute("CREATE TABLE sessions(id TEXT PRIMARY KEY, working_directory TEXT)")
    con.execute("INSERT INTO sessions VALUES (?,?)", ("fixture-devin", str(cwd)))
    con.commit()
    con.close()
    db.connect(source).execute("PRAGMA wal_checkpoint(TRUNCATE)")


def inspect(destination):
    con = sqlite3.connect(f"file:{destination / 'sluice.db'}?mode=ro", uri=True)
    rows = con.execute("SELECT run_id,engine,cwd,session_id,metadata FROM sessions ORDER BY engine").fetchall()
    result = [{"run": r[0], "engine": r[1], "cwd": r[2], "session": r[3], "metadata": json.loads(r[4])} for r in rows]
    assert con.execute("SELECT count(*) FROM projects WHERE paused=1").fetchone()[0] == 1
    assert con.execute("SELECT count(*) FROM steps WHERE status='failed'").fetchone()[0] == 3
    con.close()
    print(json.dumps(result))


if __name__ == "__main__":
    if sys.argv[1] == "build":
        build(Path(sys.argv[2]))
    elif sys.argv[1] == "inspect":
        inspect(Path(sys.argv[2]))
    else:
        raise ValueError(sys.argv[1])
