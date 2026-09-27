"""Tests for packs/agents.

External tools are faked with small sh scripts put on PATH (or pointed to via
the SLUICE_*_BIN overrides). Each fake records its argv NUL-separated so tests
can assert the exact invocation. decide.jev is exercised against a throwaway
HTTP server on 127.0.0.1 since its only seam is SLUICE_JEV_URL.
"""

import json
import os
import subprocess
import sys
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from sluice import log as L
from sluice.runner import Runner
from sluice.store import Store

AGENTS = Path(__file__).resolve().parents[1]

requires_live = pytest.mark.skipif(
    os.environ.get("SLUICE_LIVE") != "1", reason="set SLUICE_LIVE=1 to run live tests")


def read_argv(path):
    """Decode the NUL-separated argv a fake binary recorded."""
    return [a.decode() for a in path.read_bytes().split(b"\0") if a]


def seen_then_touch(marker, text):
    """A call_fn watch: create `marker` once `text` shows up in the fn's stderr."""
    def watch(stderr):
        if text in stderr:
            marker.touch()
    return watch


def init_repo(path):
    path.mkdir(parents=True, exist_ok=True)

    def g(*args, check=True):
        return subprocess.run(
            ["git", "-C", str(path), *args],
            check=check, capture_output=True, text=True).stdout.strip()

    g("init", "-b", "main")
    g("config", "user.email", "t@example.com")
    g("config", "user.name", "T")
    (path / "seed.txt").write_text("seed\n")
    g("add", ".")
    g("commit", "-m", "init")
    return SimpleNamespace(path=path, git=g)


def _progress_then_wait(wait_for, name):
    """Fake-harness lines: one on stdout, one appended to its log, then wait (up to 10 s,
    else exit 3) for `wait_for` to exist."""
    return (f"echo '{name}-harness: starting'\n"
            f"echo '{name}: step one' >> \"$log\"\n"
            f"i=0; while [ ! -e '{wait_for}' ]; do i=$((i+1)); "
            "[ $i -gt 200 ] && exit 3; sleep 0.05; done\n")


def make_devin(tmp_path, fake_bin, *, code=0, log_body="devin log output\n",
               final_body="devin finished\n", with_session=True, wait_for=None):
    argv_file = tmp_path / "devin.argv"
    spec_copy = tmp_path / "devin.spec.copy"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" > \"{argv_file}\"\n"
        'spec=""; log=""\n'
        "while [ $# -gt 0 ]; do\n"
        '  case "$1" in\n'
        '    --spec) spec="$2"; shift 2 ;;\n'
        '    --log) log="$2"; shift 2 ;;\n'
        "    *) shift ;;\n"
        "  esac\n"
        "done\n"
        f'cp "$spec" "{spec_copy}"\n'
    )
    if wait_for:
        script += _progress_then_wait(wait_for, "devin")
    script += f"printf '{log_body}' >> \"$log\"\n"
    if with_session:
        script += 'echo "sess-abc" > "$log.session"\n'
    if final_body is not None:
        script += f"printf '{final_body}' > \"$log.final\"\n"
    script += f"exit {code}\n"
    bin_dir = fake_bin("devin-harness-run", script)
    return bin_dir, argv_file, spec_copy


