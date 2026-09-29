"""A Codex thread driven through the app-server, with its TUI in the run's tmux pane."""

import base64
import contextlib
import hashlib
import json
import os
import re
import select
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import time
from pathlib import Path

from .processes import engine_env, find_argv, record, scope_command, start_time
from .supervisor import Adapter, Snapshot
from .tmux import _alive, descendants

CODEX_TRANSIENT = ("rate limit", "rate_limit", "429", "capacity", "overloaded",
                   "usage limit")
MODELS = {"sol": "gpt-6-sol", "astra": "gpt-6-astra"}
EFFORTS = {"minimal", "low", "medium", "high", "xhigh", "max"}


def _private_config(source, model, effort, search):
    """Keep the owner's settings, but disable every configured MCP server for this run."""
    lines = source.splitlines()
    names = set()
    out = []
    parent = False
    skip_table = False
    for line in lines:
        header = re.match(r"^\s*\[mcp_servers\.([^].]+)(?:\.([^]]+))?\]\s*(?:#.*)?$", line)
        if header:
            name = header.group(1)
            skip_table = header.group(2) in ("env", "http_headers")
            if skip_table:
                parent = False
                continue
            if name not in names:
                names.add(name)
                if "." not in line.split("]", 1)[0][13:]:
                    parent = True
                    out.extend((line, "enabled = false"))
                    continue
            parent = line.strip() == f"[mcp_servers.{name}]"
        elif line.lstrip().startswith("["):
            parent = False
            skip_table = False
        if skip_table:
            continue
        if parent and re.match(r"^\s*(?:enabled|env|http_headers)(?:\.[^=]+)?\s*=", line):
            continue
        out.append(line)
    prefix = [f'model = "{model}"', f'model_reasoning_effort = "{effort}"']
    if search:
        prefix.append('web_search = "live"')
    # Top-level keys must precede the first table. Existing keys are removed first.
    top = True
    kept = []
    for line in out:
        if line.lstrip().startswith("["):
            top = False
        if not (top and re.match(r"^\s*(model|model_reasoning_effort|web_search)\s*=", line)):
            kept.append(line)
    out = kept
    return "\n".join([*prefix, *out]) + "\n"


