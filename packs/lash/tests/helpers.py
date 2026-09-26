"""Test helpers for the lash pack: run fns as processes, fake binaries, throwaway git repos, and a
static checker for composites and plans (SPEC §3, §4.2, §5.2) until the core validator lands."""

from __future__ import annotations

import json
import os
import re
import stat
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

TESTS = Path(__file__).resolve().parent
PACK = TESTS.parent
REPO = PACK.parents[1]
FAKEBIN = TESTS / "fakebin.py"


# ---- running fns ---------------------------------------------------------------------------

@dataclass
class Result:
    code: int
    out: dict | None
    stderr: str
    run_dir: Path

    @property
    def error(self) -> dict:
        p = self.run_dir / "error.json"
        return json.loads(p.read_text()) if p.exists() else {}


def run_fn(name: str, inp: dict, tmp: Path, env: dict[str, str] | None = None,
           timeout: float = 120) -> Result:
    """Run `<pack>/<name>/main.py` the way the runner does (SPEC §4.1)."""
    fn_dir = PACK / name
    runs = tmp / "runs"
    runs.mkdir(exist_ok=True)
    run_dir = runs / f"{name}-{len(list(runs.iterdir()))}"
    run_dir.mkdir()
    base = {k: v for k, v in os.environ.items() if not k.startswith(("SLUICE_", "FAKE_"))}
    full = {**base, "PYTHONPATH": str(REPO / "src"), "SLUICE_RUN_DIR": str(run_dir),
            "SLUICE_FN_DIR": str(fn_dir), "SLUICE_HOME": str(tmp / "home"),
            "SLUICE_PLAN": "p", "SLUICE_NODE": name.replace(".", "-"), "SLUICE_ATTEMPT": "1",
            **(env or {})}
    p = subprocess.run(["uv", "run", "--quiet", "--script", str(fn_dir / "main.py")],
                       input=json.dumps(inp), capture_output=True, text=True, env=full,
                       cwd=run_dir, timeout=timeout, check=False)
    out = json.loads(p.stdout) if p.returncode == 0 else None
    return Result(p.returncode, out, p.stderr, run_dir)


# ---- fakes ---------------------------------------------------------------------------------

@dataclass
class Fakes:
    dir: Path
    config: dict

    @property
    def log(self) -> Path:
        return self.dir / "calls.jsonl"

    def save(self) -> None:
        (self.dir / "config.json").write_text(json.dumps(self.config))

    def respond(self, bin: str, match: list[str], stdout: str = "", exit: int = 0,
                testlogs: dict[str, str] | None = None, stderr: str = "") -> None:
        self.config.setdefault("responses", []).append(
            {"bin": bin, "match": match, "stdout": stdout, "stderr": stderr, "exit": exit,
             "testlogs": testlogs or {}})
        self.save()

    def calls(self, bin: str | None = None) -> list[dict]:
        if not self.log.exists():
            return []
        rows = [json.loads(line) for line in self.log.read_text().splitlines()]
        return [r for r in rows if bin is None or r["bin"] == bin]

    @property
    def env(self) -> dict[str, str]:
        return {"SLUICE_KILN_BIN": str(self.dir / "bin" / "kiln"),
                "SLUICE_JUST_BIN": str(self.dir / "bin" / "just"),
                "SLUICE_PYTHON_BIN": str(self.dir / "bin" / "python3"),
                "FAKE_CONFIG": str(self.dir / "config.json"), "FAKE_LOG": str(self.log)}


def make_fakes(tmp: Path, repo: Path | None = None) -> Fakes:
    d = tmp / "fakes"
    (d / "bin").mkdir(parents=True)
    for name in ("kiln", "just", "python3"):
        exe = d / "bin" / name
        exe.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{FAKEBIN}" {name} "$@"\n')
        exe.chmod(exe.stat().st_mode | stat.S_IEXEC)
    forks = tmp / "forks"
    forks.mkdir()
    f = Fakes(d, {"repo": str(repo) if repo else "", "forks_dir": str(forks), "responses": []})
    f.save()
    return f


# ---- git -----------------------------------------------------------------------------------

def git(cwd: Path, *args: str) -> str:
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                          text=True).stdout.strip()


