"""The database (SPEC §2): a home's projects, plans, plan edits, state, calls, submissions,
inbox, log, the outcomes of steps removed from plans, how far `next` has read each log and the
section leases in one SQLite file, SLUICE_HOME/sluice.db. Standard library only: fn processes
import it.

Connections open lazily, one per (home, thread, process); a connection cached before a fork is
never used in the child. Writes happen only inside `write()`: BEGIN IMMEDIATE … COMMIT, rolled
back on any exception. A nested `write()` joins the outer one, and an exception escaping it
dooms the whole transaction even when a caller catches it. Reads that must agree run inside one
`read()`. Nothing slow (a process start or stop, a sleep, a run's files) happens inside a write.
"""

from __future__ import annotations

import contextlib
import json
import os
import re
import sqlite3
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

from .errors import SluiceError

FILE = "sluice.db"
VERSION = 5
TIMEOUT = 5.0  # seconds a write waits for the lock before Busy
CACHED = 8  # connections kept per thread (one per home)
MIN_SQLITE = (3, 37)  # STRICT tables
CALL_STATUSES = ("pending", "running", "succeeded", "failed")
INBOX_STATUSES = ("open", "answered", "closed")
OUTCOME_STATUSES = ("succeeded", "failed", "skipped", "stale")  # what a removed step keeps
LIFTED = ("step", "call", "thread", "run")  # record fields also kept as columns, for filters

TABLES = f"""
CREATE TABLE projects (
  name TEXT PRIMARY KEY NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
  paused INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
  resources TEXT NOT NULL DEFAULT '{{}}' CHECK (json_type(resources) = 'object'),
  icon_text TEXT,
  icon_type TEXT,
  icon BLOB,
  icon_hash TEXT,
  created TEXT NOT NULL,
  ver INTEGER NOT NULL DEFAULT 0,
  changed TEXT,
  CHECK (icon_text IS NULL OR icon IS NULL),
  CHECK ((icon IS NULL) = (icon_type IS NULL) AND (icon IS NULL) = (icon_hash IS NULL))
) STRICT;
CREATE TABLE plans (
  project TEXT PRIMARY KEY NOT NULL REFERENCES projects ON DELETE CASCADE,
  rev INTEGER NOT NULL CHECK (rev >= 1),
  doc TEXT NOT NULL CHECK (json_type(doc) = 'object')
) STRICT;
CREATE TABLE plan_edits (
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  rev INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  at TEXT NOT NULL,
  author TEXT NOT NULL,
  reason TEXT NOT NULL,
  ops TEXT NOT NULL CHECK (json_type(ops) = 'array'),
  PRIMARY KEY (project, rev)
) STRICT;
CREATE TABLE states (
  project TEXT PRIMARY KEY NOT NULL REFERENCES projects ON DELETE CASCADE,
  doc TEXT NOT NULL CHECK (json_type(doc) = 'object')
) STRICT;
CREATE TABLE calls (
  call TEXT PRIMARY KEY NOT NULL,
  project TEXT REFERENCES projects ON DELETE CASCADE,
  fn TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ({", ".join(f"'{s}'" for s in CALL_STATUSES)})),
  inputs TEXT NOT NULL CHECK (json_type(inputs) = 'object'),
  outputs TEXT,
  error TEXT,
  direct INTEGER NOT NULL DEFAULT 0 CHECK (direct IN (0, 1)),
  pid INTEGER,
  pid_start TEXT,
  created TEXT NOT NULL,
  finished TEXT
) STRICT;
CREATE TABLE submissions (
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  run TEXT NOT NULL,
  step TEXT NOT NULL,
  outputs TEXT NOT NULL CHECK (json_type(outputs) = 'object'),
  at TEXT NOT NULL,
  PRIMARY KEY (project, run)
) STRICT;
CREATE TABLE inbox (
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  n INTEGER NOT NULL,
  title TEXT NOT NULL,
  body TEXT,
  ui TEXT,
  input TEXT,
  sender TEXT,
  run TEXT,
  status TEXT NOT NULL CHECK (status IN ({", ".join(f"'{s}'" for s in INBOX_STATUSES)})),
  created TEXT NOT NULL,
  answer TEXT,
  answered TEXT,
  closed TEXT,
  reason TEXT,
  PRIMARY KEY (project, n)
) STRICT;
CREATE TABLE deletions (
  name TEXT PRIMARY KEY NOT NULL,
  token TEXT NOT NULL,
  at TEXT NOT NULL
) STRICT;
CREATE TABLE records (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  project TEXT REFERENCES projects ON DELETE CASCADE,
  at TEXT NOT NULL,
  kind TEXT NOT NULL,
  step TEXT,
  call TEXT,
  thread TEXT,
  run TEXT,
  data TEXT NOT NULL CHECK (json_type(data) = 'object')
) STRICT;
CREATE INDEX records_project ON records (project, seq);
CREATE INDEX records_kind ON records (project, kind, seq);
CREATE INDEX records_thread ON records (project, thread, seq);
CREATE INDEX records_call ON records (call);
CREATE INDEX records_run ON records (run);
CREATE INDEX calls_status ON calls (project, status);
CREATE INDEX inbox_status ON inbox (project, status);
"""