def test_devin_success(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file, spec_copy = make_devin(tmp_path, fake_bin)
    cwd = tmp_path / "work"
    cwd.mkdir()
    code, out, err = call_fn(
        AGENTS / "agent.devin",
        {"cwd": str(cwd), "spec": "do the thing"},
        path=bin_dir,
    )
    run_dir = call_fn.run_dirs[-1]
    assert code == 0, err
    assert out == {
        "log": str(run_dir / "devin.log"),
        "final": "devin finished\n",
        "report": None,
        "session": "sess-abc",
    }
    assert read_argv(argv_file) == [
        "--cd", str(cwd),
        "--spec", str(run_dir / "spec.md"),
        "--log", str(run_dir / "devin.log"),
    ]
    assert spec_copy.read_text().startswith("do the thing")


def test_devin_log_and_report_path(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file, _ = make_devin(tmp_path, fake_bin)
    log = tmp_path / "custom.log"
    report = tmp_path / "report.md"
    report.write_text("REPORT BODY")
    code, out, err = call_fn(
        AGENTS / "agent.devin",
        {"cwd": str(tmp_path), "spec": "s", "log": str(log),
         "report_path": str(report)},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"log": str(log), "final": "devin finished\n",
                   "report": "REPORT BODY", "session": "sess-abc"}
    argv = read_argv(argv_file)
    assert "--log" in argv
    assert argv[argv.index("--log") + 1] == str(log)


def test_devin_session(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file, _ = make_devin(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.devin",
        {"cwd": str(tmp_path), "spec": "s", "session": "sess-9"},
        path=bin_dir,
    )
    assert code == 0, err
    argv = read_argv(argv_file)
    assert argv[-2:] == ["--resume", "sess-9"]


def test_devin_transient(call_fn, fake_bin, tmp_path):
    bin_dir, _, _ = make_devin(
        tmp_path, fake_bin, code=1, log_body="failed: capacity issues\n")
    code, out, err = call_fn(
        AGENTS / "agent.devin", {"cwd": str(tmp_path), "spec": "s"}, path=bin_dir)
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up
    assert out is None


def test_devin_hard_failure(call_fn, fake_bin, tmp_path):
    bin_dir, _, _ = make_devin(
        tmp_path, fake_bin, code=2, log_body="a real bug happened\n")
    code, out, _err = call_fn(
        AGENTS / "agent.devin", {"cwd": str(tmp_path), "spec": "s"}, path=bin_dir)
    assert code == 1
    assert out is None


def test_devin_streams_the_harness_and_its_log_while_it_runs(call_fn, fake_bin, tmp_path):
    marker = tmp_path / "seen"
    bin_dir, _, _ = make_devin(tmp_path, fake_bin, wait_for=marker)
    code, out, err = call_fn(
        AGENTS / "agent.devin", {"cwd": str(tmp_path), "spec": "s"},
        path=bin_dir, watch=seen_then_touch(marker, "devin: step one"))
    assert code == 0, err
    assert out["final"] == "devin finished\n"
    assert out["log"] == str(call_fn.run_dirs[-1] / "devin.log")
    lines = err.splitlines()
    assert "devin-harness: starting" in lines
    assert lines.index("devin: step one") < lines.index("devin log output")


def make_codex(tmp_path, fake_bin, *, code=0, log_body="codex log output\n",
               big_log=False, wait_for=None):
    argv_file = tmp_path / "codex.argv"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" > \"{argv_file}\"\n"
        'log=""\n'
        "while [ $# -gt 0 ]; do\n"
        '  case "$1" in\n'
        '    --log) log="$2"; shift 2 ;;\n'
        '    --spec|--cd|--model|--effort|--resume) shift 2 ;;\n'
        "    *) shift ;;\n"
        "  esac\n"
        "done\n"
        'echo "sess-codex" > "$log.session"\n'
    )
    if wait_for:
        script += _progress_then_wait(wait_for, "codex")
    if big_log:
        script += (
            "head -c 4200 /dev/zero | tr '\\0' 'x' > \"$log\"\n"
            "printf 'ENDTAIL\\n' >> \"$log\"\n"
        )
    else:
        script += f"printf '{log_body}' >> \"$log\"\n"
    script += f"exit {code}\n"
    bin_dir = fake_bin("codex-harness-run", script)
    return bin_dir, argv_file


def test_codex_success(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_codex(tmp_path, fake_bin, big_log=True)
    code, out, err = call_fn(
        AGENTS / "agent.codex",
        {"cwd": str(tmp_path), "spec": "the spec", "model": "astra"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")},
    )
    run_dir = call_fn.run_dirs[-1]
    assert code == 0, err
    # no .final file: final is the last 4000 chars of the log
    assert out["log"] == str(run_dir / "codex.log")
    assert len(out["final"]) == 4000
    assert out["final"].endswith("ENDTAIL\n")
    assert out["report"] is None
    argv = read_argv(argv_file)
    assert argv[:4] == [
        "--cd", str(tmp_path), "--spec", str(run_dir / "spec.md")]
    assert argv[argv.index("--model") + 1] == "astra"
    assert argv[argv.index("--effort") + 1] == "high"


def codex_rollout(tmp_path, *messages):
    """A codex sessions dir holding the rollout of session sess-codex: (timestamp, text) of
    each task_complete, among other records."""
    d = tmp_path / "sessions" / "2026" / "09" / "27"
    d.mkdir(parents=True)
    recs = [{"timestamp": "2000-01-01T00:00:00Z", "type": "session_meta", "payload": {}}]
    for at, text in messages:
        recs.append({"timestamp": at, "type": "response_item",
                     "payload": {"type": "message", "role": "assistant",
                                 "content": [{"type": "output_text", "text": "diff hunks"}]}})
        recs.append({"timestamp": at, "type": "event_msg",
                     "payload": {"type": "task_complete", "last_agent_message": text}})
    (d / "rollout-2026-09-27T10-00-00-sess-codex.jsonl").write_text(
        "".join(json.dumps(r) + "\n" for r in recs) + "{not json\n")
    return tmp_path / "sessions"


@pytest.mark.parametrize("fn", ["agent.codex", "agent.run"])
def test_codex_final_is_the_agents_last_message(call_fn, fake_bin, tmp_path, fn):
    bin_dir, _ = make_codex(tmp_path, fake_bin, big_log=True)
    sessions = codex_rollout(tmp_path, ("2000-01-01T00:00:01Z", "an earlier turn's answer"),
                             ("2999-01-01T00:00:00Z", "Landed at abc123. All green."))
    inp = {"cwd": str(tmp_path), "spec": "the spec"}
    code, out, err = call_fn(AGENTS / fn, {**inp, "engine": "codex"} if fn == "agent.run"
                             else inp,
                             env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run"),
                                  "SLUICE_CODEX_SESSIONS": str(sessions)})
    assert code == 0, err
    assert out["final"] == "Landed at abc123. All green."  # not the log's tail


def test_codex_final_ignores_a_previous_turns_message(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_codex(tmp_path, fake_bin, big_log=True)
    sessions = codex_rollout(tmp_path, ("2000-01-01T00:00:01Z", "an earlier turn's answer"))
    code, out, err = call_fn(AGENTS / "agent.codex", {"cwd": str(tmp_path), "spec": "s"},
                             env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run"),
                                  "SLUICE_CODEX_SESSIONS": str(sessions)})
    assert code == 0, err
    assert out["final"].endswith("ENDTAIL\n") and len(out["final"]) == 4000  # the fallback


@pytest.mark.parametrize("fn", ["agent.codex", "agent.run"])
def test_codex_diffs_fold_to_one_line_in_the_echo(call_fn, fake_bin, tmp_path, fn):
    body = ("exec\\nls\\napply patch\\ndiff --git a/x b/x\\nindex 1..2\\n--- a/x\\n+++ b/x\\n"
            "@@ -1 +1 @@\\n-old\\n+new\\n\\ndiff --git a/y b/y\\n+more\\ncodex\\nDone.\\n")
    bin_dir, _ = make_codex(tmp_path, fake_bin, log_body=body)
    inp = {"cwd": str(tmp_path), "spec": "s"}
    code, _out, err = call_fn(AGENTS / fn, {**inp, "engine": "codex"} if fn == "agent.run"
                              else inp,
                              env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")})
    assert code == 0, err
    lines = err.splitlines()
    assert "(a diff of 2 files, 10 lines: in the log)" in lines
    assert "+new" not in lines and "diff --git a/y b/y" not in lines
    assert lines.index("apply patch") < lines.index("(a diff of 2 files, 10 lines: in the log)") \
        < lines.index("Done.")
    assert "+new" in (call_fn.run_dirs[-1] / "codex.log").read_text()  # the log keeps it


def test_codex_model_and_effort_defaults(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_codex(tmp_path, fake_bin)
    env = {"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")}
    for extra, model, effort in (({}, "sol", "high"), ({"model": "astra"}, "astra", "high"),
                                 ({"model": "astra", "effort": "max"}, "astra", "max")):
        code, _out, err = call_fn(AGENTS / "agent.codex",
                                  {"cwd": str(tmp_path), "spec": "s", **extra}, env=env)
        assert code == 0, err
        argv = read_argv(argv_file)
        assert (argv[argv.index("--model") + 1], argv[argv.index("--effort") + 1]) == \
            (model, effort)
    code, _, err = call_fn(AGENTS / "agent.codex",  # luna is gone: long work goes to Devin
                           {"cwd": str(tmp_path), "spec": "s", "model": "luna"}, env=env)
    assert code == 1 and "codex models are sol, astra, got 'luna'" in err


def test_codex_session(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_codex(tmp_path, fake_bin)
    code, out, err = call_fn(
        AGENTS / "agent.codex", {"cwd": str(tmp_path), "spec": "s", "session": "sess-1"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")},
    )
    assert code == 0, err
    assert read_argv(argv_file)[-2:] == ["--resume", "sess-1"]
    assert out["session"] == "sess-codex"


def test_codex_transient(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_codex(
        tmp_path, fake_bin, code=1, log_body="http 429: rate limit hit\n")
    code, out, err = call_fn(
        AGENTS / "agent.codex",
        {"cwd": str(tmp_path), "spec": "s"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")},
    )
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up
    assert out is None


def test_codex_streams_its_log_while_it_runs(call_fn, fake_bin, tmp_path):
    marker = tmp_path / "seen"
    bin_dir, _ = make_codex(tmp_path, fake_bin, wait_for=marker)
    code, out, err = call_fn(
        AGENTS / "agent.codex", {"cwd": str(tmp_path), "spec": "s"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")},
        watch=seen_then_touch(marker, "codex: step one"))
    assert code == 0, err
    assert out["final"] == "codex: step one\ncodex log output\n"
    lines = err.splitlines()
    assert "codex-harness: starting" in lines and "codex log output" in lines


def result_event(result="did it", session="s-1", cost=0.02, **extra):
    return {"type": "result", "subtype": "success", "is_error": False, "num_turns": 2,
            "result": result, "session_id": session, "total_cost_usd": cost, **extra}


def claude_stream(result_ev=None):
    """stream-json events of a short session: text, three tool calls (one fails), the end."""
    def assistant(*content):
        return {"type": "assistant", "parent_tool_use_id": None,
                "message": {"role": "assistant", "content": list(content)}}

    return [
        {"type": "system", "subtype": "init", "session_id": "s-1", "model": "fake-model"},
        {"type": "rate_limit_event", "rate_limit_info": {"status": "allowed_warning"}},
        assistant({"type": "thinking", "thinking": ""},
                  {"type": "text", "text": "Looking at\nthe repo first."}),
        assistant({"type": "tool_use", "name": "Bash",
                   "input": {"command": "git status " + "x" * 200, "description": "d"}}),
        {"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "content": "clean", "is_error": False}]}},
        assistant({"type": "tool_use", "name": "Read", "input": {"file_path": "/w/a.py"}}),
        {"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "content": [{"type": "text", "text": "no such file"}],
             "is_error": True}]}},
        result_ev or result_event(),
    ]


def make_claude(tmp_path, fake_bin, *, code=0, stdout_obj=None, events=None,
                stderr_text="", wait_for=None):
    """A fake claude: prints `stdout_obj` as one JSON object (--output-format json), or
    `events` one per line (stream-json). With `wait_for`, it stops after the fourth event
    until that file exists (exit 3 after 10 s)."""
    argv_file = tmp_path / "claude.argv"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" > \"{argv_file}\"\n"
    )
    if stderr_text:
        script += f"printf '{stderr_text}' >&2\n"
    if stdout_obj is not None:
        script += f"printf '%s' '{json.dumps(stdout_obj)}'\n"
    for i, ev in enumerate(events or []):
        script += f"printf '%s\\n' '{json.dumps(ev)}'\nsleep 0.02\n"
        if wait_for and i == 3:
            script += (f"i=0; while [ ! -e '{wait_for}' ]; do i=$((i+1)); "
                       "[ $i -gt 200 ] && exit 3; sleep 0.05; done\n")
    script += f"exit {code}\n"
    bin_dir = fake_bin("claude", script)
    return bin_dir, argv_file


def test_claude_success(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(tmp_path, fake_bin, events=claude_stream())
    code, out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(tmp_path), "prompt": "do the thing"},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"result": "did it", "session": "s-1", "cost_usd": 0.02}
    argv = read_argv(argv_file)
    assert argv[0] == "-p"
    assert argv[1].startswith("do the thing")
    assert argv[2:] == [
        "--model", "opus",
        "--output-format", "stream-json",
        "--verbose",
        "--dangerously-skip-permissions",
    ]


def test_claude_streams_progress_while_it_runs(call_fn, fake_bin, tmp_path):
    """Each event becomes a short stderr line as it arrives: the fake stops mid-session until
    the test has seen the tool call line in the fn's stderr."""
    marker = tmp_path / "seen"
    bin_dir, _ = make_claude(tmp_path, fake_bin, events=claude_stream(), wait_for=marker)
    code, out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
        path=bin_dir, watch=seen_then_touch(marker, "tool Bash git status"))
    assert code == 0, err
    assert out == {"result": "did it", "session": "s-1", "cost_usd": 0.02}
    lines = err.splitlines()
    assert "session s-1 model fake-model" in lines
    assert "Looking at the repo first." in lines
    assert "tool Bash git status " + "x" * 89 in lines  # the command, cut to 100 chars
    assert "tool Read /w/a.py" in lines
    assert "tool error no such file" in lines
    assert "done: 2 turns, $0.0200" in lines
    assert "rate limit" not in err and "clean" not in lines


def test_claude_always_runs_opus_and_resumes_a_session(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(
        tmp_path, fake_bin, events=[result_event("r", "s-9", None)])
    code, out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(tmp_path), "prompt": "p", "session": "s-9"},
        path=bin_dir,
    )
    assert code == 0, err
    argv = read_argv(argv_file)
    assert argv[argv.index("--model") + 1] == "opus"
    assert argv[-2:] == ["--resume", "s-9"]
    assert out["cost_usd"] is None


def test_claude_refuses_a_model_input(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(tmp_path, fake_bin, events=[result_event("r", "s", None)])
    code, out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(tmp_path), "prompt": "p", "model": "sonnet"}, path=bin_dir)
    assert code == 1 and out is None
    assert "always runs Opus" in err and not argv_file.exists()


def test_run_claude_engine_refuses_a_model(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(tmp_path, fake_bin, events=[result_event("r", "s", None)])
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s", "model": "sonnet"},
        path=bin_dir)
    assert code == 1 and out is None
    assert "always runs Opus" in err and not argv_file.exists()


def test_claude_transient(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude(
        tmp_path, fake_bin, code=1, stderr_text="Error: API overloaded\\n")
    code, out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
        path=bin_dir)
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up
    assert out is None


def test_claude_transient_from_the_stream(call_fn, fake_bin, tmp_path):
    """An API error reported only in the result event is recognised too."""
    failed = result_event("API Error: 529 overloaded", subtype="error_during_execution",
                          is_error=True, api_error_status=529)
    bin_dir, _ = make_claude(tmp_path, fake_bin, code=1, events=[failed])
    code, out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"}, path=bin_dir)
    assert code == 1, err
    assert "transient (attempt 1)" in err
    assert "error error_during_execution: API Error: 529 overloaded" in err
    assert out is None


def test_claude_hard_failure(call_fn, fake_bin, tmp_path):
    """A rate-limit warning that still allowed the request does not make a failure
    transient."""
    bin_dir, _ = make_claude(
        tmp_path, fake_bin, code=1, stderr_text="Error: auth failed\\n",
        events=claude_stream()[:2])
    code, out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
        path=bin_dir)
    assert code == 1
    assert out is None
    assert "transient" not in err and "Error: auth failed" in err


def test_review(call_fn, fake_bin, tmp_path):
    repo = init_repo(tmp_path / "repo")
    (repo.path / "STANDARDS.md").write_text("be good\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "standards")
    repo.git("branch", "base")
    (repo.path / "f.txt").write_text("work\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "the work")
    before = repo.git("rev-parse", "HEAD")

    argv_file = tmp_path / "claude.argv"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" > \"{argv_file}\"\n"
        'echo "z" >> f.txt\n'
        "git add f.txt\n"
        'git -c user.email=r@e -c user.name=R commit -q -m "Fix f.txt"\n'
        f"printf '%s\\n' '{json.dumps(result_event('fixed it', 'rs', 0.01))}'\n"
    )
    bin_dir = fake_bin("claude", script)
    code, out, err = call_fn(
        AGENTS / "agent.review",
        {"cwd": str(repo.path), "base": "base",
         "standards": str(repo.path / "STANDARDS.md"), "notes": "be strict"},
        path=bin_dir,
    )
    assert code == 0, err
    assert out["commits"] == 1
    assert out["sha"] == repo.git("rev-parse", "HEAD") != before
    assert out["summary"] == "fixed it"
    argv = read_argv(argv_file)
    assert argv[0] == "-p"
    assert "git diff base...HEAD" in argv[1]
    assert str(repo.path / "STANDARDS.md") in argv[1]
    assert "be strict" in argv[1]
    assert "step-test-step" in argv[1]
    assert out["session"] == "rs" and "--resume" not in argv

    code, out, err = call_fn(
        AGENTS / "agent.review",
        {"cwd": str(repo.path), "base": "base",
         "standards": str(repo.path / "STANDARDS.md"), "session": "rs-0"},
        path=bin_dir,
    )
    assert code == 0, err
    assert read_argv(argv_file)[-2:] == ["--resume", "rs-0"]


def claude_decide(choice, p, structured=False):
    inner = {"choice": choice, "p": p}
    if structured:
        return {"result": json.dumps({"choice": "bogus", "p": 0.0}),
                "structured_output": inner, "session_id": "x"}
    return {"result": json.dumps(inner), "session_id": "x"}


def test_decide_llm_confident(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(
        tmp_path, fake_bin, stdout_obj=claude_decide("b", 0.9))
    code, out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "pick one", "options": ["a", "b"], "context": {"k": 1}},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"choice": "b", "p": 0.9, "confident": True}
    argv = read_argv(argv_file)
    assert argv[argv.index("--model") + 1] == "haiku"
    schema = json.loads(argv[argv.index("--json-schema") + 1])
    assert schema["properties"]["choice"]["enum"] == ["a", "b"]


def test_decide_llm_not_confident_and_threshold(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude(
        tmp_path, fake_bin, stdout_obj=claude_decide("a", 0.5))
    code, out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "q", "options": ["a", "b"]},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"choice": "a", "p": 0.5, "confident": False}
    code, out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "q", "options": ["a", "b"], "threshold": 0.4},
        path=bin_dir,
    )
    assert code == 0, err
    assert out["confident"] is True


