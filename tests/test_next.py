"""`sluice next` and the `next` tool: block until the next record an orchestrator acts on,
print it, exit (SPEC §8, §9). Each test drives a real `sluice next` process against a real
home; waits are bounded by the command's own --timeout."""

import json
import os
import subprocess
import sys

from mcp import Client

from sluice import log as L
from sluice.mcp_server import build_server
from tests.conftest import add, create, d, src


def next_run(home, *args, timeout=30):
    """`sluice next <args>` as its own process; returns its stdout."""
    p = subprocess.run([sys.executable, "-m", "sluice.cli", "next", *args],
                       env={**os.environ, "SLUICE_HOME": str(home)},
                       capture_output=True, text=True, timeout=timeout, check=False)
    assert p.returncode == 0, p.stderr
    return p.stdout


def msg(store, project, body, frm="worker", to="orchestrator", needs_reply=True,
        thread="t"):
    """A message record like thread.post's; returns its seq."""
    rec = {"kind": "message", "thread": thread, "from": frm, "body": body,
           "needs_reply": needs_reply}
    if to is not None:
        rec["to"] = to
    return store.append(project, rec)[0]


def status_rec(store, project, step, to, error=None, frm="running"):
    rec = {"kind": "step.status", "step": step, "from": frm, "to": to}
    if error:
        rec["error"] = error
    return store.append(project, rec)[0]


def test_wakes_on_a_failed_step_with_its_errors_last_line(store, tmp_path):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    seq = status_rec(store, "p", "fix-x", "failed", error="first line\nsecond line boom")
    out = next_run(store.home, "-p", "p", "--since-seq", str(since))
    assert out.splitlines() == ["STEP fix-x running -> failed: second line boom",
                                f"seq {seq}"]


def test_wakes_on_stale_and_skipped(store, tmp_path):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    s1 = status_rec(store, "p", "a", "stale")
    assert next_run(store.home, "-p", "p", "--since-seq",
                    str(since)).splitlines() == ["STEP a running -> stale",
                                                 f"seq {s1}"]
    s2 = status_rec(store, "p", "b", "skipped", frm="pending")
    assert next_run(store.home, "-p", "p", "--since-seq",
                    str(s1)).splitlines() == ["STEP b pending -> skipped", f"seq {s2}"]


def test_an_open_fns_success_wakes(store):
    create(store, "p", {"o": {"run": "test.open", "in": {},
                              "outputs": {"word": "string"}}})
    since = L.last_seq(store.home, "p")
    store.set_output("p", "o", {"ports": {}, "extra": {}, "results": [], "word": "w"},
                     "t", "t")
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines[0] == "STEP o pending -> succeeded" and lines[-1].startswith("seq ")


def test_a_success_completing_its_unit_wakes_with_it(store):
    create(store, "p", {"a": add(d(1), d(2)),
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}}})
    since = L.last_seq(store.home, "p")
    store.set_output("p", "a", {"sum": 3}, "t", "t")
    store.set_output("p", "b", {"sum": 4}, "t", "t")
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines[0] == "UNIT done: a … b (2 steps)"


def test_a_mid_unit_success_does_not_wake(store):
    create(store, "p", {"a": add(d(1), d(2)),
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}}})
    since = L.last_seq(store.home, "p")
    store.set_output("p", "a", {"sum": 3}, "t", "t")  # b still pending: the unit is open
    out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--timeout", "1")
    assert out.splitlines()[-1].startswith("timeout seq ") and "STEP" not in out


def test_a_question_from_someone_else_wakes_own_messages_do_not(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    msg(store, "p", "my own note back", frm="orchestrator", needs_reply=True)
    out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--timeout", "1")
    assert "orchestrator" not in out.split("timeout")[0]  # consumed silently
    seq = msg(store, "p", "which database?", to=None)
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since),
                     "--timeout", "5").splitlines()
    assert lines == ["MSG t worker -> -: which database?", f"seq {seq}"]


