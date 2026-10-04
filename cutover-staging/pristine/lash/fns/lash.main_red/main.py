# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.main_red: the next finished CI run on main, and whether it is red."""

import json
import re
import time

from sluice.fn import run, sh

FIELDS = "databaseId,headSha,status,conclusion,workflowName,event,url"
DEFAULT_WORKFLOW = "CI"  # name of .github/workflows/ci.yml in gh run list
BAZEL_FAILURE = re.compile(r"(//\S+)\s+(?:FAILED|TIMEOUT) in\b")
LIBTEST_FAILURE = re.compile(r"\btest (\S+) \.\.\. FAILED\b")
EFFECT_FAILURE = re.compile(
    r"\[\d+/\d+\]\s+(?:TIMED OUT|FAILED)\s+(?:\d+(?:\.\d+)?s\s+)?(\S+)")
GATE_COMMAND = re.compile(r"(?:^|\s)- (.+?)\s*$")


def gh(args, repo):
    return json.loads(sh(["gh", *args], cwd=repo).stdout or "null")


def parse_failed_tests(log):
    """Extract test labels and gate commands from one failed job's log."""
    found = []
    in_gate_list = False
    for line in log.splitlines():
        if "gate commands failed:" in line:
            in_gate_list = True
            continue
        if in_gate_list:
            command = GATE_COMMAND.search(line)
            if command:
                found.append(command.group(1))
                continue
            in_gate_list = False
        for pattern in (BAZEL_FAILURE, LIBTEST_FAILURE, EFFECT_FAILURE):
            match = pattern.search(line)
            if match:
                found.append(match.group(1))
                break
    return list(dict.fromkeys(found))


def next_run(inp):
    workflow = inp.get("workflow") or DEFAULT_WORKFLOW
    event = inp.get("event") or "workflow_dispatch"
    runs = gh(["run", "list", "--branch", "main", "--workflow", workflow,
               "--event", event, "--limit", "30", "--json", FIELDS], inp["repo"])
    after = inp.get("after_run") or 0
    runs = [r for r in runs if r["databaseId"] > after and r.get("event") == event
            and (workflow.endswith((".yml", ".yaml"))
                 or r.get("workflowName") == workflow)]
    done = [r for r in runs if r["status"] == "completed"]
    return min(done, key=lambda r: r["databaseId"]) if done else None


def main(inp, ctx):
    interval = inp.get("interval") or 120
    while (r := next_run(inp)) is None:
        ctx.log(f"no finished run on main yet; checking again in {interval}s")
        time.sleep(interval)
    jobs = gh(["run", "view", str(r["databaseId"]), "--json", "jobs"], inp["repo"])["jobs"]
    failed_jobs = [j for j in jobs if j.get("conclusion") == "failure"]
    failed_tests = []
    for job in failed_jobs:
        log = sh(["gh", "run", "view", "--job", str(job["databaseId"]), "--log"],
                 cwd=inp["repo"]).stdout
        failed_tests.extend(parse_failed_tests(log))
    return {"run_id": r["databaseId"], "sha": r["headSha"], "conclusion": r["conclusion"] or "",
            "red": r["conclusion"] not in ("success", "skipped", "neutral"), "url": r["url"],
            "failed_jobs": [j["name"] for j in failed_jobs],
            "failed_job_ids": [j["databaseId"] for j in failed_jobs],
            "failed_tests": list(dict.fromkeys(failed_tests))}


if __name__ == "__main__":
    run(main)