# the outcome of each finished step a plan edit removed (SPEC §6): kept, never trimmed. IF NOT
# EXISTS: a file may have the table already at version 1 (put back there by hand)
OUTCOMES = f"""
CREATE TABLE IF NOT EXISTS outcomes (
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  step TEXT NOT NULL,
  rev INTEGER NOT NULL,
  unit TEXT,
  fn TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ({", ".join(f"'{s}'" for s in OUTCOME_STATUSES)})),
  outputs TEXT,
  error TEXT,
  started TEXT,
  finished TEXT,
  run_ids TEXT,
  manual INTEGER NOT NULL DEFAULT 0 CHECK (manual IN (0, 1)),
  removed TEXT NOT NULL,
  author TEXT,
  reason TEXT,
  PRIMARY KEY (project, step, rev)
) STRICT;
CREATE INDEX IF NOT EXISTS outcomes_unit ON outcomes (project, unit);
"""

DRAIN = """
CREATE TABLE drain (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  metadata TEXT NOT NULL CHECK (json_type(metadata) = 'object')
) STRICT;
CREATE TABLE drain_projects (
  project TEXT PRIMARY KEY NOT NULL REFERENCES projects ON DELETE CASCADE
) STRICT;
"""

# how far `next` has read each project's log, when and as whom (SPEC §9): no trigger moves
# `projects.ver` for it, since the dashboard does not show it and `next` writes it every 30 s.
# IF NOT EXISTS, like OUTCOMES: it is also version 3's migration
READERS = """
CREATE TABLE IF NOT EXISTS readers (
  project TEXT PRIMARY KEY NOT NULL REFERENCES projects ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  at TEXT NOT NULL,
  me TEXT NOT NULL
) STRICT;
"""

# section leases (SPEC §6 "Resources", §7 ctx.acquire): a running step's run holding (granted
# set) or waiting for an amount of a project resource. No trigger moves `ver` for them (like
# `readers`): the dashboard does not show them. IF NOT EXISTS, like OUTCOMES: it is also part of
# version 4's migration
LEASES = """
CREATE TABLE IF NOT EXISTS leases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project TEXT NOT NULL REFERENCES projects ON DELETE CASCADE,
  resource TEXT NOT NULL,
  amount INTEGER NOT NULL CHECK (amount >= 0),
  step TEXT NOT NULL,
  run TEXT NOT NULL,
  granted TEXT,
  created TEXT NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS leases_project ON leases (project, id);
"""

VIEWS = """
CREATE VIEW steps AS
SELECT p.project, s.key AS step, s.value ->> '$.run' AS fn,
       COALESCE(e.entry ->> '$.status', 'pending') AS status,
       e.entry ->> '$.started' AS started, e.entry ->> '$.finished' AS finished,
       e.entry ->> '$.error' AS error, e.entry -> '$.outputs' AS outputs,
       COALESCE(e.entry ->> '$.manual', 0) AS manual, e.entry ->> '$.skipped' AS skipped,
       s.value ->> '$.paused' AS paused, s.value -> '$.tags' AS tags,
       s.value -> '$.after' AS after, s.value ->> '$.when' AS "when",
       s.value ->> '$.doc' AS doc, e.entry -> '$.run_ids' AS run_ids,
       e.entry ->> '$.done' AS done, e.entry ->> '$.total' AS total, e.entry AS entry,
       pr.paused AS project_paused
FROM plans p
JOIN projects pr ON pr.name = p.project
JOIN json_each(p.doc, '$.steps') s
LEFT JOIN (SELECT st.project, x.key AS step, x.value AS entry
           FROM states st, json_each(st.doc, '$.steps') x) e
  ON e.project = p.project AND e.step = s.key;
CREATE VIEW messages AS
SELECT project, seq, at, thread, data ->> '$.from' AS "from", data ->> '$.to' AS "to",
       data ->> '$.body' AS body, COALESCE(data ->> '$.needs_reply', 1) AS needs_reply,
       data -> '$.data' AS data
FROM records WHERE kind = 'message';
CREATE VIEW step_changes AS
SELECT project, seq, at, step, data ->> '$.from' AS "from", data ->> '$.to' AS "to",
       data ->> '$.error' AS error, data -> '$.run_ids' AS run_ids
FROM records WHERE kind = 'step.status';
CREATE VIEW edits AS
SELECT project, rev, seq, at, author, reason, ops FROM plan_edits;
CREATE VIEW log AS
SELECT project, seq, at, kind, json_insert(data, '$.seq', seq, '$.at', at, '$.kind', kind)
       AS record
FROM records;
"""


