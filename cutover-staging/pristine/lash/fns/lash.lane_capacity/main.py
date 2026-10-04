# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""lash.lane_capacity: the lane resource's capacity, from the machine's load."""

import os

from sluice.fn import run

LANES = 56
LOAD_MAX = 48  # 1-minute load above this (1.5x the 32 cores): admit nothing new


def main(inp, ctx):
    return {"capacity": LANES if os.getloadavg()[0] <= LOAD_MAX else 0}


if __name__ == "__main__":
    run(main)
