import json
import os
import socket
import subprocess
import sys
import time
import urllib.request

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
            "outputs": {"total": {"source": "b/sum"}},
            "steps": {"a": {"run": "test.add", "in": {"a": {"default": 1}, "b": {"default": 2}}},
                      "b": {"run": "test.add", "in": {"a": {"source": "a/sum"},
                                                      "b": {"default": 3}}}}}})
        assert json.loads(r.content[0].text) == {"rev": 1}
        deadline = time.time() + 30
        while time.time() < deadline:
            r = await c.call_tool("status", {"plan": "web"})
            status = json.loads(r.content[0].text)
            if all(s["status"] == "succeeded" for s in status["steps"]):
                break
            await anyio.sleep(0.2)
        assert status["outputs"] == {"total": 6}


def test_sluice_serve_over_streamable_http(home):
    port = free_port()
    env = {**os.environ, "SLUICE_HOME": str(home)}
    proc = subprocess.Popen([sys.executable, "-m", "sluice.cli", "serve", "--port", str(port)],
                            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        wait_for_port(port, proc)
        anyio.run(drive, f"http://127.0.0.1:{port}/mcp")
        index = urllib.request.urlopen(f"http://127.0.0.1:{port}/plans", timeout=10)
        assert index.status == 200 and b'href="/plans/web"' in index.read()
        page = urllib.request.urlopen(f"http://127.0.0.1:{port}/plans/web", timeout=10)
        body = page.read().decode()
        assert page.status == 200 and '<pre class="mermaid">' in body and "refresh" in body
    finally:
        proc.terminate()
        code = proc.wait(timeout=30)
    assert code == 0, proc.stderr.read().decode()
    assert Store(home).read_state("web")["steps"]["b"]["outputs"] == {"sum": 6}
