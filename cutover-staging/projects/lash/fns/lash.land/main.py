# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.land: the landing queue. Squash (optionally); under an explicit land lease, rebase onto main and
push; when main's new commits touched the change's code or a generated file was regenerated,
release the lease, `kiln check //...`, and go again."""

import re
import shlex
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from _lashlib import net_sh, on_main
from sluice_fn import Rejected, run, sh

REGENERATED = re.compile(r"(^|/)BUCK$|^tools/buck2/target-inventory\.json$")
ATTEMPTS = 30
# A push that lost to a concurrent one; anything else (a pre-push hook, a protection rule) is final.
RACE = re.compile(r"\[rejected\]|fetch first|non-fast-forward|cannot lock ref|stale info|failed to update ref")


def message(inp):
    """The squashed commit's title and body (lane-rules: only the change that completes a
    ticket carries its id in the title; a partial one says 'Part of' in the body)."""
    title, ticket = inp["title"].strip(), inp.get("ticket")
    if ticket and inp.get("completes"):
        return (title if title.endswith(f"({ticket})") else f"{title} ({ticket})"), ""
    return title, (f"Part of {ticket}" if ticket else "")


def squash(fork, inp):
    git = ["git", "-C", fork]
    if sh([*git, "status", "--porcelain"]).stdout.strip():
        raise RuntimeError("the fork has uncommitted changes: commit or discard them first")
    net_sh([*git, "fetch", "-q", "origin", "main"])
    base = sh([*git, "merge-base", "HEAD", "origin/main"]).stdout.strip()
    if sh([*git, "rev-list", "--count", f"{base}..HEAD"]).stdout.strip() == "0":
        raise RuntimeError("nothing to land: the fork has no commits ahead of origin/main")
    title, body = message(inp)
    sh([*git, "reset", "-q", "--soft", base])
    sh([*git, "commit", "-q", "-m", title, *(["-m", body] if body else [])])


def kiln(fork, args):
    return sh(["bash", "-c", (f"cd {shlex.quote(fork)} && . ./env.sh >/dev/null 2>&1 && "
                f"kiln {args} 2>&1")], check=False)


def names(fork, rng):
    return set(sh(["git", "-C", fork, "diff", "--name-only", rng]).stdout.split())


def regenerated(fork, path):
    """A file `kiln sync` writes: resolved by regenerating, never by merging."""
    if REGENERATED.search(path):
        return True
    head = sh(["git", "-C", fork, "show", f"origin/main:{path}"], check=False).stdout[:400]
    return "@generated" in head


def reject(inp, ctx, reason, feedback):
    """Only an intentional refusal may retry the captured work result on completion."""
    target = inp.get("work_step")
    if target is None and ctx.step and ctx.step.endswith("-land"):
        target = ctx.step.removesuffix("-land") + "-work"
    if target:
        feedback = (feedback + " Do not re-submit the old commit.").encode("utf-8")
        ctx.retry_on_failure(target, feedback[:8192].decode("utf-8", errors="ignore"))
    raise Rejected(reason)


def rebase(fork, inp, ctx):
    """Rebase onto origin/main. Conflicts confined to generated files are resolved by taking
    main's copy and regenerating; any other conflict aborts. Returns whether it regenerated."""
    git = ["git", "-C", fork]
    r, regen = sh([*git, "rebase", "-q", "origin/main"], check=False), False
    while r.returncode != 0:
        files = sh([*git, "diff", "--name-only", "--diff-filter=U"]).stdout.split()
        if not files or not all(regenerated(fork, f) for f in files):
            sh([*git, "rebase", "--abort"], check=False)
            reason = f"rebase onto origin/main conflicts in {', '.join(files) or '?'}"
            reject(inp, ctx, reason,
                   reason + ": run git pull --rebase origin main and resolve, keeping both "
                   "sides' intent; kiln check the resolved code and run the tests covering it "
                   "once; commit and submit ready=true.")
        sh([*git, "checkout", "--ours", "--", *files])  # during a rebase, ours is main
        if kiln(fork, "sync").returncode != 0:
            sh([*git, "rebase", "--abort"], check=False)
            reject(inp, ctx, "kiln sync failed while regenerating a conflicted generated file",
                   "Fix the generated-file conflict with kiln sync, check, commit and submit ready=true.")
        sh([*git, "add", "-A", "--", *files,
            *(f for f in names(fork, "HEAD") if regenerated(fork, f))])
        r, regen = sh([*git, "-c", "core.editor=true", "rebase", "--continue"], check=False), True
    return regen


