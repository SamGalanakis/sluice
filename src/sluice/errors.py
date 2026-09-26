"""Errors shared by the store, the tools and the CLI. Each maps to a JSON payload (SPEC §8)."""

from __future__ import annotations

import json
from typing import Any


class SluiceError(Exception):
    code = "bad_request"

    def __init__(self, message: str, **extra: Any):
        super().__init__(message)
        self.message = message
        self.extra = extra

    def payload(self) -> dict[str, Any]:
        return {"error": self.code, "message": self.message, **self.extra}

    def to_json(self) -> str:
        return json.dumps(self.payload())


class BadRequest(SluiceError):
    code = "bad_request"


class NotFound(SluiceError):
    code = "not_found"


class Conflict(SluiceError):
    code = "conflict"

    def __init__(self, current_rev: int, message: str | None = None):
        super().__init__(message or f"plan is at rev {current_rev}", current_rev=current_rev)
        self.current_rev = current_rev


class NotOpen(SluiceError):
    """An inbox item that is no longer open (answered or closed): acting on it is stale."""

    code = "conflict"

    def __init__(self, item_id: str, status: str):
        super().__init__(f"inbox item {item_id} is {status}, not open", status=status)


class InvalidPlan(SluiceError):
    code = "invalid"

    def __init__(self, errors: list[str], message: str | None = None):
        super().__init__(message or f"plan is invalid ({len(errors)} errors)", errors=errors)
        self.errors = errors
