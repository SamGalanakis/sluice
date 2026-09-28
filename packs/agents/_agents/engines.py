"""Shared helpers for the remaining one-shot functions in the agents pack."""

from pathlib import Path

CLAUDE_TRANSIENT = ("rate limit", "rate_limit", "overloaded", "529")


def _session(log):
    """The session id a legacy harness wrote next to its log (empty if absent)."""
    path = Path(str(log) + ".session")
    return path.read_text().strip() if path.exists() else ""
