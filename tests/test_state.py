"""State entries: `entry_of` (absent means pending) and the constructors."""

import re

from sluice import state as S

ISO = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")


def test_entry_of():
    state = {"inputs": {}, "steps": {"a": {"status": "failed", "error": "boom"}}}
    assert S.entry_of(state, "a") is state["steps"]["a"]
    assert S.entry_of(state, "b") == {"status": "pending"}
    assert S.entry_of({"steps": {}}, "b") == {"status": "pending"}


def test_pending():
    assert S.pending() == {"status": "pending"}
    assert S.pending() is not S.pending()  # a fresh dict each call


def test_pending_kept():
    kept = {"inputs_hash": "h" * 32, "run_ids": ["r0", "r1"],
            "results": [{"x": 1}, None]}
    assert S.pending_kept(kept) == {"status": "pending", "kept": kept}


def test_running():
    e = S.running("h" * 32)
    assert e["status"] == "running" and e["run_ids"] == [] and e["inputs_hash"] == "h" * 32
    assert ISO.match(e["started"])


def test_succeeded_and_failed():
    ok = S.succeeded({"x": 1})
    assert ok["status"] == "succeeded" and ok["outputs"] == {"x": 1} and ok["error"] is None
    assert ISO.match(ok["finished"])
    bad = S.failed("boom")
    assert bad["status"] == "failed" and bad["outputs"] is None and bad["error"] == "boom"
    assert ISO.match(bad["finished"])


def test_skipped():
    e = S.skipped("check/ok is false")
    assert e["status"] == "skipped" and e["skipped"] == "check/ok is false"
    assert ISO.match(e["finished"])


def test_manual():
    e = S.manual({"x": 1}, "h" * 32)
    assert e == {"status": "succeeded", "started": None, "finished": e["finished"],
                 "outputs": {"x": 1}, "manual": True, "inputs_hash": "h" * 32}
    assert S.manual({}, None)["inputs_hash"] is None


def test_cancel():
    assert S.cancel("stop it") == {"cancel": "stop it"}
    assert S.cancel("") == {"cancel": "cancelled"}


def test_statuses():
    assert set(S.STATUSES) == {"pending", "running", "succeeded", "skipped", "stale",
                               "failed"}