def _triggers() -> str:
    """`projects.ver` counts up with every change to a project's rows (a rollback takes it
    back); `changed` is the time of its last state write."""
    out = []
    for table in ("plans", "plan_edits", "states", "calls", "submissions", "inbox", "records"):
        extra = ", changed = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')" if table == "states" else ""
        for event, row in (("INSERT", "NEW"), ("UPDATE", "NEW"), ("DELETE", "OLD")):
            out.append(f"CREATE TRIGGER {table}_{event.lower()} AFTER {event} ON {table} "
                       f"WHEN {row}.project IS NOT NULL BEGIN UPDATE projects SET ver = ver + 1"
                       f"{extra} WHERE name = {row}.project; END;")
    out.append("CREATE TRIGGER projects_update AFTER UPDATE ON projects WHEN NEW.ver = OLD.ver "
               "BEGIN UPDATE projects SET ver = ver + 1 WHERE name = NEW.name; END;")
    return "\n".join(out) + "\n"


SCHEMA = TABLES + OUTCOMES + DRAIN + READERS + LEASES + VIEWS + _triggers()
# version -> the script that takes a database of that version to the next
# a project's resources (SPEC §6 "Resources"): {name: {capacity} or {capacity_fn}}
RESOURCES = ("ALTER TABLE projects ADD COLUMN resources TEXT NOT NULL DEFAULT '{}' "
             "CHECK (json_type(resources) = 'object');")
MIGRATIONS = {1: OUTCOMES, 2: "ALTER TABLE inbox ADD COLUMN run TEXT;" + DRAIN, 3: READERS,
              4: RESOURCES + LEASES}


def _split_senders(conn: sqlite3.Connection) -> None:
    """Decode version-2 senders exactly as its inbox reader did, including ambiguous names."""
    pattern = re.compile(r"^((?:step:)?[a-z0-9][a-z0-9_-]*)#([0-9A-Za-z][0-9A-Za-z_.-]*)$")
    rows = conn.execute("SELECT project, n, sender FROM inbox WHERE sender IS NOT NULL").fetchall()
    for row in rows:
        if match := pattern.match(row["sender"]):
            conn.execute("UPDATE inbox SET sender = ?, run = ? WHERE project = ? AND n = ?",
                         (*match.groups(), row["project"], row["n"]))


class Busy(SluiceError):
    """A write that could not get the database within TIMEOUT; nothing was written."""

    code = "busy"

    def __init__(self) -> None:
        super().__init__("the store is busy, try again")


class _Conn:
    def __init__(self, conn: sqlite3.Connection):
        self.conn = conn
        self.pid = os.getpid()
        self.depth = 0  # nested write() blocks open
        self.doomed = False  # an inner write() failed: the outer one rolls back
        self.reading = False
        self.after: list[Callable[[], None]] = []


_local = threading.local()
_forked: list[sqlite3.Connection] = []  # cached before a fork: never used, never closed


def old_home(home: Path) -> str | None:
    """A file of the storage before sluice.db (the home must be imported first), or None."""
    if (home / "log.jsonl").exists():
        return "log.jsonl"
    for f in sorted((home / "projects").glob("*/project.json")):
        return str(f.relative_to(home))
    return None


