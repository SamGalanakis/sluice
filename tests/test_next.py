"""`sluice next` and the `next` tool: block until the records an orchestrator acts on, print
them, exit (SPEC §8, §9). Tests drive a real `sluice next` process (or next_up itself, when a
writer must run alongside) against a real home; waits are bounded by --timeout."""

import json
import os
import sqlite3
import subprocess
import sys
import threading
import time

from mcp import Client

from sluice import db, query
from sluice import log as L
from sluice.db import Busy
from sluice.mcp_server import build_server
from sluice.runner import Runner
from sluice.store import Store
from sluice.watch import next_up, unread_alerts
from tests.conftest import add, create, d, src, write_config
from tests.schema_v2 import SCHEMA_V2


def next_run(home, *args, timeout=30):
    """`sluice next <args>` as its own process; returns its stdout. Without a --settle in
    `args` it passes --settle 0 (return at the first waking record)."""
    settle = () if "--settle" in args else ("--settle", "0")
    p = subprocess.run([sys.executable, "-m", "sluice.cli", "next", *args, *settle],
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
                                       "timeout": 5, "settle": 0})
        assert not r.is_error
        res = json.loads(r.content[0].text)
        assert sorted(res) == ["last_seq", "notes", "records", "timed_out"]
        assert res["timed_out"] is False and res["records"][0]["body"] == "ping"
        r = await c.call_tool("next", {"projects": ["p"], "since_seq": res["last_seq"],
                                       "timeout": 1, "settle": 0})
        res = json.loads(r.content[0].text)
        assert res["timed_out"] is True and res["records"] == []


# ---- units: one wake when a unit settles --------------------------------------------------

WORK_OUT = {"ports": {}, "extra": {}, "results": [], "landed": True, "summary": "all done"}


def lane(unit="u", close_paused=False):
    """A recipe-shaped unit, tagged unit:<unit>: fork -> work (an open fn) -> close -> rm."""
    tags = [f"unit:{unit}"]
    close = {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "after": [f"{unit}-work"],
             "tags": tags}
    return {f"{unit}-fork": add(d(1), d(2), tags=tags),
            f"{unit}-work": {"run": "test.open", "in": {"n": src(f"{unit}-fork/sum")},
                             "outputs": {"landed": "boolean", "summary": "string"},
                             "tags": tags},
            f"{unit}-close": {**close, "paused": True} if close_paused else close,
            f"{unit}-rm": add(d(0), d(0), after=[f"{unit}-close"], tags=tags)}


def run_step(store, project, sid, outputs, gap=0.0):
    """The step goes pending -> running (a record, like the runner's), then succeeds with
    `outputs` (step_set_output: its state and the record to succeeded); returns that seq."""
    status_rec(store, project, sid, "running", frm=None)
    time.sleep(gap)
    store.set_output(project, sid, outputs, "t", "t")
    return L.last_seq(store.home, project)


def land(store, project, unit="u", gap=0.0, summary="all done"):
    """Run the lane to success; returns the rm step's success seq."""
    run_step(store, project, f"{unit}-fork", {"sum": 3}, gap)
    run_step(store, project, f"{unit}-work", {**WORK_OUT, "summary": summary}, gap)
    run_step(store, project, f"{unit}-close", {"sum": 2}, gap)
    return run_step(store, project, f"{unit}-rm", {"sum": 0}, gap)


def in_thread(fn, *args, **kw):
    """Start fn(*args, **kw) in a thread; returns a join() that gives its result."""
    out = {}
    t = threading.Thread(target=lambda: out.setdefault("r", fn(*args, **kw)))
    t.start()

    def join():
        t.join(30)
        return out["r"]
    return join


def test_a_landed_lane_wakes_once_read_live_or_late(store):
    create(store, "p", lane())
    since = L.last_seq(store.home, "p")
    # the waiter is already polling while the records arrive
    join = in_thread(next_up, store, ["p"], since, timeout=10, settle=1)
    time.sleep(0.3)
    rm = land(store, "p", gap=0.3)
    live = join()
    assert [r["seq"] for r in live["records"]] == [rm] and live["last_seq"] == rm
    unit = live["records"][0]["unit"]
    assert unit["name"] == "u" and unit["settled"] is True
    assert [(s["id"], s["status"]) for s in unit["steps"]] == [
        ("u-fork", "succeeded"), ("u-work", "succeeded"), ("u-close", "succeeded"),
        ("u-rm", "succeeded")]
    assert unit["steps"][1]["outputs"] == {"landed": True, "summary": "all done"}  # declared
    # read late: every record is already there, each judged as of its own seq
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since), "--settle",
                     "0.3").splitlines()
    assert lines == [
        "UNIT u settled: fork succeeded · work succeeded · close succeeded · rm succeeded",
        "  fork.sum: 3",
        "  work.landed: true", "  work.summary: all done", "  close.sum: 2", "  rm.sum: 0",
        f"seq {rm}"]


