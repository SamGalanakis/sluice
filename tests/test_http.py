import json
import os
import socket
import subprocess
import sys
import time

import anyio
from mcp import Client

from sluice.store import Store


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for_port(port: int, proc: subprocess.Popen, timeout: float = 20.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        assert proc.poll() is None, proc.stderr.read().decode()
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.1)
    raise AssertionError("server did not start")


async def drive(url: str) -> None:
    async with Client(url) as c:
        r = await c.call_tool("plan_create", {"plan": "web", "reason": "smoke", "doc": {
            "nodes": {"a": {"fn": "test.add", "in": {"a": {"value": 1}, "b": {"value": 2}}},
                      "b": {"fn": "test.twice", "in": {"x": {"from": "a.sum"}}}}}})
        assert json.loads(r.content[0].text) == {"rev": 1}
        deadline = time.time() + 30
        while time.time() < deadline:
            r = await c.call_tool("status", {"plan": "web"})
            status = json.loads(r.content[0].text)
            if all(n["status"] == "succeeded" for n in status["nodes"]):
                break
            await anyio.sleep(0.2)
        assert {n["id"]: n["status"] for n in status["nodes"]} == dict.fromkeys(
            ["a", "b", "b/a", "b/b"], "succeeded")
        r = await c.call_tool("node_get", {"plan": "web", "node": "b"})
        assert json.loads(r.content[0].text)["output"] == {"y": 6}


def test_sluice_serve_over_streamable_http(home):
    port = free_port()
    env = {**os.environ, "SLUICE_HOME": str(home)}
    proc = subprocess.Popen([sys.executable, "-m", "sluice.cli", "serve", "--port", str(port)],
                            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        wait_for_port(port, proc)
        anyio.run(drive, f"http://127.0.0.1:{port}/mcp")
    finally:
        proc.terminate()
        code = proc.wait(timeout=30)
    assert code == 0, proc.stderr.read().decode()
    types = [e["type"] for e in Store(home).events("web")]
    assert types[-1] == "runner_stopped"