def make_repo(tmp: Path) -> Path:
    """A clone of a bare 'origin' with one commit on main; returns the clone."""
    origin, clone = tmp / "origin.git", tmp / "lash"
    subprocess.run(["git", "init", "-q", "--bare", "-b", "main", str(origin)], check=True)
    subprocess.run(["git", "init", "-q", "-b", "main", str(clone)], check=True)
    git(clone, "config", "user.email", "t@example.com")
    git(clone, "config", "user.name", "t")
    (clone / "README.md").write_text("lash\n")
    git(clone, "add", "README.md")
    git(clone, "commit", "-q", "-m", "init")
    git(clone, "remote", "add", "origin", str(origin))
    git(clone, "push", "-q", "-u", "origin", "main")
    return clone


def commit(repo: Path, name: str, text: str, msg: str = "change") -> str:
    (repo / name).write_text(text)
    git(repo, "add", name)
    git(repo, "commit", "-q", "-m", msg)
    return git(repo, "rev-parse", "HEAD")


# ---- bazel console shapes --------------------------------------------------------------------

def bazel_line(label: str, status: str = "PASSED", cached: bool = False) -> str:
    return f"{label:<60} {'(cached) ' if cached else ''}{status} in 1.2s\n"


def libtest(passed: int, failed: int = 0, failing: tuple[str, ...] = (), filtered: int = 0) -> str:
    lines = [f"running {passed + failed} tests"]
    lines += [f"test ok_{i} ... ok" for i in range(passed)]
    lines += [f"test {name} ... FAILED" for name in failing]
    verdict = "FAILED" if failed else "ok"
    lines.append(f"test result: {verdict}. {passed} passed; {failed} failed; 0 ignored; "
                 f"0 measured; {filtered} filtered out; finished in 0.01s")
    return "\n".join(lines) + "\n"


def log_rel(label: str) -> str:
    pkg, _, name = label.removeprefix("//").partition(":")
    return f"{pkg}/{name}/test.log"


# ---- static checking (SPEC §3 types, §4.2 composites, §5.2 plans) -----------------------------

ENGINE = ["devin", "codex", "claude"]
# Built-ins (SPEC §4.3) and the generic packs (SPEC §11) this pack binds to.
EXTERNAL: dict[str, dict[str, Any]] = {
    "core.echo": {"in": {"value": "any"}, "out": {"value": "any"}},
    "core.ask": {"in": {"question": "string", "context": "any?",
                        "to": {"optional": ["orchestrator", "human"]}}, "out": {"answer": "any"}},
    "core.fail": {"in": {"message": "string"}, "out": {}},
    "agent.run": {"in": {"engine": ENGINE, "cwd": "string", "spec": "string", "model": "string?",
                         "resume": "string?", "report_path": "string?"},
                  "out": {"final": "string", "report": "string?", "session": "string?"}},
    "agent.review": {"in": {"cwd": "string", "base": "string", "standards": "string",
                            "notes": "string?"},
                     "out": {"summary": "string", "sha": "string", "commits": "int"}},
    "git.worktree": {"in": {"repo": "string", "base": "string", "branch": "string",
                            "path": "string?"},
                     "out": {"path": "string", "branch": "string", "sha": "string"}},
    "git.worktree_rm": {"in": {"repo": "string", "path": "string", "force": "bool?"},
                        "out": {"removed": "bool"}},
    "git.head": {"in": {"path": "string"}, "out": {"branch": "string", "sha": "string"}},
    "git.merge": {"in": {"repo": "string", "source": "string", "target": "string",
                         "message": "string?", "push": "bool?"},
                  "out": {"merged": "bool", "sha": "string?", "conflicts": {"list": "string"}}},
    "git.rebase": {"in": {"path": "string", "onto": "string"},
                   "out": {"ok": "bool", "sha": "string", "conflicts": {"list": "string"}}},
    "git.push": {"in": {"path": "string", "branch": "string", "remote": "string?",
                        "force_with_lease": "bool?"}, "out": {"sha": "string"}},
    "gh.pr": {"in": {"path": "string", "base": "string", "head": "string", "title": "string",
                     "body": "string", "draft": "bool?"},
              "out": {"number": "int", "url": "string"}},
}

PRIMS = {"string", "int", "float", "bool", "any"}
SPECIAL = {"list", "map", "optional", "union", "record"}
ID = re.compile(r"^[a-z0-9][a-z0-9_-]*$")


