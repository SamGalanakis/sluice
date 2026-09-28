"""Tests for packs/agents.

External tools are faked with small sh scripts put on PATH (or pointed to via
the SLUICE_*_BIN overrides). Each fake records its argv NUL-separated so tests
can assert the exact invocation. decide.jev is exercised against a throwaway
HTTP server on 127.0.0.1 since its only seam is SLUICE_JEV_URL.
"""

import json
import os
import re
import signal
import subprocess
import sys
import time
from pathlib import Path
from types import SimpleNamespace

import pytest

from sluice import log as L
from sluice import runner
from sluice.registry import parse_fn
from sluice.runner import Runner
from sluice.store import Store

AGENTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(AGENTS))

from _agents.native.tmux import descendants

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


FAKE_CLAUDE = Path(__file__).with_name("fake_claude.py")


def make_claude(tmp_path, turns=(), **cfg):
    """A fake interactive claude (fake_claude.py) that plays `turns`. Returns the env a call
    needs and a recorder: `argv()` (its last argv) and `prompts()` (every message it got, the
    final /exit left out)."""
    d = tmp_path / "fake-claude"
    d.mkdir(exist_ok=True)
    wrapper = d / "claude"
    wrapper.write_text(f'#!/bin/sh\nexec {sys.executable} {FAKE_CLAUDE} "$@"\n')
    wrapper.chmod(0o755)
    for f in ("argv.json", "prompts.jsonl", "config.json.cursor"):
        (d / f).unlink(missing_ok=True)
    (d / "config.json").write_text(json.dumps({
        "argv": str(d / "argv.json"), "prompts": str(d / "prompts.jsonl"),
        "turns": list(turns), **cfg}))
    env = {"SLUICE_CLAUDE_BIN": str(wrapper), "FAKE_CLAUDE": str(d / "config.json"),
           "CLAUDE_CONFIG_DIR": str(tmp_path / "claude-config"), "SLUICE_AGENT_SETTLE_S": "0.3",
           "SLUICE_AGENT_POLL_S": "0.05", "SLUICE_AGENT_NUDGES": "2"}

    def raw_prompts():
        f = d / "prompts.jsonl"
        got = [json.loads(ln).rstrip("\n") for ln in f.read_text().splitlines()] \
            if f.exists() else []
        return [p for p in got if p != "/exit"]

    def prompts():  # a pointer to a file stands for the file's text
        return [Path(m.group(1)).read_text()
                if (m := re.fullmatch(r"(?:Your task|A message .*?) is in (\S+?);.*", p))
                else p for p in raw_prompts()]

    return env, SimpleNamespace(argv=lambda: json.loads((d / "argv.json").read_text()),
                                argv_file=d / "argv.json", prompts=prompts,
                                raw_prompts=raw_prompts)


def session_of(call_fn):
    """The session the last call's run recorded."""
    return json.loads((call_fn.run_dirs[-1] / "native.json").read_text())["session"]


OUT_ENV = {"SLUICE_STEP_OUTPUTS": json.dumps({"word": {"type": "string"}})}


def test_claude_success(call_fn, tmp_path):
    env, rec = make_claude(tmp_path, [{"reply": "did it"}])
    code, out, err = call_fn(AGENTS / "agent.claude",
                             {"cwd": str(tmp_path), "prompt": "do the thing"}, env=env)
    assert code == 0, err
    run_dir = call_fn.run_dirs[-1]
    assert out == {"result": "did it", "session": session_of(call_fn), "cost_usd": 0.02}
    assert out["session"]
    assert rec.argv() == ["--model", "opus", "--dangerously-skip-permissions",
                          "--settings", str(run_dir / "claude-settings.json")]
    settings = json.loads((run_dir / "claude-settings.json").read_text())
    assert sorted(settings) == ["hooks"]
    assert sorted(settings["hooks"]) == ["SessionStart", "Stop", "StopFailure",
                                         "UserPromptSubmit"]
    assert rec.prompts()[0].startswith("do the thing")
    assert f"attach: cd {run_dir} && tmux -S tmux.sock attach" in err
    assert not (run_dir / "tmux.sock").exists()


