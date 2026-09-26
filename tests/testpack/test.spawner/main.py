# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import run


def main(inp, ctx):
    ids = [f"{inp['prefix']}-{i}" for i in range(inp["count"])]
    nodes = {i: {"fn": "test.add", "in": {"a": {"value": n}, "b": {"from": f"{ctx.node}.spawned"}}}
             for n, i in enumerate(ids)} if inp.get("bad") else {
        i: {"fn": "test.add", "in": {"a": {"value": n}, "b": {"value": 100}}}
        for n, i in enumerate(ids)}
    ctx.spawn(nodes, reason=f"fan out {len(ids)}")
    return {"spawned": ids}


if __name__ == "__main__":
    run(main)
