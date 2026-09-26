import json
import os
import socket
import subprocess
import sys
import time
import urllib.error
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
        r = await c.call_tool("project_create", {"name": "web", "description": "smoke"})
        assert json.loads(r.content[0].text) == {"name": "web"}
        steps = {"a": {"run": "test.add", "in": {"a": {"default": 1}, "b": {"default": 2}}},
                 "b": {"run": "test.add", "in": {"a": {"source": "a/sum"},
                                                 "b": {"default": 3}}}}
        r = await c.call_tool("plan_patch", {"project": "web", "rev": 1, "reason": "smoke", "ops": [
            {"op": "replace", "path": "/steps", "value": steps},
            {"op": "replace", "path": "/outputs", "value": {"total": {"source": "b/sum"}}}]})
        assert json.loads(r.content[0].text) == {"rev": 2}
        deadline = time.time() + 30
        while time.time() < deadline:
            r = await c.call_tool("status", {"project": "web"})
            status = json.loads(r.content[0].text)
            if all(s["status"] == "succeeded" for s in status["steps"]):
                break
            await anyio.sleep(0.2)
        assert status["outputs"] == {"total": 6}


def get(port: int, path: str) -> tuple[int, str]:
    try:
        r = urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=10)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()
    return r.status, r.read().decode()


def test_sluice_serve_over_streamable_http_and_the_dashboard(home):
    port = free_port()
    env = {**os.environ, "SLUICE_HOME": str(home)}
    proc = subprocess.Popen([sys.executable, "-m", "sluice.cli", "serve", "--port", str(port)],
                            env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        wait_for_port(port, proc)
        anyio.run(drive, f"http://127.0.0.1:{port}/mcp")
        code, index = get(port, "/")
        assert code == 200 and 'href="/projects/web"' in index and "smoke" in index
        code, page = get(port, "/projects/web")
        assert code == 200 and '<pre class="mermaid">' in page and "datastar@v1.0.4" in page
        assert "<h2>Steps</h2>" in page and "<h2>History</h2>" in page
        code, log = get(port, "/projects/web/log?kind=step")
        assert code == 200 and "b pending → " in log and "<code>plan.edit</code>" not in log
        code, fns = get(port, "/fns?project=web")
        assert code == 200 and "<h2>Built-in</h2>" in fns and "<b>test.add</b>" in fns
        assert get(port, "/projects/nope")[0] == 404
        assert get(port, "/fns?project=nope")[0] == 404
    finally:
        proc.terminate()
        code = proc.wait(timeout=30)
    assert code == 0, proc.stderr.read().decode()
    assert Store(home).read_state("web")["steps"]["b"]["outputs"] == {"sum": 6}


def test_a_server_restart_leaves_steps_of_a_separate_runner_running(home):
    env = {**os.environ, "SLUICE_HOME": str(home)}
    store = Store(home)
    loop = subprocess.Popen([sys.executable, "-m", "sluice.cli", "loop"], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def serve() -> tuple[subprocess.Popen, int]:
        port = free_port()
        proc = subprocess.Popen([sys.executable, "-m", "sluice.cli", "serve", "--no-runner",
                                 "--port", str(port)], env=env,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        wait_for_port(port, proc)
        return proc, port

    async def plan(url: str) -> None:
        async with Client(url) as c:
            await c.call_tool("project_create", {"name": "slow"})
            await c.call_tool("plan_patch", {"project": "slow", "rev": 1, "reason": "t", "ops": [
                {"op": "replace", "path": "/steps", "value": {
                    "w": {"run": "test.window", "in": {"seconds": {"default": 3}}}}}]})

    def status() -> str:
        return store.read_state("slow")["steps"].get("w", {}).get("status", "none")

    try:
        server, port = serve()
        anyio.run(plan, f"http://127.0.0.1:{port}/mcp")
        deadline = time.time() + 10
        while status() != "running" and time.time() < deadline:
            time.sleep(0.1)
        assert status() == "running"
        server.terminate()
        assert server.wait(timeout=30) == 0
        server, port = serve()  # a restarted server; the step is still running meanwhile
        assert status() == "running"
        deadline = time.time() + 20
        while status() == "running" and time.time() < deadline:
            time.sleep(0.1)
        assert status() == "succeeded", store.read_state("slow")
        assert get(port, "/projects/slow")[0] == 200
    finally:
        for p in (locals().get("server"), loop):
            if p is not None and p.poll() is None:
                p.terminate()
                p.wait(timeout=30)
    assert loop.returncode == 0, loop.stderr.read().decode()
