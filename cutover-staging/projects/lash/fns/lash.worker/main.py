# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.worker: agent.run with the lash lane header and a worker-written summary."""

import importlib.util
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from sluice.fn import run, sh


SUMMARY_WORDS = 120
FALLBACK_CHARS = 1500


# The single source of lash worker rules (Sam 2026-10-02: one place, conditional on inputs).
# Specs carry only what is specific to their unit; never restate anything here in a spec.

WORK = """\
You work in the kiln fork {fork}: run `cd {fork} && . ./env.sh` first and never write outside it.
- Build, test, lint and format only through kiln: `kiln check <targets>` (fast, no linking), `kiln build`, `kiln test <targets>`, `kiln clippy`, `kiln fmt`, and `kiln gate lash <fork> -- ...` for anything that needs services or raw cargo. Never run raw cargo or bazel yourself. Python scripts run with python3.
- Lash builds with Buck2. After changing Cargo manifests, dependencies or features, or adding/removing a crate, binary or test file, run `kiln sync` and commit the regenerated BUCK files and tools/buck2/target-inventory.json with your change. Never hand-edit or hand-merge generated files: regenerate them. Do not edit tools/buck2/**, third-party/ or .buckconfig unless your notes grant it.
- A first-party compile killed for memory (remote exit 9): retry that label once with `-c kiln.memory_scale=2`, and name it in your report. Never raise sizes yourself.
- Never git stash (forks share one stash); use a WIP commit. Never dispatch CI workflows. Never restart or reconfigure the shared build pool.
- Your session ends when your turn ends: run builds and tests in the foreground and wait for them. `pgrep -f <pattern>` matches its own command line; wait on exit or an output marker instead.
- You implement; do not spawn subagents. Decide within your scope; a question you cannot settle goes in your report's last paragraph, and you carry on.
- No AI attribution anywhere: no Co-Authored-By, no "Generated with", no mention of Claude, Anthropic, Codex or Devin in commits, code or text.
"""

TICKET = """\
Your ticket is {ticket}: run `/home/sam/.npm-global/bin/linear issue view {ticket}` (use that full path for every linear command; it is not always on PATH) and read it in full, including the parent and related tickets it names. The ticket is your spec; the notes after this header add only what is specific to this unit and win where they differ.
"""

DESIGN = """\
Design rules:
- Go straight to the end state: delete what your change supersedes; no shims, dual paths, compat code or upcasters.
- Version freeze until the 1.0 cut: change stored shapes in place and regenerate fixtures; never bump a guarded version constant or add a migration step. scripts/check_version_bumps.py flagging in-place changes is expected.
- The freeze covers stored format versions only. When your landing changes a Restate handler's command stream or journal shape (law L21), move JOURNAL_LOGIC_EPOCH and its synthetic-next counterpart in crates/lash-restate/src/process/admission.rs (precedent: 444432ae8b, FIG-4914). Add a witness that the new generation refuses a predecessor journal before decoding, and that the predecessor keeps its drain lane.
- Typed causes stay typed across the plugin and host boundary; you may add public variants (list them in your report).
- Hosts never drive a turn: everything goes through send() and the engine's drive. Binding designs: ADR 0101, 0109, 0110, 0111, and any ADR or design doc your ticket cites (read them before changing behaviour).
- Checks are oracles: never weaken, exempt, re-pin or hide from a check to make it pass.
- Where laws run: stores are SQLite file, SQLite memory (`SqliteStoreSet::memory()`) and PostgreSQL; engines/hosts are the in-process Restate server double, live Restate and lash-sim's effect host; upgrade proofs use the synthetic-next tier. The in-memory stores, local process registry and store-journal turn host are retired; ADR prose naming them is stale.
- Traps: adding or changing a store trait method also needs `kiln run //crates/lash-sim:cross_backend_store_differential__test -- store_trait_surface_is_fully_gated` and the method in the differential's sweep. Changing crates/lash-typescript/tests/test262/census.tsv also regenerates tests/differential/sessions/generated.json.
"""

