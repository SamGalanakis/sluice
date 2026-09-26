# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""gh.run_latest: the latest GitHub Actions run on a branch, with its failed jobs."""

import json

from sluice.fn import run, sh

FAILED = {"failure", "cancelled", "timed_out"}


def main(inp, ctx):
    path = inp["path"]
    branch = inp.get("branch") or "main"
    argv = ["gh", "run", "list", "--branch", branch]
    if inp.get("workflow"):
        argv += ["--workflow", inp["workflow"]]
    argv += ["--limit", "1", "--json",
             "databaseId,headSha,status,conclusion,url,workflowName"]
    runs = json.loads(sh(argv, cwd=path).stdout)
    if not runs:
        raise RuntimeError(f"no runs on {branch}")
    r = runs[0]
    failed_jobs = []
    if r.get("conclusion") in FAILED:
        jobs = json.loads(
            sh(["gh", "run", "view", str(r["databaseId"]), "--json", "jobs"],
               cwd=path).stdout)
        failed_jobs = [j["name"] for j in jobs.get("jobs", [])
                       if j.get("conclusion") in FAILED]
    return {
        "run_id": r["databaseId"],
        "sha": r["headSha"],
        "status": r["status"],
        "conclusion": r.get("conclusion"),
        "url": r["url"],
        "workflow": r["workflowName"],
        "failed_jobs": failed_jobs,
    }


if __name__ == "__main__":
    run(main)
