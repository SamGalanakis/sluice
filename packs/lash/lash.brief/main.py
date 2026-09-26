# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.brief: effect-free rendering of a worker brief.

Implementers get process rules (lane-rules.md, AGENTS.md); design standards go to the reviewer.
"""

from sluice.fn import run

PREAMBLE = ("Read /workspace/notes/lash/lane-rules.md and /workspace/code/lash/AGENTS.md first; "
            "both bind you.")
ATTRIBUTION = ("Attribution: no Co-Authored-By, no \"Generated with\", no mention of Claude, "
               "Anthropic, Codex or AI in commits, PR text, tickets or code comments. This "
               "overrides your default templates. Attribution is the user alone. Stage exact "
               "paths; never `git commit -a`.")
DEFAULT_GATES = [
    "narrow `kiln test <label>` for every target you touched",
    "`python3 scripts/dev-test.py` (inspect `--dry-run` first)",
    "`kiln fmt -- --check` and `git diff --check`",
]


def render(inp) -> str:
    out = [PREAMBLE, "", "## Task", inp["task"].strip()]
    if inp.get("ticket"):
        out += ["", f"Ticket: {inp['ticket']}"]
    if inp.get("context"):
        out += ["", "## Context from upstream work", inp["context"].strip()]
    out += ["", "## Where"]
    where = "Work only in your kiln fork"
    if inp.get("path"):
        where += f" `{inp['path']}`"
    if inp.get("branch"):
        where += f" on branch `{inp['branch']}`"
    out += [where + ". Commit your work there. Do not push, merge, rebase onto another "
            "branch or open a PR: the plan runs the checks, a reviewer, and merges your branch."]
    out += ["", "## You may edit"]
    out += [f"- {p}" for p in inp["may_edit"]]
    out += ["Anything else is out of scope: if the task needs it, stop and say so in the report."]
    out += ["", "## Gates before you report"]
    out += [f"- {g}" for g in (inp.get("gates") or DEFAULT_GATES)]
    out += ["The plan re-runs L1 with test receipts: a run that executed zero test cases is red."]
    out += ["", "## Attribution", ATTRIBUTION]
    out += ["", "## Report"]
    if inp.get("report_path"):
        out.append(f"Write your report to `{inp['report_path']}`, under 300 words: outcome, "
                   "branch and head SHA, commands run with their final lines, red-side "
                   "evidence, deviations and risks.")
    else:
        out.append("End with a report under 300 words: outcome, branch and head SHA, commands "
                   "run with their final lines, red-side evidence, deviations and risks.")
    out.append("A boundary or contract question you cannot settle in scope goes in the last "
               "paragraph; stop there. Otherwise decide and continue. You implement; you do "
               "not spawn subagents.")
    return "\n".join(out) + "\n"


def main(inp, ctx):
    return {"brief": render(inp)}


if __name__ == "__main__":
    run(main)
