# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""figments.worker: agent.run with the figments header and a worker-written summary."""

import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from sluice_fn import run, sh

SUMMARY_WORDS = 120
FALLBACK_CHARS = 1500


def worker_header(inp, ctx):
    """The mechanics every figments worker shares; everything else is in the step's own spec."""
    fork = inp["cwd"]
    work = (f"You work in the kiln fork {fork}: run `cd {fork} && . ./env.sh` before any kiln, "
            "devenv or cargo command. Build, check, clippy, fmt and test through kiln (Bazel), "
            "not cargo. Never write outside the fork except the report and output files your "
            "spec names, and never touch /workspace/code/figments (Sam's own checkout). Never "
            "git stash (forks share one stash). Production and Scaleway are read-only. Your "
            "session ends when your turn ends: run builds and tests in the foreground and wait "
            "for them. You implement; do not spawn subagents. A question you cannot settle "
            "within scope goes in your report's last paragraph; otherwise decide and continue. "
            "No AI attribution anywhere: no Co-Authored-By trailer, no \"Generated with\" line, "
            "no mention of Claude, Anthropic, Codex, Devin or any AI in commits, PR text or code.")
    commit = ("Commit on the fork's branch (stage exact paths; never commit -a); titles are "
              "imperative, plain sentences of the outcome; pre-commit must pass (never "
              "--no-verify).")
    if inp.get("read_only"):
        vcs = ("Read-only: change no files in the fork and commit nothing. Your deliverables are "
               "the report and output files your spec names, outside the fork.")
    elif inp.get("pr"):
        vcs = (commit + " Then push the branch (`git push -u origin HEAD`; after a rebase, "
               "`--force-with-lease`) and open a PR against main with `gh pr create --base main "
               "--body-file <file>` (or update the open PR's body with `gh pr edit`). Never push "
               "to main, never merge, never enable auto-merge: Sam merges. Submit pr_url when "
               "the step declares it.")
    else:
        vcs = commit + " Do not push and do not open a PR; the plan owns that."
    output = ("Output: before you finish, write $TASK/summary.txt, 120 words or fewer: outcome, "
              "branch and HEAD SHA (and PR URL if any), gates with counts, and open items (say "
              "none if there are none). It becomes the step's summary and final. Submit any other "
              "declared outputs with step_submit.")
    return f"{work}\n\n{vcs}\n\n{output}".replace("$TASK", str(ctx.run_dir)) + "\n\n"


def bounded_summary(text):
    """Keep the worker's own report within the 120-word contract."""
    text = text.strip()
    words = list(re.finditer(r"\S+", text))
    return text[:words[SUMMARY_WORDS - 1].end()] if len(words) > SUMMARY_WORDS else text


def fallback_final(inp, out, run_dir):
    """agent.run's final message, else a harness final file."""
    message = out.get("final") or ""
    if message.strip():
        return message.strip()[:FALLBACK_CHARS]
    engine = inp["engine"]
    harness = run_dir / ("claude.log.final" if engine == "opus" else f"{engine}.log.final")
    return harness.read_text().strip()[:FALLBACK_CHARS] if harness.exists() else ""


def main(inp, ctx):
    engine = {"devin": "devin", "opus": "claude", "codex": "codex"}[inp["engine"]]
    if not (inp.get("spec") or "").strip():
        raise ValueError("empty spec: the step's spec text (or spec file) is empty")
    if inp.get("pr") and inp.get("read_only"):
        raise ValueError("pr and read_only exclude each other")
    if inp.get("effort") and inp["engine"] != "codex":
        raise ValueError("effort is for the codex engine")
    if inp.get("model") and inp["engine"] != "codex":
        raise ValueError("sol and astra are codex models; opus and devin take no model")
    spec = ctx.header(worker_header(inp, ctx) + inp["spec"])
    fwd = {k: v for k, v in inp.items() if k not in ("pr", "read_only")}
    out = ctx.builtin("agent.run", {**fwd, "engine": engine, "spec": spec,
                                    "report_path": str(ctx.run_dir / "summary.txt")})
    sent = ctx.submission()
    claimed = sent.get("head_sha")
    if isinstance(claimed, str) and claimed:
        head = sh(["git", "-C", inp["cwd"], "rev-parse", "HEAD"]).stdout.strip()
        if not head.startswith(claimed):
            raise RuntimeError(f"the worker reported head_sha {claimed} but the fork's HEAD is "
                               f"{head}")
    summary = bounded_summary(out.get("report") or "") or fallback_final(inp, out, ctx.run_dir)
    return {"summary": summary, "final": summary, "session": out["session"]}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