def test_the_prompt_goes_into_the_session_never_on_argv(call_fn, fake_bin, tmp_path):
    """stderr.log and `ps` carry no prompt: it is pasted into the session."""
    env, rec = make_claude(tmp_path)
    code, _out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(tmp_path), "prompt": "the-secret-prompt"}, env=env)
    assert code == 0, err
    assert "the-secret-prompt" not in rec.argv() and "the-secret-prompt" not in err
    assert rec.prompts()[0].startswith("the-secret-prompt")

    bin_dir, argv_file = make_claude_print(tmp_path, fake_bin,
                                           stdout_obj=claude_decide("a", 0.9))
    code, _out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "the-secret-prompt", "options": ["a", "b"]},
        path=bin_dir,
    )
    assert code == 0, err
    assert "the-secret-prompt" not in read_argv(argv_file)
    echoed = next(l for l in err.splitlines() if l.startswith("$ "))
    assert "the-secret-prompt" not in echoed
    assert "the-secret-prompt" in claude_stdin(tmp_path)


def test_claude_streams_progress(call_fn, tmp_path):
    env, _ = make_claude(tmp_path, [{
        "tool": {"name": "Bash", "input": {"command": "git status " + "x" * 200,
                                           "description": "d"}},
        "tool_error": [{"type": "text", "text": "no such file"}],
        "reply": "Looking at\nthe repo first."}])
    code, _out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                              env=env)
    assert code == 0, err
    lines = err.splitlines()
    assert "tool Bash git status " + "x" * 89 in lines  # the command, cut to 100 chars
    assert "tool error no such file" in lines
    assert "Looking at the repo first." in lines


def test_claude_always_runs_opus_and_resumes_a_session(call_fn, tmp_path):
    env, rec = make_claude(tmp_path, [{"reply": "r"}])
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env=env)
    assert code == 0, err
    first = out["session"]
    env, rec = make_claude(tmp_path, [{"reply": "again"}])
    code, out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p2", "session": first},
        env=env)
    assert code == 0, err
    argv = rec.argv()
    assert argv[argv.index("--model") + 1] == "opus"
    assert argv[-2:] == ["--resume", first]
    assert out["session"] == first and out["result"] == "again"


def test_resume_from_another_directory_is_refused(call_fn, tmp_path):
    (tmp_path / "a").mkdir()
    (tmp_path / "b").mkdir()
    env, rec = make_claude(tmp_path)
    code, out, err = call_fn(AGENTS / "agent.claude",
                             {"cwd": str(tmp_path / "a"), "prompt": "p"}, env=env)
    assert code == 0, err
    env, rec = make_claude(tmp_path)
    code, _out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(tmp_path / "b"), "prompt": "p", "session": out["session"]}, env=env)
    assert code == 1
    assert f"was started in {tmp_path / 'a'}, not {tmp_path / 'b'}" in err
    assert "cannot resume a session from another directory" in err
    assert not rec.argv_file.exists()


def test_claude_refuses_a_model_input(call_fn, tmp_path):
    env, rec = make_claude(tmp_path)
    code, out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(tmp_path), "prompt": "p", "model": "sonnet"}, env=env)
    assert code == 1 and out is None
    assert "always runs Opus" in err and not rec.argv_file.exists()


def test_run_claude_engine_refuses_a_model(call_fn, tmp_path):
    env, rec = make_claude(tmp_path)
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s", "model": "sonnet"}, env=env)
    assert code == 1 and out is None
    assert "always runs Opus" in err and not rec.argv_file.exists()


def test_a_transient_error_is_retried_in_the_same_session(call_fn, tmp_path):
    env, rec = make_claude(tmp_path, [{"error": "API Error: 529 overloaded"},
                                      {"reply": "recovered"}])
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env=env)
    assert code == 0, err
    assert "transient (attempt 1)" in err
    assert out["result"] == "recovered"
    assert rec.argv()[-2:] == ["--resume", out["session"]]
    assert rec.prompts()[-1].startswith("Your session was interrupted by a rate limit")


def test_claude_transient_until_the_retries_run_out(call_fn, tmp_path):
    env, _ = make_claude(tmp_path, [{"error": "rate_limit: usage limit reached"}] * 5)
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env=env)
    assert code == 1 and out is None
    assert "transient (attempt 3)" in err
    assert "error rate_limit: usage limit reached" in err  # the progress line


