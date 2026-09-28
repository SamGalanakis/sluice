"""Record native session processes for cleanup when the function cannot run its finally block."""

import json
import os
import subprocess
from pathlib import Path

from sluice.fn import child_env

FILE = "native-processes.json"
SCOPE = ["systemd-run", "--user", "--scope", "--collect", "-q",
         "-p", "CPUWeight=100", "-p", "IOWeight=100", "--"]
_scope_ok = None
SCRUB = ("CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_ENTRYPOINT",
         "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN",
         "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_SSE_PORT",
         "CLAUDE_PID", "CLAUDE_EFFORT", "AI_AGENT")


def engine_env():
    return {k: v for k, v in child_env().items() if k not in SCRUB}


def scope_command(argv):
    global _scope_ok
    if _scope_ok is None:
        try:
            _scope_ok = subprocess.run([*SCOPE, "true"], stdin=subprocess.DEVNULL,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                       timeout=3, check=False).returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            _scope_ok = False
    return [*SCOPE, *argv] if _scope_ok else argv


def start_time(pid):
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        return int(stat[stat.rindex(")") + 2:].split()[19])
    except (OSError, IndexError, ValueError):
        return None


def record(run_dir, name, pid):
    if not pid or (started := start_time(pid)) is None:
        return
    path = Path(run_dir) / FILE
    try:
        data = json.loads(path.read_text())
    except (OSError, ValueError):
        data = {}
    data[name] = {"pid": pid, "start_time": started}
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(data))
    os.replace(tmp, path)


def find_argv(token):
    """Find the app-server process by its unique per-run socket argument."""
    for d in Path("/proc").iterdir():
        if not d.name.isdigit():
            continue
        try:
            args = (d / "cmdline").read_bytes().split(b"\0")
        except OSError:
            continue
        if (args and Path(os.fsdecode(args[0])).name != "systemd-run"
                and token.encode() in args and b"app-server" in args):
            return int(d.name)
    return None
