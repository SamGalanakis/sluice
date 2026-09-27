# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""inline.python: run Python code given as a string."""

import contextlib
import io
import os
import re
import sys

from sluice.fn import run

FIXED = {"code", "cwd"}


class Tee(io.StringIO):
    """Keeps what the code prints and echoes it to stderr, so it shows as progress."""

    def write(self, s):
        sys.stderr.write(s)
        return super().write(s)


def main(inp, ctx):
    if inp.get("cwd"):
        os.chdir(inp["cwd"])
    scope = {"__name__": "__inline__", "inp": inp, "ctx": ctx}
    scope |= {re.sub(r"\W", "_", k): v for k, v in inp.items() if k not in FIXED}
    printed = Tee()
    with contextlib.redirect_stdout(printed):
        exec(compile(inp["code"], "<inline>", "exec"), scope)  # noqa: S102 - its purpose
    out = scope.get("out")
    outputs = {"value": out, "stdout": printed.getvalue()}
    if ctx.outputs:
        if not isinstance(out, dict):
            raise RuntimeError(f"the step declares {', '.join(ctx.outputs)}: set `out` to a "
                               "dict holding them")
        outputs |= {k: out[k] for k in ctx.outputs if k in out}
    return outputs


if __name__ == "__main__":
    run(main)
