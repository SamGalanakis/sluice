# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.fork_rm: kiln rm lash <name>, refusing tracked changes or unpushed commits unless forced.

A landed fork (HEAD on origin/main or a pushed origin/lanes/* branch, no tracked changes) is removed even when untracked files
remain (lane evidence, logs): they are archived under ARCHIVE/<name> first (within the cap),
listed in `discarded`, and the archive path is `archived`.
"""

import shutil
from pathlib import Path

from sluice.fn import run, sh

# Lane evidence (logs a report cites) outlives the fork: untracked paths are archived here
# before removal, unless they exceed the cap.
ARCHIVE = Path("/workspace/notes/lash/tasks/lanes/evidence")
ARCHIVE_CAP = 200 * 1024 * 1024


def tree_bytes(path):
    if path.is_file():
        return path.stat().st_size
    return sum(f.stat().st_size for f in path.rglob("*") if f.is_file() and not f.is_symlink())


def archive(path, name, untracked, ctx):
    sources = [path / p.rstrip("/") for p in untracked]
    total = sum(tree_bytes(s) for s in sources if s.exists())
    if not sources or total > ARCHIVE_CAP:
        if sources:
            ctx.log(f"not archiving {name}: untracked paths total {total} bytes")
        return None
    dest = ARCHIVE / name
    for src in sources:
        if not src.exists():
            continue
        target = dest / src.relative_to(path)
        target.parent.mkdir(parents=True, exist_ok=True)
        if src.is_dir():
            shutil.copytree(src, target, symlinks=True, dirs_exist_ok=True)
        else:
            shutil.copy2(src, target)
    return str(dest)


def git(path, *args, check=True):
    return sh(["git", "-C", str(path), *args], check=check)


def main(inp, ctx):
    path = Path(inp["path"])
    name = inp["name"]
    if not path.exists():
        return {"removed": False, "discarded": []}
    force = bool(inp.get("force"))
    tracked = git(path, "status", "--porcelain", "--untracked-files=no").stdout.rstrip()
    untracked = [p for p in git(path, "ls-files", "--others", "--exclude-standard",
                                "--directory").stdout.splitlines() if p]
    if not force:
        problems = []
        if tracked:
            problems.append(f"tracked changes:\n{tracked[:2000]}")
        git(path, "fetch", "-q", "origin", "main")
        landed = git(path, "merge-base", "--is-ancestor", "HEAD", "origin/main",
                     check=False).returncode == 0
        # A branch lane's HEAD is safe once pushed to its lanes/ branch.
        pushed = landed or bool(git(path, "branch", "-r", "--contains", "HEAD",
                                    "--list", "origin/lanes/*").stdout.strip())
        if not pushed:
            ahead = git(path, "log", "--oneline", "origin/main..HEAD").stdout.strip()
            problems.append(f"commits not on origin/main:\n{ahead[:2000]}")
        if problems:
            listing = "\n".join(untracked[:200])
            extra = f"\nuntracked (would be discarded):\n{listing}" if untracked else ""
            raise RuntimeError(f"fork {name} not removed (land or commit the work, or force "
                               f"to remove anyway):\n" + "\n".join(problems) + extra)
    # Lanes also keep evidence under the git-ignored target/: target/*evidence*, target/<fork>*
    # (lane scratch such as target/fig-4260/) and target/fig-3790/ (load-test run archives).
    # Each is archived on its own within the cap, so one oversized tree never drops the rest.
    target = path / "target"
    evidence = sorted({p for pat in ("*evidence*", f"{name}*", "fig-3790")
                       for p in target.glob(pat) if p.is_dir()}) if target.is_dir() else []
    archived = None
    for part in [untracked] + [[str(p.relative_to(path)) + "/"] for p in evidence]:
        if part:
            archived = archive(path, name, part, ctx) or archived
    kept = untracked + [str(p.relative_to(path)) + "/" for p in evidence]
    if kept:
        ctx.log(f"discarding {len(kept)} untracked or evidence path(s) in {name}"
                + (f" (archived to {archived})" if archived else "") + ": "
                + ", ".join(untracked[:50]))
    sh(["kiln", "rm", "lash", name])
    return {"removed": True, "discarded": untracked, "archived": archived}


if __name__ == "__main__":
    run(main)
