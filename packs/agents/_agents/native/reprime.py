"""Re-prime a session after its context was compacted: the step's own context (`sluice me`),
or, when that fails, where the task is. Run as a script, it is Claude Code's SessionStart hook:
for source "compact" it prints the context as the hook's additional context. Standard library
only: `sluice` is the host's CLI, the one the agent submits with."""

import json
import subprocess
import sys

REPRIME = ("Your context was just compacted. This is where your sluice step stands "
           "(`sluice me`); your full task is in {task}.\n\n{me}")
FALLBACK = ("Your context was just compacted. Your full task is in {task}; read it again "
            "before you continue (`sluice me` failed: {why}).")


def context(task, env=None):
    """The re-prime text for the step this process runs in (SLUICE_* from `env`, default this
    process's environment)."""
    try:
        p = subprocess.run(["sluice", "me"], capture_output=True, text=True, timeout=30,
                           stdin=subprocess.DEVNULL, check=False, env=env)
    except (OSError, subprocess.SubprocessError) as e:
        return FALLBACK.format(task=task, why=e)
    if p.returncode == 0 and p.stdout.strip():
        return REPRIME.format(task=task, me=p.stdout.strip())
    return FALLBACK.format(task=task, why=_why(p.stderr.strip() or p.stdout.strip())
                           or f"exit {p.returncode}")


def _why(text):
    """The error `sluice` printed: its JSON error's message, else its last line."""
    try:
        return str(json.loads(text).get("message"))[:200]
    except (ValueError, AttributeError):
        return (text.splitlines() or [""])[-1][:200]


def main():
    try:
        payload = json.loads(sys.stdin.read() or "{}")
    except ValueError:
        return
    if not isinstance(payload, dict) or payload.get("source") != "compact":
        return
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "SessionStart",
                                             "additionalContext": context(sys.argv[1])}}))


if __name__ == "__main__":
    main()
