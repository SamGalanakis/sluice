"""Codex app-server protocol and adapter state tests."""

import base64
import hashlib
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
from pathlib import Path

import pytest

from sluice.runner import Runner
from sluice.store import Store

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from _agents.native.codex import Codex, Rpc, _private_config
from _agents.native.processes import SCOPE, scope_command
from _agents.native.tmux import _alive, descendants


def test_private_config_disables_every_mcp_and_pins_model():
    source = ('model = "old"\n[mcp_servers.a]\ncommand = "a"\nenabled = true\n'
              '[mcp_servers.a.env]\nTOKEN = "x"\n[mcp_servers.b]\ncommand = "b"\n'
              '[mcp_servers.b.http_headers]\nAuthorization = "secret"\n'
              '[mcp_servers.c]\ncommand = "c"\nenv = { TOKEN = "inline" }\n'
              '[profiles.work]\nmodel = "profile"\n')
    got = tomllib.loads(_private_config(source, "gpt-6-astra", "max", True))
    assert got["model"] == "gpt-6-astra" and got["model_reasoning_effort"] == "max"
    assert got["web_search"] == "live"
    assert got["mcp_servers"]["a"]["enabled"] is False
    assert got["mcp_servers"]["b"]["enabled"] is False
    assert "env" not in got["mcp_servers"]["a"]
    assert "http_headers" not in got["mcp_servers"]["b"]
    assert "env" not in got["mcp_servers"]["c"]
    assert got["profiles"]["work"]["model"] == "profile"


@pytest.mark.parametrize("servers", [
    ('mcp_servers = { "a.b" = { command = "x", enabled = true, '
     'env = { TOKEN = "secret" }, http_headers = { Authorization = "secret" } } }'),
    ('mcp_servers."a.b".command = "x"\nmcp_servers."a.b".enabled = true\n'
     'mcp_servers."a.b".env.TOKEN = "secret"\n'
     'mcp_servers."a.b".http_headers.Authorization = "secret"'),
    ('[mcp_servers."a.b"]\ncommand = "x"\nenabled = true\n'
     '[mcp_servers."a.b".env]\nTOKEN = "secret"\n'
     '[mcp_servers."a.b".http_headers]\nAuthorization = "secret"'),
])
def test_private_config_handles_equivalent_server_forms_and_keeps_other_values(servers):
    source = ('model = "old"\nmodel_reasoning_effort = "low"\nweb_search = "disabled"\n'
              'count = 23\nratio = 1.25\nflag = true\nwhen = 2026-09-29T12:34:56Z\n'
              'day = 2026-09-29\nclock = 12:34:56.123\n'
              'text = "quotes \\\" and newlines\\n and tabs\\t"\n'
              'items = [1, "two", { nested = [true, false] }]\n' + servers + '\n'
              '[profiles.work]\nmodel = "profile"\nweb_search = "cached"\n'
              '[[profiles.work.tools]]\nname = "first"\n'
              '[[profiles.work.tools]]\nname = "second"\n')
    expected = tomllib.loads(source)
    expected["model"] = "gpt-6-astra"
    expected["model_reasoning_effort"] = "max"
    expected.pop("web_search")
    expected["mcp_servers"]["a.b"] = {"command": "x", "enabled": False}
    assert tomllib.loads(_private_config(source, "gpt-6-astra", "max", False)) == expected


def test_private_config_keeps_values_toml_finds_hard_to_write():
    source = ('"key with space" = 1\n"é" = "ünï \\u007f \\u001b \\\\ end"\n'
              'big = 1e300\nneg = -inf\nodd = nan\nlocal = 2026-09-29T12:34:56.5\n'
              'off = 2026-09-29T12:34:56+02:00\nempty = {}\nnone = []\n'
              'deep = [[{ a = { "b.c" = [1.5, "x"] } }]]\n')
    got = tomllib.loads(_private_config(source, "sol", "max", True))
    want = tomllib.loads(source)
    assert got.pop("odd") != got.pop("odd", 0)  # nan
    want.pop("odd")
    assert got == {**want, "model": "sol", "model_reasoning_effort": "max",
                   "web_search": "live"}