def test_an_open_step_inside_a_unit_does_not_wake_on_success(store):
    create(store, "p", lane())
    since = L.last_seq(store.home, "p")
    run_step(store, "p", "u-fork", {"sum": 3})
    run_step(store, "p", "u-work", WORK_OUT)  # close is next: the unit is still going
    out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--timeout", "1")
    assert out.splitlines() == [f"timeout seq {L.last_seq(store.home, 'p')}"]


def test_a_failed_work_step_wakes_once_with_its_held_unit(store):
    create(store, "p", lane())
    since = L.last_seq(store.home, "p")
    run_step(store, "p", "u-fork", {"sum": 3})
    status_rec(store, "p", "u-work", "running", frm=None)
    seq = status_rec(store, "p", "u-work", "failed", error="trace\nno tests pass")
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines == ["STEP u-work running -> failed: no tests pass",
                     ("  unit u: fork succeeded · work failed · close pending (held) · "
                      "rm pending (held)"),
                     "  fork.sum: 3", f"seq {seq}"]
    recs = [json.loads(x) for x in next_run(store.home, "-p", "p", "--since-seq", str(since),
                                            "--json", "--settle", "0.3").splitlines()]
    assert [r["seq"] for r in recs[:-1]] == [seq]
    assert recs[0]["unit"]["steps"][2] == {"id": "u-close", "status": "pending",
                                           "held": True, "outputs": {}}
    out = next_run(store.home, "-p", "p", "--since-seq", str(seq), "--timeout", "1")
    assert out.splitlines() == [f"timeout seq {seq}"]


def test_a_paused_step_after_work_settles_the_unit_at_works_success(store):
    create(store, "p", lane(close_paused=True))
    since = L.last_seq(store.home, "p")
    run_step(store, "p", "u-fork", {"sum": 3})
    seq = run_step(store, "p", "u-work", WORK_OUT)
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since), "--settle",
                     "0.3").splitlines()
    assert lines[0] == ("UNIT u settled: fork succeeded · work succeeded · "
                        "close pending (held) · rm pending (held)")
    assert "  work.landed: true" in lines and lines[-1] == f"seq {seq}"
    out = next_run(store.home, "-p", "p", "--since-seq", str(seq), "--timeout", "1")
    assert out.splitlines() == [f"timeout seq {seq}"]


def test_a_retried_unit_that_settles_again_wakes_again(store):
    create(store, "p", lane())
    run_step(store, "p", "u-fork", {"sum": 3})
    status_rec(store, "p", "u-work", "running", frm=None)
    first = status_rec(store, "p", "u-work", "failed", error="boom")
    status_rec(store, "p", "u-work", "pending", frm="failed")  # retried
    run_step(store, "p", "u-work", WORK_OUT)
    run_step(store, "p", "u-close", {"sum": 2})
    rm = run_step(store, "p", "u-rm", {"sum": 0})
    res = next_up(store, ["p"], first - 1, settle=0.3, timeout=5)
    assert [r["seq"] for r in res["records"]] == [first, rm]
    assert all(r["unit"]["name"] == "u" for r in res["records"])


def test_two_units_settling_within_the_window_arrive_together(store):
    create(store, "p", {**lane("a"), **lane("b")})
    since = L.last_seq(store.home, "p")
    join = in_thread(next_up, store, ["p"], since, timeout=10, settle=1)
    a = land(store, "p", "a")
    time.sleep(0.2)
    b = land(store, "p", "b")
    res = join()
    assert [r["seq"] for r in res["records"]] == [a, b] and res["last_seq"] == b
    assert [r["unit"]["name"] for r in res["records"]] == ["a", "b"]


