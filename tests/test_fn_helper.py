"""The stdlib helper that every fn's main.py runs under (SPEC §7)."""

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from sluice.fn import ShError, sh_stream

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


def _waits_for(marker, before="", after=""):
    """A shell command: print `before`, wait (up to 10 s) for `marker` to exist, print
    `after`; exits 3 if the marker never came."""
    script = (f"{before}; i=0; while [ ! -e '{marker}' ]; do i=$((i+1)); "
              f"[ $i -gt 200 ] && exit 3; sleep 0.05; done; {after}")
    return ["sh", "-c", script]


def test_sh_stream_passes_each_line_on_as_it_arrives(tmp_path):
    marker = tmp_path / "seen"
    seen = []

    def on_line(line, source):
        seen.append((source, line))
        marker.touch()  # the command goes on only once its first line got here

    p = sh_stream(_waits_for(marker, "echo one; echo err >&2", "echo two"), on_line)
    assert p.returncode == 0
    assert p.stdout == "one\ntwo\n" and p.stderr == "err\n"
    assert ("stdout", "one") in seen and ("stdout", "two") in seen and ("stderr", "err") in seen


def test_sh_stream_follows_a_log_file_and_raises_on_failure(tmp_path):
    log, marker = tmp_path / "tool.log", tmp_path / "seen"
    log.write_text("old line\n")  # there before: not passed on
    seen = []

    def on_line(line, source):
        seen.append((source, line))
        if line == "first":
            marker.touch()

    cmd = _waits_for(marker, f"echo first > '{log}'",
                     f"printf 'second\\nno newline' >> '{log}'; echo bad >&2; exit 4")
    with pytest.raises(ShError) as e:
        sh_stream(cmd, on_line, follow=log)
    assert e.value.code == 4 and e.value.stderr == "bad\n"
    follow = [line for source, line in seen if source == "follow"]
    assert follow == ["first", "second", "no newline"]


def test_sh_stream_feeds_input_to_the_commands_stdin(tmp_path, capfd):
    """input= keeps a prompt off argv and feeds it on stdin instead."""
    out_file = tmp_path / "stdin.txt"
    p = sh_stream(["sh", "-c", f"cat > '{out_file}'"], input="the-secret-prompt\n")
    assert p.returncode == 0
    assert out_file.read_text() == "the-secret-prompt\n"
    assert "the-secret-prompt" not in capfd.readouterr().err  # the echoed argv carries none


def test_sh_stream_echoes_one_trimmed_line_per_line_by_default(tmp_path, capfd):
    p = sh_stream(["sh", "-c", "printf 'a  b\\tc\\n'; head -c 500 /dev/zero | tr '\\0' x"],
                  check=False)
    assert p.returncode == 0
    err = capfd.readouterr().err.splitlines()
    assert err[1:] == ["a b c", "x" * 200]


def test_child_env_drops_the_fns_own_environment(monkeypatch, tmp_path):
    import sys

    from sluice.fn import child_env
    venv = str(sys.prefix)
    monkeypatch.setenv("VIRTUAL_ENV", venv)
    monkeypatch.setenv("PATH", f"{venv}/bin:/usr/bin")
    for k in ("PATH", "PYTHONPATH", "VIRTUAL_ENV"):
        monkeypatch.delenv(f"SLUICE_HOST_{k}", raising=False)
    env = child_env({"X": "1"})
    assert "VIRTUAL_ENV" not in env and env["PATH"] == "/usr/bin" and env["X"] == "1"
    monkeypatch.setenv("SLUICE_HOST_PATH", "/host/bin")
    monkeypatch.setenv("SLUICE_HOST_PYTHONPATH", "")
    monkeypatch.setenv("SLUICE_HOST_VIRTUAL_ENV", "/host/venv")
    monkeypatch.setenv("PYTHONPATH", "/sluice/src")
    env = child_env()
    assert (env["PATH"], env["VIRTUAL_ENV"]) == ("/host/bin", "/host/venv")
    assert "PYTHONPATH" not in env


def test_with_step_notes(tmp_path):
    """An open fn's step text gains its extra inputs, the outputs to submit and the
    step-thread note; listen=False drops only the note, no step drops it all."""
    from sluice.fn import Context, with_step_notes
    ctx = Context(project="p", step="Build.X", run_id="r1", run_dir=tmp_path,
                  home=tmp_path, fn_dir=tmp_path,
                  extra_inputs={"n": {"type": "int"}},
                  outputs={"branch": {"type": "string", "doc": "The pushed branch"}})
    text = with_step_notes("do it", {"n": 3}, ctx, None)
    inputs = text.index("## Inputs")
    outputs = text.index("## Outputs you must submit")
    thread = text.index("Messages for you arrive on sluice thread")
    assert inputs < outputs < thread
    assert "`n` (int):\n3" in text
    assert "- `branch` (string): The pushed branch" in text
    assert ('"project": "p", "step": "Build.X", "run": "r1", '
            '"outputs": {"branch": <string>}') in text
    assert "step-build-x" in text and '"since_seq": 0' in text
    without = with_step_notes("do it", {"n": 3}, ctx, False)
    assert "sluice thread" not in without and "## Inputs" in without
    quiet = Context(project="", step="", run_id="", run_dir=tmp_path, home=tmp_path,
                    fn_dir=tmp_path)
    assert with_step_notes("do it", {"n": 3}, quiet, None) == "do it"