def test_decide_llm_prefers_structured_output(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude(
        tmp_path, fake_bin, stdout_obj=claude_decide("b", 0.95, structured=True))
    code, out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "q", "options": ["a", "b"]},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"choice": "b", "p": 0.95, "confident": True}


def test_decide_llm_choice_not_in_options(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude(
        tmp_path, fake_bin, stdout_obj=claude_decide("zzz", 0.9))
    code, out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "q", "options": ["a", "b"]},
        path=bin_dir,
    )
    assert code == 1
    assert out is None
    assert "not in options" in err


def test_decide_llm_model_override(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(
        tmp_path, fake_bin, stdout_obj=claude_decide("a", 0.9))
    code, _out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "q", "options": ["a"]},
        env={"SLUICE_DECIDE_MODEL": "custom-model"},
        path=bin_dir,
    )
    assert code == 0, err
    argv = read_argv(argv_file)
    assert argv[argv.index("--model") + 1] == "custom-model"


def test_decide_llm_transient(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude(
        tmp_path, fake_bin, code=1, stderr_text="Error 529: overloaded\\n")
    code, _out, err = call_fn(
        AGENTS / "decide.llm", {"question": "q", "options": ["a"]}, path=bin_dir)
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up


@requires_live
@pytest.mark.live
def test_agent_devin_live(call_fn, tmp_path):
    repo = init_repo(tmp_path / "repo")
    code, _out, err = call_fn(
        AGENTS / "agent.devin",
        {"cwd": str(repo.path),
         "spec": "Create a file hello.txt containing exactly hi, then "
                 "git add it and commit it with the message 'add hello'. "
                 "Change nothing else."},
    )
    assert code == 0, err
    assert (repo.path / "hello.txt").exists()
    assert int(repo.git("rev-list", "--count", "HEAD")) >= 2


@requires_live
@pytest.mark.live
def test_agent_claude_live(call_fn, tmp_path):
    repo = init_repo(tmp_path / "repo")
    code, out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(repo.path),
         "prompt": "create hello.txt containing hi and commit it"},
    )
    assert code == 0, err
    assert (repo.path / "hello.txt").exists()
    assert int(repo.git("rev-list", "--count", "HEAD")) >= 2
    assert out["session"]