def test_claude_hard_failure(call_fn, tmp_path):
    env, _ = make_claude(tmp_path, exit_at_start=1, stderr="Error: auth failed")
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env=env)
    assert code == 1 and out is None
    assert "transient" not in err and "Error: auth failed" in err


def test_the_workspace_trust_dialog_is_answered(call_fn, tmp_path):
    env, _ = make_claude(tmp_path, [{"reply": "trusted"}], trust=True)
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env=env)
    assert code == 0, err
    assert out["result"] == "trusted"


def test_a_turn_without_the_declared_outputs_is_nudged_then_fails(call_fn, tmp_path):
    env, rec = make_claude(tmp_path, [{"reply": "done, I think"}] * 3)
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env={**env, **OUT_ENV})
    assert code == 1 and out is None
    assert "without submitting word (nudged 2 times)" in err
    assert "Its last message: done, I think" in err
    prompts = rec.prompts()
    assert len(prompts) == 3
    assert prompts[1].startswith("Your turn ended but these outputs are not submitted: word.")


def test_a_nudged_agent_that_submits_succeeds(call_fn, tmp_path):
    env, rec = make_claude(tmp_path, [{"reply": "done"}, {"submit": {"word": "w"}}])
    code, _out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env={**env, **OUT_ENV})
    assert code == 0, err
    assert "nudge 1/2: not submitted: word" in err and len(rec.prompts()) == 2


def test_claude_waits_for_its_background_shell_without_nudging(call_fn, tmp_path):
    """The incident: the model starts a background build and ends its turn; the session waits
    for the task notification, whose turn submits."""
    env, rec = make_claude(tmp_path, [{"reply": "building", "background_s": 1.5},
                                      {"submit": {"word": "built"}, "reply": "built"}])
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env={**env, **OUT_ENV})
    assert code == 0, err
    assert "waiting: a background shell is running" in err
    assert "task notification: sleep finished" in err
    assert len(rec.prompts()) == 1 and out["result"] == "built"


def test_claude_waits_for_its_wakeup_without_nudging(call_fn, tmp_path):
    env, rec = make_claude(tmp_path, [{"reply": "scheduled", "wakeup_s": 1.5},
                                      {"submit": {"word": "awake"}, "reply": "awake"}])
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env={**env, **OUT_ENV})
    assert code == 0, err
    assert "waiting: a wakeup at " in err and "wakeup: Claude resuming /loop wakeup" in err
    assert len(rec.prompts()) == 1 and out["result"] == "awake"


def test_without_declared_outputs_background_work_is_waited_for(call_fn, tmp_path):
    env, _ = make_claude(tmp_path, [{"reply": "started", "background_s": 1.0},
                                      {"reply": "finished"}])
    code, out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                             env=env)
    assert code == 0, err
    assert out["result"] == "finished"


def test_the_task_is_handed_over_as_a_file(call_fn, tmp_path):
    """With the step's notes it is many lines, which the TUI would collapse into a paste the
    model does not take as a request: one typed line points at task.md instead."""
    env, rec = make_claude(tmp_path)
    code, _out, err = call_fn(AGENTS / "agent.claude",
                              {"cwd": str(tmp_path), "prompt": "the task"}, env=env)
    assert code == 0, err
    task_md = call_fn.run_dirs[-1] / "task.md"
    assert rec.raw_prompts() == [f"Your task is in {task_md}; read it fully, then do it."]
    assert task_md.read_text().startswith("the task\n\nMessages for you")


def test_a_thread_message_reaches_the_live_session(call_fn, tmp_path):
    env, rec = make_claude(tmp_path, [{"reply": "working", "busy_s": 2.0}, {"reply": "noted"}])
    project = tmp_path / "sluice-home" / "projects" / "test-project"
    project.mkdir(parents=True)

    def post(stderr):
        if "task delivered" in stderr and not (project / "log.jsonl").exists():
            L.append(project, [{"kind": "message", "thread": "step-test-step",
                                "from": "orchestrator", "body": "please also do X"}])

    code, _out, err = call_fn(AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
                              env=env, watch=post)
    assert code == 0, err
    assert rec.raw_prompts()[1] == ("Message from orchestrator on your sluice thread "
                                    "`step-test-step`: please also do X")
    assert "thread message from orchestrator typed into the session" in err


