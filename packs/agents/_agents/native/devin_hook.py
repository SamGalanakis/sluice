"""Append one Devin lifecycle event to this run's hook stream."""

import os
import sys


def main():
    data = sys.stdin.buffer.read().strip()
    if not data:
        return
    fd = os.open(sys.argv[1], os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, data.replace(b"\n", b"") + b"\n")
    finally:
        os.close(fd)


if __name__ == "__main__":
    main()
