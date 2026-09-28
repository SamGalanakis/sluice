"""core.external (SPEC §5, §6, §8): a step whose work happens outside sluice. The runner never
starts it; once ready it waits until someone sets its outputs by hand or cancels it."""

import re

import pytest

from sluice import calls as C
from sluice import log as L
from sluice import plan as P
from sluice import views, watch
from sluice.errors import BadRequest, InvalidPlan
from sluice.registry import BUILTIN_DIR, load
from sluice.store import EXTERNAL_WAIT
from tests.conftest import TESTPACK, add, create, d, settle, src, statuses

REG = load({"builtin": [BUILTIN_DIR], "global": [TESTPACK]})


def external(outputs=None, **ins):
    step = {"run": "core.external", "doc": "Five workers of the lash orchestrator", "in": ins}
    return {**step, "outputs": outputs} if outputs is not None else step


def waiting(store, project, sid):
    return next(s for s in store.status(project, all=True)["steps"] if s["id"] == sid) \
        .get("waiting")


def card(page, sid):
    """A step's card on the board, up to its end."""
    return re.search(rf'<a class="node [^>]*id="n-{sid}".*?</a>', page, re.DOTALL)[0]


def kinds(store, project, since):
    return [(r["kind"], r.get("to")) for r in L.read(store.home, project, since)["records"]]


# ---- the fn ------------------------------------------------------------------------------

def test_it_loads_as_an_open_builtin_with_no_main_py():
    fn = REG.get("core.external")
    assert fn is not None and fn.scope == "builtin" and fn.open and fn.external
    assert not fn.native and fn.inputs == {} and fn.outputs == {}
    assert not (BUILTIN_DIR / "core.external" / "main.py").exists()
    assert "never starts it" in fn.doc
    # only the built-in is external: a fn of that name elsewhere is a collision, not it
    assert not REG.get("core.echo").external


def test_fn_call_refuses_it(store):
    with pytest.raises(BadRequest, match="outside sluice and never runs"):
        C.create(store, "core.external", {}, None)


def test_it_binds_extra_inputs_declares_outputs_and_does_not_scatter():
    errs, plan = P.validate({"inputs": {"items": "string[]"}, "outputs": {}, "steps": {
        "a": add(d(1), d(2)),
        "x": external({"url": "string"}, n=src("a/sum"), note=d("hi")),
        "s": {**external(item=src("items")), "scatter": "item"},
    }}, REG)
    assert errs == [("steps.s.scatter: fn core.external is one piece of work done outside "
                     "sluice; it does not scatter")]
    x = plan.steps["x"]
    assert {k: str(t) for k, t in x.extra.items()} == {"n": "int", "note": "Any"}
    assert {k: str(t) for k, t in x.outputs.items()} == {"url": "string"}


# ---- the runner --------------------------------------------------------------------------

def test_a_ready_external_step_is_never_started(store, runner):
    create(store, "p", {"a": add(d(1), d(2)), "x": external({"url": "string"}, n=src("a/sum")),
                        "y": {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "after": ["x"]}})
    store.pause_steps("p", ["a", "x", "y"], paused=False)
    assert waiting(store, "p", "x") == ["step a is pending"]
    settle(runner, store, "p", lambda s: s.get("a", {}).get("status") == "succeeded")
    since = L.last_seq(store.home, "p")
    for _ in range(5):
        runner.tick()
    assert statuses(store, "p") == {"a": "succeeded", "x": "pending", "y": "pending"}
    assert L.last_seq(store.home, "p") == since  # nothing logged for becoming ready
    assert not [k for k in runner.active if k[0] == "step"]  # no process
    runs = store.runs_dir("p")
    a_runs = set(store.read_state("p")["steps"]["a"]["run_ids"])
    assert {p.name for p in runs.iterdir()} <= a_runs  # no run dir of its own
    assert waiting(store, "p", "x") == [EXTERNAL_WAIT]
    assert waiting(store, "p", "y") == ["after step x, which is pending"]


def test_when_false_skips_it_and_a_paused_one_says_paused(store, runner):
    create(store, "p", {"x": {**external(), "when": "go"},
                        "z": {**external(), "paused": "not yet"}}, inputs={"go": "boolean"})
    store.pause_steps("p", ["x"], paused=False)
    store.set_input("p", "go", False, "t", "t")
    runner.tick()
    assert statuses(store, "p") == {"x": "skipped", "z": "pending"}
    assert waiting(store, "p", "z") == ["paused: not yet"]
    store.update_project("p", paused=True)
    store.pause_steps("p", ["z"], paused=False)
    assert waiting(store, "p", "z") == ["the project is paused"]