def test_cancel_leaves_no_session_behind(tmp_path):
    """SIGTERM to the fn's process group (step_cancel) ends the tmux server, claude and the
    background shell claude started."""
    env, _ = make_claude(tmp_path, [{"reply": "building", "background_s": 60}])
    fn, _errs = parse_fn(json.loads((AGENTS / "agent.claude" / "fn.json").read_text()),
                         AGENTS / "agent.claude")
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    e = runner.fn_env(Store(tmp_path / "home"), "p", fn, "s", "r", run_dir)
    e.update(env)
    err = run_dir / "stderr.log"
    with open(err, "w") as f:
        p = subprocess.Popen(["uv", "run", "--quiet", "--script",
                              str(AGENTS / "agent.claude" / "main.py")],
                             stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=f,
                             env=e, cwd=run_dir, text=True, start_new_session=True)
        p.stdin.write(json.dumps({"cwd": str(tmp_path), "prompt": "p"}))
        p.stdin.close()
        deadline = time.time() + 30
        while time.time() < deadline and "waiting:" not in err.read_text():
            time.sleep(0.05)
        assert "waiting: a background shell is running" in err.read_text()
        pids = descendants(_server_pid(run_dir))
        assert len(pids) >= 2  # claude and its sleep
        os.killpg(p.pid, signal.SIGTERM)
        assert p.wait(timeout=10) != 0
    assert not any(Path(f"/proc/{pid}").exists() and _running(pid) for pid in pids)
    assert not (run_dir / "tmux.sock").exists()


def _server_pid(run_dir):
    return int(subprocess.run(["tmux", "-S", "tmux.sock", "display-message", "-p", "#{pid}"],
                              cwd=run_dir, capture_output=True, text=True,
                              check=True).stdout)


def _running(pid):
    try:
        return Path(f"/proc/{pid}/stat").read_text().split(") ")[1][0] != "Z"
    except OSError:
        return False


def make_claude_print(tmp_path, fake_bin, *, code=0, stdout_obj=None, stderr_text=""):
    """A fake `claude -p` for decide.llm: reads its prompt on stdin into claude.stdin, prints
    `stdout_obj` as one JSON object (--output-format json)."""
    argv_file = tmp_path / "claude.argv"
    stdin_file = tmp_path / "claude.stdin"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" > \"{argv_file}\"\n"
        f"cat > \"{stdin_file}\"\n"
    )
    if stderr_text:
        script += f"printf '{stderr_text}' >&2\n"
    if stdout_obj is not None:
        script += f"printf '%s' '{json.dumps(stdout_obj)}'\n"
    script += f"exit {code}\n"
    bin_dir = fake_bin("claude", script)
    return bin_dir, argv_file


def claude_stdin(tmp_path):
    """The prompt the fake `claude -p` got on stdin (kept off argv so stderr.log and ps stay
    free of it)."""
    return (tmp_path / "claude.stdin").read_text()