def test_settle_max_cuts_a_stream_of_wakes_off_and_the_next_call_continues(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    stop, seqs = threading.Event(), []

    def write():
        while not stop.is_set():
            seqs.append(status_rec(store, "p", "x", "failed", error="again"))
            stop.wait(0.1)
    writer = threading.Thread(target=write)
    writer.start()
    try:
        t0 = time.monotonic()
        one = next_up(store, ["p"], since, timeout=10, settle=1, settle_max=0.8)
        took = time.monotonic() - t0
    finally:
        stop.set()
        writer.join(10)
    assert one["records"] and took < 2.5
    two = next_up(store, ["p"], one["last_seq"], timeout=5, settle=0.3)
    assert [r["seq"] for r in one["records"] + two["records"]] == seqs  # each once, in order


def test_settle_zero_returns_at_the_first_and_a_window_takes_the_rest(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    s1 = status_rec(store, "p", "x", "failed", error="one")
    s2 = status_rec(store, "p", "x", "failed", error="two")
    assert next_run(store.home, "-p", "p", "--since-seq", str(since), "--settle",
                    "0").splitlines() == ["STEP x running -> failed: one", f"seq {s1}"]
    assert next_run(store.home, "-p", "p", "--since-seq", str(since), "--settle",
                    "0.3").splitlines() == ["STEP x running -> failed: one",
                                            "STEP x running -> failed: two", f"seq {s2}"]


def test_the_cursor_moves_once_per_batch_without_misses_or_repeats(store, tmp_path):
    create(store, "p", {"x": add(d(1), d(2))})
    cursor = tmp_path / "next.seq"
    cursor.write_text(f"{L.last_seq(store.home, 'p')}\n")
    s1 = status_rec(store, "p", "x", "failed", error="one")
    s2 = status_rec(store, "p", "x", "failed", error="two")
    tail = msg(store, "p", "trailing", needs_reply=False)
    out = next_run(store.home, "-p", "p", "--cursor", str(cursor), "--settle", "0.3")
    assert out.splitlines() == ["NOTE t worker -> orchestrator: trailing",
                                "STEP x running -> failed: one",
                                "STEP x running -> failed: two", f"seq {tail}"]
    assert int(cursor.read_text()) == tail
    s3 = status_rec(store, "p", "x", "failed", error="three")
    out = next_run(store.home, "-p", "p", "--cursor", str(cursor), "--settle", "0.3")
    assert out.splitlines() == ["STEP x running -> failed: three", f"seq {s3}"]
    assert int(cursor.read_text()) == s3 and s1 < s2 < tail < s3


def test_json_keeps_outputs_whole_and_full_text_cuts_them_at_600(store):
    create(store, "p", lane())
    since = L.last_seq(store.home, "p")
    long = "word " * 300
    land(store, "p", summary=long)
    text = next_run(store.home, "-p", "p", "--since-seq", str(since), "--settles",
                    "full").splitlines()
    cut = next(x for x in text if x.startswith("  work.summary: "))
    assert cut == "  work.summary: " + ("word " * 120)[:600] + "…"
    recs = [json.loads(x) for x in next_run(store.home, "-p", "p", "--since-seq", str(since),
                                            "--json").splitlines()]
    assert recs[0]["unit"]["steps"][1]["outputs"]["summary"] == long
    assert recs[-1] == {"seq": recs[0]["seq"], "timed_out": False}


def test_an_untagged_component_is_a_unit_named_by_its_first_step(store):
    create(store, "p", {"a": add(d(1), d(2)),
                        "b": {"run": "test.add", "in": {"a": src("a/sum"), "b": d(1)}}})
    since = L.last_seq(store.home, "p")
    store.set_output("p", "a", {"sum": 3}, "t", "t")  # b still pending: the unit is going
    out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--timeout", "1")
    assert out.splitlines()[-1].startswith("timeout seq ")
    store.set_output("p", "b", {"sum": 4}, "t", "t")
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines[:3] == ["UNIT a settled: a succeeded · b succeeded", "  a.sum: 3",
                         "  b.sum: 4"]


def test_an_untagged_step_after_a_recipe_unit_does_not_join_it(store):
    plan = lane()
    plan["audit"] = add(d(1), d(1), after=["u-work"])  # untagged, reads nothing: standalone
    create(store, "p", plan)
    since = L.last_seq(store.home, "p")
    run_step(store, "p", "u-fork", {"sum": 3})
    run_step(store, "p", "u-work", {"landed": True, "summary": "ok", "ports": {}, "extra": {},
                                    "results": []})
    run_step(store, "p", "audit", {"sum": 2})  # a plain standalone success: no wake
    out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--timeout", "1")
    assert out.splitlines()[-1].startswith("timeout seq ")
    run_step(store, "p", "u-close", {"sum": 2})
    run_step(store, "p", "u-rm", {"sum": 0})
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines[0] == "UNIT u settled: fork succeeded · work succeeded · close succeeded · " \
                       "rm succeeded"
    assert "  work.landed: true" in lines and "  work.summary: ok" in lines
    assert not any(x.startswith("  work.ports") for x in lines)  # declared outputs only


def test_a_unit_added_paused_does_not_settle_until_a_step_finishes(store):
    create(store, "p", {sid: {**s, "paused": True} for sid, s in lane().items()})
    since = L.last_seq(store.home, "p")
    for sid in lane():  # the runner's record for each new step
        status_rec(store, "p", sid, "pending", frm=None)
    out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--timeout", "1")
    assert out.splitlines()[-1].startswith("timeout seq ")  # held from the start: no wake
    store.set_output("p", "u-fork", {"sum": 3}, "t", "t")  # finished, the rest still paused
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines[0].startswith("UNIT u settled: fork succeeded · work pending (held)")


def test_a_project_paused_by_someone_else_wakes_it(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    store.update_project("p", paused=True, author="orchestrator", reason="mine")  # my own
    store.update_project("p", paused=False, author="orchestrator")
    store.update_project("p", paused=True, author="dashboard", reason="deploying")
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines[0] == "PROJECT p paused by dashboard: deploying"


def test_all_wakes_on_every_record(store):
    create(store, "p", lane())
    since = L.last_seq(store.home, "p")
    seq = status_rec(store, "p", "u-fork", "running", frm=None)
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since), "--all").splitlines()
    assert lines == ["STEP u-fork pending -> running", f"seq {seq}"]


async def test_the_mcp_tool_returns_a_batch(store):
    create(store, "p", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "p")
    status_rec(store, "p", "x", "failed", error="one")
    msg(store, "p", "fyi", needs_reply=False)
    status_rec(store, "p", "x", "failed", error="two")
    async with Client(build_server(store)) as c:
        r = await c.call_tool("next", {"projects": "p", "since_seq": since, "timeout": 5,
                                       "settle": 0.3})
        res = json.loads(r.content[0].text)
        assert sorted(res) == ["last_seq", "notes", "records", "timed_out"]
        assert [x["error"] for x in res["records"]] == ["one", "two"]
        assert [x["body"] for x in res["notes"]] == ["fyi"]
        assert res["last_seq"] == res["records"][-1]["seq"]


# ---- nobody reading ----------------------------------------------------------------------

def readers(store):
    with store.rx() as conn:
        rows = db.all_rows(conn, "SELECT * FROM readers")
    return {r["project"]: {"seq": r["seq"], "at": r["at"], "me": r["me"]} for r in rows}


def read_at(store, project, at):
    """Set when `next` last read the project (as if it had been then)."""
    with store.tx() as conn:
        conn.execute("UPDATE readers SET at = ? WHERE project = ?", (at, project))


def test_next_notes_how_far_it_has_read_each_project(store):
    create(store, "p", {"x": add(d(1), d(2))})
    create(store, "q", {"x": add(d(1), d(2))})
    since = L.last_seq(store.home, "q")
    got = next_up(store, ["p", "q"], since, me="lead", timeout=0)
    assert got["timed_out"]
    assert {p: (r["seq"], r["me"]) for p, r in readers(store).items()} == {
        "p": (since, "lead"), "q": (since, "lead")}
    seq = status_rec(store, "p", "x", "failed", error="boom")
    next_up(store, ["p"], since, me="lead", timeout=5, settle=0)
    assert readers(store)["p"]["seq"] == seq and readers(store)["q"]["seq"] == since
    next_up(store, ["p"], since, me="other", timeout=0)  # an older since_seq never goes back
    assert readers(store)["p"]["seq"] == seq and readers(store)["p"]["me"] == "other"
    assert not (store.home / "next.json").exists()


def test_two_nexts_on_different_projects_keep_both_positions(store):
    create(store, "p", {"x": add(d(1), d(2))})
    create(store, "q", {"x": add(d(1), d(2))})
    outs = {}
    threads = [threading.Thread(target=lambda p=p: outs.setdefault(
        p, next_run(store.home, "-p", p, "--me", f"lead-{p}", "--timeout", "0")))
        for p in ("p", "q")]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert {p: r["me"] for p, r in readers(store).items()} == {"p": "lead-p", "q": "lead-q"}


def test_noting_what_next_read_is_best_effort(store, monkeypatch):
    create(store, "p", {"x": add(d(1), d(2))})

    def busy():
        raise Busy()

    monkeypatch.setattr(store, "tx", busy)
    assert next_up(store, ["p", "gone"], 0, timeout=0)["timed_out"]
    monkeypatch.undo()
    next_up(store, ["p", "gone"], 0, timeout=0)  # a project that is not there gets no row
    assert list(readers(store)) == ["p"]


def test_a_deleted_projects_position_goes_with_it(store):
    create(store, "p", {"x": add(d(1), d(2))})
    next_up(store, ["p"], 0, timeout=0)
    with store.tx() as conn:
        conn.execute("DELETE FROM projects WHERE name = 'p'")
    assert readers(store) == {}


def test_a_waking_record_nobody_reads_is_posted_to_the_inbox_once(store):
    create(store, "p", {"x": add(d(1), d(2))})
    create(store, "quiet", {"x": add(d(1), d(2))})  # never read by next: never alerted
    since = L.last_seq(store.home, "p")
    next_up(store, ["p"], since, timeout=0)  # the orchestrator reads once, then is gone
    status_rec(store, "quiet", "x", "failed", error="boom")
    seq = status_rec(store, "p", "fix-x", "failed", error="first\nboom")
    msg(store, "p", "a note", needs_reply=False)  # not a waking record
    assert unread_alerts(store, 30) == []  # it is fresh
    later = time.time() + 3600
    [item] = unread_alerts(store, 30, now=later)
    assert item["title"] == f"No orchestrator has read p's log for 30 min (seq {seq})"
    assert (item["from"], item["seq"]) == ("sluice", seq)
    assert "run" not in item and "waiting" not in store.inbox("p")[0]
    assert "STEP fix-x running -> failed: boom" in item["body"]
    assert f"sluice next -p p --since-seq {since}" in item["body"]
    # its own inbox.post wakes too, but the first unread record is the same: no second item
    assert unread_alerts(store, 30, now=later) == []
    store.inbox_close("p", item["id"], "seen", "test")
    assert unread_alerts(store, 30, now=later) == []
    # the orchestrator is back and reads it all: nothing is unread
    next_up(store, ["p"], since, timeout=5, settle=0.1, settle_max=1)
    assert unread_alerts(store, 30, now=later + 3600) == []
    assert [i["title"] for i in store.inbox("quiet", "all")] == []


def test_a_new_threshold_does_not_alert_a_record_again(store):
    create(store, "p", {"x": add(d(1), d(2))})
    next_up(store, ["p"], L.last_seq(store.home, "p"), timeout=0)
    seq = status_rec(store, "p", "x", "failed", error="boom")
    later = time.time() + 3600
    [item] = unread_alerts(store, 30, now=later)
    assert unread_alerts(store, 45, now=later) == []
    assert unread_alerts(store, 5, now=later) == []
    store.inbox_close("p", item["id"], "seen", "test")
    assert unread_alerts(store, 10, now=later) == []
    assert [(i["from"], i["seq"]) for i in store.inbox("p", "all")] == [("sluice", seq)]


def test_no_alert_while_next_has_read_the_project_lately(store):
    create(store, "p", {"x": add(d(1), d(2))})
    next_up(store, ["p"], L.last_seq(store.home, "p"), timeout=0)
    status_rec(store, "p", "x", "failed", error="boom")
    assert unread_alerts(store, 30, now=time.time() + 60) == []  # read a minute ago
    read_at(store, "p", "2020-01-01T00:00:00Z")
    store.update_project("p", archived=True)
    assert unread_alerts(store, 30, now=time.time() + 3600) == []  # archived: not checked
    store.update_project("p", archived=False)
    assert len(unread_alerts(store, 30, now=time.time() + 3600)) == 1


def test_the_runner_alerts_only_with_unread_alert_min(store):
    create(store, "p", {"x": add(d(1), d(2))})
    next_up(store, ["p"], L.last_seq(store.home, "p"), timeout=0)
    read_at(store, "p", "2020-01-01T00:00:00Z")
    status_rec(store, "p", "y", "failed", error="boom")
    time.sleep(1.1)  # the record's `at` has whole seconds
    Runner(store).tick()
    assert store.inbox("p", "all") == []
    store.config["unread_alert_min"] = 0.001
    Runner(store).tick()
    assert [i["from"] for i in store.inbox("p", "all")] == ["sluice"]


def test_a_version_2_file_gets_the_readers_table(tmp_path):
    """A real version-2 file (the SCHEMA before the inbox's `run` column, the drain tables and
    `readers`) is upgraded in place to the current schema, keeping its rows; `next` then notes
    where it has read."""
    home = tmp_path / "h"
    write_config(home)
    conn = sqlite3.connect(home / db.FILE, autocommit=True)
    conn.execute("PRAGMA journal_mode = WAL")
    conn.executescript(SCHEMA_V2)
    conn.execute("PRAGMA user_version = 2")
    conn.execute("INSERT INTO projects (name, description, created) VALUES "
                 "('p', 'old', '2026-09-01T00:00:00Z')")
    conn.execute("INSERT INTO records (project, at, kind, data) VALUES "
                 "('p', '2026-09-01T00:00:00Z', 'project.create', '{}')")
    conn.close()
    store = Store(home)
    with store.rx() as c:
        assert c.execute("PRAGMA user_version").fetchone()[0] == db.VERSION
        tables = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type = 'table'")}
        assert {"readers", "drain", "drain_projects"} <= tables
        assert "run" in {r[1] for r in c.execute("PRAGMA table_info(inbox)")}
        assert tuple(db.one(c, "SELECT name, description, resources FROM projects")) == (
            "p", "old", "{}")
        assert db.one(c, "SELECT count(*) FROM records")[0] == 1
    next_up(store, ["p"], 0, me="lead", timeout=0)
    assert {p: (r["seq"], r["me"]) for p, r in readers(store).items()} == {"p": (1, "lead")}
    assert "readers" in query.run(home, "SELECT name FROM sqlite_master WHERE type = "
                                        "'table' AND name = 'readers'")["rows"][0]


# ---- how much of a settled unit a batch shows: --settles, and messages first -----------------

SUMMARY = "Landed the parser split.\n" + "Details " * 60
BIG_OUT = {**WORK_OUT, "summary": SUMMARY, "sha": "127c2443a5", "evidence": "log " * 400,
           "unresolved": ["flaky test", "docs"]}


def big_lane():
    """lane() whose work step declares long outputs too (evidence, unresolved)."""
    plan = lane()
    plan["u-work"]["outputs"] = {"landed": "boolean", "summary": "string", "sha": "string",
                                 "evidence": "string", "unresolved": "string[]"}
    return plan


def land_big(store):
    run_step(store, "p", "u-fork", {"sum": 3})
    run_step(store, "p", "u-work", BIG_OUT)
    run_step(store, "p", "u-close", {"sum": 2})
    return run_step(store, "p", "u-rm", {"sum": 0})


HEAD = "UNIT u settled: fork succeeded · work succeeded · close succeeded · rm succeeded"


def test_settles_short_prints_only_short_outputs_and_names_the_rest(store):
    create(store, "p", big_lane())
    since = L.last_seq(store.home, "p")
    rm = land_big(store)
    lines = next_run(store.home, "-p", "p", "--since-seq", str(since)).splitlines()
    assert lines == [HEAD, "  fork.sum: 3", "  work.landed: true",
                     "  work.summary: Landed the parser split.", "  work.sha: 127c2443a5",
                     "  close.sum: 2", "  rm.sum: 0",
                     "  (+ work.evidence, work.unresolved: sluice query or --settles full)",
                     f"seq {rm}"]
    long_first = "word " * 100  # a summary's first line is cut to 200
    assert next_run(store.home, "-p", "p", "--since-seq", str(since), "--settles",
                    "short") == "\n".join(lines) + "\n"
    create(store, "q", lane())
    s = L.last_seq(store.home, "q")
    run_step(store, "q", "u-fork", {"sum": 3})
    run_step(store, "q", "u-work", {**WORK_OUT, "summary": long_first})
    run_step(store, "q", "u-close", {"sum": 2})
    run_step(store, "q", "u-rm", {"sum": 0})
    got = next_run(store.home, "-p", "q", "--since-seq", str(s)).splitlines()
    assert "  work.summary: " + long_first.strip()[:200] + "…" in got


def test_settles_none_prints_the_unit_line_only_and_full_every_output_cut(store):
    create(store, "p", big_lane())
    since = L.last_seq(store.home, "p")
    rm = land_big(store)
    assert next_run(store.home, "-p", "p", "--since-seq", str(since), "--settles",
                    "none").splitlines() == [HEAD, f"seq {rm}"]
    full = next_run(store.home, "-p", "p", "--since-seq", str(since), "--settles", "full",
                    "--cut", "20").splitlines()
    assert full[0] == HEAD and full[-1] == f"seq {rm}"
    assert "  work.evidence: " + ("log " * 5)[:20] + "…" in full
    assert '  work.unresolved: ["flaky test","docs"…' in full  # JSON is cut too
    assert "  work.sha: 127c2443a5" in full and not any(x.startswith("  (+") for x in full)
    default = next_run(store.home, "-p", "p", "--since-seq", str(since), "--settles", "full")
    assert "  work.evidence: " + ("log " * 150)[:600] + "…" in default.splitlines()


def test_json_is_whole_under_every_settles(store):
    create(store, "p", big_lane())
    since = L.last_seq(store.home, "p")
    land_big(store)
    for mode in ("short", "full", "none"):
        out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--json",
                       "--settles", mode, "--cut", "5")
        rec = json.loads(out.splitlines()[0])
        assert rec["unit"]["steps"][1]["outputs"] == {
            k: BIG_OUT[k] for k in ("landed", "summary", "sha", "evidence", "unresolved")}