# ---- settling it -------------------------------------------------------------------------

def test_set_by_hand_its_dependents_run_and_a_retry_makes_it_wait_again(store, runner):
    create(store, "p", {"x": external({"n": "int"}),
                        "y": {"run": "test.add", "in": {"a": src("x/n"), "b": d(1)}}})
    store.pause_steps("p", ["x", "y"], paused=False)
    runner.tick()
    store.set_output("p", "x", {"n": 41}, "t", "landed")
    e = store.read_state("p")["steps"]["x"]
    assert e["status"] == "succeeded" and e["manual"] is True
    steps = settle(runner, store, "p", lambda s: s.get("y", {}).get("status") == "succeeded")
    assert steps["y"]["outputs"] == {"sum": 42}
    store.retry("p", ["x"])
    runner.tick()
    assert store.read_state("p")["steps"]["x"]["status"] == "pending"
    assert waiting(store, "p", "x") == [EXTERNAL_WAIT]


def test_cancel_fails_a_pending_external_step_and_still_refuses_other_pending_ones(store,
                                                                                   runner):
    create(store, "p", {"x": external(), "y": add(d(1), d(2), paused=True)})
    store.pause_steps("p", ["x"], paused=False)
    runner.tick()
    before = store.read_state("p")
    with pytest.raises(BadRequest, match=r"only a running step \(or a pending core.external "
                                         r"one\) can be cancelled: y is pending"):
        store.cancel_steps("p", ["x", "y"], author="t", reason="gone")
    assert store.read_state("p") == before  # refused, changing nothing
    since = L.last_seq(store.home, "p")
    assert store.cancel_steps("p", ["x"], author="t", reason="moved to CI") == ["x"]
    e = store.read_state("p")["steps"]["x"]
    assert e["status"] == "failed" and e["error"] == "cancelled: moved to CI"
    assert kinds(store, "p", since) == [("step.cancel", None), ("step.status", "failed")]
    runner.tick()  # the runner has nothing to stop
    assert store.read_state("p")["steps"]["x"]["error"] == "cancelled: moved to CI"
    store.retry("p", ["x"])
    store.cancel_steps("p", ["x"])
    assert store.read_state("p")["steps"]["x"]["error"] == "cancelled"


# ---- sluice next -------------------------------------------------------------------------

def test_setting_its_outputs_wakes_the_orchestrator(store):
    create(store, "p", {"x": external({"n": "int"}),
                        "y": {"run": "test.add", "in": {"a": src("x/n"), "b": d(1)}}})
    since = L.last_seq(store.home, "p")
    store.set_output("p", "x", {"n": 1}, "t", "t")
    recs = [{**r, "project": "p"} for r in L.read(store.home, "p", since)["records"]]
    done = [r for r in recs if r["kind"] == "step.status"]
    assert [watch._classify(store, r, "orchestrator") for r in done] == ["wake"]
    assert "unit" not in done[0]  # it wakes as open work done, not as a finished unit


# ---- the board ---------------------------------------------------------------------------

def outside_project(store):
    """`fork` succeeded; `ext`, external and ready, reads it, with `c1` -> `c2` -> `c3`
    behind it; `later`, external, waits on `run`, which is running; `bad` failed."""
    one = {"run": "test.add", "in": {"a": d(1), "b": d(1)}}
    create(store, "v", {
        "fork": one, "ext": external({"url": "string"}, n=src("fork/sum")),
        "c1": {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "after": ["ext"]},
        "c2": {"run": "test.add", "in": {"a": src("c1/sum"), "b": d(1)}},
        "c3": {"run": "test.add", "in": {"a": src("c2/sum"), "b": d(1)}},
        "run": one, "later": {**external(), "after": ["run"]}, "bad": one})
    with store.tx():
        store.write_state("v", {"inputs": {}, "steps": {
            "fork": {"status": "succeeded", "outputs": {"sum": 2},
                     "started": "2026-09-28T10:00:00Z", "finished": "2026-09-28T10:05:00Z"},
            "run": {"status": "running", "started": "2026-09-28T10:00:00Z", "run_ids": []},
            "bad": {"status": "failed", "error": "boom"}}})


