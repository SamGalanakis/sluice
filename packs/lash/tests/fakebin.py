"""A fake external binary (kiln, just, python3) driven by a JSON config.

Invoked as `fakebin.py <bin-name> <argv...>`. Every call is appended to $FAKE_LOG. `kiln fork
lash <name>` and `kiln rm lash <name>` manage real git worktrees of the config's repo; other
calls answer from the first response whose `bin` matches and whose `match` is a prefix of argv,
writing its `testlogs` under `<cwd>/bazel-testlogs` first. No match: exit 0, no output.
"""

import json
import os
import subprocess
import sys
from pathlib import Path


def main() -> int:
    name, argv = sys.argv[1], sys.argv[2:]
    cfg = json.loads(Path(os.environ["FAKE_CONFIG"]).read_text())
    with open(os.environ["FAKE_LOG"], "a") as f:
        f.write(json.dumps({"bin": name, "argv": argv, "cwd": os.getcwd()}) + "\n")
    if name == "kiln" and argv[:2] == ["fork", "lash"]:
        path = Path(cfg["forks_dir"]) / argv[2]
        subprocess.run(["git", "-C", cfg["repo"], "fetch", "-q", "origin"], check=True)
        subprocess.run(["git", "-C", cfg["repo"], "worktree", "add", "-q", "--detach", str(path),
                        "origin/main"], check=True, stdout=sys.stderr)
        print(f"kiln: fork {argv[2]} ready")
        print(path)
        return 0
    if name == "kiln" and argv[:2] == ["rm", "lash"]:
        path = Path(cfg["forks_dir"]) / argv[2]
        subprocess.run(["git", "-C", cfg["repo"], "worktree", "remove", "--force", str(path)],
                       check=True)
        return 0
    for r in cfg.get("responses", []):
        if r["bin"] == name and argv[: len(r["match"])] == r["match"]:
            for rel, text in r.get("testlogs", {}).items():
                p = Path.cwd() / "bazel-testlogs" / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(text)
            sys.stdout.write(r.get("stdout", ""))
            sys.stderr.write(r.get("stderr", ""))
            return int(r.get("exit", 0))
    return 0


if __name__ == "__main__":
    sys.exit(main())