def test_run_devin(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file, _ = make_devin(tmp_path, fake_bin)
    cwd = tmp_path / "work"
    cwd.mkdir()
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "devin", "cwd": str(cwd), "spec": "do it"},
        path=bin_dir,
    )
    run_dir = call_fn.run_dirs[-1]
    assert code == 0, err
    assert out == {"final": "devin finished\n", "report": None,
                   "session": "sess-abc"}
    assert read_argv(argv_file) == [
        "--cd", str(cwd),
        "--spec", str(run_dir / "spec.md"),
        "--log", str(run_dir / "devin.log"),
    ]


def test_run_devin_no_session(call_fn, fake_bin, tmp_path):
    bin_dir, _, _ = make_devin(tmp_path, fake_bin, with_session=False)
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "devin", "cwd": str(tmp_path), "spec": "s"},
        path=bin_dir,
    )
    assert code == 0, err
    assert out["session"] == ""


def test_run_codex(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_codex(tmp_path, fake_bin)
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "codex", "cwd": str(tmp_path), "spec": "s", "model": "astra"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")},
    )
    run_dir = call_fn.run_dirs[-1]
    assert code == 0, err
    assert out == {"final": "codex log output\n", "report": None,
                   "session": "sess-codex"}
    argv = read_argv(argv_file)
    assert argv[:4] == [
        "--cd", str(tmp_path), "--spec", str(run_dir / "spec.md")]
    assert argv[argv.index("--model") + 1] == "astra"
    assert argv[argv.index("--effort") + 1] == "high"


