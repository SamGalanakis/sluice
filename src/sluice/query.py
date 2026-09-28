"""A read-only SQL `query` tool for trusted agents (SPEC §8): one SELECT against the
home's sluice.db, guarded against accidental expensive or huge queries — not adversarial
SQL. Every call opens its own read-only connection (never a cached write one), an
authorizer allows only reads, SQLite's limits are turned down, and a progress handler
aborts a runaway statement.
"""

from __future__ import annotations

import json
import re
import sqlite3
import time
from pathlib import Path
from typing import Any

from . import db
from .errors import BadRequest

MAX_LIMIT = 1000
TIMEOUT = 2.0  # seconds of wall time a query may run (see run's progress handler)
RESPONSE = 1_000_000  # ~bytes of row JSON a result may hold before it truncates
PROGRESS_OPS = 500  # VDBE instructions between progress-handler calls

_ALLOWED = frozenset((sqlite3.SQLITE_SELECT, sqlite3.SQLITE_READ,
                      sqlite3.SQLITE_FUNCTION, sqlite3.SQLITE_RECURSIVE))
_ACTIONS = {v: k for k in (
    "SQLITE_CREATE_INDEX", "SQLITE_CREATE_TABLE", "SQLITE_CREATE_TEMP_INDEX",
    "SQLITE_CREATE_TEMP_TABLE", "SQLITE_CREATE_TEMP_TRIGGER", "SQLITE_CREATE_TEMP_VIEW",
    "SQLITE_CREATE_TRIGGER", "SQLITE_CREATE_VIEW", "SQLITE_DELETE", "SQLITE_DROP_INDEX",
    "SQLITE_DROP_TABLE", "SQLITE_DROP_TEMP_INDEX", "SQLITE_DROP_TEMP_TABLE",
    "SQLITE_DROP_TEMP_TRIGGER", "SQLITE_DROP_TEMP_VIEW", "SQLITE_DROP_TRIGGER",
    "SQLITE_DROP_VIEW", "SQLITE_INSERT", "SQLITE_PRAGMA", "SQLITE_READ", "SQLITE_SELECT",
    "SQLITE_TRANSACTION", "SQLITE_UPDATE", "SQLITE_ATTACH", "SQLITE_DETACH",
    "SQLITE_ALTER_TABLE", "SQLITE_REINDEX", "SQLITE_ANALYZE", "SQLITE_CREATE_VTABLE",
    "SQLITE_DROP_VTABLE", "SQLITE_FUNCTION", "SQLITE_SAVEPOINT", "SQLITE_RECURSIVE")
    if (v := getattr(sqlite3, k, None)) is not None}

_LIMITS = [(sqlite3.SQLITE_LIMIT_LENGTH, 1_000_000),
           (sqlite3.SQLITE_LIMIT_SQL_LENGTH, 100_000),
           (sqlite3.SQLITE_LIMIT_COLUMN, 200),
           (sqlite3.SQLITE_LIMIT_EXPR_DEPTH, 200),
           (sqlite3.SQLITE_LIMIT_COMPOUND_SELECT, 50)]
if (vdbe := getattr(sqlite3, "SQLITE_LIMIT_VDBE_OP", None)) is not None:
    _LIMITS.append((vdbe, 250_000))

_CONSTRAINT = {"PRIMARY", "FOREIGN", "UNIQUE", "CHECK", "CONSTRAINT"}


def _cell(value: Any, column: str) -> Any:
    if isinstance(value, (bytes, memoryview)):
        raise BadRequest(f'column "{column}" holds binary data: select hex("{column}") or '
                         f'length("{column}") instead')
    return value


def _message(e: sqlite3.Error, denied: list[int], timeout: float) -> str:
    if e.sqlite_errorcode & 0xFF == sqlite3.SQLITE_INTERRUPT:
        return f"interrupted: the query ran past {timeout:g} s"
    if denied:
        return (f"{e} — the query is read-only; "
                f"{_ACTIONS.get(denied[-1], f'action {denied[-1]}')} is not allowed")
    return str(e)


