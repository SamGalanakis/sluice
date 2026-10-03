# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice_fn import run


def main(inp, ctx):
    return {"parts": inp["text"].split()}


if __name__ == "__main__":
    run(main)