def test_run_codex_refuses_another_model_and_effort_elsewhere(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_codex(tmp_path, fake_bin)
    env = {"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")}
    code, _, err = call_fn(AGENTS / "agent.run", {"engine": "codex", "cwd": str(tmp_path),
                                                  "spec": "s", "model": "gpt-4"}, env=env)
    assert code == 1 and "codex models are sol, astra, got 'gpt-4'" in err
    code, _, err = call_fn(AGENTS / "agent.run", {"engine": "devin", "cwd": str(tmp_path),
                                                  "spec": "s", "effort": "max"}, env=env)
    assert code == 1 and "effort is for the codex engine" in err
    assert not argv_file.exists()


def test_run_claude(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(
        tmp_path, fake_bin, events=claude_stream(result_event("done", "s-42", 0.01)))
    report = tmp_path / "rep.md"
    report.write_text("REP")
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "the prompt",
         "report_path": str(report)},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"final": "done", "report": "REP", "session": "s-42"}
    assert "tool Read /w/a.py" in err.splitlines()
    argv = read_argv(argv_file)
    assert argv[0] == "-p"
    assert argv[1].startswith("the prompt")
    assert argv[2:] == [
        "--model", "opus",
        "--output-format", "stream-json",
        "--verbose",
        "--dangerously-skip-permissions",
    ]


