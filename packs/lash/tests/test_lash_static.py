"""Static checks: every fn.json is well-formed, composites and the example plan bind only to
fns and ports that exist, with fitting types (SPEC §3, §4, §4.2, §5.2)."""

import copy
import json

import pytest
from helpers import EXTERNAL, PACK, check_composite, check_plan, load_pack, norm, registry

FNS = load_pack()
EXPECTED = {"lash.fork", "lash.fork_rm", "check.l0", "check.l1", "check.l2", "check.l3",
            "lash.brief", "lash.task", "lash.arc_land", "lash.gate", "linear.update"}


def test_pack_has_the_specified_fns():
    assert EXPECTED <= set(FNS)
    assert not set(FNS) & set(EXTERNAL), "a lash fn shadows a built-in or generic-pack fn"


@pytest.mark.parametrize("name", sorted(FNS))
def test_fn_json_has_spec_keys(name):
    fn = FNS[name]
    assert fn["_dir"].name == name, "a fn lives in <pack>/<fn.name>/"
    assert isinstance(fn["version"], int) and fn["version"] >= 1
    assert fn.get("description")
    for t in [*fn["in"].values(), *fn["out"].values()]:
        norm(t)  # raises on a malformed type
    main = fn["_dir"] / "main.py"
    if "graph" in fn:
        assert not main.exists(), "a composite has no main.py"
        for key in ("effects", "timeout", "slots", "retry"):
            assert key not in fn, f"{key} does not apply to composites"
    else:
        src = main.read_text()
        assert src.startswith("# /// script\n"), "main.py declares PEP 723 metadata"
        assert "run(main)" in src
        assert fn.get("timeout", "1h")[-1] in "smh"


@pytest.mark.parametrize("name", sorted(n for n, f in FNS.items() if "graph" in f))
def test_composite_binds_only_existing_fns_and_ports(name):
    assert check_composite(FNS[name], registry()) == []


def test_checker_catches_bad_bindings():
    """The static checker itself must reject what the core validator would."""
    reg = registry()
    task = copy.deepcopy(FNS["lash.finish"])
    task["graph"]["nodes"]["merge"]["in"]["branch"] = {"from": "$in.branch"}
    task["graph"]["nodes"]["review"]["in"]["cwd"] = {"from": "standards.nope"}
    task["graph"]["nodes"]["rm"]["when"][0]["value"] = "yes"
    del task["graph"]["nodes"]["recheck"]["in"]["path"]
    task["graph"]["nodes"]["red_after_review"]["in"]["message"] = {"from": "recheck.tests_run"}
    errs = "\n".join(check_composite(task, reg))
    assert "no input port 'branch'" in errs
    assert "standards.nope" in errs
    assert "when[0].value" in errs
    assert "recheck.in.path: required port unbound" in errs
    assert "red_after_review.in.message" in errs


def test_example_plan_is_valid():
    doc = json.loads((PACK / "examples" / "arc-plan.json").read_text())
    assert check_plan(doc, registry()) == []
    tasks = {k: n for k, n in doc["nodes"].items() if n["fn"] == "lash.task"}
    assert len(tasks) == 3
    assert [k for k, n in tasks.items() if "pg-schema" in n.get("claims", [])] == ["store"]
    assert doc["nodes"]["host"]["in"]["context"] == {"from": "store.review"}
    assert set(doc["nodes"]["land"]["after"]) == set(tasks)


def test_example_plan_checker_catches_undeclared_claims_and_cycles():
    doc = json.loads((PACK / "examples" / "arc-plan.json").read_text())
    doc["resources"] = {}
    doc["nodes"]["store"]["after"] = ["host"]
    errs = "\n".join(check_plan(doc, registry()))
    assert "undeclared resource 'pg-schema'" in errs
    assert "cycle" in errs


def test_gate_spawn_matches_attempt_ports():
    """lash.gate builds lash.attempt nodes from these names; keep them in step."""
    gate, attempt = FNS["lash.gate"]["in"], FNS["lash.attempt"]["in"]
    src = (PACK / "lash.gate" / "main.py").read_text()
    for port in ("name", "ladder", "brief", "path", "branch", "base", "arc", "labels",
                 "allow_no_tests", "report_path"):
        assert port in gate and port in attempt and f'"{port}"' in src
