# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""inline.python payload. User stdout is an output; protocol stdout stays private."""
import contextlib
import io
import os
import re
import sys

from sluice_fn import child_env, run, sh


class Tee(io.StringIO):
    def write(self, text):
        sys.stderr.write(text)
        return super().write(text)


def main(inp, ctx):
    if inp.get("cwd"):
        os.chdir(inp["cwd"])
    environment = child_env()
    os.environ.clear()
    os.environ.update(environment)
    scope = {"__name__": "__inline__", "inp": inp, "ctx": ctx, "sh": sh}
    seen = {}
    for name in ctx.extra_inputs:
        var = re.sub(r"[^a-zA-Z0-9_]", "_", name)
        if not var.isidentifier() or var in scope or var == "out" or var in seen:
            raise ValueError(f"extra input {name!r} collides with inline variable {var!r}")
        seen[var] = name
        scope[var] = inp.get(name)
    printed = Tee()
    with contextlib.redirect_stdout(printed):
        exec(compile(inp["code"], "<inline>", "exec"), scope)
    out = scope.get("out")
    outputs = {"value": out, "stdout": printed.getvalue()}
    # Builtin outputs are not step-declared submission fields.
    declared = {name: port for name, port in ctx.outputs.items() if name not in outputs}
    if declared:
        if not isinstance(out, dict):
            raise RuntimeError("the step declares outputs: set `out` to a dict holding them")
        outputs.update({name: out[name] for name in declared if name in out})
    return outputs


if __name__ == "__main__":
    run(main)
