"""Shared fixtures for pack tests.

call_fn runs a fn's main.py through `uv run --script` exactly like the runner
(SPEC §4.1): JSON on stdin, SLUICE_* env, cwd = the run dir. fake_bin writes an
executable fake tool into a directory the test prepends to PATH.
"""

import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
SRC = REPO / "src"


def pytest_configure(config):
    config.addinivalue_line(
        "markers", "live: exercises the real external tools; needs SLUICE_LIVE=1")


@pytest.fixture
def call_fn(tmp_path):
    run_dirs = []

    def _call(fn_dir, inp, env=None, path=None, watch=None):
        """Run the fn; returns (exit code, parsed stdout or None, stderr). With `watch`, the
        fn's stderr goes to stderr.log in its run dir (as under the runner) and
        watch(stderr so far) is called every 0.05 s while it runs."""
        run_dir = Path(tempfile.mkdtemp(prefix="run-", dir=tmp_path))
        run_dirs.append(run_dir)
        e = {
            **os.environ,
            "PYTHONPATH": str(SRC),
            "SLUICE_HOME": str(tmp_path / "sluice-home"),
            "SLUICE_PROJECT": "test-project",
            "SLUICE_STEP": "test-step",
            "SLUICE_RUN_ID": "test-run",
            "SLUICE_RUN_DIR": str(run_dir),
            "SLUICE_BACKOFF": "0",
            "SLUICE_FN_DIR": str(fn_dir),
        }
        if env:
            e.update({k: str(v) for k, v in env.items()})
        if path:
            dirs = [path] if isinstance(path, (str, Path)) else list(path)
            e["PATH"] = os.pathsep.join([*(str(d) for d in dirs), e["PATH"]])
        argv = ["uv", "run", "--quiet", "--script", str(Path(fn_dir) / "main.py")]
        if watch is None:
            p = subprocess.run(argv, input=json.dumps(inp), text=True, capture_output=True,
                               env=e, cwd=run_dir, check=False)
            code, stdout, stderr = p.returncode, p.stdout, p.stderr
        else:
            err_file = run_dir / "stderr.log"
            with open(err_file, "w") as err:
                p = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=err, text=True, env=e, cwd=run_dir)
                p.stdin.write(json.dumps(inp))
                p.stdin.close()
                while p.poll() is None:
                    watch(err_file.read_text())
                    time.sleep(0.05)
                code, stdout, stderr = p.returncode, p.stdout.read(), err_file.read_text()
        try:
            out = json.loads(stdout)
        except json.JSONDecodeError:
            out = None
        return code, out, stderr

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
