# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
import signal
import subprocess
import sys

from sluice.fn import run


def main(inp, ctx):
    # detach: like Claude Code's Bash tool, out of reach of a kill of this process group,
    # so only this fn's own SIGTERM handling stops it.
    child = subprocess.Popen(["sleep", "60"], start_new_session=bool(inp.get("detach")))
    signal.signal(signal.SIGTERM, lambda *_: (child.kill(), sys.exit(143)))
    (ctx.run_dir / "child.pid").write_text(str(child.pid))
    child.wait()
    return {}


if __name__ == "__main__":
    run(main)
