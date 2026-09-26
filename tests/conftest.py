import json
import socket
import threading
import time
from pathlib import Path

import pytest
import uvicorn

from sluice.mcp_server import build_server
from sluice.runner import Runner
from sluice.store import Store

TESTPACK = Path(__file__).parent / "testpack"
DONE = ("succeeded", "failed")


def write_config(home: Path, **extra) -> None:
    home.mkdir(parents=True, exist_ok=True)
    cfg = {"fn_dirs": [str(TESTPACK)], "max_parallel": 8, **extra}
    (home / "config.json").write_text(json.dumps(cfg))


@pytest.fixture
def home(tmp_path: Path) -> Path:
    h = tmp_path / "home"
    write_config(h)
    return h


@pytest.fixture
def store(home: Path) -> Store:
    return Store(home)


@pytest.fixture
def port(store):
    """A dashboard served from this process (no runner), polling every 0.1 s."""
    stop = threading.Event()
    app = build_server(store, stop, interval=0.1).streamable_http_app()
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        free = s.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(app, host="127.0.0.1", port=free,
                                           log_level="warning"))
    thread = threading.Thread(target=server.run, daemon=True)
    thread.start()
    deadline = time.time() + 10
    while not server.started:
        assert time.time() < deadline, "server did not start"
        time.sleep(0.05)
    yield free
    stop.set()
    server.should_exit = True
    thread.join(timeout=10)
    assert not thread.is_alive()


@pytest.fixture
def runner(store: Store):
    r = Runner(store)
    yield r
    for a in r.active.values():
        a.kill()


def create(store: Store, project: str, steps: dict, **doc) -> int:
    """A project whose plan is `steps` plus `inputs`/`outputs` (one edit: rev 2)."""
    store.create_project(project, f"the {project} project", "test", "test")
    plan = {"inputs": {}, "outputs": {}, "steps": steps, **doc}
    ops = [{"op": "replace", "path": f"/{k}", "value": v} for k, v in plan.items()]
    return store.patch(project, 1, ops, "test", "test")


def write_fn(root: Path, dirname: str, inputs: dict | None = None, outputs: dict | None = None,
             main: str | None = "", spec: dict | None = None) -> Path:
    """A fn dir root/<dirname>/ with fn.json named after it (`spec` overrides keys) and main.py
    (unless main is None)."""
    d = root / dirname
    d.mkdir(parents=True, exist_ok=True)
    spec = {"name": dirname, "inputs": inputs or {}, "outputs": outputs or {}, **(spec or {})}
    (d / "fn.json").write_text(json.dumps(spec))
    if main is not None:
        (d / "main.py").write_text(main)
    return d


def statuses(store: Store, project: str) -> dict[str, str]:
    return {k: v["status"] for k, v in store.read_state(project)["steps"].items()}


def settle(runner: Runner, store: Store, project: str, until=None,
           timeout: float = 30.0) -> dict:
    """Tick until `until(steps)` holds (default: every step finished). Returns state steps."""
    until = until or (lambda s: all(e["status"] in DONE for e in s.values()))
    deadline = time.time() + timeout
    while time.time() < deadline:
        runner.tick()
        steps = store.read_state(project)["steps"]
        if steps and until(steps):
            return steps
        time.sleep(0.05)
    raise AssertionError(f"timed out; statuses: {statuses(store, project)}")
