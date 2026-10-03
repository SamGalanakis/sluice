# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice_fn import run


def main(inp, ctx):
    return {"summary": "wrote the plan", "final": "the final answer",
            "report": "runs/a/report.md"}


if __name__ == "__main__":
    run(main)
