#!/usr/bin/python3
"""Private supervisor-journal fixture, independent of production CLI dispatch."""
import json
import os
from pathlib import Path
import sys
import time
import uuid

assert sys.argv[1] == "agent-hook"
engine, event = sys.argv[2:4]
run = Path(os.environ["SLUICE_RUN_DIR"])
assert "sluice-test-" in str(run.resolve())
journal = run / "engine-hooks"
journal.mkdir(mode=0o700, exist_ok=True)
key = uuid.uuid4().hex
body = {"engine": engine, "run": os.environ["SLUICE_RUN_ID"], "event": event,
        "payload": json.load(sys.stdin)}
tmp = journal / f"{key}.tmp"
with tmp.open("x") as f:
    os.chmod(tmp, 0o600)
    json.dump(body, f)
    f.flush()
    os.fsync(f.fileno())
tmp.rename(journal / f"{key}.request.json")
reply = journal / f"{key}.reply.json"
deadline = time.monotonic() + 10
while not reply.exists():
    if time.monotonic() >= deadline:
        raise TimeoutError(event)
    time.sleep(0.005)
result = json.loads(reply.read_text())
if "Err" in result:
    raise RuntimeError(result["Err"])
result = result["Ok"]
if result["stdout"] is not None:
    print(json.dumps(result["stdout"]))
sys.exit(result["exit_code"])
