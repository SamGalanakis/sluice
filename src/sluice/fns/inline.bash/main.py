# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""inline.bash: run a bash script given as a string."""

import json
import re

from sluice.fn import run, sh_stream


def env_value(value):
    return value if isinstance(value, str) else json.dumps(value)


def main(inp, ctx):
    out_file = ctx.run_dir / "out.json"
    out_file.unlink(missing_ok=True)
    env = {"OUT": str(out_file)}
    for name in ctx.extra_inputs:
        if (value := inp.get(name)) is not None:
            env[re.sub(r"\W", "_", name)] = env_value(value)
    res = sh_stream(["bash", "-e", "-o", "pipefail", "-c", inp["code"]], cwd=inp.get("cwd"),
                    check=inp.get("check") is not False, env=env)
    outputs = {"stdout": res.stdout, "stderr": res.stderr, "code": res.returncode}
    if ctx.outputs:
        if not out_file.exists():
            raise RuntimeError(f"the step declares {', '.join(ctx.outputs)}: write them as one "
                               'JSON object to "$OUT"')
        given = json.loads(out_file.read_text())
        if not isinstance(given, dict):
            raise RuntimeError("$OUT must hold one JSON object")
        outputs |= {k: given[k] for k in ctx.outputs if k in given}
    return outputs


if __name__ == "__main__":
    run(main)
