# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import Transient, run


def main(inp, ctx):
    if ctx.attempt < inp["succeed_on"]:
        raise Transient(f"not yet (attempt {ctx.attempt})")
    return {"attempt": ctx.attempt}


if __name__ == "__main__":
    run(main)