class Rpc:
    """The small synchronous Unix WebSocket subset used by Codex's JSON-RPC endpoint."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(2)
        self.sock.connect(str(path))
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall(("GET /rpc HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
                           "Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n"
                           f"Sec-WebSocket-Key: {key}\r\n\r\n").encode())
        data = b""
        while b"\r\n\r\n" not in data:
            data += self.sock.recv(4096)
        head, self.buffer = data.split(b"\r\n\r\n", 1)
        expected = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-"
                                                   "C5AB0DC85B11").encode()).digest())
        if b" 101 " not in head.split(b"\r\n", 1)[0] or expected not in head:
            raise RuntimeError(f"Codex app-server WebSocket handshake failed: {head[:200]!r}")
        self.next_id = 0
        self.events = []
        self.fragments = bytearray()

    def close(self):
        self.sock.close()

    def _send(self, obj):
        data = json.dumps(obj).encode()
        mask = os.urandom(4)
        n = len(data)
        size = bytes([n]) if n < 126 else (b"\x7e" + struct.pack("!H", n) if n < 65536
                                         else b"\x7f" + struct.pack("!Q", n))
        self.sock.sendall(b"\x81" + bytes([size[0] | 128]) + size[1:] + mask
                          + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def _frames(self):
        """Parse only complete frames; leave a partial header or payload in the buffer."""
        out = []
        while len(self.buffer) >= 2:
            first, second = self.buffer[:2]
            n, offset = second & 127, 2
            if n == 126:
                if len(self.buffer) < 4:
                    break
                n, offset = struct.unpack("!H", self.buffer[2:4])[0], 4
            elif n == 127:
                if len(self.buffer) < 10:
                    break
                n, offset = struct.unpack("!Q", self.buffer[2:10])[0], 10
            masked = bool(second & 128)
            end = offset + (4 if masked else 0) + n
            if len(self.buffer) < end:
                break
            mask = self.buffer[offset:offset + 4] if masked else None
            data = self.buffer[offset + (4 if masked else 0):end]
            self.buffer = self.buffer[end:]
            if mask:
                data = bytes(b ^ mask[i % 4] for i, b in enumerate(data))
            opcode = first & 15
            if opcode == 8:
                raise ConnectionError("Codex app-server closed the WebSocket")
            if opcode == 9:
                reply_mask = os.urandom(4)
                self.sock.sendall(b"\x8a" + bytes([len(data) | 128]) + reply_mask
                                  + bytes(b ^ reply_mask[i % 4] for i, b in enumerate(data)))
                continue
            if opcode == 10:
                continue
            if opcode not in (0, 1) or (opcode == 1 and self.fragments):
                raise ConnectionError("invalid Codex WebSocket frame sequence")
            self.fragments.extend(data)
            if first & 128:
                out.append(json.loads(self.fragments))
                self.fragments.clear()
        return out

    def _read_available(self, timeout):
        if not select.select([self.sock], [], [], timeout)[0]:
            return False
        chunk = self.sock.recv(65536)
        if not chunk:
            raise ConnectionError("Codex app-server closed the connection")
        self.buffer += chunk
        return True

    def request(self, method, params=None, timeout=30):
        self.next_id += 1
        rid = self.next_id
        self._send({"id": rid, "method": method, "params": params or {}})
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            frames = self._frames()
            for index, msg in enumerate(frames):
                if msg.get("id") == rid and "method" not in msg:
                    self.events.extend(frames[index + 1:])
                    if msg.get("error"):
                        raise RuntimeError(f"Codex {method}: {msg['error']}")
                    return msg.get("result") or {}
                self.events.append(msg)
            self._read_available(max(0, deadline - time.monotonic()))
        raise TimeoutError(f"Codex {method} timed out")

    def drain(self):
        self.events.extend(self._frames())
        while self._read_available(0):
            self.events.extend(self._frames())
        out, self.events = self.events, []
        return out


class Codex(Adapter):
    name = "codex"
    transient = CODEX_TRANSIENT
    wait_signal = False

    def __init__(self, model="sol", effort=None):
        if model not in MODELS:
            raise ValueError(f"codex models are {', '.join(MODELS)}, got {model!r}")
        self.model = MODELS[model]
        self.effort = effort or "high"
        if self.effort not in EFFORTS:
            raise ValueError(f"invalid codex effort {self.effort!r}")
        self.thread = self.turn = self.message = self.error = ""
        self.turns = self.version = 0
        self.starts = 0
        self.started = set()  # turn ids already counted in `starts`
        self.lines = []
        self.server = self.rpc = self.log_file = None
        self.app_pid = None
        self.app_start = None
        self.busy = False

    def session_cwd(self, session):
        registry = (Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")
                    / "codex-native-sessions" / f"{session}.json")
        if registry.exists():
            return json.loads(registry.read_text()).get("cwd")
        for path in (Path.home() / ".codex" / "sessions").glob(
                f"*/*/*/rollout-*-{session}.jsonl"):
            with path.open(errors="replace") as stream:
                for line in stream:
                    try:
                        rec = json.loads(line)
                    except ValueError:
                        continue
                    if rec.get("type") == "session_meta":
                        return rec.get("payload", {}).get("cwd")
        return None

    def prepare(self, run_dir, cwd, session):
        self.run_dir, self.cwd = Path(run_dir), cwd
        self.resuming = bool(session)
        self.short_dir = Path(tempfile.mkdtemp(prefix="sluice-codex-"))
        self.socket_path = self.short_dir / "app.sock"
        self.registry_dir = (Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")
                             / "codex-native-sessions")
        self.registry_dir.mkdir(parents=True, exist_ok=True)
        meta = self.registry_dir / f"{session}.json" if session else None
        homes = self.registry_dir.parent / "codex-native-homes"
        homes.mkdir(parents=True, exist_ok=True)
        if meta and meta.exists():
            private = Path(json.loads(meta.read_text())["home"])
            if not private.is_dir():
                raise FileNotFoundError(f"Codex session {session} home is missing: {private}")
            target = homes / session
            if private != target:
                if not target.exists():
                    shutil.copytree(private, target, symlinks=True,
                                    ignore=lambda root, names: ({"config.toml"}
                                                                if Path(root) == private else set()))
                private = target
        else:
            private = homes / (session or f"pending-{self.run_dir.name}")
            private.mkdir(parents=True, exist_ok=True)
        source = Path.home() / ".codex"
        if session and (not meta or not meta.exists()):
            found = False
            for rollout in (source / "sessions").glob(f"*/*/*/rollout-*-{session}.jsonl"):
                target = private / "sessions" / rollout.relative_to(source / "sessions")
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(rollout, target)
                found = True
                break
            if not found:
                raise FileNotFoundError(f"Codex session {session} has no saved rollout or home")
        for name in ("auth.json", "skills", "memories", "rules", "prompts", "plugins",
                     "AGENTS.md", "AGENTS.override.md"):
            src, dest = source / name, private / name
            if src.exists() and not dest.exists():
                dest.symlink_to(src, target_is_directory=src.is_dir())
        search = os.environ.get("SLUICE_CODEX_SEARCH") == "1"
        config = (source / "config.toml").read_text() if (source / "config.toml").exists() else ""
        fd = os.open(private / "config.toml", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w") as f:
            f.write(_private_config(config, self.model, self.effort, search))
        self._env = {**engine_env(), "CODEX_HOME": str(private)}
        env_file = Path(cwd) / "env.sh"
        if not env_file.exists() and Path(cwd).name == "merged":
            env_file = Path(cwd).parent / "env.sh"
        if env_file.exists():
            matches = re.findall(r'^export CARGO_TARGET_DIR="(/[A-Za-z0-9._/,:@%+=-]+)"$',
                                 env_file.read_text(), re.MULTILINE)
            if len(matches) == 1:
                self._env["CARGO_TARGET_DIR"] = matches[0]
        self._env.pop("CARGO_TARGET_DIR_AUTO", None)
        self._env.update(CARGO_INCREMENTAL="0", CARGO_PROFILE_DEV_DEBUG="line-tables-only",
                         CARGO_PROFILE_TEST_DEBUG="line-tables-only")
        self.log_file = (self.run_dir / "app-server.log").open("w")
        self.progress_file = (self.run_dir / "codex.log").open("w")
        cmd = [os.environ.get("SLUICE_CODEX_CLI", "codex"), "app-server", "--listen",
               f"unix://{self.socket_path}"]
        self.server = subprocess.Popen(
            scope_command(cmd), cwd=cwd, env=self._env, stdin=subprocess.DEVNULL,
            stdout=self.log_file, stderr=subprocess.STDOUT, start_new_session=True)
        record(self.run_dir, "app_scope", self.server.pid)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.server.poll() is not None:
                raise RuntimeError("Codex app-server exited: "
                                   + (self.run_dir / "app-server.log").read_text()[-1000:])
            if self.socket_path.exists():
                self.app_pid = find_argv(f"unix://{self.socket_path}")
                self.app_start = start_time(self.app_pid) if self.app_pid else None
                record(self.run_dir, "app_server", self.app_pid)
                try:
                    self.rpc = Rpc(self.socket_path)
                    break
                except (OSError, ConnectionError):
                    pass
            time.sleep(0.1)
        if not self.rpc:
            raise TimeoutError("Codex app-server did not become ready")
        self.rpc.request("initialize", {"clientInfo": {"name": "sluice", "version": "0.1"},
                                        "capabilities": {"experimentalApi": True}})
        self.rpc._send({"method": "initialized"})
        self.private = private
        if session:
            result = self.rpc.request("thread/resume", {"threadId": session,
                                                         "approvalPolicy": "never",
                                                         "sandbox": "danger-full-access"})
            self.thread = result["thread"]["id"]
            self._save_home()

    def _save_home(self):
        if self.private.name.startswith("pending-"):
            target = self.private.parent / self.thread
            old = self.private
            old.rename(target)
            old.symlink_to(target, target_is_directory=True)
            self.pending_link = old
            self.private = target
        (self.registry_dir / f"{self.thread}.json").write_text(json.dumps(
            {"home": str(self.private), "cwd": self.cwd}))

    def argv(self):
        cmd = [os.environ.get("SLUICE_CODEX_CLI", "codex")]
        if self.resuming:
            return [*cmd, "resume", "--remote", f"unix://{self.socket_path}", self.thread]
        return [*cmd, "--dangerously-bypass-approvals-and-sandbox", "--remote",
                f"unix://{self.socket_path}"]

    def env(self):
        return self._env

    def wait_ready(self, tmux, timeout):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if tmux.dead() is not None:
                raise RuntimeError("Codex TUI exited before it was ready: " + tmux.capture()[-1000:])
            for event in self.rpc.drain():
                if event.get("method") == "thread/started" and not self.thread:
                    self.thread = (event.get("params", {}).get("thread") or {}).get("id", "")
                    if self.thread:
                        self._save_home()
            if self.thread and "›" in tmux.capture():
                return
            time.sleep(0.2)
        raise TimeoutError("Codex TUI did not become ready")

    def deliver(self, tmux, text):
        input_items = [{"type": "text", "text": text}]
        if self.busy and self.turn:
            try:
                self.rpc.request("turn/steer", {"threadId": self.thread,
                                                 "expectedTurnId": self.turn,
                                                 "input": input_items})
            except RuntimeError:
                self.poll(tmux)
                if self.busy:
                    raise
            else:
                self.starts += 1  # the running turn took the message
                self.version += 1
                return
        if not self.busy:
            result = self.rpc.request("turn/start", {"threadId": self.thread,
                                                      "input": input_items,
                                                      "model": self.model,
                                                      "effort": self.effort})
            self.turn = result["turn"]["id"]
            self._count_start(self.turn)  # its turn/started comes before the resume subscribes
            self.busy = True
            if not self.resuming:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    try:
                        self.rpc.request("thread/resume", {"threadId": self.thread})
                        self.resuming = True
                        break
                    except RuntimeError as e:
                        if not any(marker in str(e) for marker in
                                   ("no rollout found", "is empty",
                                    "list_turns is not supported yet")):
                            raise
                        time.sleep(0.2)
                if not self.resuming:
                    raise TimeoutError("Codex thread did not become subscribable after "
                                       "turn/start")
        self.version += 1

    def _count_start(self, turn):
        """Count a turn as started once, whether turn/start's reply or its turn/started
        notification shows it first."""
        if turn not in self.started:
            self.started.add(turn)
            self.starts += 1

    def poll(self, tmux):
        try:
            events = self.rpc.drain()
        except (ConnectionError, OSError) as e:
            detail = ""
            with contextlib.suppress(OSError):
                detail = (self.run_dir / "app-server.log").read_text()[-1000:]
            return Snapshot("exited", self.turns, progress=self.version,
                            error=f"{e}: {detail}", exit_status="app-server disconnected")
        for event in events:
            method, params = event.get("method", ""), event.get("params") or {}
            if params.get("threadId") not in (None, self.thread):
                continue
            if method == "turn/started":
                self.busy = True
                self.turn = (params.get("turn") or {}).get("id", self.turn)
                self._count_start(self.turn)
            elif method in ("turn/completed", "turn/failed"):
                turn = params.get("turn") or {}
                if turn.get("id") not in (None, self.turn):
                    continue
                self.busy = False
                self.turns += 1
                self.error = str(turn.get("error") or "")
                if turn.get("status") == "failed" and not self.error:
                    self.error = str(params)
            elif method == "item/completed":
                item = params.get("item") or {}
                kind = item.get("type")
                if kind == "agentMessage":
                    self.message = item.get("text") or self.message
                    self.lines.append("codex: " + " ".join(self.message.split())[:240])
                elif kind in ("commandExecution", "fileChange", "webSearch"):
                    self.lines.append("tool " + kind + " " + str(item.get("command") or
                                                                  item.get("query") or "")[:200])
            elif method == "error":
                self.error = str(params.get("message") or params)
                self.lines.append("codex error: " + self.error[:240])
            self.version += 1
        if self.server.poll() is not None or tmux.dead() is not None:
            if self.server.poll() is not None and not self.error:
                with contextlib.suppress(OSError):
                    self.error = (self.run_dir / "app-server.log").read_text()[-1000:]
            return Snapshot("exited", self.turns, progress=self.version, error=self.error,
                            exit_status=str(self.server.returncode or tmux.dead()))
        return Snapshot("busy" if self.busy else "idle" if self.turns else "starting",
                        self.turns, progress=self.version, error=self.error,
                        starts=self.starts)

    def progress(self):
        lines, self.lines = self.lines, []
        for line in lines:
            self.progress_file.write(line + "\n")
        self.progress_file.flush()
        return lines

    def session_id(self):
        return self.thread

    def roots(self, tmux):
        """The TUI and the app-server, which runs the agent's commands."""
        return [tmux.pane_pid(), self.app_pid]

    def final(self):
        return self.message

    def exit(self, tmux):
        with contextlib.suppress(Exception):
            tmux.keys("C-c")

    def close(self):
        if self.rpc:
            with contextlib.suppress(OSError):
                self.rpc.close()
        if self.server:
            app_pid = self.app_pid
            if app_pid and start_time(app_pid) != self.app_start:
                app_pid = None
            if not app_pid and hasattr(self, "socket_path"):
                app_pid = find_argv(f"unix://{self.socket_path}")
            children = descendants(app_pid) if app_pid else descendants(self.server.pid)
            for pid in children:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(pid, signal.SIGTERM)
            with contextlib.suppress(ProcessLookupError):
                os.killpg(self.server.pid, signal.SIGTERM)
            if app_pid:
                with contextlib.suppress(ProcessLookupError):
                    os.kill(app_pid, signal.SIGTERM)
            try:
                self.server.wait(timeout=0.75)
            except subprocess.TimeoutExpired:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(self.server.pid, signal.SIGKILL)
                if app_pid:
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(app_pid, signal.SIGKILL)
                self.server.wait()
            for pid in children:
                if _alive(pid):
                    with contextlib.suppress(ProcessLookupError):
                        os.kill(pid, signal.SIGKILL)
        if self.log_file:
            self.log_file.close()
        if hasattr(self, "progress_file"):
            self.progress_file.close()
        if hasattr(self, "short_dir"):
            shutil.rmtree(self.short_dir, ignore_errors=True)