def norm(t: Any) -> Any:
    """Normalise a SPEC §3 type; raises ValueError on a malformed one."""
    if isinstance(t, str):
        if t.endswith("?"):
            return ("optional", norm(t[:-1]))
        if t not in PRIMS:
            raise ValueError(f"unknown primitive {t!r}")
        return t
    if isinstance(t, list):
        if not t or not all(isinstance(x, str) for x in t):
            raise ValueError(f"bad enum {t!r}")
        return ("enum", frozenset(t))
    if isinstance(t, dict):
        if len(t) == 1 and next(iter(t)) in SPECIAL:
            k, v = next(iter(t.items()))
            if k in ("list", "map", "optional"):
                return (k, norm(v))
            if k == "record":
                return ("record", {f: norm(x) for f, x in v.items()})
            return ("union", {tag: norm({"record": r}) for tag, r in v.items()})
        return ("record", {f: norm(x) for f, x in t.items()})
    raise ValueError(f"bad type {t!r}")


def is_opt(t: Any) -> bool:
    return isinstance(t, tuple) and t[0] == "optional"


def fits(out: Any, inp: Any) -> bool:
    if inp == "any" or out == "any":
        return True
    if is_opt(inp):
        return fits(out[1] if is_opt(out) else out, inp[1])
    if is_opt(out):
        return False
    if isinstance(out, str) and isinstance(inp, str):
        return out == inp or (out, inp) == ("int", "float")
    if isinstance(out, tuple) and out[0] == "enum":
        return inp == "string" or (isinstance(inp, tuple) and inp[0] == "enum"
                                   and out[1] <= inp[1])
    if isinstance(out, tuple) and isinstance(inp, tuple) and out[0] == inp[0]:
        if out[0] in ("list", "map"):
            return fits(out[1], inp[1])
        if out[0] == "record":
            for f, ft in inp[1].items():
                if f not in out[1]:
                    if not is_opt(ft):
                        return False
                elif not fits(out[1][f], ft):
                    return False
            return True
        if out[0] == "union":
            return all(tag in inp[1] and fits(r, inp[1][tag]) for tag, r in out[1].items())
    return False


def check_value(t: Any, v: Any, path: str = "") -> list[str]:
    if t == "any":
        return []
    if is_opt(t):
        return [] if v is None else check_value(t[1], v, path)
    prim = {"string": str, "int": int, "float": (int, float), "bool": bool}
    if isinstance(t, str):
        ok = isinstance(v, prim[t]) and not (t in ("int", "float") and isinstance(v, bool))
        return [] if ok else [f"{path}: expected {t}, got {v!r}"]
    kind = t[0]
    if kind == "enum":
        return [] if isinstance(v, str) and v in t[1] else [f"{path}: expected one of {sorted(t[1])}, got {v!r}"]
    if kind == "list":
        if not isinstance(v, list):
            return [f"{path}: expected list"]
        return [e for i, x in enumerate(v) for e in check_value(t[1], x, f"{path}[{i}]")]
    if kind == "map":
        if not isinstance(v, dict):
            return [f"{path}: expected map"]
        return [e for k, x in v.items() for e in check_value(t[1], x, f"{path}.{k}")]
    if kind == "record":
        if not isinstance(v, dict):
            return [f"{path}: expected record"]
        errs = []
        for f, ft in t[1].items():
            if f in v:
                errs += check_value(ft, v[f], f"{path}.{f}")
            elif not is_opt(ft):
                errs.append(f"{path}.{f}: missing")
        return errs
    return []


def load_pack() -> dict[str, dict]:
    fns = {}
    for fj in sorted(PACK.glob("*/fn.json")):
        doc = json.loads(fj.read_text())
        doc["_dir"] = fj.parent
        fns[doc["name"]] = doc
    return fns


def registry() -> dict[str, dict]:
    return {**EXTERNAL, **load_pack()}


def _navigate(t: Any, fields: list[str], where: str) -> tuple[Any, list[str]]:
    """Follow record fields; passing through an optional makes the result optional."""
    optional = False
    for f in fields:
        if t == "any":
            return "any", []
        if is_opt(t):
            optional, t = True, t[1]
        if not (isinstance(t, tuple) and t[0] == "record" and f in t[1]):
            return None, [f"{where}: no field {f!r}"]
        t = t[1][f]
    if optional and not is_opt(t) and t != "any":
        t = ("optional", t)
    return t, []