def test_review(call_fn, tmp_path):
    repo = init_repo(tmp_path / "repo")
    (repo.path / "STANDARDS.md").write_text("be good\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "standards")
    repo.git("branch", "base")
    (repo.path / "f.txt").write_text("work\n")
    repo.git("add", ".")
    repo.git("commit", "-m", "the work")
    before = repo.git("rev-parse", "HEAD")

    fix = ('echo z >> f.txt && git add f.txt && '
           'git -c user.email=r@e -c user.name=R commit -q -m "Fix f.txt"')
    env, rec = make_claude(tmp_path, [{"run": fix, "reply": "fixed it"}])
    code, out, err = call_fn(
        AGENTS / "agent.review",
        {"cwd": str(repo.path), "base": "base",
         "standards": str(repo.path / "STANDARDS.md"), "notes": "be strict"},
        env=env,
    )
    assert code == 0, err
    assert out["commits"] == 1
    assert out["sha"] == repo.git("rev-parse", "HEAD") != before
    assert out["summary"] == "fixed it"
    prompt = rec.prompts()[0]
    assert "git diff base...HEAD" in prompt
    assert str(repo.path / "STANDARDS.md") in prompt
    assert "be strict" in prompt
    assert "step-test-step" in prompt
    assert out["session"] and "--resume" not in rec.argv()

    first = out["session"]
    env, rec = make_claude(tmp_path)
    code, out, err = call_fn(
        AGENTS / "agent.review",
        {"cwd": str(repo.path), "base": "base",
         "standards": str(repo.path / "STANDARDS.md"), "session": first},
        env=env,
    )
    assert code == 0, err
    assert rec.argv()[-2:] == ["--resume", first]


def claude_decide(choice, p, structured=False):
    inner = {"choice": choice, "p": p}
    if structured:
        return {"result": json.dumps({"choice": "bogus", "p": 0.0}),
                "structured_output": inner, "session_id": "x"}
    return {"result": json.dumps(inner), "session_id": "x"}


def test_decide_llm_confident(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude_print(
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
    bin_dir, _ = make_claude_print(
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
    # 0.0 is a bound threshold, not "no threshold"
    code, out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "q", "options": ["a", "b"], "threshold": 0.0},
        path=bin_dir,
    )
    assert code == 0, err
    assert out["confident"] is True


def test_decide_llm_prefers_structured_output(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude_print(
        tmp_path, fake_bin, stdout_obj=claude_decide("b", 0.95, structured=True))
    code, out, err = call_fn(
        AGENTS / "decide.llm",
        {"question": "q", "options": ["a", "b"]},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"choice": "b", "p": 0.95, "confident": True}


def test_decide_llm_choice_not_in_options(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude_print(
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
    bin_dir, argv_file = make_claude_print(
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
    bin_dir, _ = make_claude_print(
        tmp_path, fake_bin, code=1, stderr_text="Error 529: overloaded\\n")
    code, _out, err = call_fn(
        AGENTS / "decide.llm", {"question": "q", "options": ["a"]}, path=bin_dir)
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up


def test_decide_llm_transient_rate_limit(call_fn, fake_bin, tmp_path):
    """claude's own rate_limit error string retries too."""
    bin_dir, _ = make_claude_print(
        tmp_path, fake_bin, code=1, stderr_text="API Error: rate_limit\\n")
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


INCIDENT = (
    "In your working directory, start the shell command `sleep 90 && echo built > out.txt` "
    "as a background Bash task (run_in_background). Do not wait for it in the foreground: end "
    "your turn and let its completion notification bring you back, or schedule a wakeup. Once "
    "out.txt exists, submit its content (without the newline) as `word`.")


@requires_live
@pytest.mark.live
def test_claude_live_declared_output_incident_and_resume(tmp_path):
    """On the real claude, under a runner: a step that must submit an output; the incident
    (a background build the model waits for across turns, then submits from); a second step
    that resumes the incident step's session in the same directory."""
    home = tmp_path / "home"
    home.mkdir()
    (home / "config.json").write_text(json.dumps({"fn_dirs": [str(AGENTS)]}))
    work = tmp_path / "work"
    work.mkdir()
    store = Store(home)
    store.create_project("p", "", "t", "t")

    def claude_step(prompt, outputs, **extra):
        return {"run": "agent.claude", "outputs": outputs,
                "in": {"cwd": {"default": str(work)}, "prompt": {"default": prompt}, **extra}}

    steps = {
        "pick": claude_step("Submit the word blue as `word`. Nothing else to do.",
                            {"word": "string"}),
        "build": claude_step(INCIDENT, {"word": "string"}),
        "again": claude_step("Submit, as `again`, the word you submitted in your previous "
                             "turn. Nothing else to do.", {"again": "string"},
                             session={"source": "build/session"}),
    }
    store.patch("p", 1, [{"op": "replace", "path": "/steps", "value": steps}], "t", "t")
    runner = Runner(store)
    deadline = time.time() + 1200
    while time.time() < deadline:
        runner.tick()
        st = store.read_state("p")["steps"]
        if len(st) == 3 and all(e["status"] in ("succeeded", "failed") for e in st.values()):
            break
        time.sleep(0.5)
    st = store.read_state("p")["steps"]

    def stderr(step):
        return (store.runs_dir("p") / st[step]["run_ids"][-1] / "stderr.log").read_text()

    for step in steps:
        assert st[step]["status"] == "succeeded", (step, st[step].get("error"), stderr(step))
    assert st["pick"]["outputs"]["word"] == "blue"
    assert st["build"]["outputs"]["word"] == "built"
    assert (work / "out.txt").read_text().strip() == "built"
    assert "nudge" not in stderr("build") and "waiting: " in stderr("build")
    assert st["again"]["outputs"]["session"] == st["build"]["outputs"]["session"]
    assert st["again"]["outputs"]["again"] == "built"
    for step in steps:
        run_dir = store.runs_dir("p") / st[step]["run_ids"][-1]
        assert not (run_dir / "tmux.sock").exists()


@requires_live
@pytest.mark.live
def test_claude_live_thread_message_reaches_the_session(tmp_path):
    """A message posted on the step's thread while it runs is typed into the live session;
    the agent acts on it without polling."""
    home = tmp_path / "home"
    home.mkdir()
    (home / "config.json").write_text(json.dumps({"fn_dirs": [str(AGENTS)]}))
    work = tmp_path / "work"
    work.mkdir()
    store = Store(home)
    store.create_project("p", "", "t", "t")
    prompt = ("The orchestrator will send you, as a message in this session, the word to "
              "submit as `word`. Do not poll or check anything; wait for the message, then "
              "submit the word it names.")
    store.patch("p", 1, [{"op": "replace", "path": "/steps", "value": {"ask": {
        "run": "agent.claude", "outputs": {"word": "string"},
        "in": {"cwd": {"default": str(work)}, "prompt": {"default": prompt}}}}}], "t", "t")
    runner = Runner(store)
    posted = False
    deadline = time.time() + 400
    while time.time() < deadline:
        runner.tick()
        st = store.read_state("p")["steps"]
        e = st.get("ask", {})
        if e.get("status") in ("succeeded", "failed"):
            break
        runs = list(store.runs_dir("p").glob("*/stderr.log"))
        if not posted and runs and "task delivered" in runs[0].read_text():
            L.append_locked(store.log_dir("p"), [{
                "kind": "message", "thread": "step-ask", "from": "orchestrator", "to": "ask",
                "body": "The word is: heron"}])
            posted = True
        time.sleep(0.5)
    err = (store.runs_dir("p") / e["run_ids"][-1] / "stderr.log").read_text()
    assert e["status"] == "succeeded", (e.get("error"), err)
    assert e["outputs"]["word"] == "heron"
    assert "thread message from orchestrator typed into the session" in err


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


def test_run_claude(call_fn, tmp_path):
    env, rec = make_claude(tmp_path, [{"tool": {"name": "Read", "input": {"file_path": "/w/a.py"}},
                                       "reply": "done"}])
    report = tmp_path / "rep.md"
    report.write_text("REP")
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "the prompt",
         "report_path": str(report)},
        env=env,
    )
    assert code == 0, err
    assert out == {"final": "done", "report": "REP", "session": session_of(call_fn)}
    assert "tool Read /w/a.py" in err.splitlines()
    assert rec.prompts()[0].startswith("the prompt")
    assert rec.argv()[:3] == ["--model", "opus", "--dangerously-skip-permissions"]


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

    env, _ = make_claude(tmp_path)
    code, out, err = call_fn(
        AGENTS / "agent.run", {"engine": "claude", "cwd": str(tmp_path), "spec": "s"}, env=env)
    assert code == 0, err
    env, rec = make_claude(tmp_path)
    code, _out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s",
         "session": out["session"]},
        env=env,
    )
    assert code == 0, err
    assert rec.argv()[-2:] == ["--resume", out["session"]]


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

    env, _ = make_claude(tmp_path, [{"error": "529 overloaded"}] * 5)
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s"},
        env=env,
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


def test_step_thread_claude(call_fn, tmp_path):
    """A live session's thread messages are pasted in, so its note says so instead of asking
    the agent to poll log_read; asking back works as before."""
    env, rec = make_claude(tmp_path)
    code, _out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"}, env=env)
    assert code == 0, err
    prompt = rec.prompts()[0]
    assert ("Messages for you on sluice thread `step-test-step` of project `test-project` "
            "are pasted into this session as they arrive; you need not poll") in prompt
    assert "log_read" not in prompt
    assert '"name": "thread.post"' in prompt and '"from": "test-step"' in prompt
    assert '"to": "orchestrator"' in prompt and "sluice tool fn_call" in prompt

    env, rec = make_claude(tmp_path)
    code, _out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p", "listen": False},
        env=env)
    assert code == 0, err
    assert rec.raw_prompts()[0] == "p"  # one short line: typed as it is


def test_step_thread_run(call_fn, fake_bin, tmp_path):
    """agent.run appends the section once, whatever the engine."""
    env, rec = make_claude(tmp_path)
    code, _out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s"}, env=env)
    assert code == 0, err
    assert rec.prompts()[0].count("sluice thread `step-test-step`") == 1

    bin_dir, _, _ = make_devin(tmp_path, fake_bin)
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


def prompt_of(name, call_fn, rec):
    """The task text an agent fn handed its CLI: spec.md for the harnesses, else the first
    message pasted into the claude session."""
    if name in ("agent.claude", "agent.review", "agent.run"):
        return rec.prompts()[0]
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
    bin_dir = None
    if name == "agent.codex":
        bin_dir, rec = make_codex(tmp_path, fake_bin)
        env = {**env, "SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")}
    elif name == "agent.devin":
        bin_dir, rec, _ = make_devin(tmp_path, fake_bin)
    else:  # its session must submit the declared outputs to finish
        claude_env, rec = make_claude(tmp_path, [{"submit": {"branch": "b",
                                                             "report": {"ok": True}}}])
        env = {**env, **claude_env}
    code, _out, err = call_fn(AGENTS / name, {"cwd": str(cwd), **base, **inputs}, env=env,
                              path=bin_dir)
    assert code == 0, err
    return prompt_of(name, call_fn, rec)


