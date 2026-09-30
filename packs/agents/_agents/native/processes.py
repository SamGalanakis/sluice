"""Record native session processes for cleanup when the function cannot run its finally block,
and find the background work an agent left running."""

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
# Nobody can answer a git prompt in a supervised session: fail instead of waiting on one.
GIT_ENV = {"GIT_TERMINAL_PROMPT": "0", "GIT_EDITOR": "true", "GIT_MERGE_AUTOEDIT": "no"}


def engine_env():
    """The environment an engine runs in: the fn's tools' own (child_env) without a parent
    Claude Code's session markers, and with git never prompting."""
    return {**{k: v for k, v in child_env().items() if k not in SCRUB}, **GIT_ENV}


def scope_command(argv):
    """argv in a transient systemd scope of its own when the user manager can make one, in the
    slice SLUICE_AGENT_SLICE names (e.g. one with a memory limit) when it is set."""
    global _scope_ok
    if _scope_ok is None:
        try:
            _scope_ok = subprocess.run([*SCOPE, "true"], stdin=subprocess.DEVNULL,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                       timeout=3, check=False).returncode == 0
        except (OSError, subprocess.TimeoutExpired):
            _scope_ok = False
    if not _scope_ok:
        return argv
    if slice_ := os.environ.get("SLUICE_AGENT_SLICE", "").strip():
        return [*SCOPE[:-1], f"--slice={slice_}", "--", *argv]
    return [*SCOPE, *argv]


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


def _stat(pid):
    """(parent pid, state) of a live process, or None."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return None
    rest = stat[stat.rindex(")") + 2:].split()
    return int(rest[1]), rest[0]


def cgroup(pid):
    """The cgroup (v2) a process is in, or None."""
    try:
        text = Path(f"/proc/{pid}/cgroup").read_text()
    except OSError:
        return None
    return next((ln[3:] for ln in text.splitlines() if ln.startswith("0::")), None)


def detached(roots):
    """The background work an engine's session started and let go: processes in the private
    cgroup of a root (tmux puts each pane in its own scope; scope_command does the same for an
    app-server) whose parent is outside it. A tool's shell that backgrounds a job (`cmd &`,
    `nohup`, `setsid`) exits and the job is re-parented out of the scope, but stays in it. The
    engine's own children (its helpers, MCP servers, the shells it tracks itself) are not
    counted, nor the roots and their ancestors. A cgroup this process shares is not private:
    then nothing is found. Returns {pid: name}."""
    roots = [r for r in roots if r]
    mine = cgroup(os.getpid())
    out = {}
    for group in {cgroup(r) for r in roots} - {None, "/", mine}:
        try:
            procs = {int(p) for p in Path(f"/sys/fs/cgroup{group}/cgroup.procs")
                     .read_text().split()}
        except (OSError, ValueError):
            continue
        skip = set()
        for pid in roots:
            while pid in procs and pid not in skip:
                skip.add(pid)
                pid = (_stat(pid) or (0, ""))[0]
        for pid in procs - skip:
            st = _stat(pid)
            if st and st[1] != "Z" and st[0] not in procs:
                try:
                    out[pid] = Path(f"/proc/{pid}/comm").read_text().strip()
                except OSError:
                    continue
    return out