def test_run_session(call_fn, fake_bin, tmp_path):
    bin_dir, devin_argv, _ = make_devin(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "devin", "cwd": str(tmp_path), "spec": "s",
         "session": "sess-9"},
        path=bin_dir,
    )
    assert code == 0, err
    assert read_argv(devin_argv)[-2:] == ["--resume", "sess-9"]

    _, claude_argv = make_claude(
        tmp_path, fake_bin, events=[result_event("x", "sess-7")])
    code, _out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s",
         "session": "sess-7"},
        path=bin_dir,
    )
    assert code == 0, err
    assert read_argv(claude_argv)[-2:] == ["--resume", "sess-7"]


def test_run_transient_per_engine(call_fn, fake_bin, tmp_path):
    bin_dir, _, _ = make_devin(
        tmp_path, fake_bin, code=1, log_body="capacity issues\n")
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "devin", "cwd": str(tmp_path), "spec": "s"},
        path=bin_dir,
    )
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up
    assert out is None

    make_codex(tmp_path, fake_bin, code=1, log_body="429 rate limit\n")
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "codex", "cwd": str(tmp_path), "spec": "s"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")},
        path=bin_dir,
    )
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up
    assert out is None

    make_claude(tmp_path, fake_bin, code=1, stderr_text="529 overloaded\\n")
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s"},
        path=bin_dir,
    )
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up
    assert out is None


def spec_of(call_fn):
    """The spec.md the last fn call wrote into its run dir."""
    return (call_fn.run_dirs[-1] / "spec.md").read_text()


def test_step_thread_devin(call_fn, fake_bin, tmp_path):
    """Running as a plan step, the spec gains the step-thread section, reading from the
    project log's last seq when the fn started."""
    project = tmp_path / "sluice-home" / "projects" / "test-project"
    project.mkdir(parents=True)
    (project / "log.jsonl").write_text("".join(
        json.dumps({"seq": n, "at": "t", "kind": "step.status"}) + "\n" for n in range(1, 43)))
    bin_dir, _, spec_copy = make_devin(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.devin", {"cwd": str(tmp_path), "spec": "do it"},
        path=bin_dir)
    assert code == 0, err
    spec = spec_copy.read_text()
    assert spec.startswith("do it")
    assert "sluice thread `step-test-step` of project `test-project`" in spec
    assert '"threads": ["step-test-step"], "since_seq": 42}' in spec
    assert "<last>" not in spec
    assert '"project": "test-project"' in spec
    assert '"name": "thread.post"' in spec
    assert '"from": "test-step"' in spec
    assert '"to": "orchestrator"' in spec
    assert "sluice tool log_read" in spec
    assert "sluice tool fn_call" in spec


def test_step_thread_off(call_fn, fake_bin, tmp_path):
    """listen: false, and running outside a plan step, leave the spec alone."""
    bin_dir, _, spec_copy = make_devin(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.devin",
        {"cwd": str(tmp_path), "spec": "s", "listen": False}, path=bin_dir)
    assert code == 0, err
    assert spec_copy.read_text() == "s"

    code, _out, err = call_fn(
        AGENTS / "agent.devin", {"cwd": str(tmp_path), "spec": "s"},
        env={"SLUICE_STEP": ""}, path=bin_dir)
    assert code == 0, err
    assert spec_copy.read_text() == "s"

    code, _out, err = call_fn(
        AGENTS / "agent.devin", {"cwd": str(tmp_path), "spec": "s"},
        env={"SLUICE_PROJECT": ""}, path=bin_dir)
    assert code == 0, err
    assert spec_copy.read_text() == "s"


