# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import run


def main(inp, ctx):
    ctx.log(f"echoing {inp['msg']}")
    print("stray print goes to stderr")
    return {"msg": inp["msg"], "attempt": ctx.attempt}


if __name__ == "__main__":
    run(main)
