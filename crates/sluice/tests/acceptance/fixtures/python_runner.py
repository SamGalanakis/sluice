"""One real Codex session under the old runner, with private paths and explicit cleanup."""
import json
import os
from pathlib import Path
import signal
import sys
import time

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / "src"))
from sluice import db
from sluice.runner import Runner, kill, _native_roots
from sluice.store import Store

root = Path(sys.argv[1]).resolve()
assert "sluice-test-" in str(root)
source = root / "python-home"
assert Path(os.environ["SLUICE_HOME"]).resolve() == source
assert Path(os.environ["HOME"]).resolve() == root / "owner"
assert not source.exists()
source.mkdir(mode=0o700)
(source / "config.json").write_text(json.dumps({"fn_dirs": [str(REPO / "packs/agents")]}))
store = Store(source)
store.create_project("import-resume", "Labelled G7 scratch Python Codex session", "fixture", "fixture")
cwd = root / "work"
spec = (
    "This is the labelled g7_codex_import_resume scratch acceptance gate. "
    "Create first.txt containing original and commit it with message 'Record the Python fixture turn.' "
    "Make exactly one commit. Do no other work and do not submit word yet. "
    "End this turn with the exact final text G7_WAITING. You will receive upgrade feedback later. "
    "The harness will interrupt the runner while you are waiting for that feedback."
)
plan = {"inputs": {}, "outputs": {}, "steps": {"work": {"run": "agent.run", "outputs": {"word": "string"},
        "in": {k: {"default": v} for k, v in {"engine": "codex", "cwd": str(cwd), "spec": spec,
                                              "model": "sol", "effort": "low", "listen": False}.items()}}}}
store.patch("import-resume", 1, [{"op": "replace", "path": f"/{k}", "value": v} for k, v in plan.items()], "fixture", "fixture")
runner = Runner(store)


def stop(*_):
    # Only runs admitted by this fixture runner. Preserve their running source state.
    kill(*(run for active in runner.active.values() for run in active.runs))
    raise SystemExit(0)


signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
deadline = time.monotonic() + 240
try:
    while time.monotonic() < deadline:
        runner.tick()
        entry = store.read_state("import-resume")["steps"].get("work", {})
        for rid in entry.get("run_ids", []):
            run = store.runs_dir("import-resume") / rid
            native = run / "native.json"
            progress = run / "codex.log"
            if native.exists() and progress.exists() and "G7_WAITING" in progress.read_text():
                checkpoint = json.loads(native.read_text())
                if checkpoint.get("session"):
                    assert entry["status"] == "running"
                    (root / "python-ready.json").write_text(json.dumps({"old_run": rid, "run_dir": str(run), "checkpoint": checkpoint}))
                    while True:
                        time.sleep(0.1)
        if entry.get("status") == "failed":
            raise RuntimeError(entry.get("error", "Python agent failed"))
        time.sleep(0.05)
    raise TimeoutError("Python Codex did not commit and reach its waiting turn")
finally:
    owned = [run for active in runner.active.values() for run in active.runs]
    kill(*owned)
    checks = [{"run_dir": str(run.run_dir), "native_empty": not _native_roots(run.run_dir),
               "wrapper_reaped": run.proc is None or run.proc.poll() is not None} for run in owned]
    assert checks and all(c["native_empty"] and c["wrapper_reaped"] for c in checks)
    db.connect(source).execute("PRAGMA wal_checkpoint(TRUNCATE)")
    (root / "python-cleanup.json").write_text(json.dumps(checks))