AGENT_FNS = ["agent.claude", "agent.codex", "agent.devin", "agent.run", "agent.review"]


@pytest.mark.parametrize("name", AGENT_FNS)
def test_a_block_is_told_its_inputs_and_the_outputs_to_submit(name, call_fn, fake_bin,
                                                               tmp_path):
    text = block_call(name, call_fn, fake_bin, tmp_path, BLOCK_INPUTS, BLOCK_ENV)
    inputs = text.index("## Inputs")
    outputs = text.index("## Outputs you must submit")
    thread = text.index("Messages for you")
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
    assert "Messages for you" in text


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

def run_plan(tmp_path, steps, submit):
    """Run `steps` (a plan using the agents pack) under a runner on a scratch home whose
    claude is the interactive fake, submitting `submit` for every declared output through the
    step_submit command in its task (nothing when None); returns (store, state steps)."""
    home = tmp_path / "home"
    home.mkdir()
    (home / "config.json").write_text(json.dumps({"fn_dirs": [str(AGENTS)]}))
    turn = {"reply": "done"} | ({"submit_cli": submit} if submit is not None else {})
    env, _ = make_claude(tmp_path, [turn] * len(steps), cost=0.0)
    (home / ".env").write_text("".join(f"{k}={v}\n" for k, v in env.items()))
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
    session = st["a"]["outputs"]["session"]
    assert st["a"]["outputs"] == {"result": "done", "session": session, "cost_usd": 0.0,
                                  "word": "blue"}
    assert st["b"]["status"] == "succeeded", st["b"].get("error")
    assert st["b"]["outputs"]["echo"] == "blue"
    [run_b] = st["b"]["run_ids"]
    prompt = json.loads((store.runs_dir("p") / run_b / "input.json").read_text())
    assert prompt["word"] == "blue" and prompt["session"] == session
    assert st["b"]["outputs"]["session"] == session
    subs = L.read(store.log_dir("p"), kinds=["step.submit"])["records"]
    assert [(r["step"], r["outputs"]) for r in subs] == [("a", {"word": "blue"}),
                                                         ("b", {"echo": "blue"})]


def test_an_agent_that_does_not_submit_fails_its_step(tmp_path):
    _, st = run_plan(tmp_path, {"a": {**claude_block(), "outputs": {"word": "string"}}},
                     submit=None)
    assert st["a"]["status"] == "failed"
    assert "the agent ended its turn without submitting word (nudged 2 times)" \
        in st["a"]["error"]
