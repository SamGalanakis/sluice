"""thread.post / thread.wait (SPEC §10): messages are `message` records in the project's log."""

import json
import os
import subprocess
import sys
import time
from pathlib import Path

from sluice import calls
from sluice import log as L
from sluice.registry import BUILTIN_DIR
from sluice.runner import run_call_direct

SRC = str(Path(__file__).resolve().parents[1] / "src")


def env_for(store, project):
    run_dir = store.home.parent / "run"  # the fn writes output.json/error.json here
    run_dir.mkdir(exist_ok=True)
    return {**os.environ, "SLUICE_HOME": str(store.home), "SLUICE_PROJECT": project,
            "PYTHONPATH": SRC, "SLUICE_RUN_DIR": str(run_dir)}


def run_fn(store, name, inp, project="p", timeout=60):
    """The fn's main.py as the runner runs it (uv, JSON on stdin)."""
    p = subprocess.run(["uv", "run", "--quiet", "--script", str(BUILTIN_DIR / name / "main.py")],
                       input=json.dumps(inp), text=True, capture_output=True,
                       env=env_for(store, project), cwd=store.home.parent, timeout=timeout,
                       check=False)
    return p.returncode, json.loads(p.stdout) if p.returncode == 0 else None, p.stderr


def post(store, project, **inp):
    call = calls.create(store, "thread.post", inp, project, direct=True)
    res = run_call_direct(store, call, project)
    assert res["status"] == "succeeded", res
    return res["outputs"]["seq"]


def test_posts_append_messages_in_seq_order(store):
    store.create_project("p")
    s1 = post(store, "p", thread="questions", body="which db?", **{"from": "worker"})
    s2 = post(store, "p", thread="questions", body="postgres", to="worker",
              data={"why": "ops"}, **{"from": "lead"})
    s3 = post(store, "p", thread="other", body="unrelated", **{"from": "x"})
    assert s1 < s2 < s3
    msgs = L.read(store.home, "p", threads=["questions"])["records"]
    assert [(m["seq"], m["from"], m["body"]) for m in msgs] == [
        (s1, "worker", "which db?"), (s2, "lead", "postgres")]
    assert msgs[0]["kind"] == "message" and "to" not in msgs[0] and "data" not in msgs[0]
    assert (msgs[1]["to"], msgs[1]["data"]) == ("worker", {"why": "ops"})


def test_a_post_says_whether_it_needs_a_reply(store):
    store.create_project("p")
    post(store, "p", thread="step-a", body="which db?", to="orchestrator", **{"from": "a"})
    post(store, "p", thread="step-a", body="moving the helpers", to="orchestrator",
         needs_reply=False, **{"from": "a"})
    msgs = L.read(store.home, "p", threads=["step-a"])["records"]
    assert [m["needs_reply"] for m in msgs] == [True, False]  # a watcher can wake on questions


def test_concurrent_posters_in_processes_get_unique_seqs(store):
    store.create_project("p")
    code = ("import sys\nfrom sluice.fns._lib.threads import post\n"
            "print(*[post('t', str(i), sys.argv[1]) for i in range(10)])")
    procs = [subprocess.Popen([sys.executable, "-c", code, f"w{k}"], stdout=subprocess.PIPE,
                              text=True, env=env_for(store, "p"), cwd=store.home.parent)
             for k in range(6)]
    seqs = []
    for proc in procs:
        out, _ = proc.communicate(timeout=60)
        assert proc.returncode == 0
        mine = [int(x) for x in out.split()]
        assert mine == sorted(mine) and len(mine) == 10
        seqs += mine
    assert len(set(seqs)) == 60 and min(seqs) > L.read(store.home, "p")["records"][0]["seq"]
    msgs = L.read(store.home, "p", threads=["t"])["records"]
    assert [m["seq"] for m in msgs] == sorted(seqs)


