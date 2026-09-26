"""Installing a pack is copying it into a fns dir: the copied functions then load in
the global scope, verify reports no problems and they run (SPEC §2)."""

import json
import shutil
import subprocess
from pathlib import Path

from mcp import Client

from sluice.mcp_server import build_server
from sluice.store import Store
from sluice.verify import verify

PACKS = Path(__file__).resolve().parents[1]


def copy_pack(pack: str, fns: Path) -> None:
    """packs/<pack>/* into `fns`, like a user installing it (tests stay behind)."""
    for entry in (PACKS / pack).iterdir():
        if entry.is_dir() and entry.name != "tests":
            shutil.copytree(entry, fns / entry.name)


async def call(c, tool, **args):
    r = await c.call_tool(tool, args)
    return r.is_error, json.loads(r.content[0].text)


async def test_copied_packs_are_global_clean_and_run(tmp_path):
    home = tmp_path / "sluice-home"
    for pack in ("jev", "git"):
        copy_pack(pack, home / "fns")
    store = Store(home)

    listing = {e["name"]: e for e in store.registry().listing()}
    copied = {"jev.ask", "jev.choice", "jev.score", "jev.noul", "git.worktree",
              "git.worktree_rm", "git.head", "git.merge", "git.rebase", "git.push",
              "gh.pr"}
    assert copied <= set(listing)
    assert {listing[n]["scope"] for n in copied} == {"global"}
    assert verify(store) == {"ok": True, "problems": []}

    repo = tmp_path / "repo"
    repo.mkdir()

    def g(*args):
        return subprocess.run(["git", "-C", str(repo), *args],
                              check=True, capture_output=True, text=True).stdout.strip()

    g("init", "-b", "main")
    g("config", "user.email", "t@example.com")
    g("config", "user.name", "T")
    (repo / "f.txt").write_text("one\n")
    g("add", ".")
    g("commit", "-m", "init")

    async with Client(build_server(store)) as c:
        err, res = await call(c, "fn_call", name="git.head",
                              inputs={"path": str(repo)}, direct=True)
        assert not err, res
        assert res["status"] == "succeeded"
        assert res["outputs"] == {"branch": "main", "sha": g("rev-parse", "HEAD")}

        # a jev fn copied next to _jev imports its helper and fails cleanly without a key
        err, res = await call(c, "fn_call", name="jev.noul",
                              inputs={"state": "x", "instructions": "y?"}, direct=True)
        assert not err, res
        assert res["status"] == "failed"
        assert "TYPESAFE_API_KEY is not set" in res["error"]