def _open(home: Path) -> sqlite3.Connection:
    if sqlite3.sqlite_version_info < MIN_SQLITE:
        raise SluiceError(f"sluice needs SQLite {'.'.join(map(str, MIN_SQLITE))} or later, "
                          f"this Python has {sqlite3.sqlite_version}")
    path = home / FILE
    if not path.exists():
        if (old := old_home(home)) is not None:
            raise SluiceError(
                f"{home} predates the SQLite store (it has {old} and no {FILE}): import it "
                f"into a new home with the one-off importer, or point SLUICE_HOME elsewhere")
        home.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, timeout=TIMEOUT, autocommit=True)
    try:
        conn.row_factory = sqlite3.Row
        conn.execute(f"PRAGMA busy_timeout = {int(TIMEOUT * 1000)}")
        conn.execute("PRAGMA foreign_keys = ON")
        conn.execute("PRAGMA synchronous = FULL")
        _bootstrap(conn, path)
    except sqlite3.OperationalError as e:
        conn.close()
        if _locked(e):  # another opener held the file past TIMEOUT (_wal, or a lock wait)
            raise Busy() from None
        raise
    except BaseException:
        conn.close()
        raise
    return conn


def _version(conn: sqlite3.Connection) -> int:
    return conn.execute("PRAGMA user_version").fetchone()[0]


def _bootstrap(conn: sqlite3.Connection, path: Path) -> None:
    """A new file gets the whole schema and its version, an older one its MIGRATIONS in turn,
    each in one transaction (the version is checked again under the lock, so two first opens
    race safely); an unknown version is refused, and a current one needs nothing."""
    version = _version(conn)
    if version == VERSION:
        return
    if version == 0 or version in MIGRATIONS:
        if version == 0:
            _wal(conn)
        _begin(conn)
        try:
            version = was = _version(conn)
            if version == 0:
                conn.executescript(SCHEMA)
                version = VERSION
            while version in MIGRATIONS:
                conn.executescript(MIGRATIONS[version])
                if version == 2:
                    _split_senders(conn)
                version += 1
            if version != was:
                conn.execute(f"PRAGMA user_version = {version}")
            conn.execute("COMMIT")
        except BaseException:
            _rollback(conn)
            raise
    if version != VERSION:
        raise SluiceError(f"{path} has schema version {version}; this sluice knows version "
                          f"{VERSION}")


def _locked(e: sqlite3.OperationalError) -> bool:
    return e.sqlite_errorcode & 0xFF in (sqlite3.SQLITE_BUSY, sqlite3.SQLITE_LOCKED)


def _wal(conn: sqlite3.Connection) -> None:
    """Switch a new file to WAL. The switch does not wait on the busy handler, so while
    another first open holds the file it is retried until TIMEOUT (then _open says Busy)."""
    deadline = time.monotonic() + TIMEOUT
    while True:
        try:
            conn.execute("PRAGMA journal_mode = WAL")
            return
        except sqlite3.OperationalError as e:
            if not _locked(e) or time.monotonic() > deadline:
                raise
        time.sleep(0.02)


def _begin(conn: sqlite3.Connection) -> None:
    try:
        conn.execute("BEGIN IMMEDIATE")
    except sqlite3.OperationalError as e:
        if _locked(e):
            raise Busy() from None
        raise


def _rollback(conn: sqlite3.Connection) -> None:
    if conn.in_transaction:
        with contextlib.suppress(sqlite3.Error):
            conn.execute("ROLLBACK")


def _get(home: Path | str) -> _Conn:
    key = os.path.realpath(home)
    cache: dict[str, _Conn] = _local.__dict__.setdefault("conns", {})
    c = cache.pop(key, None)
    if c is not None and c.pid != os.getpid():
        _forked.append(c.conn)
        c = None
    if c is None:
        c = _Conn(_open(Path(key)))
        for old in [k for k, v in cache.items() if not (v.depth or v.reading)][:-CACHED + 1]:
            gone = cache.pop(old)
            if gone.pid == os.getpid():
                gone.conn.close()
            else:
                _forked.append(gone.conn)
    cache[key] = c  # the most recently used last
    return c


def connect(home: Path | str) -> sqlite3.Connection:
    """This thread's connection to the home's database (opened, and the schema created, on
    first use). Refuses a home from before the SQLite store and an unknown schema version."""
    return _get(home).conn


@contextlib.contextmanager
def write(home: Path | str) -> Iterator[sqlite3.Connection]:
    """A write transaction (BEGIN IMMEDIATE; Busy when the lock does not come within TIMEOUT).
    Inside another one it joins it. `after_commit` callbacks run once the outermost commits."""
    c = _get(home)
    if c.reading:
        raise RuntimeError("a read transaction is never promoted to a write")
    if c.depth:
        c.depth += 1
        try:
            yield c.conn
        except BaseException:
            c.doomed = True
            raise
        finally:
            c.depth -= 1
        return
    _begin(c.conn)
    c.depth, c.doomed, c.after = 1, False, []
    try:
        yield c.conn
        if c.doomed:
            raise SluiceError("nothing was written: a part of the change failed")
        c.conn.execute("COMMIT")
    except BaseException:
        _rollback(c.conn)
        c.after = []
        raise
    finally:
        c.depth = 0
    after, c.after = c.after, []
    for fn in after:
        fn()


