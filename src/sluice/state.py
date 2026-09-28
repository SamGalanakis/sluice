"""A project's state entries (SPEC §6): the one definition of their shape.

Absent means pending: a step with no entry has not run. Every entry carries `status`
(one of STATUSES); the constructors below give the rest of the shape, timestamps
included, for each kind of write the runner, the plan and the store make.
"""

from __future__ import annotations

from typing import Any

from .util import now_iso

STATUSES = ("pending", "running", "succeeded", "skipped", "stale", "failed")


def entry_of(state: dict[str, Any], sid: str) -> dict[str, Any]:
    """The step's entry in the state; absent means pending."""
    return state["steps"].get(sid, pending())


def pending() -> dict[str, Any]:
    """A fresh entry: what a retried or un-skipped step resets to."""
    return {"status": "pending"}


def pending_kept(kept: dict[str, Any]) -> dict[str, Any]:
    """A retried scattered step, pending again: `kept` ({inputs_hash, run_ids, results}
    of the failed run) lets the runner re-use the items that already succeeded."""
    return {"status": "pending", "kept": kept}


def running(inputs_hash: str) -> dict[str, Any]:
    """The entry a step starts with (its `run_ids` fill in as the runs start)."""
    return {"status": "running", "started": now_iso(), "run_ids": [],
            "inputs_hash": inputs_hash}


def succeeded(outputs: Any) -> dict[str, Any]:
    """A finished run's success; merged into the running entry, which keeps `started`,
    `run_ids` and friends."""
    return {"status": "succeeded", "finished": now_iso(), "outputs": outputs,
            "error": None}


def failed(error: str) -> dict[str, Any]:
    """A finished run's failure (a cancelled run is one whose error says so)."""
    return {"status": "failed", "finished": now_iso(), "outputs": None, "error": error}


def skipped(reason: str) -> dict[str, Any]:
    """Skipped without running: `when` was false, or it reads a skipped step."""
    return {"status": "skipped", "skipped": reason, "finished": now_iso()}


def manual(outputs: Any, inputs_hash: str | None) -> dict[str, Any]:
    """step_set_output: succeeded, set by hand; `inputs_hash` is null when forced."""
    return {"status": "succeeded", "started": None, "finished": now_iso(),
            "outputs": outputs, "manual": True, "inputs_hash": inputs_hash}


def cancel(reason: str) -> dict[str, str]:
    """The flag step_cancel sets on a running entry; the runner fails it next tick."""
    return {"cancel": reason or "cancelled"}
