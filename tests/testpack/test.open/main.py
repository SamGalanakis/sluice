# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Submit through the Rust run capability, including invalid and repeated submissions."""
from sluice_fn import CallbackError, run


def main(inp, ctx):
    results = []
    for outputs in inp["attempts"] or []:
        try:
            result = ctx.submit(outputs)
            results.append({"code": 0, "out": result})
        except CallbackError as error:
            results.append({"code": 1, "out": error.envelope})
    return {"ports": {"inputs": ctx.extra_inputs, "outputs": ctx.outputs},
            "extra": {k: inp[k] for k in ctx.extra_inputs}, "results": results}


if __name__ == "__main__":
    run(main)
