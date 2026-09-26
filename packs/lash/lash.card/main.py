# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.card: the arc PR body. Tickets follow lane-rules: `Part of` / `Refs`, never
Fixes/Closes/Resolves (the completing ticket id belongs in the PR title)."""

from sluice.fn import run

DOORS = {
    "one-way": "one-way: hard to undo (durable format, public surface, data). Opened as DRAFT; "
               "a human reviews before it lands.",
    "two-way": "two-way: a revert undoes it. Lands on green checks.",
}


def render(inp) -> str:
    out = ["## Merge-danger card", "", "| | |", "|---|---|",
           f"| Door | {DOORS[inp['door']]} |",
           f"| Blast radius | {' '.join(inp['blast'].split())} |"]
    known = inp.get("known_reds") or []
    out.append(f"| Known reds carried | {len(known)} |")
    out += ["", "## What changed", inp["what"].strip()]
    out += ["", "## Gates (L2)", "```", inp["checks"].strip(), "```"]
    if known:
        out += ["", "Known reds (registered, with owner and expiry):"]
        out += [f"- {k}" for k in known]
    tickets = [f"Part of {t}" for t in inp.get("part_of") or []]
    tickets += [f"Refs {t}" for t in inp.get("refs") or []]
    if tickets:
        out += ["", *tickets]
    return "\n".join(out) + "\n"


def main(inp, ctx):
    return {"body": render(inp), "draft": inp["door"] == "one-way"}


if __name__ == "__main__":
    run(main)