FMT_REFUSAL = re.compile(r"cargo fmt[^\n]*Failed[\s\S]*files were modified by this hook")


def amend_formatting(fork):
    """Fold the hook's formatting edits into HEAD. Only tracked files the hook modified are
    staged; returns False when the hook left nothing to amend."""
    git = ["git", "-C", fork]
    changed = sh([*git, "diff", "--name-only"]).stdout.split()
    if not changed:
        return False
    sh([*git, "add", "--", *changed])
    sh([*git, "commit", "-q", "--amend", "--no-edit", "--no-verify"])
    return True


BUILD_INPUT = re.compile(r"\.(rs|bzl|toml)$|(^|/)BUCK$|(^|/)Cargo\.lock$")  # what `kiln check` compiles


def land(fork, log, ctx, inp):
    """Hold the land lease only to rebase and push. A change main's new commits touched (in code,
    not docs) or one that needed regenerating is checked with the lock released, then
    retried; a clean, untouched rebase pushes at once."""
    git = ["git", "-C", fork]
    built = None
    tail, fmt_fixed = "", False
    for _ in range(ATTEMPTS):
        log("waiting for the land lease")
        with ctx.acquire("land", 1):  # one rebase+push at a time, granted in step priority
            net_sh([*git, "fetch", "-q", "origin", "main"])
            if built is None:
                built = sh([*git, "merge-base", "HEAD", "origin/main"]).stdout.strip()
            if sh([*git, "rev-list", "--count", "origin/main..HEAD"]).stdout.strip() == "0":
                raise RuntimeError("nothing to land: the fork has no commits ahead of origin/main")
            mine = names(fork, "origin/main...HEAD")
            main = sh([*git, "rev-parse", "origin/main"]).stdout.strip()
            touched = {f for f in mine & names(fork, f"{built}..{main}") if BUILD_INPUT.search(f)}
            regen = rebase(fork, inp, ctx)
            if not (regen or touched):
                push = sh([*git, "push", "-q", "origin", "HEAD:main"], check=False)
                if push.returncode == 0:
                    return tail
                out = push.stdout + push.stderr
                if FMT_REFUSAL.search(out) and not fmt_fixed:
                    # The pre-push cargo-fmt hook reformatted files: amend with exactly those
                    # formatting changes and push again (no round-trip to the worker).
                    fmt_fixed = True
                    log("pre-push fmt hook reformatted files; amending formatting only")
                    if amend_formatting(fork):
                        continue
                if not RACE.search(out):  # a hook or other refusal
                    why = "\n".join(out.strip().splitlines()[-25:])
                    raise RuntimeError(f"push refused (not a lost race):\n{why}")
                log("push lost a race; rebasing again")
                continue
        log(f"checking: main's new commits touch {sorted(touched)[:5]}" if touched
            else "checking: a generated file was regenerated")
        b = kiln(fork, "check //...")
        tail = "\n".join(b.stdout.strip().splitlines()[-40:])
        if b.returncode != 0:
            errors = "\n".join(l for l in tail.splitlines() if "error" in l or "-->" in l)[:1500]
            reason = f"kiln check failed after the rebase onto {main[:10]}:\n{tail}"
            reject(inp, ctx, reason,
                   f"The land step's kiln check failed after rebasing onto main {main[:10]}:\n"
                   f"{errors}\nRun git pull --rebase origin main, fix it, kiln check, run the "
                   "tests covering the fix once, commit and submit ready=true.")
        built = main
    raise RuntimeError(f"no clean push after {ATTEMPTS} attempts")


def main(inp, ctx):
    fork = inp["fork"]
    log = getattr(ctx, "log", print)
    if inp.get("ready") is False:
        reason = ("not ready: the work step submitted ready=false. Unresolved: "
                  + (inp.get("unresolved") or "(none given)")[:1500])
        reject(inp, ctx, reason, reason + ". Finish the work, commit and submit ready=true.")
    if inp.get("title"):
        with ctx.acquire("land", 1):
            squash(fork, inp)
    tail = land(fork, log, ctx, inp)
    sha = sh(["git", "-C", fork, "rev-parse", "HEAD"]).stdout.strip()
    if not on_main(fork, sha, fetch=False):
        raise RuntimeError(f"HEAD {sha} is not on origin/main after the push")
    subject = sh(["git", "-C", fork, "log", "-1", "--format=%s"]).stdout.strip()
    message = sh(["git", "-C", fork, "log", "-1", "--format=%B"]).stdout.strip()
    return {"landed_sha": sha, "subject": subject, "message": message, "tail": tail,
            "landed": True}


if __name__ == "__main__":
    run(main)
