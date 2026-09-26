"""Shared fixtures for pack tests.

call_fn runs a fn's main.py through `uv run --script` exactly like the runner
(SPEC §4.1): JSON on stdin, SLUICE_* env, cwd = the run dir. fake_bin writes an
executable fake tool into a directory the test prepends to PATH.
"""

import json
import os
import subprocess
import tempfile
from pathlib import Path

import pytest

PACKS = Path(__file__).resolve().parent
SRC = PACKS.parents[1]  # the directory that contains the sluice package


def pytest_configure(config):
    config.addinivalue_line(
        "markers", "live: exercises the real external tools; needs SLUICE_LIVE=1")


@pytest.fixture
def call_fn(tmp_path):
    run_dirs = []

    def _call(fn_dir, inp, env=None, path=None):
        run_dir = Path(tempfile.mkdtemp(prefix="run-", dir=tmp_path))
        run_dirs.append(run_dir)
        e = {
            **os.environ,
            "PYTHONPATH": str(SRC),
            "SLUICE_HOME": str(tmp_path / "sluice-home"),
            "SLUICE_PLAN": "test-plan",
            "SLUICE_NODE": "test-node",
            "SLUICE_RUN_ID": "test-run",
            "SLUICE_RUN_DIR": str(run_dir),
            "SLUICE_ATTEMPT": "1",
            "SLUICE_IDEMPOTENCY_KEY": "test-plan/test-node/1",
            "SLUICE_FN_DIR": str(fn_dir),
        }
        if env:
            e.update({k: str(v) for k, v in env.items()})
        if path:
            dirs = [path] if isinstance(path, (str, Path)) else list(path)
            e["PATH"] = os.pathsep.join([*(str(d) for d in dirs), e["PATH"]])
        p = subprocess.run(
            ["uv", "run", "--quiet", "--script", str(Path(fn_dir) / "main.py")],
            input=json.dumps(inp),
            text=True,
            capture_output=True,
            env=e,
            cwd=run_dir,
            check=False,
        )
        try:
            out = json.loads(p.stdout)
        except json.JSONDecodeError:
            out = None
        return p.returncode, out, p.stderr

    _call.run_dirs = run_dirs
    return _call


@pytest.fixture
def fake_bin(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)

    def _make(name, body):
        f = bin_dir / name
        f.write_text(body)
        f.chmod(0o755)
        return bin_dir

    return _make