def test_notes_are_held_then_printed_before_the_waking_record(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    msg(store, "p", "chose postgres", needs_reply=False)
    seq = status_rec(store, "p", "x", "failed", error="nope")
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines == ["NOTE t worker -> orchestrator: chose postgres",
                     "STEP x running -> failed: nope", f"seq {seq}"]


def test_inbox_post_and_answer_wake(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    store.inbox_post("p", "which env?", "body", None, None, "x")
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines[0] == "INBOX post i1 which env?"
    store.inbox_answer("p", "i1", {"action": "staging"}, "person")
    lines = next_run(store.home, "-p", "p", "--since-seq",
                     str(int(lines[-1].split()[1]))).splitlines()
    assert lines[0] == "INBOX answer i1 staging"


def test_several_projects_watch_together(store):
    create(store, "p", {"x": add(d(1), d(2))})
    create(store, "q", {"y": add(d(3), d(4))})
    since = L.last_seq(store.home, "q")
    status_rec(store, "q", "y", "failed", error="bad")
    out = next_run(store.home, "-p", "p", "-p", "q", "--since-seq", str(since), "--json")
    recs = [json.loads(l) for l in out.splitlines()]
    assert recs[0]["project"] == "q" and recs[0]["step"] == "y"
    assert recs[-1]["seq"] == recs[0]["seq"]


def test_the_cursor_round_trips_without_misses_or_repeats(store, tmp_path):
    create(store, "p", {"x": add(d(1), d(2))})
    cursor = tmp_path / "next.seq"
    msg(store, "p", "old", needs_reply=False)
    # a missing cursor starts from now and is written, even on a timeout
    out = next_run(store.home, "-p", "p", "--cursor", str(cursor), "--timeout", "1")
    assert out.splitlines()[-1].startswith("timeout seq ")
    start = int(cursor.read_text())
    seq = status_rec(store, "p", "x", "failed", error="one")
    msg(store, "p", "more", needs_reply=False)
    s2 = status_rec(store, "p", "x", "failed", error="two")
    out = next_run(store.home, "-p", "p", "--cursor", str(cursor))
    assert out.splitlines() == ["STEP x running -> failed: one", f"seq {seq}"]
    assert int(cursor.read_text()) == seq  # the note and second record wait for next time
    out = next_run(store.home, "-p", "p", "--cursor", str(cursor))
    assert out.splitlines() == ["NOTE t worker -> orchestrator: more",
                               "STEP x running -> failed: two", f"seq {s2}"]
    assert int(cursor.read_text()) == s2
    assert L.read(store.home, "p", start)["records"][0]["seq"] > start


def test_timeout_and_starting_from_now(store):
    create(store, "p", {"x": add(d(1), d(2))})
    status_rec(store, "p", "x", "failed", error="old")  # before "now"
    out = next_run(store.home, "-p", "p", "--timeout", "1")
    assert out.splitlines()[-1].startswith("timeout seq ")


def test_json_output_is_records_then_the_seq(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    msg(store, "p", "fyi", needs_reply=False)
    seq = status_rec(store, "p", "x", "failed", error="e")
    out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--json")
    *records, last = [json.loads(l) for l in out.splitlines()]
    assert [r["kind"] for r in records] == ["message", "step.status"]
    assert records[0]["needs_reply"] is False and records[1]["seq"] == seq
    assert last == {"seq": seq, "timed_out": False}


async def test_the_mcp_tools_shape(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    msg(store, "p", "ping", to=None)
    async with Client(build_server(store)) as c:
        r = await c.call_tool("next", {"projects": ["p"], "since_seq": since,
                                       "timeout": 5})
        assert not r.is_error
        res = json.loads(r.content[0].text)
        assert sorted(res) == ["last_seq", "notes", "records", "timed_out"]
        assert res["timed_out"] is False and res["records"][0]["body"] == "ping"
        r = await c.call_tool("next", {"projects": ["p"], "since_seq": res["last_seq"],
                                       "timeout": 1})
        res = json.loads(r.content[0].text)
        assert res["timed_out"] is True and res["records"] == []
