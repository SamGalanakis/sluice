# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import run


def main(inp, ctx):
    if inp.get("ok") or (ctx.home / "boom-ok").exists():
        return {"done": True}
    ctx.log("about to explode")
    raise RuntimeError("boom")


if __name__ == "__main__":
    run(main)