def run(home: Path | str, sql: str, params: list | None = None, limit: int = 200,
        timeout: float = TIMEOUT) -> dict[str, Any]:
    """Run `sql` read-only against a home's database: {columns, rows, truncated}. `limit`
    caps the rows (at most `limit + 1` are fetched; one more means `truncated`), and the
    rows' JSON may not pass ~1 MB. Aborts a statement still running after `timeout` s.
    Raises BadRequest for anything refused or failed."""
    if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= MAX_LIMIT:
        raise BadRequest(f"limit: expected an int in 1..{MAX_LIMIT}")
    conn = sqlite3.connect(f"file:{Path(home) / db.FILE}?mode=ro", uri=True,
                           autocommit=True)
    try:
        conn.execute("PRAGMA query_only = ON")
        for op, value in _LIMITS:
            conn.setlimit(op, value)
        denied = []

        def authorize(action, arg1, _arg2, _name, _source):
            if action not in _ALLOWED or (action == sqlite3.SQLITE_FUNCTION
                                          and arg1 == "load_extension"):
                denied.append(action)
                return sqlite3.SQLITE_DENY
            return sqlite3.SQLITE_OK

        conn.set_authorizer(authorize)
        deadline = time.monotonic() + timeout
        # Cooperative: checked only between VDBE instructions, so a single huge scalar can
        # run past the deadline; the length limit bounds how big it can grow.
        conn.set_progress_handler(lambda: time.monotonic() > deadline, PROGRESS_OPS)
        cur = conn.execute(sql, tuple(params or ()))
        try:
            fetched = cur.fetchmany(limit + 1)
            truncated = len(fetched) > limit
            rows, size = [], 0
            for row in fetched[:limit]:
                cells = [_cell(v, cur.description[i][0]) for i, v in enumerate(row)]
                rows.append(cells)
                size += len(json.dumps(cells, ensure_ascii=False)) + 1
                if size > RESPONSE:
                    truncated = True
                    break
            columns = [d[0] for d in cur.description]
        finally:
            cur.close()
    except sqlite3.Error as e:
        raise BadRequest(_message(e, denied, timeout)) from None
    finally:
        conn.close()
    return {"columns": columns, "rows": rows, "truncated": truncated}


def _select_items(sql: str) -> list[str]:
    """The items of a CREATE VIEW's outermost SELECT list: commas inside parens or quotes
    do not split, and the list ends at the first top-level FROM."""
    items, cur, depth, quote = [], "", 0, None
    i = sql.index("SELECT") + len("SELECT")
    while i < len(sql):
        if depth == 0 and quote is None \
                and re.match(r"\s*FROM\b", sql[i:], re.IGNORECASE):
            break
        ch = sql[i]
        if quote:
            if ch == quote:
                quote = None
        elif ch in ("'", '"', "`"):
            quote = ch
        elif ch == "[":
            quote = "]"
        elif ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
        elif ch == "," and depth == 0:
            items.append(cur)
            cur = ""
            i += 1
            continue
        cur += ch
        i += 1
    items.append(cur)
    return items


def _alias(item: str) -> str:
    """A select item's column name: its `AS` alias, else its last word."""
    m = re.search(r"\bAS\s+(\S+)\s*$", item, re.IGNORECASE)
    return (m.group(1) if m else item.split()[-1]).strip("\"'`[]").rsplit(".", 1)[-1]


def objects() -> list[tuple[str, str, list[str]]]:
    """(kind, name, columns) of every CREATE TABLE and CREATE VIEW in db.SCHEMA."""
    out = []
    for m in re.finditer(r"CREATE (TABLE|VIEW) (?:IF NOT EXISTS )?(\w+)", db.SCHEMA):
        kind, name = m.group(1).lower(), m.group(2)
        rest = db.SCHEMA[m.end():]
        if kind == "table":
            body = rest[rest.index("(") + 1:rest.index(") STRICT")]
            cols = [w for w in (line.split()[0].strip('"`[],')
                                for line in body.splitlines() if line.strip())
                    if w.upper() not in _CONSTRAINT]
        else:
            cols = [_alias(item) for item in _select_items(rest)]
        out.append((kind, name, cols))
    return out


_INTRO = """\
Run one read-only SELECT against the home's SQLite database (sluice.db) — for questions
the other tools do not answer: joins, aggregates, a look across projects. Returns
{columns, rows, truncated}: at most `limit` rows, stopping early once the rows' JSON
passes ~1 MB (then truncated is true). A query still running after 2 s is interrupted
(the check runs between VM instructions, so one huge scalar can run past it; values are
capped at 1 MB). Only reads: no writes, ATTACH, PRAGMA or load_extension; BLOB cells are
refused — select hex(col) or length(col) instead. Views join the raw tables for you."""

_EXAMPLES = """\
Examples:
  Paused steps carrying a tag across projects (steps):
    SELECT project, step, status FROM steps, json_each(steps.tags)
     WHERE paused IS NOT NULL AND paused <> 0 AND json_each.value = 'docs'
  Messages on a thread since a seq (messages):
    SELECT seq, "from", "to", body FROM messages
     WHERE project = 'p' AND thread = 't' AND seq > 42 ORDER BY seq
  Failed steps and their errors across projects (steps):
    SELECT project, step, error FROM steps WHERE status = 'failed'

Args:
    sql: one SELECT or WITH statement; ? placeholders bind `params`.
    params: the values for the placeholders, in order (default none).
    limit: at most this many rows, 1-1000 (default 200)."""


def doc() -> str:
    """The `query` tool's docstring: the limits, then every table and view with its
    columns (parsed from db.py's schema text, so the docs cannot drift) and examples."""
    parts = [_INTRO, ""]
    kinds = {"table": [], "view": []}
    for kind, name, cols in objects():
        kinds[kind].append(f"  {name}({', '.join(cols)})")
    parts += ["Tables:", *kinds["table"], "", "Views:", *kinds["view"], "", _EXAMPLES]
    return "\n".join(parts)