def test_a_ready_external_step_is_live_outside_work_not_a_halt(store):
    outside_project(store)
    board = views.load_board(store, "v")
    assert board.blocks["ext"].mark == "external" and board.blocks["ext"].status == "pending"
    assert board.blocks["later"].mark == "pending"  # not ready: any pending step
    assert not board.halts("ext")
    assert {"ext", "c1", "c2", "c3"}.isdisjoint(board.unreachable)
    page = views.project_page(store, "v", ver="x")
    ext = card(page, "ext")
    assert ext.startswith('<a class="node card is-external"')
    assert '<span class="g g-external" title="external">' in ext
    assert '<span class="vh">external, </span>' in ext
    assert re.search(r'<span class="dur">outside · <time datetime="2026-09-28T10:05:00Z" '
                     r'data-since="2026-09-28T10:05:00Z">', ext)
    for sid in ("c1", "c2", "c3"):  # its dependents stay on the default Runnable board
        assert f'id="n-{sid}"' in page
    assert card(page, "later").startswith('<a class="node card is-pending')
    # it counts as pending; the stuck line names only the failure, and does not say
    # "Stopped" while outside work goes on
    assert board.counts["pending"] == 5
    stuck = re.search(r'<p class="stuck">.*?</p>', page)[0]
    assert "bad</a> failed" in stuck and "ext" not in stuck and "Stopped:" not in stuck
    # with nothing ready outside it, the line says Stopped again
    views_board = views.load_board(store, "v")
    views_board.blocks["ext"].outside = False
    views_board.blocks["run"].entry["status"] = "pending"
    assert "Stopped:" in views.attention(views_board, lambda s: s)


def test_an_external_step_without_waits_says_just_outside(store):
    create(store, "v", {"x": external({"url": "string"})})
    page = views.project_page(store, "v", ver="x")
    assert '<span class="dur">outside</span>' in page
    index = views.index(store, ver="x")
    assert "Waiting on 1 step done outside sluice." in index


def test_its_drawer_shows_its_doc_outputs_and_how_to_settle_it(store):
    create(store, "v", {"x": {**external({"url": {"type": "string", "doc": "The PR"}}),
                              "doc": "Done by **five workers** of lash, in `wt-a`..`wt-e`"}})
    html_ = views.step_detail(store, "v", "x")
    assert "Outside sluice" in html_
    assert '<div class="outside md"><p>Done by <strong>five workers</strong>' in html_
    assert "Set its outputs with <code>step_set_output</code> when the work lands" in html_
    assert '<p class="d-doc">' not in html_  # the doc is not said twice
    assert re.search(r'<span class="f-name">url</span>.*?not set yet.*?The PR', html_,
                     re.DOTALL)
    assert "external</span>" in html_
    store.set_output("v", "x", {"url": "u"}, "t", "t")
    assert "Outside sluice" not in views.step_detail(store, "v", "x")


# ---- moving a step's work out ------------------------------------------------------------

def test_a_failed_open_step_moves_out_by_a_patch_and_a_retry(store, runner):
    create(store, "p", {
        "w": {"run": "test.open", "in": {"attempts": d([]), "spec": d("do it"),
                                          "cwd": d("/src")},
              "outputs": {"landed": "boolean"}},
        "after": {"run": "test.add", "in": {"a": d(1), "b": d(1)}, "after": ["w"]},
        "reads": {"run": "test.add", "in": {"a": src("w/results.0"), "b": d(1)}}})
    with store.tx():
        store.write_state("p", {"inputs": {}, "steps": {
            "w": {"status": "failed", "error": "cancelled: fanned out"}}})
    rev = store.get("p")["rev"]
    move = [{"op": "replace", "path": "/steps/w/run", "value": "core.external"}]
    # its old fn's inputs are extra inputs now, and its declared outputs stay; a step reading
    # an output of the old fn needs it declared too
    with pytest.raises(InvalidPlan) as err:
        store.patch("p", rev, move, "t", "move out")
    assert err.value.errors == [("steps.reads.in.a: step w (fn core.external) has no output "
                                 "results")]
    rev = store.patch("p", rev, move + [{"op": "add", "path": "/steps/w/outputs/results",
                                         "value": "Any[]"}], "t", "move out")
    store.retry("p", ["w"])
    store.pause_steps("p", ["w", "after", "reads"], paused=False)
    for _ in range(3):
        runner.tick()
    assert statuses(store, "p")["w"] == "pending"
    assert waiting(store, "p", "w") == [EXTERNAL_WAIT]
    assert {k: str(t) for k, t in store.plan("p")[1].steps["w"].extra.items()} == {
        "attempts": "Any", "spec": "Any", "cwd": "Any"}
    store.set_output("p", "w", {"landed": True, "results": [1]}, "t", "landed")
    steps = settle(runner, store, "p")
    assert {k: e["status"] for k, e in steps.items()} == {
        "w": "succeeded", "after": "succeeded", "reads": "succeeded"}
    assert steps["reads"]["outputs"] == {"sum": 2}
