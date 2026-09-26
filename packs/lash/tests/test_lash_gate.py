"""lash.gate: green passes, red spawns the next attempt, the last red fails (inbox)."""

import json

import pytest
from helpers import check_graph, registry, run_fn

BRIEF = "Read /workspace/notes/lash/lane-rules.md ... ORIGINAL BRIEF"


def gate_input(**over):
    inp = {
        "name": "fig-1-store", "attempt": 1, "engine": "devin", "brief": BRIEF,
        "session": "sess-1", "model": "swe-2", "path": "/forks/fig-1-store",
        "branch": "fig-1-store", "base": "origin/fig-1", "arc": "fig-1",
        "labels": ["//crates/a:tests"], "report_path": "/r.md",
        "ok": False, "summary": "L1 red: 4 test cases executed",
        "log": "noise\n" * 5000 + "LAST LOG LINE", "failures": ["//crates/a:tests m::bad"],
    }
    inp.update(over)
    return inp


@pytest.fixture
def plan_home(tmp_path):
    """A plan whose top-level task node holds the pg-schema claim."""
    d = tmp_path / "home" / "plans" / "p"
    d.mkdir(parents=True)
    d.joinpath("plan.json").write_text(json.dumps({
        "id": "p", "resources": {"pg-schema": 1},
        "nodes": {"store": {"fn": "lash.task", "in": {}, "claims": ["pg-schema"]}}}))
    return {"SLUICE_NODE": "store/attempt/gate"}


def spawned(out):
    nodes = out["_spawn"]["nodes"]
    assert len(nodes) == 1
    return next(iter(nodes.items()))


def test_green_passes_without_spawning(tmp_path):
    r = run_fn("lash.gate", gate_input(ok=True), tmp_path)
    assert r.code == 0, r.stderr
    assert r.out == {"ok": True, "attempt": 1, "next": None, "summary": "L1 green on attempt 1"}


def test_red_attempt_1_resumes_the_warm_session_with_the_failing_output(tmp_path, plan_home):
    r = run_fn("lash.gate", gate_input(), tmp_path, plan_home)
    assert r.code == 0, r.stderr
    assert r.out["ok"] is False and r.out["next"] == "fig-1-store-a2"
    node_id, node = spawned(r.out)
    assert node_id == "fig-1-store-a2"
    assert node["fn"] == "lash.attempt"
    assert node["claims"] == ["pg-schema"]
    ins = {k: b["value"] for k, b in node["in"].items()}
    assert ins["attempt"] == 2 and ins["engine"] == "devin" and ins["resume"] == "sess-1"
    assert ins["model"] == "swe-2"
    assert ins["ladder"] == ["devin", "devin", "claude"]
    assert "//crates/a:tests m::bad" in ins["spec"] and "LAST LOG LINE" in ins["spec"]
    assert "ORIGINAL BRIEF" not in ins["spec"], "a warm session already has the brief"
    assert ins["brief"] == BRIEF and ins["path"] == "/forks/fig-1-store"
    # The spawned node is a valid plan node for lash.attempt.
    plan_nodes = {node_id: node}
    assert check_graph(plan_nodes, registry(), None, "spawn", {"pg-schema": 1}) == []


def test_red_attempt_2_escalates_to_a_fresh_engine(tmp_path, plan_home):
    inp = gate_input(attempt=2, ladder=["devin", "devin", "claude"])
    r = run_fn("lash.gate", inp, tmp_path, {"SLUICE_NODE": "fig-1-store-a2/gate"})
    assert r.code == 0, r.stderr
    node_id, node = spawned(r.out)
    assert node_id == "fig-1-store-a3"
    ins = {k: b["value"] for k, b in node["in"].items()}
    assert ins["engine"] == "claude" and ins["attempt"] == 3
    assert "resume" not in ins and "model" not in ins
    assert ins["spec"].startswith(BRIEF)
    assert "//crates/a:tests m::bad" in ins["spec"]


def test_red_on_the_last_rung_fails_for_the_inbox(tmp_path):
    r = run_fn("lash.gate", gate_input(attempt=3), tmp_path)
    assert r.code == 1
    msg = r.error["message"]
    assert "L1 red after 3 attempts" in msg and "/forks/fig-1-store" in msg
    assert "m::bad" in msg
    assert not (r.run_dir / "output.json").exists()


def test_ladder_length_is_the_budget(tmp_path):
    r = run_fn("lash.gate", gate_input(ladder=["codex"]), tmp_path)
    assert r.code == 1
    r = run_fn("lash.gate", gate_input(engine="codex", ladder=["codex", "codex"]), tmp_path)
    assert r.code == 0
    assert spawned(r.out)[1]["in"]["resume"] == {"value": "sess-1"}


def test_spawn_id_avoids_existing_nodes(tmp_path, plan_home):
    p = tmp_path / "home" / "plans" / "p" / "plan.json"
    doc = json.loads(p.read_text())
    doc["nodes"]["fig-1-store-a2"] = {"fn": "lash.attempt", "in": {}}
    p.write_text(json.dumps(doc))
    r = run_fn("lash.gate", gate_input(), tmp_path, plan_home)
    assert r.out["next"] == "fig-1-store-a2-2"

