"""A private tmux server for one run: its socket is `tmux.sock` in the run dir, so runs never
share a server and cleanup is one `kill-server`.

Every tmux call runs with the run dir as its working directory and names the socket relatively:
a Unix socket path is capped at 108 bytes, and run dirs are often longer than that."""

import contextlib
import os
import shlex
import signal
import subprocess
import time
from pathlib import Path

SOCKET = "tmux.sock"
SESSION = "main"
TIMEOUT = 10.0  # seconds for one tmux command
CONF = "set -g remain-on-exit on\nset -g history-limit 50000\n"


def descendants(pid):
    """Every live process below `pid` (from /proc's parent links)."""
    children = {}
    for d in Path("/proc").iterdir():
        if not d.name.isdigit():
            continue
        try:
            stat = (d / "stat").read_text()
        except OSError:
            continue
        ppid = int(stat[stat.rindex(")") + 2:].split()[1])
        children.setdefault(ppid, []).append(int(d.name))
    out, todo = [], list(children.get(pid, []))
    while todo:
        p = todo.pop()
        out.append(p)
        todo.extend(children.get(p, []))
    return out


def _alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    try:  # a zombie is dead for our purposes
        return Path(f"/proc/{pid}/stat").read_text().split(") ")[1][0] != "Z"
    except (OSError, IndexError):
        return False


class Tmux:
    def __init__(self, run_dir):
        self.dir = Path(run_dir)

    @property
    def attach(self):
        """The command a person runs to watch or steer the live session."""
        return f"cd {shlex.quote(str(self.dir.resolve()))} && tmux -S {SOCKET} attach"

    def run(self, *args, check=True, env=None):
        env = {k: v for k, v in (env or os.environ).items() if k not in ("TMUX", "TMUX_PANE")}
        p = subprocess.run(["tmux", "-S", SOCKET, *args], cwd=self.dir, env=env, text=True,
                           capture_output=True, timeout=TIMEOUT, stdin=subprocess.DEVNULL,
                           check=False)
        if check and p.returncode != 0:
            raise RuntimeError(f"tmux {args[0]} failed: {p.stderr.strip()}")
        return p

    def start(self, argv, cwd, env, width=220, height=50):
        """Start the server with one session running `argv` in `cwd` (exec'd directly, so the
        pane's pid is the engine's). The server takes `env` as its environment."""
        (self.dir / "tmux.conf").write_text(CONF)
        p = subprocess.run(
            ["tmux", "-f", "tmux.conf", "-S", SOCKET, "new-session", "-d", "-s", SESSION,
             "-x", str(width), "-y", str(height), "-c", str(cwd), "--", *argv],
            cwd=self.dir, env={k: v for k, v in env.items() if k not in ("TMUX", "TMUX_PANE")},
            text=True, capture_output=True, timeout=TIMEOUT, stdin=subprocess.DEVNULL,
            check=False)
        if p.returncode != 0:
            raise RuntimeError(f"tmux could not start the session: {p.stderr.strip()}")

    def info(self, fmt):
        p = self.run("display-message", "-p", "-t", SESSION, fmt, check=False)
        return p.stdout.strip() if p.returncode == 0 else None

    def pane_pid(self):
        v = self.info("#{pane_pid}")
        return int(v) if v and v.isdigit() else None

    def dead(self):
        """None while the pane's process runs, else its exit status ("" when unknown). A
        server that is gone counts as dead."""
        v = self.info("#{pane_dead} #{pane_dead_status}")
        if v is None:
            return ""
        flag, _, status = v.partition(" ")
        return status if flag == "1" else None

    def capture(self, join=False, history=0):
        """The pane's visible text, with `history` lines of scrollback above it ("" when the
        capture fails: callers read that as not ready yet). A dead pane's last output has
        scrolled into its history."""
        args = ["capture-pane", "-p", "-t", SESSION] + (["-J"] if join else []) \
            + (["-S", str(-history)] if history else [])
        try:
            p = self.run(*args, check=False)
        except (subprocess.SubprocessError, OSError):
            return ""
        return p.stdout if p.returncode == 0 else ""

    def keys(self, *keys):
        self.run("send-keys", "-t", SESSION, *keys)

    def paste(self, data):
        """Paste bytes as one bracketed paste, through a buffer loaded from a file: tmux caps
        one command at about 16 KB, and `-p` keeps interior newlines as data."""
        f = self.dir / f".paste-{os.getpid()}.bin"
        f.write_bytes(data)
        try:
            self.run("load-buffer", "-b", "sluice-paste", f.name)
            self.run("paste-buffer", "-p", "-d", "-b", "sluice-paste", "-t", SESSION)
        finally:
            f.unlink(missing_ok=True)

    def kill(self, grace=2.0):
        """End the server and every process under it: the engine and whatever it started
        (background shells included). SIGTERM first, SIGKILL after `grace` seconds."""
        v = self.info("#{pid}")
        server = int(v) if v and v.isdigit() else None
        pids = descendants(server) if server else []
        for pid in pids:
            with contextlib.suppress(OSError):
                os.kill(pid, signal.SIGTERM)
        with contextlib.suppress(subprocess.SubprocessError, OSError):
            self.run("kill-server", check=False)
        everyone = pids + ([server] if server else [])
        deadline = time.monotonic() + grace
        while time.monotonic() < deadline and any(_alive(p) for p in everyone):
            time.sleep(0.05)
        for pid in everyone:
            if _alive(pid):
                with contextlib.suppress(OSError):
                    os.kill(pid, signal.SIGKILL)
        (self.dir / SOCKET).unlink(missing_ok=True)
