"""status(view="units") (SPEC §8): one compact row per unit — state, age, engine, step marks,
what a blocked unit waits on, its threads' last message — and a line of at most 80
characters."""

import json
from datetime import UTC, datetime, timedelta

import pytest
from mcp import Client

from sluice.errors import BadRequest
from sluice.mcp_server import build_server
from sluice.watch import units
from tests.conftest import add, create, d, src


def ago(minutes):
    return (datetime.now(UTC) - timedelta(minutes=minutes)).strftime("%Y-%m-%dT%H:%M:%SZ")


def lane(unit, engine=None, **fork):
    """A recipe-shaped unit tagged unit:<unit>: fork -> work (an open fn, binding
    engine/model/effort from `engine`) -> landed -> close. `fork` adds keys to the fork."""
    tags = [f"unit:{unit}"]
    work_in = {"n": src(f"{unit}-fork/sum"), **{k: d(v) for k, v in (engine or {}).items()}}
    return {f"{unit}-fork": add(d(1), d(2), tags=tags, **fork),
            f"{unit}-work": {"run": "test.open", "in": work_in, "tags": tags,
                             "outputs": {"landed": "boolean"}},
            f"{unit}-landed": add(d(1), d(1), after=[f"{unit}-work"], tags=tags),
            f"{unit}-close": add(d(0), d(0), after=[f"{unit}-landed"], tags=tags)}


def put(store, project, **entries):
    """Set these steps' state entries as the runner would leave them."""
    state = store.read_state(project)
    state["steps"].update(entries)
    store.write_state(project, state)


def done(minutes, **outputs):
    return {"status": "succeeded", "started": ago(minutes + 1), "finished": ago(minutes),
            "outputs": outputs or {"sum": 3}}


def post(store, project, thread, body, needs_reply=True, frm="worker"):
    store.append(project, {"kind": "message", "thread": thread, "from": frm, "body": body,
                           "needs_reply": needs_reply})


@pytest.fixture
def board(store):
    """a running 42m on opus·xhigh with a question; b failed; c blocked after b-work; d
    done; e pending, never started; solo a standalone pending step."""
    create(store, "p", {**lane("a", {"engine": "opus", "effort": "xhigh"}), **lane("b"),
                        **lane("c", after=["b-work"]), **lane("d"), **lane("e"),
                        "solo": add(d(1), d(1))})
    put(store, "p", **{
        "a-fork": done(50), "a-work": {"status": "running", "started": ago(42)},
        "b-fork": done(30), "b-work": {"status": "failed", "started": ago(20),
                                       "finished": ago(10), "error": "boom"},
        "d-fork": done(9), "d-work": done(8, landed=True), "d-landed": done(7),
        "d-close": done(6)})
    post(store, "p", "step-a-work", "which crate owns the parser, core or cli?")
    post(store, "p", "step-b-work", "gave up", needs_reply=False)
    return store


def rows(store, **kw):
    return {r["unit"]: r for r in units(store, "p", **kw)["units"]}


def test_each_unit_state_with_its_marks_age_and_order(board):
    res = units(board, "p")
    assert res["done_units"] == {"units": 1, "steps": 4}  # d is done: left out
    got = {r["unit"]: r for r in res["units"]}
    assert {u: r["state"] for u, r in got.items()} == {
        "a": "running", "b": "failed", "c": "blocked", "e": "pending", "solo": "pending"}
    assert got["a"]["steps"] == "fork✓ work▶ landed· close·"
    assert got["b"]["steps"] == "fork✓ work✗ landed· close·"
    assert got["solo"]["steps"] == "solo·"
    assert 42 * 60 <= got["a"]["age"] < 43 * 60  # how long its running step has run
    assert 10 * 60 <= got["b"]["age"] < 11 * 60  # since its last change
    assert got["e"]["age"] is None  # nothing about it ever changed
    assert [r["unit"] for r in res["units"]][:2] == ["a", "b"]  # oldest first, unknown last
    assert rows(board, every=True)["d"]["state"] == "settled"


def test_engine_blocked_edge_and_last_message(board):
    got = rows(board)
    assert got["a"]["engine"] == "opus·xhigh" and got["b"]["engine"] == ""
    assert got["c"]["blocked"] == "after b-work (failed)"
    assert got["a"]["blocked"] == ""
    assert got["a"]["last"] == "Q: which crate owns the parser, core or cli?"
    assert got["b"]["last"] == "gave up"  # a note is no question


def test_a_paused_step_marks_and_blocks_its_unit(store):
    create(store, "p", lane("u", paused="waiting for the owner"))
    got = rows(store)["u"]
    assert got["state"] == "blocked" and got["steps"] == "fork‖ work· landed· close·"
    assert got["blocked"] == "paused: waiting for the owner"


def test_the_line_fits_in_80_characters(board):
    got = rows(board)
    line = got["a"]["line"]
    assert line.startswith("a") and "▶ 42m" in line and "opus·xhigh" in line
    assert "fork✓ work▶ landed· close·" in line and '"Q: which crate' in line
    assert all(len(r["line"]) <= 80 for r in got.values())
    assert "after b-work (failed)" in got["c"]["line"]


def test_a_long_name_and_message_are_cut_to_fit(store):
    name = "a-very-long-unit-name-for-a-figure-4201"
    create(store, "p", lane(name, {"engine": "codex", "model": "sol", "effort": "high"}))
    put(store, "p", **{f"{name}-work": {"status": "running", "started": ago(5)}})
    post(store, "p", f"step-{name}-work", "why " * 200)
    row = units(store, "p")["units"][0]
    assert len(row["line"]) <= 80 and "…" in row["line"]
    assert row["line"].startswith("a-very-long-unit-…")
    assert "codex·sol·high" in row["line"] and "▶" in row["line"]
    assert row["unit"] == name and len(row["last"]) <= 200 and row["last"].startswith("Q: why")


def test_state_and_tags_filter(board):
    assert list(rows(board, state="failed")) == ["b"]
    assert set(rows(board, state=["blocked", "pending"])) == {"c", "e", "solo"}
    assert list(rows(board, tags=["unit:d"])) == ["d"]  # a selection shows done units too
    assert list(rows(board, steps=["a-landed"])) == ["a"]
    with pytest.raises(BadRequest):
        units(board, "p", state="stuck")


async def test_the_status_tool_takes_the_units_view(board):
    async with Client(build_server(board)) as c:
        r = await c.call_tool("status", {"project": "p", "view": "units",
                                         "state": "running"})
        res = json.loads(r.content[0].text)
        assert [u["unit"] for u in res["units"]] == ["a"]
        assert sorted(res["units"][0]) == ["age", "blocked", "engine", "last", "line",
                                           "state", "steps", "unit"]
        r = await c.call_tool("status", {"project": "p", "state": "running"})
        assert r.is_error and "units view" in json.loads(r.content[0].text)["message"]
        r = await c.call_tool("status", {"project": "p"})  # the default view is unchanged
        assert "steps" in json.loads(r.content[0].text)
