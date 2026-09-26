import json
import time
from pathlib import Path

import pytest

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
def runner(store: Store):
    r = Runner(store)
    yield r
    for a in r.active.values():
        a.kill()


def create(store: Store, pid: str, steps: dict, **doc) -> int:
    return store.create(pid, {"label": pid, "steps": steps, **doc}, "test", "test")


def statuses(store: Store, pid: str) -> dict[str, str]:
    return {k: v["status"] for k, v in store.read_state(pid)["steps"].items()}


def settle(runner: Runner, store: Store, pid: str, until=None, timeout: float = 30.0) -> dict:
    """Tick until `until(steps)` holds (default: every step finished). Returns state steps."""
    until = until or (lambda s: all(e["status"] in DONE for e in s.values()))
    deadline = time.time() + timeout
    while time.time() < deadline:
        runner.tick()
        steps = store.read_state(pid)["steps"]
        if steps and until(steps):
            return steps
        time.sleep(0.05)
    raise AssertionError(f"timed out; statuses: {statuses(store, pid)}")
