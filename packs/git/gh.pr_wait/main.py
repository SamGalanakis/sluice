# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""gh.pr_wait: poll a PR until checks settle, it merges/closes/conflicts, or timeout."""

import json
import sys
import time
from pathlib import Path

from sluice.fn import ShError, Transient, run, sh

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _git.refs import ref

FIELDS = "state,mergeable,headRefOid,url,statusCheckRollup"
FAILED_CONCLUSIONS = {"FAILURE", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED",
                      "STARTUP_FAILURE"}
FAILED_STATES = {"FAILURE", "ERROR"}


def pr_data(inp):
    """One `gh pr view` poll; a failing call is worth a retry."""
    try:
        return json.loads(
            sh(["gh", "pr", "view", ref("pr", inp["pr"]), "--json", FIELDS],
               cwd=inp["path"]).stdout)
    except ShError as e:
        raise Transient(f"gh pr view failed: {e}") from e


def checks(rollup):
    """statusCheckRollup -> (any still pending, names of the failed ones)."""
    pending, failed = False, []
    for c in rollup or []:
        if "status" in c:  # a CheckRun
            if c["status"] != "COMPLETED":
                pending = True
            elif c.get("conclusion") in FAILED_CONCLUSIONS:
                failed.append(c.get("name") or c.get("workflowName") or "check")
        elif "state" in c:  # a StatusContext
            if c["state"] in FAILED_STATES:
                failed.append(c.get("context") or "context")
            elif c["state"] != "SUCCESS":
                pending = True
    return pending, failed


def main(inp, ctx):
    interval = 60 if inp.get("interval") is None else inp["interval"]
    timeout = 21600 if inp.get("timeout") is None else inp["timeout"]
    # A PR's checks register a few seconds after it is opened or pushed: until then the rollup
    # is empty, which is not "no CI". Only an empty rollup this long counts as green.
    no_checks = 120 if inp.get("no_checks_s") is None else inp["no_checks_s"]
    start = time.monotonic()
    deadline = start + timeout
    state, failed, sha, url = "timeout", [], "", ""
    while True:
        data = pr_data(inp)
        sha = data.get("headRefOid") or sha
        url = data.get("url") or url
        if data.get("state") == "MERGED":
            state, failed = "merged", []
            break
        if data.get("state") == "CLOSED":
            state, failed = "closed", []
            break
        if data.get("mergeable") == "CONFLICTING":
            state, failed = "conflicting", []
            break
        rollup = data.get("statusCheckRollup") or []
        pending, failed = checks(rollup)
        if not rollup and time.monotonic() - start < no_checks:
            pending = True
        if failed and not pending:
            state = "red"
            break
        if inp["until"] == "checks" and not pending:
            state, failed = "green", []
            break
        if time.monotonic() >= deadline:
            state = "timeout"
            break
        time.sleep(min(interval, max(0.0, deadline - time.monotonic())))
    return {"state": state, "sha": sha, "url": url, "failed": failed}


if __name__ == "__main__":
    run(main, retries=3)