def test_step_thread_codex(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_codex(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.codex", {"cwd": str(tmp_path), "spec": "s"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")})
    assert code == 0, err
    assert "sluice thread `step-test-step`" in spec_of(call_fn)


def test_step_thread_claude(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(tmp_path, fake_bin, events=[result_event("r", "s")])
    code, _out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
        path=bin_dir)
    assert code == 0, err
    prompt = read_argv(argv_file)[1]
    assert "sluice thread `step-test-step`" in prompt
    assert '"since_seq": 0}' in prompt  # no log yet


def test_step_thread_run(call_fn, fake_bin, tmp_path):
    """agent.run appends the section once, whatever the engine."""
    bin_dir, argv_file = make_claude(tmp_path, fake_bin, events=[result_event("r", "s")])
    code, _out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s"},
        path=bin_dir)
    assert code == 0, err
    argv = read_argv(argv_file)
    assert "sluice thread `step-test-step`" in argv[argv.index("-p") + 1]

    make_devin(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "devin", "cwd": str(tmp_path), "spec": "s", "listen": False},
        path=bin_dir)
    assert code == 0, err
    assert spec_of(call_fn) == "s"


def test_step_thread_sanitizes_step(call_fn, fake_bin, tmp_path):
    bin_dir, _, spec_copy = make_devin(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.devin", {"cwd": str(tmp_path), "spec": "s"},
        env={"SLUICE_STEP": "Build.Mac OS"}, path=bin_dir)
    assert code == 0, err
    assert "`step-build-mac-os`" in spec_copy.read_text()


# ---- typed agent blocks: extra inputs, declared outputs, step_submit ----------------------

STEP_INPUTS = {"interface": {"type": "string"}, "branches": {"type": "string[]"}}
STEP_OUTPUTS = {"branch": {"type": "string", "doc": "The branch you pushed"},
                "report": {"type": {"type": "record", "fields": {"ok": "boolean"}}, "doc": ""}}
BLOCK_ENV = {"SLUICE_STEP_INPUTS": json.dumps(STEP_INPUTS),
             "SLUICE_STEP_OUTPUTS": json.dumps(STEP_OUTPUTS)}
BLOCK_INPUTS = {"interface": "docs/api.md\nsecond line", "branches": ["a", "b"]}


def prompt_of(name, call_fn, argv_file):
    """The task text an agent fn handed its CLI: spec.md for the harnesses, else claude's -p
    argument."""
    if name in ("agent.claude", "agent.review", "agent.run"):
        argv = read_argv(argv_file)
        return argv[argv.index("-p") + 1]
    return spec_of(call_fn)


def block_call(name, call_fn, fake_bin, tmp_path, inputs, env):
    """Run agent fn `name` against its fake with these extra inputs and env; return the text
    it handed its CLI."""
    base = {"agent.claude": {"prompt": "the task"}, "agent.codex": {"spec": "the task"},
            "agent.devin": {"spec": "the task"},
            "agent.run": {"engine": "claude", "spec": "the task"},
            "agent.review": {"base": "HEAD", "standards": "S.md"}}[name]
    cwd = tmp_path / "repo"
    if not cwd.exists():
        init_repo(cwd)
    if name == "agent.codex":
        bin_dir, argv_file = make_codex(tmp_path, fake_bin)
        env = {**env, "SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")}
    elif name == "agent.devin":
        bin_dir, argv_file, _ = make_devin(tmp_path, fake_bin)
    else:
        bin_dir, argv_file = make_claude(tmp_path, fake_bin, events=[result_event()])
    code, _out, err = call_fn(AGENTS / name, {"cwd": str(cwd), **base, **inputs}, env=env,
                              path=bin_dir)
    assert code == 0, err
    return prompt_of(name, call_fn, argv_file)


AGENT_FNS = ["agent.claude", "agent.codex", "agent.devin", "agent.run", "agent.review"]


@pytest.mark.parametrize("name", AGENT_FNS)
def test_a_block_is_told_its_inputs_and_the_outputs_to_submit(name, call_fn, fake_bin,
                                                               tmp_path):
    text = block_call(name, call_fn, fake_bin, tmp_path, BLOCK_INPUTS, BLOCK_ENV)
    inputs = text.index("## Inputs")
    outputs = text.index("## Outputs you must submit")
    thread = text.index("Messages for you arrive on sluice thread")
    assert inputs < outputs < thread
    assert ("`interface` (string):\ndocs/api.md\nsecond line\n\n"
            "`branches` (string[]):\n[\n  \"a\",\n  \"b\"\n]") in text
    assert "- `branch` (string): The branch you pushed\n" in text
    assert '- `report` ({"type": "record", "fields": {"ok": "boolean"}})\n' in text
    assert ("`sluice tool step_submit '{\"project\": \"test-project\", \"step\": \"test-step\", "
            "\"run\": \"test-run\", \"outputs\": {\"branch\": <string>, \"report\": "
            "<{\"type\": \"record\", \"fields\": {\"ok\": \"boolean\"}}>}}'`") in text
    assert "If it returns `invalid`, fix what it lists and submit again" in text


