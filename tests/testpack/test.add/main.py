# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice_fn import run


def main(inp, ctx):
    ctx.log(f"adding {inp['a']} + {inp['b']}")
    return {"sum": inp["a"] + inp["b"]}


if __name__ == "__main__":
    run(main)