def test_the_adapter_imports_without_any_installed_package(tmp_path):
    """A wrapper fn importing agent.run runs in its own environment: nothing to install
    beyond sluice itself."""
    pack = Path(__file__).resolve().parents[1]
    src = Path(__import__("sluice").__file__).parents[1]
    code = ("import sys; sys.path[:0] = sys.argv[1:]; "
            "from _agents.native.codex import _private_config; "
            "print(_private_config('a = { b = 1 }', 'sol', 'max', False), end='')")
    out = subprocess.run([sys.executable, "-I", "-S", "-c", code, str(pack), str(src)],
                         capture_output=True, text=True, check=True).stdout
    assert tomllib.loads(out) == {"a": {"b": 1}, "model": "sol", "model_reasoning_effort": "max"}


@pytest.mark.parametrize("source", ['model = [', 'model = "a"\nmodel = "b"'])
def test_private_config_reports_malformed_toml(source):
    with pytest.raises(ValueError, match="invalid Codex config TOML"):
        _private_config(source, "sol", "max", False)


@pytest.mark.parametrize("source", ['mcp_servers = 1', 'mcp_servers.x = "server"'])
def test_private_config_reports_invalid_server_tables(source):
    with pytest.raises(TypeError, match="MCP server.*table"):
        _private_config(source, "sol", "max", False)