def check_graph(nodes: dict, reg: dict, ins: dict | None, where: str,
                resources: dict | None = None) -> list[str]:
    """Check a composite graph (ins = the composite's in ports) or a plan (ins=None)."""
    errs: list[str] = []

    def ref_type(ref: str, at: str) -> Any:
        head, *rest = ref.split(".")
        if head == "$in":
            if ins is None or not rest or rest[0] not in ins:
                errs.append(f"{at}: unknown ref {ref!r}")
                return None
            t, e = _navigate(norm(ins[rest[0]]), rest[1:], at)
            errs.extend(e)
            return t
        if head not in nodes:
            errs.append(f"{at}: ref {ref!r} names no node")
            return None
        fn = reg.get(nodes[head]["fn"])
        if fn is None or not rest or rest[0] not in fn["out"]:
            errs.append(f"{at}: ref {ref!r}: no output port")
            return None
        t, e = _navigate(norm(fn["out"][rest[0]]), rest[1:], at)
        errs.extend(e)
        return t

    deps: dict[str, set[str]] = {}
    for nid, node in nodes.items():
        at = f"{where}.nodes.{nid}"
        if not ID.match(nid):
            errs.append(f"{at}: bad node id")
        fn = reg.get(node.get("fn"))
        if fn is None:
            errs.append(f"{at}: unknown fn {node.get('fn')!r}")
            continue
        deps[nid] = set(node.get("after", []))
        for a in node.get("after", []):
            if a not in nodes:
                errs.append(f"{at}.after: unknown node {a!r}")
        for port, b in node.get("in", {}).items():
            pat = f"{at}.in.{port}"
            if port not in fn["in"]:
                errs.append(f"{pat}: {node['fn']} has no input port {port!r}")
                continue
            pt = norm(fn["in"][port])
            if "from" in b:
                st = ref_type(b["from"], pat)
                if not b["from"].startswith("$in."):
                    deps[nid].add(b["from"].split(".")[0])
                if st is not None and not fits(st, pt):
                    errs.append(f"{pat}: {st} does not fit {pt}")
            elif "value" in b:
                errs += check_value(pt, b["value"], pat)
            elif "file" in b:
                if not fits("string", pt):
                    errs.append(f"{pat}: file binding needs a string port")
            else:
                errs.append(f"{pat}: binding needs from, value or file")
        for port, pt in fn["in"].items():
            if port not in node.get("in", {}) and not is_opt(norm(pt)):
                errs.append(f"{at}.in.{port}: required port unbound")
        for i, cond in enumerate(node.get("when", [])):
            wat = f"{at}.when[{i}]"
            if cond.get("op") not in ("eq", "ne", "in", "truthy", "falsy"):
                errs.append(f"{wat}: bad op")
            wt = ref_type(cond["from"], wat)
            if not cond["from"].startswith("$in."):
                deps[nid].add(cond["from"].split(".")[0])
            if wt is not None and cond.get("op") in ("eq", "ne"):
                errs += check_value(wt, cond.get("value"), f"{wat}.value")
        for c in node.get("claims", []):
            if resources is None or c not in resources:
                errs.append(f"{at}.claims: undeclared resource {c!r}")
    # acyclic
    state: dict[str, int] = {}

    def visit(n: str) -> bool:
        if state.get(n) == 1:
            return False
        if state.get(n) == 2:
            return True
        state[n] = 1
        ok = all(visit(d) for d in deps.get(n, ()) if d in nodes)
        state[n] = 2
        return ok

    if not all(visit(n) for n in nodes):
        errs.append(f"{where}: cycle")
    return errs


def check_composite(fn: dict, reg: dict) -> list[str]:
    g = fn["graph"]
    where = fn["name"]
    errs = check_graph(g["nodes"], reg, fn["in"], where)
    for port, b in g.get("out", {}).items():
        if port not in fn["out"]:
            errs.append(f"{where}.graph.out.{port}: not an out port")
            continue
        head, *rest = b["from"].split(".")
        node = g["nodes"].get(head)
        target = reg.get(node["fn"]) if node else None
        if not target or not rest or rest[0] not in target["out"]:
            errs.append(f"{where}.graph.out.{port}: bad ref {b['from']!r}")
            continue
        if not fits(norm(target["out"][rest[0]]), norm(fn["out"][port])):
            errs.append(f"{where}.graph.out.{port}: type mismatch")
    for port in fn["out"]:
        if port not in g.get("out", {}):
            errs.append(f"{where}.graph.out.{port}: unbound")
    return errs


def check_plan(doc: dict, reg: dict) -> list[str]:
    errs = []
    if not ID.match(doc.get("id", "")):
        errs.append("id: bad plan id")
    return errs + check_graph(doc["nodes"], reg, None, "plan", doc.get("resources", {}))