PROOF = """\
Proof before landing (Sam: minimal and fast, fix forward; this is the whole gate and it overrides any ticket or note asking for more):
1. The tests your change adds or changes, plus any your ticket names, run ONCE on the cheapest tier (SQLite stores, the in-process Restate server double), by full test path; check in the report `kiln test` prints that they executed. "Once" bans repeat runs for confidence or flake-hunting; re-running after you change code or fix a broken invocation (one target per invocation, its own selectors only) is normal.
2. A law for a bug fails once on the unfixed code (write it first).
3. One `kiln clippy` on the final code, and `kiln fmt` right before your final commit (the pre-push hook refuses unformatted code and fails the land step).
4. Only when they apply: exports changed -> `//crates/lash:ui_fixtures` and `//crates/lash:facade_completeness`; a serialized shape changed -> `kiln build //:schema_checks`; tools/buck2 rules or CI workflows changed -> `kiln build //:schema_checks`, `python3 tools/buck2/sync.py --check` and `scripts/ci/repository-gates.sh`.
Nothing else: no repeat runs (never --runs_per_test), no dependents or affected-tests gate, no dev-test, no suites, no PostgreSQL or live-Restate legs, no E2E or soak, unless your change is in the PostgreSQL store or the Restate adapter itself (then that code's own tests, once). Main's scheduled full run covers the rest and reds are fixed forward. A failing test your change does not touch blocks nothing, unless your ticket's done-when names it (then making it pass is your task): name it in your unresolved output and carry on; never investigate it or wait for a classification.
"""

COMMIT = """\
Commits: stage exact paths (never commit -a). The title is one plain sentence of the outcome, ending " ({ticket})" only on the commit that completes the ticket, with "Closes {ticket}" in its body; a partial commit says "Part of {ticket}" in its body.
"""

QUEUED = """\
Landing: the plan's land step lands your commit; you never push. Rebase onto main (`git pull --rebase origin main`) right before your final commit, resolving any conflicts and checking the resolved code, so the land step applies cleanly; you may also rebase while you work to pick up fixes. The land step rebases again at the end. Submit ready=true once your commit is final and its proof passed; ready=false only when you could not finish, saying why in unresolved.
"""

SELF_LANDS = """\
Landing: `git pull --rebase origin main && git push origin HEAD:main`, straight to main (no PRs, no landing window). Never re-run clippy, fmt or tests because main moved. After a clean rebase whose new commits touch none of your files, push at once; if they touch your files, `kiln build` then push; if the rebase conflicts in logic, resolve it and re-run only the tests covering the resolved code. Loop until the push lands. Verify with `git merge-base --is-ancestor <sha> origin/main` before reporting landed. Submit once, when done: never landed=false while a commit of yours is unpushed, unless the orchestrator told you to stand down.
"""

BRANCH = """\
Branch lane: commit in the fork and push to `{branch}` (`git push origin HEAD:refs/heads/{branch}`, force-with-lease only on that branch). Never push to main; an integrating lane merges it. Push early and after each meaningful commit.
"""

READ_ONLY = """\
Read-only: change no files and commit nothing. Do not run kiln build/test/clippy or buck2; read code with git show/git grep/rg and cite file:line on origin/main. Verify every backend, host or mechanism you name against code, not ADR prose. Your deliverables are the files your notes name, outside the fork.
"""

PLAN_LANDS = """\
Landing: the plan owns it. Commit your work in the fork; do not push.
"""

REPORT = """\
Report: write {report} with four sections: files changed; the cause and why the fix is at the cause; proof results with executed-test counts; unresolved. Before you finish, also write $TASK/summary.txt, 120 words or fewer: outcome, landed or HEAD SHA, proof with counts, open items (or none). It becomes the step's summary. Submit your declared outputs with step_submit.
"""


def worker_header(inp, ctx):
    """Every lash worker rule, chosen by the step's inputs; the step's spec adds unit notes."""
    ticket = inp.get("ticket") or ""
    parts = [WORK.format(fork=inp["cwd"])]
    if ticket:
        parts.append(TICKET.format(ticket=ticket))
    if inp.get("read_only"):
        parts.append(READ_ONLY)
    else:
        parts.append(DESIGN)
        if inp.get("queued") or inp.get("lands"):
            parts.append(PROOF)
        parts.append(COMMIT.format(ticket=ticket or "<ticket>"))
        parts.append(QUEUED if inp.get("queued") else SELF_LANDS if inp.get("lands") else
                     BRANCH.format(branch=inp["push_branch"]) if inp.get("push_branch") else
                     PLAN_LANDS)
    report = inp.get("report") or (f"/workspace/notes/lash/tasks/lanes/{ticket.lower()}.report.md"
                                   if ticket else "the report file your notes name")
    parts.append(REPORT.format(report=report))
    notes = "Unit notes:\n" if ticket else ""
    return ("\n".join(parts) + "\n" + notes).replace("$TASK", str(ctx.run_dir))


