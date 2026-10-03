"""Version local assets together, including URLs imported by JavaScript modules."""

import hashlib
import re
from functools import cache
from pathlib import Path

STATIC = Path(__file__).resolve().parent / "static"
_FILES = {p.name: p.read_bytes() for p in sorted(STATIC.iterdir()) if p.is_file()}
_hash = hashlib.sha256()
for _name, _body in _FILES.items():
    _hash.update(_name.encode() + b"\0" + _body + b"\0")
VERSION = _hash.hexdigest()[:16]
_LOCAL_URL = re.compile(rb"([\"'])/static/([^\"'?]+)\1")


def digest(name: str) -> str:
    """The asset set's version; imports and page tags share this identity."""
    if name not in _FILES:
        raise KeyError(name)
    return VERSION


def url(name: str) -> str:
    return f"/static/{name}?v={digest(name)}"


@cache
def content(name: str) -> bytes:
    """JS gets versioned local references so each module evaluates once per page."""
    body = _FILES[name]
    if not name.endswith(".js"):
        return body

    def versioned(match: re.Match[bytes]) -> bytes:
        target = match[2].decode()
        if target not in _FILES:
            return match[0]
        return match[1] + url(target).encode() + match[1]

    return _LOCAL_URL.sub(versioned, body)