def test_malformed_owner_config_leaves_existing_private_config_intact(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("SLUICE_HOME", str(tmp_path / "sluice"))
    source = tmp_path / ".codex"
    source.mkdir()
    (source / "config.toml").write_text("model = [")
    private = tmp_path / "sluice" / "codex-native-homes" / "t"
    private.mkdir(parents=True)
    config = private / "config.toml"
    config.write_text('model = "saved"\n')
    registry = tmp_path / "sluice" / "codex-native-sessions"
    registry.mkdir()
    (registry / "t.json").write_text(json.dumps({"home": str(private), "cwd": str(tmp_path)}))
    with pytest.raises(ValueError, match="invalid Codex config TOML"):
        Codex().prepare(tmp_path, str(tmp_path), "t")
    assert config.read_text() == 'model = "saved"\n'


@pytest.mark.parametrize("fn_name", ["agent.codex", "agent.run"])
def test_copied_codex_script_writes_the_private_config(
        fn_name, tmp_path, call_fn, fake_bin):
    installed = tmp_path / "installed" / "fns"
    pack = Path(__file__).resolve().parents[1]
    shutil.copytree(pack / fn_name, installed / fn_name)
    shutil.copytree(pack / "_agents", installed / "_agents")
    source_dir = tmp_path / ".codex"
    source_dir.mkdir()
    source = 'mcp_servers = { "a.b" = { command = "x", env = { TOKEN = "secret" } } }\n'
    (source_dir / "config.toml").write_text(source)
    captured = tmp_path / "private.toml"
    tools = fake_bin("codex", '#!/bin/sh\ncat "$CODEX_HOME/config.toml" > "$CAPTURE_CONFIG"\n'
                     'echo "config captured" >&2\nexit 77\n')
    fake_bin("tmux", "#!/bin/sh\nexit 0\n")
    fake_bin("systemd-run", "#!/bin/sh\nexit 1\n")
    inp = {"cwd": str(tmp_path), "spec": "test config"}
    if fn_name == "agent.run":
        inp["engine"] = "codex"
    code, _out, err = call_fn(installed / fn_name, inp, path=tools, env={
        "HOME": tmp_path, "CAPTURE_CONFIG": captured, "SLUICE_CODEX_CLI": tools / "codex",
    })
    assert code != 0 and "Codex app-server exited: config captured" in err, err
    assert tomllib.loads(captured.read_text())["mcp_servers"]["a.b"] == {
        "command": "x", "enabled": False}
    assert (source_dir / "config.toml").read_text() == source


class FakeRpc:
    def __init__(self):
        self.calls = []
        self.events = []

    def request(self, method, params):
        self.calls.append((method, params))
        return {"turn": {"id": "turn-1"}} if method == "turn/start" else {}

    def drain(self):
        events, self.events = self.events, []
        return events


def test_turn_start_steer_and_completed_message(tmp_path):
    codex = Codex()
    codex.thread = "thread-1"
    codex.resuming = True
    codex.rpc = FakeRpc()
    codex.progress_file = (tmp_path / "progress.log").open("w")
    codex.deliver(None, "first")
    codex.deliver(None, "steer")
    assert [m for m, _ in codex.rpc.calls] == ["turn/start", "turn/steer"]
    assert codex.rpc.calls[1][1]["expectedTurnId"] == "turn-1"
    codex.rpc.events = [
        {"method": "item/completed", "params": {"threadId": "thread-1",
          "item": {"type": "commandExecution", "command": "pwd"}}},
        {"method": "item/completed", "params": {"threadId": "thread-1",
          "item": {"type": "agentMessage", "text": "Finished."}}},
        {"method": "turn/completed", "params": {"threadId": "thread-1",
          "turn": {"id": "turn-1", "status": "completed"}}},
    ]
    class Pane:
        def dead(self):
            return None
    class Server:
        def poll(self):
            return None
    codex.server = Server()
    snap = codex.poll(Pane())
    assert (snap.state, snap.turns, codex.final()) == ("idle", 1, "Finished.")
    assert codex.progress() == ["tool commandExecution pwd", "codex: Finished."]
    codex.progress_file.close()
    assert "Finished." in (tmp_path / "progress.log").read_text()


def test_failed_steer_after_turn_end_starts_a_new_turn():
    codex = Codex()
    codex.thread, codex.turn, codex.busy, codex.resuming = "t", "old", True, True

    class RpcRace(FakeRpc):
        def request(self, method, params):
            self.calls.append((method, params))
            if method == "turn/steer":
                self.events = [{"method": "turn/completed", "params": {
                    "threadId": "t", "turn": {"id": "old", "status": "completed"}}}]
                raise RuntimeError("turn already completed")
            return {"turn": {"id": "new"}}

    class Pane:
        def dead(self):
            return None

    class Server:
        def poll(self):
            return None

    codex.rpc, codex.server = RpcRace(), Server()
    codex.deliver(Pane(), "answer")
    assert [m for m, _ in codex.rpc.calls] == ["turn/steer", "turn/start"]
    assert codex.turn == "new"


def test_resume_waits_for_the_new_threads_rollout_to_be_written():
    codex = Codex()
    codex.thread, codex.resuming = "t", False

    class RpcEmpty(FakeRpc):
        def request(self, method, params):
            out = super().request(method, params)
            if method == "thread/resume" and len(self.calls) < 4:
                raise RuntimeError("Codex thread/resume: {'code': -32603, 'message': 'failed "
                                   "to read thread: rollout at /h/rollout-t.jsonl is empty'}")
            return out

    codex.rpc = RpcEmpty()
    codex.deliver(None, "task")
    assert [m for m, _ in codex.rpc.calls] == ["turn/start", *["thread/resume"] * 3]
    assert codex.resuming and codex.busy


def test_a_turn_counts_as_started_once_from_its_reply_or_its_notification():
    codex = Codex()
    codex.thread, codex.resuming = "thread-1", True
    codex.rpc = FakeRpc()

    class Pane:
        def dead(self):
            return None

    class Server:
        def poll(self):
            return None

    codex.server = Server()
    codex.deliver(None, "task")  # turn/started was sent before anyone subscribed
    assert codex.starts == 1
    codex.rpc.events = [{"method": "turn/started", "params": {
        "threadId": "thread-1", "turn": {"id": "turn-1"}}}]
    codex.poll(Pane())
    assert codex.starts == 1
    codex.deliver(None, "steer")  # the running turn takes it
    assert codex.starts == 2
    codex.rpc.events = [{"method": "turn/started", "params": {
        "threadId": "thread-1", "turn": {"id": "turn-2"}}}]
    codex.poll(Pane())
    assert codex.starts == 3


def test_codex_home_and_engine_environment(tmp_path, monkeypatch):
    import _agents.native.codex as mod

    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("SLUICE_HOME", str(tmp_path / "sluice"))
    monkeypatch.setenv("SLUICE_HOST_PATH", "/usr/bin")
    monkeypatch.setenv("SLUICE_HOST_PYTHONPATH", "")
    monkeypatch.setenv("SLUICE_HOST_VIRTUAL_ENV", "")
    monkeypatch.setenv("PATH", "/wrong/venv/bin:/usr/bin")
    monkeypatch.setenv("PYTHONPATH", "/wrong/sluice/src")
    monkeypatch.setenv("VIRTUAL_ENV", "/wrong/venv")
    monkeypatch.setenv("CLAUDECODE", "1")
    source = tmp_path / ".codex"
    source.mkdir()
    (source / "config.toml").write_text("[mcp_servers.x]\ncommand = 'x'\n"
                                        "[mcp_servers.x.env]\nTOKEN = 'secret'\n")
    run_dir = tmp_path / "run"
    run_dir.mkdir()

    class Server:
        pid = 999999999
        def poll(self):
            return None

    class RpcStub(FakeRpc):
        def __init__(self, path):
            super().__init__()
        def _send(self, obj):
            pass
        def request(self, method, params):
            if method == "thread/resume":
                return {"thread": {"id": params["threadId"]}}
            return {}

    def popen(argv, **kw):
        socket_arg = next(a for a in argv if a.startswith("unix://"))
        Path(socket_arg[7:]).touch()
        return Server()

    monkeypatch.setattr(mod, "Rpc", RpcStub)
    monkeypatch.setattr(mod.subprocess, "Popen", popen)
    monkeypatch.setattr(mod, "scope_command", lambda argv: argv)
    monkeypatch.setattr(mod, "find_argv", lambda token: None)
    codex = Codex()
    codex.prepare(run_dir, str(tmp_path), None)
    codex.rpc.events = [{"method": "thread/started", "params": {"thread": {"id": "thread-1"}}}]

    class Pane:
        def dead(self):
            return None
        def capture(self):
            return "›"

    codex.wait_ready(Pane(), 1)
    private = tmp_path / "sluice/codex-native-homes/thread-1"
    assert private.is_dir() and codex.private == private
    assert json.loads((tmp_path / "sluice/codex-native-sessions/thread-1.json").read_text())[
        "home"] == str(private)
    assert private.joinpath("config.toml").stat().st_mode & 0o777 == 0o600
    assert "env" not in tomllib.loads(private.joinpath("config.toml").read_text())[
        "mcp_servers"]["x"]
    assert codex.env()["PATH"] == "/usr/bin"
    assert "PYTHONPATH" not in codex.env() and "VIRTUAL_ENV" not in codex.env()
    assert "CLAUDECODE" not in codex.env()
    pending_alias = codex.pending_link
    codex.server = codex.rpc = None
    codex.close()
    assert pending_alias.is_symlink() and pending_alias.resolve() == private
    old = run_dir / "codex-home"
    old.mkdir()
    (old / "sessions").mkdir()
    (old / "sessions/old.jsonl").write_text("saved\n")
    (old / "config.toml").write_text("TOKEN = 'old'\n")
    (old / "config.toml").chmod(0o664)
    (tmp_path / "sluice/codex-native-sessions/legacy.json").write_text(json.dumps(
        {"home": str(old), "cwd": str(tmp_path)}))
    resumed = Codex()
    resumed.prepare(run_dir, str(tmp_path), "legacy")
    assert resumed.private == tmp_path / "sluice/codex-native-homes/legacy"
    assert resumed.private.joinpath("sessions/old.jsonl").read_text() == "saved\n"
    assert resumed.private.joinpath("config.toml").stat().st_mode & 0o777 == 0o600
    assert "old" not in resumed.private.joinpath("config.toml").read_text()
    assert json.loads((tmp_path / "sluice/codex-native-sessions/legacy.json").read_text())[
        "home"] == str(resumed.private)
    resumed.log_file.close()
    resumed.progress_file.close()


def test_missing_codex_private_home_is_clear(tmp_path, monkeypatch):
    monkeypatch.setenv("SLUICE_HOME", str(tmp_path))
    registry = tmp_path / "codex-native-sessions"
    registry.mkdir()
    (registry / "t.json").write_text(json.dumps({"home": str(tmp_path / "gone"),
                                                  "cwd": str(tmp_path)}))
    with pytest.raises(FileNotFoundError, match="home is missing"):
        Codex().prepare(tmp_path, str(tmp_path), "t")


def test_corrupt_codex_rollout_line_does_not_hide_cwd(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("SLUICE_HOME", str(tmp_path / "sluice"))
    rollout = tmp_path / ".codex/sessions/2026/09/28/rollout-a-t.jsonl"
    rollout.parent.mkdir(parents=True)
    rollout.write_text('bad json\n{"type":"session_meta","payload":{"cwd":"/work"}}\n')
    assert Codex().session_cwd("t") == "/work"


def test_systemd_scope_uses_lane_weights(monkeypatch):
    import _agents.native.processes as mod
    monkeypatch.setattr(mod, "_scope_ok", True)
    assert scope_command(["tmux", "new-session"]) == [*SCOPE, "tmux", "new-session"]


def test_rpc_handshake_request_and_notification(tmp_path):
    path = tmp_path / "app.sock"
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(path))
    listener.listen()
    seen = []

    def server():
        conn, _ = listener.accept()
        with conn:
            request = b""
            while b"\r\n\r\n" not in request:
                request += conn.recv(4096)
            key = request.split(b"Sec-WebSocket-Key: ", 1)[1].split(b"\r\n", 1)[0]
            accept = base64.b64encode(hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-"
                                                   b"C5AB0DC85B11").digest())
            conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: "
                         + accept + b"\r\n\r\n")
            header = conn.recv(2)
            n = header[1] & 127
            if n == 126:
                n = struct.unpack("!H", conn.recv(2))[0]
            mask = conn.recv(4)
            payload = b""
            while len(payload) < n:
                payload += conn.recv(n - len(payload))
            seen.append(json.loads(bytes(b ^ mask[i % 4] for i, b in enumerate(payload))))
            def frame(obj):
                data = json.dumps(obj).encode()
                return b"\x81" + (bytes([len(data)]) if len(data) < 126
                                   else b"\x7e" + struct.pack("!H", len(data))) + data
            conn.sendall(frame({"method": "thread/started", "params": {"thread": {"id": "t"}}})
                         + frame({"id": 1, "result": {"ok": True}}))
            conn.recv(1)
    worker = threading.Thread(target=server)
    worker.start()
    rpc = Rpc(path)
    assert rpc.request("initialize", {"clientInfo": {"name": "test"}}) == {"ok": True}
    assert rpc.drain()[0]["method"] == "thread/started"
    rpc.close()
    worker.join(timeout=2)
    listener.close()
    assert seen == [{"id": 1, "method": "initialize", "params":
                     {"clientInfo": {"name": "test"}}}]