@contextlib.contextmanager
def read(home: Path | str) -> Iterator[sqlite3.Connection]:
    """A read transaction: every statement inside sees one snapshot. Inside a write or another
    read it joins that."""
    c = _get(home)
    if c.depth or c.reading:
        yield c.conn
        return
    c.conn.execute("BEGIN")
    c.reading = True
    try:
        yield c.conn
    finally:
        c.reading = False
        _rollback(c.conn)  # it wrote nothing: ending it either way releases the snapshot


def after_commit(home: Path | str, fn: Callable[[], None]) -> None:
    """Call `fn` once the current write transaction commits (never if it rolls back); now when
    there is none."""
    c = _get(home)
    if c.depth:
        c.after.append(fn)
    else:
        fn()


def in_write(home: Path | str) -> bool:
    return _get(home).depth > 0


def all_rows(conn: sqlite3.Connection, sql: str, params: Any = ()) -> list[sqlite3.Row]:
    """Every row, the cursor closed before returning (an open one pins the WAL)."""
    cur = conn.execute(sql, params)
    try:
        return cur.fetchall()
    finally:
        cur.close()


def one(conn: sqlite3.Connection, sql: str, params: Any = ()) -> sqlite3.Row | None:
    cur = conn.execute(sql, params)
    try:
        return cur.fetchone()
    finally:
        cur.close()


def insert_record(conn: sqlite3.Connection, project: str | None, at: str,
                  rec: dict[str, Any]) -> int:
    """Store a record (`kind` and its fields; its own `at` wins over `at`); returns its seq.
    Its fields stay whole in `data`, explicit nulls included; LIFTED ones given as strings are
    copied into columns."""
    data = {k: v for k, v in rec.items() if k not in ("seq", "at", "kind")}
    lifted = [v if isinstance(v := rec.get(k), str) else None for k in LIFTED]
    cur = conn.execute(
        "INSERT INTO records (project, at, kind, step, call, thread, run, data) "
        "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        (project, rec.get("at") or at, rec["kind"], *lifted,
         json.dumps(data, ensure_ascii=False)))
    return int(cur.lastrowid)


def record_of(row: sqlite3.Row) -> dict[str, Any]:
    """A records row as the record it stored: {seq, at, kind, ...its fields}."""
    return {"seq": row["seq"], "at": row["at"], "kind": row["kind"], **json.loads(row["data"])}


def submission(home: Path | str, project: str, run: str) -> dict[str, Any] | None:
    """What the agent of a run has submitted (step_submit), or None."""
    with read(home) as conn:
        row = one(conn, "SELECT outputs FROM submissions WHERE project = ? AND run = ?",
                  (project, run))
    return json.loads(row["outputs"]) if row else None


def backup(home: Path | str, dest: Path, force: bool = False) -> int:
    """An online copy of the home's database at `dest`, through SQLite's backup API in one
    step, so it is the snapshot of one read transaction while the runner and the server go on
    writing. It is written to a temp file beside `dest`, then renamed into place; an existing
    `dest` is refused (SluiceError) unless `force`. Returns the copy's size in bytes."""
    home, dest = Path(home), Path(dest)
    if dest.is_dir():
        raise SluiceError(f"{dest} is a directory; give the backup file's path")
    if dest.exists() and dest.resolve() == (home / FILE).resolve():
        raise SluiceError(f"{dest} is the home's own database")
    if dest.exists() and not force:
        raise SluiceError(f"{dest} exists; pass --force to overwrite it")
    tmp = dest.with_name(f".{dest.name}.{os.getpid()}.tmp")
    src = _open(home)
    try:
        out = sqlite3.connect(tmp)
        try:
            src.backup(out)  # pages=-1: every page in one step, one consistent snapshot
        finally:
            out.close()
        os.replace(tmp, dest)
    finally:
        src.close()
        for leftover in (tmp, tmp.with_name(tmp.name + "-wal"), tmp.with_name(tmp.name + "-shm")):
            with contextlib.suppress(FileNotFoundError):
                leftover.unlink()
    return dest.stat().st_size