@pytest.mark.parametrize("name", AGENT_FNS)
def test_a_block_without_ports_gets_only_the_thread_note(name, call_fn, fake_bin, tmp_path):
    text = block_call(name, call_fn, fake_bin, tmp_path, {}, {})
    assert "## Inputs" not in text and "## Outputs" not in text and "step_submit" not in text
    assert "Messages for you arrive on sluice thread" in text


def test_sections_apply_one_at_a_time_and_listen_false_drops_only_the_note(
        call_fn, fake_bin, tmp_path):
    only_out = {"SLUICE_STEP_OUTPUTS": BLOCK_ENV["SLUICE_STEP_OUTPUTS"]}
    text = block_call("agent.devin", call_fn, fake_bin, tmp_path, {"listen": False}, only_out)
    assert text.startswith("the task\n\n## Outputs you must submit\n")
    assert "## Inputs" not in text and "sluice thread" not in text
    only_in = {"SLUICE_STEP_INPUTS": json.dumps({"n": {"type": "Any"}})}
    text = block_call("agent.devin", call_fn, fake_bin, tmp_path, {"n": {"k": 1}}, only_in)
    assert text.startswith('the task\n\n## Inputs\n\n`n` (Any):\n{\n  "k": 1\n}\n\n'
                           "Messages for you")


# ---- the same, end to end under the runner -----------------------------------------------

SUBMITTING_CLAUDE = '''#!{python}
"""A fake claude that does what the prompt says: fills the step_submit command's
placeholders and runs it through the sluice CLI, then reports a result."""
import json, re, sys
from sluice.cli import main

prompt = sys.argv[sys.argv.index("-p") + 1]
submit = {submit!r}
if submit:
    cmd = re.search(r"sluice tool step_submit '(.*?)'`", prompt).group(1)
    args = json.loads(re.sub(r"<[^>]+>", json.dumps(submit), cmd))
    code = main(["tool", "step_submit", json.dumps(args)])
    print(f"step_submit exited {{code}}", file=sys.stderr)
print(json.dumps({{"type": "result", "subtype": "success", "is_error": False, "num_turns": 1,
                  "result": "done", "session_id": "s-run", "total_cost_usd": 0.0}}))
'''


def run_plan(tmp_path, steps, submit):
    """Run `steps` (a plan using the agents pack) under a runner on a scratch home whose
    claude is the fake above; returns (store, state steps)."""
    home = tmp_path / "home"
    home.mkdir()
    (home / "config.json").write_text(json.dumps({"fn_dirs": [str(AGENTS)]}))
    fake = tmp_path / "claude-fake"
    fake.write_text(SUBMITTING_CLAUDE.format(python=sys.executable, submit=submit))
    fake.chmod(0o755)
    (home / ".env").write_text(f"SLUICE_CLAUDE_BIN={fake}\n")
    store = Store(home)
    store.create_project("p", "", "t", "t")
    store.patch("p", 1, [{"op": "replace", "path": "/steps", "value": steps}], "t", "t")
    runner = Runner(store)
    deadline = time.time() + 120
    while time.time() < deadline:
        runner.tick()
        st = store.read_state("p")["steps"]
        if st and all(e["status"] in ("succeeded", "failed") for e in st.values()):
            return store, st
        time.sleep(0.1)
    raise AssertionError(f"timed out: {store.read_state('p')['steps']}")


def claude_block(**extra):
    return {"run": "agent.claude", "in": {"cwd": {"default": "/tmp"},
                                          "prompt": {"default": "Pick a word."}, **extra}}


def test_an_agent_that_submits_hands_its_outputs_downstream(tmp_path):
    store, st = run_plan(tmp_path, {
        "a": {**claude_block(), "outputs": {"word": "string"}},
        "b": {**claude_block(word={"source": "a/word"}, session={"source": "a/session"}),
              "outputs": {"echo": "string"}},
    }, submit="blue")
    assert st["a"]["status"] == "succeeded", st["a"].get("error")
    assert st["a"]["outputs"] == {"result": "done", "session": "s-run", "cost_usd": 0.0,
                                  "word": "blue"}
    assert st["b"]["status"] == "succeeded", st["b"].get("error")
    assert st["b"]["outputs"]["echo"] == "blue"
    [run_b] = st["b"]["run_ids"]
    prompt = json.loads((store.runs_dir("p") / run_b / "input.json").read_text())
    assert prompt["word"] == "blue" and prompt["session"] == "s-run"
    subs = L.read(store.log_dir("p"), kinds=["step.submit"])["records"]
    assert [(r["step"], r["outputs"]) for r in subs] == [("a", {"word": "blue"}),
                                                         ("b", {"echo": "blue"})]


def test_an_agent_that_does_not_submit_fails_its_step(tmp_path):
    _, st = run_plan(tmp_path, {"a": {**claude_block(), "outputs": {"word": "string"}}},
                     submit=None)
    assert st["a"]["status"] == "failed"
    assert "declared outputs not submitted: word" in st["a"]["error"]
    assert "step_submit" in st["a"]["error"]
