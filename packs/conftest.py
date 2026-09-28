"""Shared fixtures for pack tests.

call_fn runs a fn's main.py through `uv run --script` exactly like the runner
(SPEC §4.1): JSON on stdin, the runner's fn_env, cwd = the run dir. fake_bin writes an
executable fake tool into a directory the test prepends to PATH.
"""

import json
import os
import subprocess
import tempfile
import time
from pathlib import Path

import pytest

from sluice import runner
from sluice.registry import parse_fn
from sluice.store import Store


def pytest_configure(config):
    config.addinivalue_line(
        "markers", "live: exercises the real external tools; needs SLUICE_LIVE=1")


@pytest.fixture
def call_fn(tmp_path):
    run_dirs = []
    store = Store(tmp_path / "sluice-home")
    store.create_project("test-project")  # what its step's messages and submission belong to

    def _call(fn_dir, inp, env=None, path=None, watch=None):
        """Run the fn; returns (exit code, parsed stdout or None, stderr). With `watch`, the
        fn's stderr goes to stderr.log in its run dir (as under the runner) and
        watch(stderr so far) is called every 0.05 s while it runs."""
        run_dir = Path(tempfile.mkdtemp(prefix="run-", dir=tmp_path))
        run_dirs.append(run_dir)
        fn, errs = parse_fn(json.loads((Path(fn_dir) / "fn.json").read_text()),
                            Path(fn_dir))
        assert fn is not None, errs
        e = runner.fn_env(store, "test-project", fn, "test-step", "test-run", run_dir)
        e["SLUICE_BACKOFF"] = "0"
        if env:
            e.update({k: str(v) for k, v in env.items()})
        if path:
            dirs = [path] if isinstance(path, (str, Path)) else list(path)
            # the fake tools are on the host's PATH as well (what child_env restores)
            e["PATH"] = e["SLUICE_HOST_PATH"] = \
                os.pathsep.join([*(str(d) for d in dirs), e["PATH"]])
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
