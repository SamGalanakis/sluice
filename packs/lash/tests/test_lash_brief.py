"""lash.brief renders the lane-rules preamble, scope, gates, attribution and report path."""

from helpers import run_fn

PREAMBLE = ("Read /workspace/notes/lash/lane-rules.md and /workspace/code/lash/AGENTS.md first; "
            "both bind you.")


def test_brief_content(tmp_path):
    r = run_fn("lash.brief", {
        "task": "Add send_batch to the store.",
        "may_edit": ["crates/lash-store/**", "crates/lash-sim/**"],
        "gates": ["`kiln test //crates/lash-store:tests`"],
        "report_path": "/reports/fig-1.md",
        "path": "/forks/fig-1",
        "branch": "fig-1",
        "context": "Upstream added the table.",
        "ticket": "FIG-1",
    }, tmp_path)
    assert r.code == 0, r.stderr
    brief = r.out["brief"]
    assert brief.splitlines()[0] == PREAMBLE
    assert "Add send_batch to the store." in brief
    assert "- crates/lash-store/**\n- crates/lash-sim/**\n" in brief
    assert "- `kiln test //crates/lash-store:tests`" in brief
    assert "`/reports/fig-1.md`" in brief
    assert "`/forks/fig-1` on branch `fig-1`" in brief
    assert "Upstream added the table." in brief and "FIG-1" in brief
    assert "no Co-Authored-By" in brief and "Generated with" in brief
    assert "never `git commit -a`" in brief


def test_brief_defaults_gates_to_the_lash_loop(tmp_path):
    r = run_fn("lash.brief", {"task": "t", "may_edit": ["docs/**"]}, tmp_path)
    assert r.code == 0, r.stderr
    brief = r.out["brief"]
    assert "python3 scripts/dev-test.py" in brief
    assert "kiln fmt -- --check" in brief
    assert "End with a report" in brief