def test_messages_print_first_and_whole_ahead_of_a_big_settle(store):
    create(store, "p", big_lane())
    since = L.last_seq(store.home, "p")
    msg(store, "p", "moved the helpers", needs_reply=False)
    rm = land_big(store)
    question = "Which crate owns the parser?\n" + "Context: " + "because " * 120
    q = msg(store, "p", question, thread="step-u-work")
    for mode in ("short", "full"):
        out = next_run(store.home, "-p", "p", "--since-seq", str(since), "--settle", "0.3",
                       "--settles", mode)
        lines = out.splitlines()
        assert lines[0] == "NOTE t worker -> orchestrator: moved the helpers"
        assert lines[1] == "MSG step-u-work worker -> orchestrator: Which crate owns the parser?"
        assert lines[2] == "  " + question.splitlines()[1].strip()  # whole, never cut
        assert lines[3] == HEAD and lines[-1] == f"seq {q}" and rm < q
    recs = [json.loads(x) for x in next_run(store.home, "-p", "p", "--since-seq", str(since),
                                            "--settle", "0.3", "--json").splitlines()[:-1]]
    assert [r["kind"] for r in recs] == ["message", "message", "step.status"]
    assert recs[1]["body"] == question


def test_a_bad_cut_is_refused(store):
    create(store, "p", lane())
    p = subprocess.run([sys.executable, "-m", "sluice.cli", "next", "-p", "p", "--cut", "0",
                        "--timeout", "0"], env={**os.environ, "SLUICE_HOME": str(store.home)},
                       capture_output=True, text=True, timeout=30, check=False)
    assert p.returncode == 1 and "--cut" in p.stderr


