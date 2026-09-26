"""Tests for packs/agents.

External tools are faked with small sh scripts put on PATH (or pointed to via
the SLUICE_*_BIN overrides). Each fake records its argv NUL-separated so tests
can assert the exact invocation. decide.jev is exercised against a throwaway
HTTP server on 127.0.0.1 since its only seam is SLUICE_JEV_URL.
"""

import json
import os
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest

AGENTS = Path(__file__).resolve().parents[1]

requires_live = pytest.mark.skipif(
    os.environ.get("SLUICE_LIVE") != "1", reason="set SLUICE_LIVE=1 to run live tests")


def read_argv(path):
    """Decode the NUL-separated argv a fake binary recorded."""
    return [a.decode() for a in path.read_bytes().split(b"\0") if a]


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


def make_devin(tmp_path, fake_bin, *, code=0, log_body="devin log output\n",
               final_body="devin finished\n", with_session=True):
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
        f"printf '{log_body}' > \"$log\"\n"
    )
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
    }
    assert read_argv(argv_file) == [
        "--cd", str(cwd),
        "--spec", str(run_dir / "spec.md"),
        "--log", str(run_dir / "devin.log"),
    ]
    assert spec_copy.read_text() == "do the thing"


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
                   "report": "REPORT BODY"}
    argv = read_argv(argv_file)
    assert "--log" in argv
    assert argv[argv.index("--log") + 1] == str(log)


def test_devin_resume(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file, _ = make_devin(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.devin",
        {"cwd": str(tmp_path), "spec": "s", "resume": "sess-9"},
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


def make_codex(tmp_path, fake_bin, *, code=0, log_body="codex log output\n",
               big_log=False):
    argv_file = tmp_path / "codex.argv"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" > \"{argv_file}\"\n"
        'log=""\n'
        "while [ $# -gt 0 ]; do\n"
        '  case "$1" in\n'
        '    --log) log="$2"; shift 2 ;;\n'
        '    --spec|--cd|--model|--resume) shift 2 ;;\n'
        "    *) shift ;;\n"
        "  esac\n"
        "done\n"
        'echo "sess-codex" > "$log.session"\n'
    )
    if big_log:
        script += (
            "head -c 4200 /dev/zero | tr '\\0' 'x' > \"$log\"\n"
            "printf 'ENDTAIL\\n' >> \"$log\"\n"
        )
    else:
        script += f"printf '{log_body}' > \"$log\"\n"
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


def test_codex_no_model(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_codex(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.codex",
        {"cwd": str(tmp_path), "spec": "s"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")},
    )
    assert code == 0, err
    assert "--model" not in read_argv(argv_file)


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


def make_claude(tmp_path, fake_bin, *, code=0, stdout_obj=None, stderr_text=""):
    argv_file = tmp_path / "claude.argv"
    script = (
        "#!/bin/sh\n"
        f"printf '%s\\0' \"$@\" > \"{argv_file}\"\n"
    )
    if stderr_text:
        script += f"printf '{stderr_text}' >&2\n"
    if stdout_obj is not None:
        script += f"printf '%s' '{json.dumps(stdout_obj)}'\n"
    script += f"exit {code}\n"
    bin_dir = fake_bin("claude", script)
    return bin_dir, argv_file


def test_claude_success(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(
        tmp_path, fake_bin,
        stdout_obj={"result": "did it", "session_id": "s-1",
                    "total_cost_usd": 0.02})
    code, out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(tmp_path), "prompt": "do the thing"},
        path=bin_dir,
    )
    assert code == 0, err
    assert out == {"result": "did it", "session": "s-1", "cost_usd": 0.02}
    assert read_argv(argv_file) == [
        "-p", "do the thing",
        "--model", "opus",
        "--output-format", "json",
        "--dangerously-skip-permissions",
    ]


def test_claude_model_and_session(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(
        tmp_path, fake_bin,
        stdout_obj={"result": "r", "session_id": "s-9", "total_cost_usd": None})
    code, out, err = call_fn(
        AGENTS / "agent.claude",
        {"cwd": str(tmp_path), "prompt": "p", "model": "sonnet",
         "session": "s-9"},
        path=bin_dir,
    )
    assert code == 0, err
    argv = read_argv(argv_file)
    assert argv[argv.index("--model") + 1] == "sonnet"
    assert argv[-2:] == ["--resume", "s-9"]
    assert out["cost_usd"] is None


def test_claude_transient(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude(
        tmp_path, fake_bin, code=1, stderr_text="Error: API overloaded\\n")
    code, out, err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
        path=bin_dir)
    assert code == 1, err
    assert "transient (attempt 1)" in err  # the helper retried before giving up
    assert out is None


def test_claude_hard_failure(call_fn, fake_bin, tmp_path):
    bin_dir, _ = make_claude(
        tmp_path, fake_bin, code=1, stderr_text="Error: auth failed\\n")
    code, out, _err = call_fn(
        AGENTS / "agent.claude", {"cwd": str(tmp_path), "prompt": "p"},
        path=bin_dir)
    assert code == 1
    assert out is None


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
        "printf '%s' "
        '\'{"result":"fixed it","session_id":"rs","total_cost_usd":0.01}\'\n'
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
    assert out["session"] is None


def test_run_codex(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_codex(tmp_path, fake_bin)
    code, out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "codex", "cwd": str(tmp_path), "spec": "s", "model": "sol"},
        env={"SLUICE_CODEX_BIN": str(bin_dir / "codex-harness-run")},
    )
    run_dir = call_fn.run_dirs[-1]
    assert code == 0, err
    assert out == {"final": "codex log output\n", "report": None,
                   "session": "sess-codex"}
    argv = read_argv(argv_file)
    assert argv[:4] == [
        "--cd", str(tmp_path), "--spec", str(run_dir / "spec.md")]
    assert argv[argv.index("--model") + 1] == "sol"


def test_run_claude(call_fn, fake_bin, tmp_path):
    bin_dir, argv_file = make_claude(
        tmp_path, fake_bin,
        stdout_obj={"result": "done", "session_id": "s-42",
                    "total_cost_usd": 0.01})
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
    assert read_argv(argv_file) == [
        "-p", "the prompt",
        "--model", "opus",
        "--output-format", "json",
        "--dangerously-skip-permissions",
    ]


def test_run_resume(call_fn, fake_bin, tmp_path):
    bin_dir, devin_argv, _ = make_devin(tmp_path, fake_bin)
    code, _out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "devin", "cwd": str(tmp_path), "spec": "s",
         "resume": "sess-9"},
        path=bin_dir,
    )
    assert code == 0, err
    assert read_argv(devin_argv)[-2:] == ["--resume", "sess-9"]

    _, claude_argv = make_claude(
        tmp_path, fake_bin,
        stdout_obj={"result": "x", "session_id": "sess-7"})
    code, _out, err = call_fn(
        AGENTS / "agent.run",
        {"engine": "claude", "cwd": str(tmp_path), "spec": "s",
         "resume": "sess-7"},
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
