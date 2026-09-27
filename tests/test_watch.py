"""`sluice watch` (SPEC §10): follow a log, one JSON line per matching record."""

import io
import json
import os
import subprocess
import sys
import threading
import time

from sluice.watch import follow
from tests.conftest import create, settle


def add(a, b=1):
    return {"run": "test.add", "in": {"a": {"default": a}, "b": {"default": b}}}


def message(thread, body):
    return {"kind": "message", "thread": thread, "from": "t", "body": body}


def test_follow_prints_matching_records_from_now_or_since_a_seq(store):
    store.create_project("p")
    store.append("p", message("q", "before"))
    out = io.StringIO()
    rounds = iter(range(3))

    def stop():  # three polls, appending between them
        i = next(rounds, None)
        if i == 1:
            store.append("p", message("q", "after"), message("r", "other thread"),
                         {"kind": "step.status", "step": "s", "from": None, "to": "pending"})
        return i is None

    follow(store.project_dir("p"), out, kinds=["message"], threads=["q"], interval=0, stop=stop)
    assert [json.loads(x)["body"] for x in out.getvalue().splitlines()] == ["after"]

    out = io.StringIO()
    rounds = iter(range(1))
    follow(store.project_dir("p"), out, threads=["q"], since_seq=0, interval=0,
           stop=lambda: next(rounds, None) is None)
    assert [json.loads(x)["body"] for x in out.getvalue().splitlines()] == ["before", "after"]


def test_follow_holds_notes_until_a_question_when_waking_on_questions(store):
    store.create_project("p")
    out = io.StringIO()
    rounds = iter(range(4))
    seen = []

    def stop():  # a note, then a question: nothing is printed until the question
        i = next(rounds, None)
        seen.append(out.getvalue().count("\n"))
        if i == 0:
            store.append("p", {**message("q", "fyi"), "needs_reply": False})
        if i == 2:
            store.append("p", {**message("q", "which db?"), "needs_reply": True})
        return i is None

    follow(store.project_dir("p"), out, threads=["q"], since_seq=1, interval=0, stop=stop,
           wake="questions")
    assert seen[:3] == [0, 0, 0]
    assert [json.loads(x)["body"] for x in out.getvalue().splitlines()] == ["fyi", "which db?"]


def test_sluice_watch_streams_messages_and_step_changes(store, runner, home):
    create(store, "p", {"a": add(1), "b": {"run": "test.add",
                                           "in": {"a": {"source": "a/sum"}, "b": {"default": 1}}}})
    proc = subprocess.Popen(
        [sys.executable, "-m", "sluice.cli", "watch", "-p", "p", "--kinds",
         "step.status,message", "--threads", "questions"],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        env={**os.environ, "SLUICE_HOME": str(home)})
    lines: list[dict] = []

    def reader():
        for line in proc.stdout:
            lines.append(json.loads(line))

    t = threading.Thread(target=reader, daemon=True)
    t.start()
    try:
        time.sleep(1.5)  # it starts from the end of the log: nothing so far
        assert lines == [] and proc.poll() is None
        settle(runner, store, "p")
        store.append("p", message("questions", "ready?"), message("elsewhere", "skip me"))
        deadline = time.time() + 10
        while time.time() < deadline and not any(x["kind"] == "message" for x in lines):
            time.sleep(0.1)
        time.sleep(1)
        assert proc.poll() is None  # never exits by itself
    finally:
        proc.kill()
        proc.wait(10)
    steps = [(x["step"], x["to"]) for x in lines if x["kind"] == "step.status"]
    assert ("a", "succeeded") in steps and ("b", "succeeded") in steps
    assert steps.index(("a", "succeeded")) < steps.index(("b", "succeeded"))
    assert [x["body"] for x in lines if x["kind"] == "message"] == ["ready?"]
    assert {x["kind"] for x in lines} == {"step.status", "message"}
    seqs = [x["seq"] for x in lines]
    assert seqs == sorted(set(seqs))


def test_sluice_watch_refuses_an_unknown_project_or_kind(home):
    env = {**os.environ, "SLUICE_HOME": str(home)}
    p = subprocess.run([sys.executable, "-m", "sluice.cli", "watch", "-p", "zz"],
                       capture_output=True, text=True, env=env, timeout=30, check=False)
    assert p.returncode == 1 and json.loads(p.stderr)["error"] == "not_found"
    p = subprocess.run([sys.executable, "-m", "sluice.cli", "watch", "--kinds", "nope"],
                       capture_output=True, text=True, env=env, timeout=30, check=False)
    assert p.returncode == 1 and "unknown kind 'nope'" in json.loads(p.stderr)["message"]
