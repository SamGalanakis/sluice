"""`sluice me` and the `step_context` tool (SPEC §9, §10): the answer to "where do I
stand" for the agent inside a step — no pasted notes."""

import json
import sys

from mcp import Client

from sluice.cli import main
from sluice.mcp_server import build_server
from tests.conftest import add, create, d, settle, src


def test_me_inside_a_real_step(store, runner, monkeypatch):
    """A test fn runs `sluice me` as its step's agent would: the env the runner sets
    identifies the step, and the output has everything the agent needs to go on."""
    monkeypatch.setenv("TEST_PYTHON", sys.executable)
    create(store, "p", {
        "a": {"run": "test.brief", "in": {}},
        "b": {"run": "test.me", "in": {"seen": src("a/summary")},
              "outputs": {"report": "string", "scratch": "string?"},
              "doc": "Check me"}})
    store.set_output("p", "a", {"summary": "wrote the plan", "final": "x" * 500,
                                "report": "runs/a/report.md"}, "t", "t")
    store.append("p",
                 {"kind": "message", "thread": "step-b", "from": "b",
                  "to": "orchestrator", "body": "part 1 done, proceed?",
                  "needs_reply": True},
                 {"kind": "message", "thread": "step-b", "from": "orchestrator",
                  "body": "yes", "needs_reply": False},
                 {"kind": "message", "thread": "step-b", "from": "orchestrator",
                  "to": "b", "body": "skip the Windows build", "needs_reply": True})
    steps = settle(runner, store, "p")
    out = steps["b"]["outputs"]
    assert out["code"] == 0, out["err"]
    text, rid = out["me"], steps["b"]["run_ids"][0]
    assert text.startswith("step b (test.me) — running") and f"run {rid}" in text
    assert "doc: Check me" in text and 'seen="wrote the plan"' in text
    upstream = next(l for l in text.splitlines() if l.startswith("upstream a"))
    assert "upstream a (test.brief): succeeded" in upstream
    assert '"wrote the plan"' in upstream and "runs/a/report.md" in upstream
    assert "x" * 500 not in text  # `final` stays out once a summary is shown
    assert "message orchestrator: skip the Windows build" in text
    assert "part 1 done" not in text  # answered
    submit = next(l for l in text.splitlines() if "step_submit" in l)
    assert f'"run": "{rid}"' in submit
    assert submit.index('"report"') < submit.index('"scratch"')  # required first
    assert "scratch" in next(l for l in text.splitlines() if l.startswith("submit:"))
    assert "thread step-b" in text and "thread.post" in text


def test_me_outside_a_step_says_so_and_exits_1(home, monkeypatch, capsys):
    monkeypatch.setenv("SLUICE_HOME", str(home))
    for var in ("SLUICE_PROJECT", "SLUICE_STEP", "SLUICE_RUN_ID"):
        monkeypatch.delenv(var, raising=False)
    assert main(["me"]) == 1
    assert "--project" in capsys.readouterr().err


def test_me_with_flags_works_outside_a_steps_env(store, monkeypatch, capsys):
    create(store, "p", {"a": add(d(1), d(2), doc="adds"),
                        "o": {"run": "test.open", "in": {},
                              "outputs": {"word": "string"}}})
    for var in ("SLUICE_PROJECT", "SLUICE_STEP", "SLUICE_RUN_ID"):
        monkeypatch.delenv(var, raising=False)
    monkeypatch.setenv("SLUICE_HOME", str(store.home))
    assert main(["me", "--project", "p", "--step", "a"]) == 0
    out = capsys.readouterr().out
    assert out.startswith("step a (test.add) — pending") and "doc: adds" in out
    assert main(["me", "--project", "p", "--step", "o"]) == 0
    out = capsys.readouterr().out
    assert '"run": "<run>"' in out  # no run yet: the command shows the placeholder


async def test_the_step_context_tools_shape(store):
    create(store, "p", {"a": add(d(1), d(2), doc="adds")})
    async with Client(build_server(store)) as c:
        r = await c.call_tool("step_context", {"project": "p", "step": "a"})
        assert not r.is_error
        ctx = json.loads(r.content[0].text)
        assert sorted(ctx) == ["ask", "doc", "elapsed", "finished", "fn", "inputs",
                               "messages", "project", "run", "started", "status", "step",
                               "submit", "thread", "upstream"]
        assert ctx["step"] == "a" and ctx["fn"] == "test.add" and ctx["status"] == "pending"
        assert ctx["thread"] == "step-a"
        r = await c.call_tool("step_context", {"project": "p", "step": "zz"})
        assert r.is_error and json.loads(r.content[0].text)["error"] == "not_found"