async def test_the_mcp_tool_takes_settles_and_puts_messages_first(store):
    create(store, "p", big_lane())
    since = L.last_seq(store.home, "p")
    land_big(store)
    msg(store, "p", "which crate? " * 60, thread="step-u-work")
    args = {"projects": ["p"], "since_seq": since, "timeout": 5, "settle": 0.3}
    async with Client(build_server(store)) as c:
        got = {}
        for mode in (None, "short", "full", "none"):
            r = await c.call_tool("next", {**args, **({"settles": mode} if mode else {})})
            assert not r.is_error, r.content[0].text
            got[mode] = json.loads(r.content[0].text)
    assert got[None] == got["short"]  # short is the default
    first, settled = got["short"]["records"]
    assert first["kind"] == "message" and first["body"] == "which crate? " * 60
    work = settled["unit"]["steps"][1]
    assert work["outputs"] == {"landed": True, "summary": "Landed the parser split.",
                               "sha": "127c2443a5"}
    assert work["omitted"] == ["evidence", "unresolved"]
    assert got["full"]["records"][1]["unit"]["steps"][1]["outputs"]["evidence"] == \
        BIG_OUT["evidence"]
    assert "omitted" not in got["full"]["records"][1]["unit"]["steps"][1]
    assert all("outputs" not in s for s in got["none"]["records"][1]["unit"]["steps"])