@pytest.mark.parametrize("continuation", [False, True])
def test_rpc_drain_buffers_a_slow_partial_frame_and_continuation(tmp_path, continuation):
    path = tmp_path / "app.sock"
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(path))
    listener.listen()

    def frame(opcode, data, fin=True):
        return bytes([(128 if fin else 0) | opcode, 127]) + struct.pack("!Q", len(data)) + data

    def server():
        conn, _ = listener.accept()
        with conn:
            request = b""
            while b"\r\n\r\n" not in request:
                request += conn.recv(4096)
            key = request.split(b"Sec-WebSocket-Key: ", 1)[1].split(b"\r\n", 1)[0]
            accept = base64.b64encode(hashlib.sha1(key + b"258EAFA5-E914-47DA-95CA-"
                                                   b"C5AB0DC85B11").digest())
            conn.sendall(b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: "
                         + accept + b"\r\n\r\n")
            time.sleep(0.05)
            body = json.dumps({"method": "item/completed", "params": {"text": "x" * 300000}}).encode()
            first = frame(1, body[:150000], False) if continuation else frame(1, body)
            conn.sendall(first[:len(first) // 2])
            time.sleep(0.3)
            conn.sendall(first[len(first) // 2:])
            if continuation:
                conn.sendall(frame(0, body[150000:]))
            time.sleep(0.5)

    worker = threading.Thread(target=server)
    worker.start()
    rpc = Rpc(path)
    time.sleep(0.1)
    started = time.monotonic()
    assert rpc.drain() == []
    assert time.monotonic() - started < 0.15
    events, deadline = [], time.monotonic() + 5  # the rest arrives ~0.35 s later, or later
    while not events and time.monotonic() < deadline:  # under load
        time.sleep(0.05)
        events = rpc.drain()
    assert len(events) == 1 and len(events[0]["params"]["text"]) == 300000
    rpc.close()
    worker.join(timeout=2)
    listener.close()


def test_close_ends_the_app_server_process_tree(tmp_path):
    codex = Codex()
    codex.short_dir = tmp_path / "socket"
    codex.short_dir.mkdir()
    codex.server = subprocess.Popen(
        [sys.executable, "-c", ("import subprocess,time; "
                                "subprocess.Popen(['sleep','60']); time.sleep(60)")],
        start_new_session=True)
    try:
        deadline = time.monotonic() + 3
        while not descendants(codex.server.pid) and time.monotonic() < deadline:
            time.sleep(0.02)
        children = descendants(codex.server.pid)
        assert children
        codex.close()
        assert codex.server.poll() is not None
        assert all(not _alive(pid) for pid in children)
        assert not codex.short_dir.exists()
    finally:
        if codex.server.poll() is None:
            codex.close()


@pytest.mark.skipif(os.environ.get("SLUICE_LIVE") != "1", reason="set SLUICE_LIVE=1")
@pytest.mark.live
def test_codex_live_declared_output_and_resume():
    scratch = Path(os.environ.get("SLUICE_LIVE_DIR", tempfile.gettempdir())) / "sluice-live"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="codex-live-", dir=scratch) as root:
        root = Path(root)
        home, work = root / "home", root / "work"
        home.mkdir()
        work.mkdir()
        (home / "config.json").write_text(json.dumps({"fn_dirs": [str(Path(__file__).parents[1])]}))
        (home / ".env").write_text("SLUICE_AGENT_GRACE_MIN=0.02\n"
                                   "SLUICE_AGENT_SETTLE_S=0.5\n")
        store = Store(home)
        store.create_project("p", "", "t", "t")

        def step(spec, output, *, engine="agent.codex", **extra):
            inputs = {"cwd": {"default": str(work)}, "spec": {"default": spec}, **extra}
            if engine == "agent.run":
                inputs["engine"] = {"default": "codex"}
            return {"run": engine, "outputs": {output: "string"}, "in": inputs}

        steps = {
            "pick": step("Submit the literal word blue as `word` using the command in your "
                         "task. Then finish.", "word"),
            "again": step("Submit the word you submitted in the preceding turn as `again`. "
                          "Then finish.", "again", engine="agent.run",
                          session={"source": "pick/session"}),
        }
        store.patch("p", 1, [{"op": "replace", "path": "/steps", "value": steps}], "t", "t")
        runner = Runner(store)
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            runner.tick()
            state = store.read_state("p")["steps"]
            if len(state) == 2 and all(e["status"] in ("succeeded", "failed")
                                       for e in state.values()):
                break
            time.sleep(0.2)
        state = store.read_state("p")["steps"]
        for name in steps:
            run_dir = store.runs_dir("p") / state[name]["run_ids"][-1]
            assert state[name]["status"] == "succeeded", (name, state[name].get("error"),
                                                              (run_dir / "stderr.log").read_text())
            assert not (run_dir / "tmux.sock").exists()
        assert state["pick"]["outputs"]["word"] == "blue"
        assert state["again"]["outputs"]["again"] == "blue"
        assert state["again"]["outputs"]["session"] == state["pick"]["outputs"]["session"]
