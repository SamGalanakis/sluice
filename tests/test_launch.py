import json
import subprocess
import sys

from sluice.util import atomic_write_json


def launch(run_dir, argv, stdin=None, env=None):
    atomic_write_json(run_dir / "cmd.json", {"argv": argv, "env": env or {}, "cwd": str(run_dir)})
    if stdin is not None:
        atomic_write_json(run_dir / "input.json", stdin)
    p = subprocess.run([sys.executable, "-m", "sluice.launch", str(run_dir)], check=False,
                       timeout=30)
    assert p.returncode == 0
    return json.loads((run_dir / "exit.json").read_text())["code"]


def test_launcher_wires_stdin_stdout_stderr_env_and_cwd(tmp_path):
    script = ("import json, os, sys; d = json.load(sys.stdin); "
              "print(json.dumps({'x': d['x'], 'env': os.environ['SLUICE_X'], "
              "'cwd': os.getcwd()})); print('log', file=sys.stderr); sys.exit(3)")
    code = launch(tmp_path, [sys.executable, "-c", script], {"x": 5}, {"SLUICE_X": "y"})
    assert code == 3
    assert json.loads((tmp_path / "stdout.log").read_text()) == {"x": 5, "env": "y",
                                                                 "cwd": str(tmp_path)}
    assert (tmp_path / "stderr.log").read_text() == "log\n"


def test_launcher_reports_a_missing_program_and_signals(tmp_path):
    (tmp_path / "a").mkdir()
    assert launch(tmp_path / "a", ["/nonexistent/prog"]) == 127
    assert "could not start" in (tmp_path / "a" / "stderr.log").read_text()
    (tmp_path / "b").mkdir()
    killed = launch(tmp_path / "b", [sys.executable, "-c", "import os; os.kill(os.getpid(), 9)"])
    assert killed == 128 + 9