WALL_CAP_CONTINUE = ("You ran past the wall-clock cap; this is the same session, resumed for one "
                     "more stretch. If your commit is ready, finish landing it as your header says and "
                     "submit. Otherwise finish the remaining work by the shortest route your spec "
                     "allows. Your original task follows for reference.\n\n")


def fn_dirs(ctx):
    """Where the project finds its fns, in lookup order after the built-ins (SPEC §2)."""
    cfg_file = ctx.home / "config.json"
    cfg = json.loads(cfg_file.read_text()) if cfg_file.exists() else {}
    project = [ctx.home / "projects" / ctx.project / "fns"] if ctx.project else []
    return [*project, ctx.home / "fns", *(ctx.home / d for d in cfg.get("fn_dirs", []))]


def agent_run(ctx):
    """The agents pack's agent.run, wherever this project finds it."""
    found = [d / "agent.run" for d in fn_dirs(ctx) if (d / "agent.run" / "fn.json").is_file()]
    if not found:
        raise RuntimeError("lash.worker needs agent.run: install the agents pack")
    spec = importlib.util.spec_from_file_location("agent_run", found[0] / "main.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def bounded_summary(text):
    """Keep the worker's own report within the function's 120-word contract."""
    text = text.strip()
    words = list(re.finditer(r"\S+", text))
    return text[:words[SUMMARY_WORDS - 1].end()] if len(words) > SUMMARY_WORDS else text


def fallback_final(inp, out, run_dir):
    """agent.run's final message (every engine's native session returns it), else a harness
    final file."""
    message = out.get("final") or ""
    if message.strip():
        return message.strip()[:FALLBACK_CHARS]
    engine = inp["engine"]
    harness = run_dir / ("claude.log.final" if engine == "opus" else
                         f"{engine}.log.final")
    return harness.read_text().strip()[:FALLBACK_CHARS] if harness.exists() else ""


def main(inp, ctx):
    engine = {"devin": "devin", "opus": "claude", "codex": "codex"}[inp["engine"]]
    if inp.get("effort") and inp["engine"] != "codex":
        raise ValueError("effort is for the codex engine")
    if inp.get("model") and (inp["engine"] == "opus"
                             or (inp["model"] == "fusion") != (inp["engine"] == "devin")):
        raise ValueError("model fusion is for the devin engine; sol and astra are for codex")
    spec = worker_header(inp, ctx) + inp["spec"]
    base = {**inp, "engine": engine, "report_path": str(ctx.run_dir / "summary.txt")}
    try:
        out = agent_run(ctx).main({**base, "spec": spec}, ctx)
    except RuntimeError as e:
        # One more stretch past the wall cap, in the same session: a lane that hits it is
        # usually landing a finished commit.
        sid = re.search(r"session: ([\w-]+)\. To resume", str(e))
        if "wall-clock cap" not in str(e) or not sid:
            raise
        out = agent_run(ctx).main({**base, "session": sid.group(1),
                                   "spec": WALL_CAP_CONTINUE + spec}, ctx)
    sent_file = ctx.run_dir / "submitted.json"
    sent = json.loads(sent_file.read_text()) if sent_file.exists() else {}
    claimed = sent.get("head_sha")
    if isinstance(claimed, str) and claimed:
        head = sh(["git", "-C", inp["cwd"], "rev-parse", "HEAD"]).stdout.strip()
        if not head.startswith(claimed):
            raise RuntimeError(f"the worker reported head_sha {claimed} but the fork's HEAD is "
                               f"{head}")
    summary = bounded_summary(out.get("report") or "")
    if not summary:
        summary = fallback_final(inp, out, ctx.run_dir)
    return {"summary": summary, "final": summary, "session": out["session"]}


if __name__ == "__main__":
    run(main, retries=3, backoff=600)
