import json
import time
from pathlib import Path

import pytest

from sluice import lifecycle as L
from sluice.runner import Runner
from sluice.store import Store

TESTPACK = Path(__file__).parent / "testpack"


def write_config(home: Path, **extra) -> None:
    cfg = {"packs": [str(TESTPACK)], "slots": {"default": 8, "heavy": 1}, "tick": "1s", **extra}
    home.mkdir(parents=True, exist_ok=True)
    (home / "config.json").write_text(json.dumps(cfg))


@pytest.fixture
def home(tmp_path: Path) -> Path:
    h = tmp_path / "home"
    write_config(h)
    return h


@pytest.fixture
def store(home: Path) -> Store:
    return Store(home)


def _kill_all(store: Store) -> None:
    for pid in store.plan_ids():
        for e in store.read_state(pid)["nodes"].values():
            if e.get("status") == "running":
                L.kill_group(e.get("pid"), grace=0.2)


@pytest.fixture
def runner(store: Store):
    r = Runner(store)
    yield r
    _kill_all(store)
    for p in r.procs.values():
        p.wait(timeout=5)


def create(store: Store, pid: str, nodes: dict, **doc) -> int:
    return store.create(pid, {"title": pid, "nodes": nodes, **doc}, "test", "test")


def statuses(store: Store, pid: str) -> dict[str, str]:
    return {k: v["status"] for k, v in store.read_state(pid)["nodes"].items()}


def settle(runner: Runner, store: Store, pid: str, until=None, timeout: float = 30.0) -> dict:
    """Tick until `until(nodes)` holds (default: every node terminal). Returns state nodes."""
    def done(n):
        return all(e["status"] in L.TERMINAL for e in n.values())

    until = until or done
    deadline = time.time() + timeout
    while time.time() < deadline:
        runner.tick()
        nodes = store.read_state(pid)["nodes"]
        if nodes and until(nodes):
            return nodes
        time.sleep(0.05)
    raise AssertionError(f"timed out; statuses: {statuses(store, pid)}")


def output(store: Store, pid: str, nid: str):
    _, exp = store.expanded(pid)
    nodes = store.read_state(pid)["nodes"]
    return L.Values(store, pid, exp, nodes).output(nid)


def event_types(store: Store, pid: str, node: str | None = None) -> list[str]:
    return [e["type"] for e in store.events(pid) if node is None or e.get("node") == node]