def test_wait_filters_by_recipient(store):
    store.create_project("p")
    post(store, "p", thread="t", body="for bob", to="bob", **{"from": "a"})
    second = post(store, "p", thread="t", body="for all", **{"from": "a"})
    post(store, "p", thread="t", body="for alice", to="alice", **{"from": "a"})
    code, out, err = run_fn(store, "thread.wait", {"thread": "t", "to": "alice", "timeout": 5})
    assert code == 0, err
    assert [m["body"] for m in out["messages"]] == ["for all", "for alice"]
    assert out["last_seq"] == L.last_seq(store.home, "p")  # the call records count too
    code, out, _ = run_fn(store, "thread.wait", {"thread": "t", "since_seq": second})
    assert [m["body"] for m in out["messages"]] == ["for alice"]


def test_wait_returns_on_a_post_from_another_process(store):
    store.create_project("p")
    waiter = subprocess.Popen(
        ["uv", "run", "--quiet", "--script", str(BUILTIN_DIR / "thread.wait" / "main.py")],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        env=env_for(store, "p"), cwd=store.home.parent)
    waiter.stdin.write(json.dumps({"thread": "answers", "since_seq": 1, "timeout": 60}))
    waiter.stdin.close()
    time.sleep(1.5)
    assert waiter.poll() is None  # still waiting
    post(store, "p", thread="noise", body="not this one", **{"from": "x"})
    t0 = time.monotonic()
    seq = post(store, "p", thread="answers", body="42", **{"from": "oracle"})
    out = json.loads(waiter.stdout.read())
    assert waiter.wait(timeout=10) == 0
    assert time.monotonic() - t0 < 5
    assert [(m["seq"], m["body"]) for m in out["messages"]] == [(seq, "42")]
    assert out["last_seq"] >= seq


def test_wait_on_questions_lets_notes_ride_with_the_next_question(store):
    store.create_project("p")
    post(store, "p", thread="t", body="fyi", needs_reply=False, **{"from": "w"})
    t0 = time.monotonic()
    code, out, err = run_fn(store, "thread.wait", {"thread": "t", "wake": "questions",
                                                   "timeout": 2})
    assert code == 0, err
    assert time.monotonic() - t0 >= 2  # a note alone does not wake it
    assert [m["body"] for m in out["messages"]] == ["fyi"]  # but comes back at the timeout
    post(store, "p", thread="t", body="which db?", **{"from": "w"})
    code, out, err = run_fn(store, "thread.wait", {"thread": "t", "wake": "questions",
                                                   "timeout": 30})
    assert [m["body"] for m in out["messages"]] == ["fyi", "which db?"]
    code, _, err = run_fn(store, "thread.wait", {"thread": "t", "wake": "nope", "timeout": 1})
    assert code == 1 and "wake: expected one of any, questions" in err


def test_wait_times_out_empty(store):
    store.create_project("p")
    t0 = time.monotonic()
    code, out, err = run_fn(store, "thread.wait", {"thread": "t", "timeout": 1})
    assert code == 0, err
    assert out == {"messages": [], "last_seq": L.last_seq(store.home, "p")}
    assert time.monotonic() - t0 >= 1


def test_threads_need_a_project(store):
    code, _, err = run_fn(store, "thread.post", {"thread": "t", "body": "x", "from": "me"},
                          project="")
    assert code == 1 and "SLUICE_PROJECT is empty" in err
    call = calls.create(store, "thread.post", {"thread": "t", "body": "x", "from": "me"}, None,
                        direct=True)
    res = run_call_direct(store, call, None)
    assert res["status"] == "failed" and "SLUICE_PROJECT is empty" in res["error"]
    code, _, err = run_fn(store, "thread.post", {"thread": "Bad Name", "body": "x",
                                                 "from": "me"}, project="p")
    assert code == 1 and "no project 'p'" in err
    store.create_project("p")
    code, _, err = run_fn(store, "thread.post", {"thread": "Bad Name", "body": "x",
                                                 "from": "me"}, project="p")
    assert code == 1 and "thread names match" in err
