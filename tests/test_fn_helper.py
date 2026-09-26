"""The stdlib helper that every fn's main.py runs under (SPEC §7)."""

import json
import os
import subprocess
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src"


def _run(tmp_path, body, stdin="{}"):
    script = tmp_path / "main.py"
    script.write_text("from sluice.fn import run, Transient\n" + body)
    env = {**os.environ, "PYTHONPATH": str(SRC), "SLUICE_RUN_DIR": str(tmp_path),
           "SLUICE_BACKOFF": "0"}
    p = subprocess.run([sys.executable, str(script)], input=stdin, env=env,
                       text=True, capture_output=True, check=False)
    return p.returncode, p.stdout, p.stderr


def test_output_is_the_only_thing_on_stdout(tmp_path):
    code, out, err = _run(tmp_path, "def main(inp, ctx):\n    print('noise')\n"
                                    "    return {'x': inp['a'] + 1}\nrun(main)\n", '{"a": 1}')
    assert code == 0, err
    assert json.loads(out) == {"x": 2}
    assert "noise" in err


def test_transient_is_retried_then_succeeds(tmp_path):
    code, out, err = _run(tmp_path, "def main(inp, ctx):\n"
                                    "    if ctx.attempt < 3:\n        raise Transient('busy')\n"
                                    "    return {'attempt': ctx.attempt}\n"
                                    "run(main, retries=2)\n")
    assert code == 0, err
    assert json.loads(out) == {"attempt": 3}


def test_transient_past_the_retry_budget_fails(tmp_path):
    code, out, err = _run(tmp_path, "def main(inp, ctx):\n    raise Transient('busy')\n"
                                    "run(main, retries=1)\n")
    assert code == 1
    assert out == ""
    assert "transient (attempt 1)" in err and "attempt 2" not in err.split("Traceback")[0]


def test_other_errors_fail_without_retry(tmp_path):
    code, _out, err = _run(tmp_path, "def main(inp, ctx):\n    raise ValueError('bad')\n"
                                    "run(main, retries=5)\n")
    assert code == 1
    assert "transient" not in err
    assert json.loads((tmp_path / "error.json").read_text())["type"] == "ValueError"
