# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice_fn import run


def main(inp, ctx):
    return {"n": "seven", "report": {"outcome": "ok"}}


if __name__ == "__main__":
    run(main)
