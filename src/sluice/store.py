"""The workspace (SPEC §2, §5, §6): config and function scopes on disk; projects with their
plan, edit history, state, calls, inbox and log in the home's database (db.py); and the edits
made by hand (manual values). Each logical change is one write transaction."""

from __future__ import annotations

import copy
import datetime as dt
import hashlib
import json
import os
import secrets
import shutil
import threading
import unicodedata
import xml.etree.ElementTree as ET
from collections import Counter
from collections.abc import Callable
from contextlib import AbstractContextManager
from pathlib import Path
from sqlite3 import Connection
from typing import Any

import jsonpatch
import jsonpointer

from . import db
from . import inbox as I
from . import log as L
from . import plan as P
from . import recipe as RC
from . import registry as R
from . import state as S
from . import types as T
from .errors import BadRequest, Conflict, InvalidPlan, NotFound, NotOpen
from .util import atomic_write_json, atomic_write_text, now_iso, read_json

DEFAULT_CONFIG: dict[str, Any] = {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420},
                                  "log_max": L.DEFAULT_MAX}
# the image types an icon may be, sniffed from the file's content (never its name): an SVG
# parses as XML with an <svg> root, the rest by magic bytes
ICON_TYPES = {"svg": "image/svg+xml", "png": "image/png", "webp": "image/webp",
              "jpg": "image/jpeg", "gif": "image/gif"}
ICON_MAX = 256 * 1024  # the largest image icon (bytes)
ICON_TEXT_MAX = 16  # characters of a text icon, stripped
ANSWER_KEYS = {"action": str, "params": dict, "values": dict, "text": str}
# what a projects/<name>/ dir may already hold when a project of that name is created
PREPARED = {"fns", ".env"}


def default_home() -> Path:
    return Path(os.environ.get("SLUICE_HOME") or Path.home() / ".sluice")



BRIEF = 200  # characters of a string value `status(brief=True)` keeps
# a ready core.external step's `waiting`: nothing in sluice will start it
EXTERNAL_WAIT = "external: set its outputs with step_set_output"


def _brief(value: Any) -> Any:
    """A value with every string over BRIEF characters cut to its start and a note of how
    much more there is."""
    if isinstance(value, str) and len(value) > BRIEF:
        return f"{value[:BRIEF]}… [{len(value) - BRIEF} more characters]"
    if isinstance(value, dict):
        return {k: _brief(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_brief(v) for v in value]
    return value


def _sniff_icon(path: Path) -> tuple[str, bytes]:
    """(extension, content) of the image at `path`: an SVG parses as XML with an <svg> root,
    the others match magic bytes; BadRequest says what was wrong."""
    try:
        too_big = path.stat().st_size > ICON_MAX
        data = b"" if too_big else path.read_bytes()
    except OSError:
        raise BadRequest(f"icon: no readable file at {path}") from None
    if too_big:
        raise BadRequest(f"icon: {path} is over {ICON_MAX // 1024} KB")
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png", data
    if data[:6] in (b"GIF87a", b"GIF89a"):
        return "gif", data
    if data.startswith(b"\xff\xd8\xff"):
        return "jpg", data
    if data[:4] == b"RIFF" and data[8:12] == b"WEBP":
        return "webp", data
    try:
        root = ET.fromstring(data)
        ok = root.tag.rpartition("}")[2] == "svg"
    except ET.ParseError:
        ok = False
    if ok:
        return "svg", data
    raise BadRequest(f"icon: {path} is not an SVG, PNG, WebP, JPEG or GIF image")


def _dumps(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False)


class Store:
    """All reads and writes of a SLUICE_HOME. Safe across threads and processes: every write
    is one transaction of the home's database (db.py)."""

    def __init__(self, home: Path | str | None = None):
        self.home = Path(home) if home is not None else default_home()
        self.config = copy.deepcopy(DEFAULT_CONFIG)
        if (self.home / "config.json").exists():
            self.config.update(read_json(self.home / "config.json"))
        self.listeners: list[Callable[[], None]] = []  # called after every accepted edit
        self._parsed: dict[str, tuple[Any, P.Plan]] = {}
        self._scans: dict[tuple, tuple[tuple, tuple[list[R.Entry], list]]] = {}
        self._scan_lock = threading.Lock()

    # ---- paths ----

    def show(self, path: Path) -> str:
        """A path for messages: relative to SLUICE_HOME when inside it."""
        try:
            return str(Path(path).resolve().relative_to(self.home.resolve()))
        except ValueError:
            return str(path)

    def project_dir(self, name: str) -> Path:
        """Where a project's files live (its fns/, .env and runs/)."""
        if not isinstance(name, str) or not P.ID_RE.match(name):
            raise NotFound(f"no project {name!r} (project names match {P.ID_RE.pattern})")
        return self.home / "projects" / name

    def runs_dir(self, project: str | None) -> Path:
        """The run dirs of a project, or of the calls without one (SLUICE_HOME/runs)."""
        return (self.project_dir(project) if project else self.home) / "runs"

    def global_fn_dirs(self) -> list[Path]:
        return [self.home / "fns", *(self.home / d for d in self.config["fn_dirs"])]

    # ---- the database ----

    def tx(self) -> AbstractContextManager[Connection]:
        """A write transaction (db.write): nested ones join it; listeners hear of it once it
        commits."""
        return db.write(self.home)

    def rx(self) -> AbstractContextManager[Connection]:
        """A read transaction (db.read): one snapshot for the reads inside it."""
        return db.read(self.home)

    def _row(self, conn: Connection, name: str) -> Any:
        """The project's row, or NotFound."""
        row = db.one(conn, "SELECT * FROM projects WHERE name = ?", (name,)) \
            if isinstance(name, str) else None
        if row is None:
            raise NotFound(f"no project {name!r}")
        return row

    # ---- the log (SPEC §6b) ----

    def log_cap(self) -> int:
        return int(self.config.get("log_max") or L.DEFAULT_MAX)

    def append(self, project: str | None, *records: dict[str, Any]) -> list[int]:
        """Append records to the project's (or the home's) log; returns their seqs."""
        with self.tx() as conn:
            return L.append(conn, project, list(records), self.log_cap())

    # ---- functions (SPEC §2 scopes) ----

    def _scan(self, scope: str, dirs: list[Path], missing_ok: list[Path]) -> tuple:
        """Scan a scope's dirs, cached until a fn.json or main.py in them changes."""
        dirs = [d.resolve() for d in dirs]
        missing_ok = [d.resolve() for d in missing_ok]
        key = (scope, tuple(dirs))
        fp = R.fingerprint(dirs)
        with self._scan_lock:
            hit = self._scans.get(key)
            if hit is None or hit[0] != fp:
                hit = self._scans[key] = (fp, R.scan(scope, dirs, self.show, missing_ok))
        return fp, hit[1]

    def registry(self, project: str | None = None) -> R.Registry:
        """What a project (or, without one, the global context) sees: built-in, global and
        project functions in lookup order, with every problem found. Never raises for them."""
        scopes = [("builtin", [R.BUILTIN_DIR], []),
                  ("global", self.global_fn_dirs(), [self.home / "fns"])]
        if project is not None:
            self.project(project)
            fns = self.project_dir(project) / "fns"
            scopes.append(("project", [fns], [fns]))
        entries: list[R.Entry] = []
        problems: list[dict[str, str]] = []
        key = []
        for scope, dirs, missing_ok in scopes:
            fp, (found, probs) = self._scan(scope, dirs, missing_ok)
            key.append(fp)
            entries += found
            problems += probs
        return R.Registry(entries, problems, self.show, key=tuple(key))

    def usable_registry(self, project: str | None = None) -> R.Registry:
        """The registry, refusing when any fn it covers has a problem (SPEC §2): plan edits,
        manual values, fn_call and runs wait until verify is clean for these scopes."""
        reg = self.registry(project)
        if reg.blocking:
            who = f"project {project}" if project else "the global functions"
            raise InvalidPlan([f"{p['where']}: {p['message']}" for p in reg.blocking],
                              f"{who}: function problems block edits and runs until fixed "
                              f"(see verify)")
        return reg

    def fn(self, name: str, project: str | None = None) -> R.Fn:
        fn = self.registry(project).get(name)
        if fn is None:
            raise NotFound(f"no fn {name!r}" + (f" in project {project}" if project else ""))
        return fn

    def fn_save(self, raw: Any, main_py: Any, project: str | None = None) -> dict[str, str]:
        """Validate a fn.json and write it with main.py into the project's (or the global)
        fns/<name>/. Refuses a name that collides with another scope (SPEC §2)."""
        scope = "project" if project else "global"
        fn, errs = R.parse_fn(raw, Path(), scope, check_dir=False)
        if not isinstance(main_py, str) or not main_py.strip():
            errs.append("main_py: the fn's Python source is required")
        if fn is None or errs:
            raise InvalidPlan(errs, "not a valid fn")
        name = fn.name
        if project is not None:
            self.project(project)
        root = (self.project_dir(project) if project else self.home) / "fns"
        target = root / name
        clash = []
        if project is not None:
            other = self.registry(None).get(name)  # built-in or global
            if other is not None:
                clash.append(f"the {other.scope} fn at {self.show(other.dir)}")
        else:
            for e in self.registry(None).entries:
                if e.name == name and e.dir.resolve() != target.resolve():
                    clash.append(f"the {e.scope} fn at {self.show(e.dir)}")
            for p in self.project_names():
                if (self.project_dir(p) / "fns" / name / "fn.json").exists():
                    clash.append(f"the fn of project {p}")
        if clash:
            raise BadRequest(f"fn {name} would collide with {', '.join(clash)}")
        target.mkdir(parents=True, exist_ok=True)
        atomic_write_text(target / "main.py", main_py)
        atomic_write_json(target / "fn.json", raw)
        return {"scope": scope, "path": str(target)}

    # ---- projects ----

    def project_names(self) -> list[str]:
        with self.rx() as conn:
            return [r[0] for r in db.all_rows(conn, "SELECT name FROM projects ORDER BY name")]

    def project(self, name: str) -> dict[str, Any]:
        """{name, description, archived, paused, icon?} (icon: a text icon's text)."""
        with self.rx() as conn:
            row = self._row(conn, name)
        info = {"name": row["name"], "description": row["description"],
                "archived": bool(row["archived"]), "paused": bool(row["paused"])}
        if row["icon_text"] is not None:
            info["icon"] = row["icon_text"]
        return info

    def create_project(self, name: str, description: str = "", author: str = "",
                       reason: str = "", icon: str | None = None) -> dict[str, str]:
        """A project with the empty plan at rev 1 and an empty state (SPEC §5); `icon` as in
        update_project. Its directory comes when something needs it (runs/, fns/); one that is
        already there may hold only fns/ and .env."""
        if not isinstance(name, str) or not P.ID_RE.match(name):
            raise BadRequest(f"project names match {P.ID_RE.pattern}, got {name!r}")
        if not isinstance(description, str):
            raise BadRequest("description: expected a string")
        resolved = self._read_icon(icon) if icon is not None else None
        d = self.project_dir(name)
        with self.tx() as conn:
            if db.one(conn, "SELECT 1 FROM projects WHERE name = ?", (name,)) is not None:
                raise BadRequest(f"project {name!r} already exists")
            if db.one(conn, "SELECT 1 FROM deletions WHERE name = ?", (name,)) is not None:
                raise BadRequest(f"project {name!r} is still being removed; try again shortly")
            left = sorted(p.name for p in d.iterdir() if p.name not in PREPARED) \
                if d.is_dir() else []
            if left:
                raise BadRequest(f"{self.show(d)} is left over from an earlier project (it holds "
                                 f"{', '.join(left)}); remove it first")
            conn.execute("INSERT INTO projects (name, description, created) VALUES (?, ?, ?)",
                         (name, description, now_iso()))
            self._set_icon(conn, name, resolved)
            doc = copy.deepcopy(P.EMPTY)
            conn.execute("INSERT INTO plans (project, rev, doc) VALUES (?, 1, ?)",
                         (name, _dumps(doc)))
            conn.execute("INSERT INTO states (project, doc) VALUES (?, ?)",
                         (name, _dumps({"inputs": {}, "steps": {}})))
            self._log(conn, name, 1, author, reason or "project created",
                      [{"op": "add", "path": "", "value": doc}])
            self.notify()
        return {"name": name}

    def update_project(self, name: str, description: str | None = None,
                       archived: bool | None = None, paused: bool | None = None,
                       icon: str | None = None) -> dict[str, str]:
        """Replace the description and/or set `archived` (an archived project stays whole and
        keeps running; the dashboard lists it apart) and/or
        `paused` (no step of it starts until unpaused; running ones finish) and/or the icon:
        an absolute path to an image (SVG, PNG, WebP, JPEG or GIF, at most 256 KB, copied in)
        or a short text icon (at most 16 characters, no control characters); "" removes the
        icon. A project has at most one of the two."""
        if description is not None and not isinstance(description, str):
            raise BadRequest("description: expected a string")
        for key, value in (("archived", archived), ("paused", paused)):
            if value is not None and not isinstance(value, bool):
                raise BadRequest(f"{key}: expected true or false")
        resolved = self._read_icon(icon) if icon is not None else None
        with self.tx() as conn:
            row = self._row(conn, name)
            new = {k: v for k, v in (("description", description), ("archived", archived),
                                     ("paused", paused)) if v is not None and row[k] != v}
            if new:
                conn.execute(f"UPDATE projects SET {', '.join(f'{k} = ?' for k in new)} "
                             "WHERE name = ?", (*new.values(), name))
            if icon is not None:
                self._set_icon(conn, name, resolved)
            self.notify()
        return {"name": name}

    def _read_icon(self, icon: Any) -> tuple[str, Any] | None:
        """Resolve an `icon` argument: ("image", (ext, content)) or ("text", text) to set,
        None to clear. A string starting with / or ~ must be a readable image file; it is
        never a text icon."""
        if not isinstance(icon, str):
            raise BadRequest("icon: expected a string")
        icon = icon.strip()
        if not icon:
            return None
        if icon.startswith(("/", "~")):
            return "image", _sniff_icon(Path(icon).expanduser())
        if len(icon) > ICON_TEXT_MAX:
            raise BadRequest(f"icon: a text icon is at most {ICON_TEXT_MAX} characters")
        if any(unicodedata.category(c) == "Cc" for c in icon):
            raise BadRequest("icon: a text icon may not contain control characters")
        return "text", icon

    def _set_icon(self, conn: Connection, name: str, resolved: tuple[str, Any] | None) -> None:
        """Store a resolved icon: an image's bytes, type and sha256, or a text; setting either
        clears the other, None clears both. Nothing is written when nothing changes."""
        text = kind = data = digest = None
        if resolved is not None and resolved[0] == "image":
            ext, data = resolved[1]
            kind, digest = ICON_TYPES[ext], hashlib.sha256(data).hexdigest()
        elif resolved is not None:
            text = resolved[1]
        row = db.one(conn, "SELECT icon_text, icon_hash FROM projects WHERE name = ?", (name,))
        if (row["icon_text"], row["icon_hash"]) != (text, digest):
            conn.execute("UPDATE projects SET icon_text = ?, icon_type = ?, icon = ?, "
                         "icon_hash = ? WHERE name = ?", (text, kind, data, digest, name))

    def icon(self, name: str) -> dict[str, Any] | None:
        """The project's icon, as projects_list reports it: {"kind": "image", "type":
        <content type>} or {"kind": "text", "text": <text>}; None when it has neither."""
        with self.rx() as conn:
            row = self._row(conn, name)
        return _icon(row)

    def icon_image(self, name: str) -> tuple[str, bytes, str] | None:
        """The project's image icon: (content type, bytes, sha256), or None."""
        with self.rx() as conn:
            row = db.one(conn, "SELECT icon_type, icon, icon_hash FROM projects WHERE name = ?",
                         (name,))
        if row is None:
            raise NotFound(f"no project {name!r}")
        return (row["icon_type"], row["icon"], row["icon_hash"]) if row["icon"] else None

    def icon_hash(self, name: str) -> str | None:
        """The sha256 of the project's image icon (its cache identity), or None."""
        with self.rx() as conn:
            row = db.one(conn, "SELECT icon_hash FROM projects WHERE name = ?", (name,))
        return row["icon_hash"] if row else None

    def delete_project(self, name: str) -> dict[str, Any]:
        """Delete a project and everything it holds (plan, state, log, inbox, calls, runs).
        Refused unless it is archived first, none of its steps is running and no non-direct
        call on it is pending or running (a direct call runs in the caller's own process; it
        can record nothing once the project is gone). One transaction deletes the rows and
        records the deletion (`deletions`), which keeps the name from being created again
        until its directory is gone; once the outermost transaction commits, the directory
        moves to SLUICE_HOME/trash/ and is removed (finish_deletions)."""
        token = secrets.token_hex(4)
        with self.tx() as conn:
            if not self._row(conn, name)["archived"]:
                raise BadRequest(f"archive project {name!r} before deleting it")
            running = [s for s, e in self.read_state(name)["steps"].items()
                       if e.get("status") == "running"]
            if running:
                raise BadRequest(f"project {name!r} has running steps: {', '.join(running)}")
            live = [f"{r['call']} ({r['status']})" for r in db.all_rows(
                conn, "SELECT call, status FROM calls WHERE project = ? AND direct = 0 AND "
                      "status IN ('pending', 'running') ORDER BY call", (name,))]
            if live:
                raise BadRequest(
                    f"project {name!r} has pending or running calls: {', '.join(live)}")
            conn.execute("DELETE FROM projects WHERE name = ?", (name,))
            conn.execute("INSERT INTO deletions (name, token, at) VALUES (?, ?, ?)",
                         (name, token, now_iso()))
            self.notify()
            db.after_commit(self.home, lambda: self._remove_dir(name, token))
        self._parsed.pop(name, None)
        return {"deleted": name}

    def _remove_dir(self, name: str, token: str) -> bool:
        """Remove a deleted project's directory — moved to trash/<name>-<token>, then removed
        — and then its `deletions` row. Idempotent; a step that fails leaves the row for the
        runner's GC to finish. Returns whether it is done. The move happens in a write
        transaction that still finds the row, and the row stops a project of the name from
        being created, so nothing here can touch a project created after the deletion (not
        even from a GC pass that read the row before another process finished it)."""
        d, trash = self.project_dir(name), self.home / "trash" / f"{name}-{token}"
        try:
            if trash.exists():  # an earlier attempt's leftover: os.replace needs room
                shutil.rmtree(trash)
            with self.tx() as conn:
                if db.one(conn, "SELECT 1 FROM deletions WHERE name = ? AND token = ?",
                          (name, token)) is None:
                    return True  # finished already
                if d.exists():
                    trash.parent.mkdir(parents=True, exist_ok=True)
                    os.replace(d, trash)
            if trash.exists():
                shutil.rmtree(trash)
            with self.tx() as conn:
                if d.exists():  # something wrote there again: moved on the next pass
                    return False
                conn.execute("DELETE FROM deletions WHERE name = ? AND token = ?",
                             (name, token))
        except (OSError, db.Busy):
            return False
        return True

    def finish_deletions(self) -> list[str]:
        """Finish every deletion whose directory removal failed or was cut short (a failed
        move or removal, a process that died after the commit); returns the names still
        pending."""
        with self.rx() as conn:
            rows = db.all_rows(conn, "SELECT name, token FROM deletions ORDER BY name")
        return [r["name"] for r in rows if not self._remove_dir(r["name"], r["token"])]

    def archived(self, name: str) -> bool:
        try:
            return self.project(name)["archived"]
        except NotFound:
            return False

    def paused(self, name: str) -> bool:
        try:
            return self.project(name)["paused"]
        except NotFound:
            return False

    def projects(self) -> list[dict[str, Any]]:
        out = []
        with self.rx() as conn:
            rows = db.all_rows(conn, "SELECT p.name, p.description, p.archived, p.paused, "
                                     "p.icon_text, p.icon_type, l.rev, l.doc, s.doc AS state "
                                     "FROM projects p JOIN plans l ON l.project = p.name "
                                     "JOIN states s ON s.project = p.name ORDER BY p.name")
        for row in rows:
            doc, state = json.loads(row["doc"]), json.loads(row["state"])
            counts = Counter(S.entry_of(state, s)["status"] for s in doc["steps"])
            entry = {"name": row["name"], "description": row["description"],
                     "rev": row["rev"], "counts": dict(counts),
                     "archived": bool(row["archived"]), "paused": bool(row["paused"])}
            if (icon := _icon(row)) is not None:
                entry["icon"] = icon
            out.append(entry)
        return out

    # ---- the plan ----

    def get(self, project: str) -> dict[str, Any]:
        """The project's current plan, including `rev`."""
        with self.rx() as conn:
            row = db.one(conn, "SELECT rev, doc FROM plans WHERE project = ?", (project,))
        if row is None:
            raise NotFound(f"no project {project!r}")
        return {**json.loads(row["doc"]), "rev": row["rev"]}

    def plan(self, project: str) -> tuple[dict[str, Any], P.Plan]:
        """The current document and its parsed plan (cached per rev and fn set)."""
        doc = self.get(project)
        reg = self.registry(project)
        key = (doc["rev"], reg.key)
        hit = self._parsed.get(project)
        if hit is None or hit[0] != key:
            errs, plan = P.validate(_body(doc), reg)
            if errs:
                raise InvalidPlan(errs, f"the plan of project {project} no longer validates")
            hit = self._parsed[project] = (key, plan)
        return doc, hit[1]

    def patch(self, project: str, rev: int, ops: Any, author: str, reason: str,
              start: bool = True) -> int:
        """Apply an RFC 6902 patch at `rev`. Unless `start`, a step it adds comes in paused
        (unless the step itself says `paused`); that pause is one more op in the history.
        Raises Conflict, InvalidPlan or NotFound."""
        with self.tx() as conn:
            cur = self.get(project)
            if rev != cur["rev"]:
                raise Conflict(cur["rev"])
            reg = self.usable_registry(project)
            old = _body(cur)
            new = apply_ops(old, ops)
            if not start and isinstance(new.get("steps"), dict):
                held = [{"op": "add", "path": f"/steps/{sid}/paused", "value": True}
                        for sid, s in new["steps"].items()
                        if sid not in old["steps"] and isinstance(s, dict) and "paused" not in s]
                if held:
                    ops = [*ops, *held]
                    new = apply_ops(new, held)
            errs, _ = P.validate(new, reg)
            new_steps = new.get("steps") if isinstance(new.get("steps"), dict) else {}
            for sid, e in self.read_state(project)["steps"].items():
                if e["status"] != "running":
                    continue
                if sid not in new_steps:
                    errs.append(f"steps.{sid}: cannot remove a running step")
                elif _unpaused(new_steps[sid]) != _unpaused(old["steps"].get(sid)):
                    errs.append(f"steps.{sid}: cannot change a running step (only pause it)")
            if errs:
                raise InvalidPlan(errs)
            conn.execute("UPDATE plans SET rev = ?, doc = ? WHERE project = ?",
                         (rev + 1, _dumps(new), project))
            self._log(conn, project, rev + 1, author, reason, ops)
            self.notify()
        return rev + 1

    # ---- one step of the plan: plan_patch for a single step, at the current rev ----

    def add_step(self, project: str, sid: str, step: Any, author: str, reason: str,
                 start: bool = False) -> int:
        if not isinstance(sid, str) or not P.ID_RE.match(sid):
            raise BadRequest(f"step ids match {P.ID_RE.pattern}, got {sid!r}")
        with self.tx():
            cur = self.get(project)
            if sid in cur["steps"]:
                raise BadRequest(f"step {sid!r} already exists (step_update changes it)")
            return self.patch(project, cur["rev"],
                              [{"op": "add", "path": f"/steps/{sid}", "value": step}],
                              author, reason or f"add step {sid}", start)

    def update_step(self, project: str, sid: str, changes: Any, author: str,
                    reason: str) -> int:
        """Merge `changes` into a step: each key replaces that field of it, null removes it."""
        if not isinstance(changes, dict) or not changes:
            raise BadRequest("changes: expected an object of step field -> new value")
        with self.tx():
            cur = self.get(project)
            if sid not in cur["steps"]:
                raise NotFound(f"the plan of project {project} has no step {sid!r}")
            new = copy.deepcopy(cur["steps"][sid])
            for key, value in changes.items():
                if value is None:
                    new.pop(key, None)
                else:
                    new[key] = value
            return self.patch(project, cur["rev"],
                              [{"op": "replace", "path": f"/steps/{sid}", "value": new}],
                              author, reason or f"update step {sid}")

    def remove_steps(self, project: str, steps: Any = None, tags: Any = None,
                     author: str = "", reason: str = "") -> dict[str, Any]:
        """Remove the selected steps in one edit. Returns {rev, steps}."""
        with self.tx():
            chosen = self.select_steps(project, steps, tags)
            cur = self.get(project)
            rev = self.patch(project, cur["rev"],
                             [{"op": "remove", "path": f"/steps/{sid}"} for sid in chosen],
                             author, reason or f"remove {', '.join(chosen)}")
        return {"rev": rev, "steps": chosen}

    def select_steps(self, project: str, steps: Any = None, tags: Any = None,
                     subtree: bool = False) -> list[str]:
        """Step ids by id and/or tag, with everything downstream of them when `subtree` (the
        steps that read from or run after them, transitively), in plan order. A single id or
        tag counts as a list of one."""
        steps = [steps] if isinstance(steps, str) else steps
        tags = [tags] if isinstance(tags, str) else tags
        for name, v in (("steps", steps), ("tags", tags)):
            if v is not None and not (isinstance(v, list) and all(isinstance(x, str) for x in v)):
                raise BadRequest(f"{name}: expected an array of strings")
        if not steps and not tags:
            raise BadRequest("select steps by `steps` (ids) and/or `tags`")
        _, plan = self.plan(project)
        missing = [s for s in steps or [] if s not in plan.steps]
        if missing:
            raise NotFound(f"the plan of project {project} has no step {', '.join(missing)}")
        chosen = set(steps or []) | {s.id for s in plan.steps.values()
                                     if set(s.tags) & set(tags or [])}
        if subtree:
            below: dict[str, list[str]] = {}
            for s in plan.steps.values():
                for w in s.waits:
                    below.setdefault(w, []).append(s.id)
            todo = list(chosen)
            while todo:
                for d in below.get(todo.pop(), []):
                    if d not in chosen:
                        chosen.add(d)
                        todo.append(d)
        return [sid for sid in plan.steps if sid in chosen]

    def prune(self, project: str, older_than_hours: Any = 0, author: str = "",
              reason: str = "") -> dict[str, Any]:
        """plan_prune: remove every step of every done unit whose last step finished at least
        `older_than_hours` ago, in one edit (the history keeps them). A unit a plan output
        reads stays (removing it would break the plan). Returns {rev, units, steps}: the
        number of units and the ids removed; no edit when there is nothing to remove."""
        if isinstance(older_than_hours, bool) or not isinstance(older_than_hours, int | float) \
                or older_than_hours < 0:
            raise BadRequest("older_than_hours: expected a number of hours, 0 or more")
        cutoff = dt.datetime.now(dt.UTC) - dt.timedelta(hours=older_than_hours)
        with self.tx():
            doc, plan = self.plan(project)
            state = self.read_state(project)
            kept = {r.step for r in plan.outputs.values() if r.step}
            gone: list[list[str]] = []
            for unit in P.done_units(plan, state):
                ends = [_parse_time(S.entry_of(state, sid).get("finished")) for sid in unit]
                last = max((t for t in ends if t is not None), default=None)
                if kept.isdisjoint(unit) and last is not None and last <= cutoff:
                    gone.append(unit)
            ids = [sid for unit in gone for sid in unit]
            if not ids:
                return {"rev": doc["rev"], "units": 0, "steps": []}
            n = len(gone)
            rev = self.patch(project, doc["rev"],
                             [{"op": "remove", "path": f"/steps/{sid}"} for sid in ids], author,
                             reason or f"prune {n} done unit{'s' if n != 1 else ''}")
        return {"rev": rev, "units": n, "steps": ids}

    def pause_steps(self, project: str, steps: Any = None, tags: Any = None,
                    subtree: bool = False, paused: bool = True, author: str = "",
                    reason: str = "") -> dict[str, Any]:
        """Pause (or unpause) the selected steps in one edit: `paused` becomes the reason when
        one is given, else true; unpausing removes it. Returns {rev, steps}."""
        if not isinstance(paused, bool):
            raise BadRequest("paused: expected true or false")
        with self.tx():
            chosen = self.select_steps(project, steps, tags, subtree)
            cur = self.get(project)
            mark: Any = (reason.strip() or True) if paused else None
            ops = []
            for sid in chosen:
                if cur["steps"][sid].get("paused", False) not in (False, None) and paused and \
                        not reason.strip():
                    continue  # already paused: keep its reason
                if mark is None:
                    if "paused" in cur["steps"][sid]:
                        ops.append({"op": "remove", "path": f"/steps/{sid}/paused"})
                elif cur["steps"][sid].get("paused") != mark:
                    ops.append({"op": "add", "path": f"/steps/{sid}/paused", "value": mark})
            if not ops:
                return {"rev": cur["rev"], "steps": chosen}
            what = "pause" if paused else "unpause"
            rev = self.patch(project, cur["rev"], ops, author,
                             reason or f"{what} {', '.join(chosen)}")
        return {"rev": rev, "steps": chosen}

    def cancel_steps(self, project: str, steps: Any = None, tags: Any = None,
                     author: str = "", reason: str = "") -> list[str]:
        """Ask the runner to stop the selected running steps: it kills their processes and
        fails them with `cancelled` (and the reason). A pending core.external step (work done
        outside sluice) fails so at once. Refused unless every one is either."""
        with self.tx():
            chosen = self.select_steps(project, steps, tags)
            _, plan = self.plan(project)
            state = self.read_state(project)
            outside = {s for s in chosen if plan.steps[s].fn.external
                       and S.entry_of(state, s)["status"] == "pending"}
            idle = [f"{s} is {S.entry_of(state, s)['status']}"
                    for s in chosen
                    if S.entry_of(state, s)["status"] != "running" and s not in outside]
            if idle:
                raise BadRequest("only a running step (or a pending core.external one) can be "
                                 f"cancelled: {', '.join(idle)}")
            for sid in chosen:
                if sid in outside:
                    state["steps"][sid] = S.failed(f"cancelled: {reason}" if reason
                                                   else "cancelled")
                else:
                    state["steps"][sid].update(S.cancel(reason))
            self.write_state(project, state)
            self.append(project, *({"kind": "step.cancel", "step": sid, "author": author,
                                    "reason": reason} for sid in chosen),
                        *({"kind": "step.status", "step": sid, "from": "pending",
                           "to": "failed", "error": state["steps"][sid]["error"]}
                          for sid in chosen if sid in outside))
            self.notify()
        return chosen

    def _log(self, conn: Connection, project: str, rev: int, author: str, reason: str,
             ops: list | None = None, kind: str = "plan.edit", **fields: Any) -> None:
        """A history record: an edit (`plan.edit` with `ops`, also kept in plan_edits with its
        seq), or a manual value (its kind and arguments). In the caller's transaction."""
        rec = {"kind": kind, "rev": rev, "author": author, "reason": reason, **fields}
        if ops is not None:
            rec["ops"] = ops
        seq = L.append(conn, project, [rec], self.log_cap())[0]
        if kind == "plan.edit":
            conn.execute("INSERT INTO plan_edits (project, rev, seq, at, author, reason, ops) "
                         "SELECT project, ?, seq, at, ?, ?, ? FROM records WHERE seq = ?",
                         (rev, author, reason, _dumps(ops), seq))

    def notify(self) -> None:
        """Tell the listeners of an accepted change: once the transaction around it commits."""
        db.after_commit(self.home, lambda: [fn() for fn in list(self.listeners)])

    def history(self, project: str, since_rev: int | None = None) -> list[dict[str, Any]]:
        """plan_history: every edit of the plan (plan_edits) and the manual values still in the
        log, in seq order."""
        self.project(project)
        recs = L.read(self.home, project, kinds=L.HISTORY_KINDS, history=True)["records"]
        return [e for e in recs if since_rev is None or e["rev"] > since_rev]

    # ---- recipes (SPEC §5) ----

    def _recipes(self, project: str) -> dict[str, RC.Recipe]:
        """The recipes a project sees: SLUICE_HOME/recipes/, then its own recipes/ (its own
        win on a name clash)."""
        self.project(project)
        return RC.scan([("global", self.home / "recipes"),
                        ("project", self.project_dir(project) / "recipes")])

    def recipes(self, project: str) -> list[dict[str, Any]]:
        """recipe_list: each recipe the project sees, by name: {name, doc, params, scope}, or
        {name, scope, error} for a broken one."""
        return [r.summary() for _, r in sorted(self._recipes(project).items())]

    def unit_add(self, project: str, recipe: Any, params: Any, start: bool = False,
                 author: str = "", reason: str = "") -> dict[str, Any]:
        """Expand a recipe with `params` (`unit` among them) and add its steps in one edit at
        the current rev, each tagged `unit:<unit>` before its own tags; unless `start`, they
        come in paused. Refuses ids the plan already has. Returns {rev, steps}."""
        if not isinstance(recipe, str):
            raise BadRequest("recipe: expected a recipe's name")
        found = self._recipes(project).get(recipe)
        if found is None:
            raise NotFound(f"project {project} sees no recipe {recipe!r} (recipe_list lists "
                           "them)")
        steps, errs = RC.expand(found, params)
        errs += [f"steps.{sid}: ids match {P.ID_RE.pattern}" for sid in steps
                 if not P.ID_RE.match(sid)]
        if errs:
            raise InvalidPlan(errs, f"recipe {recipe} does not expand with these params")
        unit = params[RC.UNIT]
        for step in steps.values():
            if isinstance(step, dict) and isinstance(step.get("tags", []), list):
                step["tags"] = list(dict.fromkeys([f"unit:{unit}", *step.get("tags", [])]))
        with self.tx():
            cur = self.get(project)
            taken = [sid for sid in steps if sid in cur["steps"]]
            if taken:
                raise BadRequest(f"steps {', '.join(taken)} already exist in the plan of "
                                 f"project {project}")
            rev = self.patch(project, cur["rev"],
                             [{"op": "add", "path": f"/steps/{sid}", "value": step}
                              for sid, step in steps.items()],
                             author, reason or f"add unit {unit} (recipe {recipe})", start)
        return {"rev": rev, "steps": list(steps)}

    # ---- state ----

    def read_state(self, project: str) -> dict[str, Any]:
        with self.rx() as conn:
            row = db.one(conn, "SELECT doc FROM states WHERE project = ?", (project,))
        if row is None:
            raise NotFound(f"no project {project!r}")
        return json.loads(row["doc"])

    def write_state(self, project: str, state: dict[str, Any]) -> None:
        """Replace the project's state (in the caller's transaction, if any)."""
        with self.tx() as conn:
            text = _dumps(state)
            cur = conn.execute("UPDATE states SET doc = ? WHERE project = ? AND doc != ?",
                               (text, project, text))
            if cur.rowcount == 0:
                self._row(conn, project)

    def status(self, project: str, steps: Any = None, tags: Any = None,
               brief: bool = False, all: bool = False) -> dict[str, Any]:
        """The plan's inputs, outputs and steps (with `steps` and/or `tags`, only those; else,
        unless `all`, without the done units, counted in `done_units`); with `brief`, their
        long strings cut (`_brief`). One snapshot of plan, state and project."""
        with self.rx():
            only = set(self.select_steps(project, steps, tags)) if steps or tags else None
            doc, plan = self.plan(project)
            state = self.read_state(project)
            project_paused = self.paused(project)
        done = P.done_units(plan, state) if only is None and not all else []
        if done:
            only = set(plan.steps) - {sid for u in done for sid in u}
        outputs = {}
        for name, ref in plan.outputs.items():
            ok, v = P.value_of(ref, plan, state)
            outputs[name] = v if ok else None
        rows = []
        for sid, step in plan.steps.items():
            if only is not None and sid not in only:
                continue
            e = S.entry_of(state, sid)
            row = {"id": sid, "run": step.fn.name, "status": e["status"],
                   "started": e.get("started"), "finished": e.get("finished")}
            row.update({k: e[k] for k in ("outputs", "error") if e.get(k) is not None})
            row.update({"doc": step.doc} if step.doc else {})
            row.update({"paused": step.pause_reason or True} if step.paused else {})
            row.update({"tags": step.tags} if step.tags else {})
            row.update({"after": step.after} if step.after else {})
            row.update({"when": str(step.when)} if step.when else {})
            row.update({"skipped": e.get("skipped")} if e["status"] == "skipped" else {})
            if e["status"] == "pending":  # why it has not started
                held = ([f"paused: {step.pause_reason}" if step.pause_reason else "paused"]
                        if step.paused else [])
                held += ["the project is paused"] if project_paused else []
                row["waiting"] = held + P.not_ready(step, plan, state)
                if step.fn.external and not row["waiting"]:
                    row["waiting"] = [EXTERNAL_WAIT]
            rows.append({**row, "manual": bool(e.get("manual"))})
        out = {"rev": doc["rev"], "paused": project_paused,
               "inputs": {n: state["inputs"].get(n) for n in plan.inputs}}
        if plan.input_docs:
            out["input_docs"] = dict(plan.input_docs)
        if brief:
            out["inputs"], outputs = _brief(out["inputs"]), _brief(outputs)
            for row in rows:
                if "outputs" in row:
                    row["outputs"] = _brief(row["outputs"])
        out = {**out, "outputs": outputs, "steps": rows}
        if done:
            out["done_units"] = {"units": len(done), "steps": sum(map(len, done))}
        return out

    # ---- manual values (SPEC §6) ----

    def _plan_for_write(self, project: str) -> tuple[dict[str, Any], P.Plan]:
        self.usable_registry(project)
        return self.plan(project)

    def set_input(self, project: str, name: str, value: Any, author: str, reason: str) -> None:
        with self.tx() as conn:
            doc, plan = self._plan_for_write(project)
            if name not in plan.inputs:
                raise NotFound(f"the plan of project {project} has no input {name!r}")
            errs = T.check_value(plan.inputs[name], value, f"inputs.{name}")
            if errs:
                raise InvalidPlan(errs)
            state = self.read_state(project)
            state["inputs"][name] = value
            self.write_state(project, state)
            self._log(conn, project, doc["rev"], author, reason, kind="plan.input", name=name,
                      value=value)
            self.notify()

    def set_step_input(self, project: str, step: str, name: str, value: Any, author: str,
                       reason: str, rev: int | None = None) -> int:
        with self.tx():
            cur = self.get(project)
            if step not in cur["steps"]:
                raise NotFound(f"the plan of project {project} has no step {step!r}")
            op = {"op": "add", "path": f"/steps/{step}/in/{name}", "value": {"default": value}}
            ops = [{"op": "add", "path": f"/steps/{step}/in", "value": {}}, op] \
                if "in" not in cur["steps"][step] else [op]
            return self.patch(project, cur["rev"] if rev is None else rev, ops, author, reason)

    def _status_change(self, project: str, step: str, before: str | None, after: str) -> None:
        if before != after:
            self.append(project, {"kind": "step.status", "step": step, "from": before,
                                  "to": after})

    def set_output(self, project: str, step: str, outputs: Any, author: str, reason: str,
                   force: bool = False) -> None:
        """step_set_output: the step succeeds with these outputs (manual). Refused while what
        it reads is not ready, unless `force` (then its inputs are unknown: it turns stale once
        they are all there)."""
        with self.tx() as conn:
            doc, plan = self._plan_for_write(project)
            if step not in plan.steps:
                raise NotFound(f"the plan of project {project} has no step {step!r}")
            s = plan.steps[step]
            types = {k: s.output_type(k) for k in s.outputs}
            errs = T.check_value(T.record_of(types), outputs, "outputs")
            if isinstance(outputs, dict):
                errs += [f"outputs.{k}: step {step} (fn {s.fn.name}) has no output {k}"
                         for k in outputs if k not in types]
            if errs:
                raise InvalidPlan(errs)
            state = self.read_state(project)
            before = state["steps"].get(step, {}).get("status")
            if before == "running":
                raise BadRequest(f"step {step} is running")
            waiting = P.not_ready(s, plan, state)
            if waiting and not force:
                raise InvalidPlan(waiting, f"step {step} reads values that are not ready; "
                                  "pass force: true to set its outputs anyway (it turns stale "
                                  "once they are)")
            h = None if waiting else P.inputs_hash(s, plan, state)
            state["steps"][step] = S.manual(outputs, h)
            self.write_state(project, state)
            extra = {"force": True} if force else {}
            self._log(conn, project, doc["rev"], author, reason, kind="step.output", step=step,
                      outputs=outputs, **extra)
            self._status_change(project, step, before, "succeeded")
            self.notify()

    def retry(self, project: str, steps: Any = None, tags: Any = None, author: str = "",
              reason: str = "") -> list[str]:
        """step_retry: the selected steps, each failed, stale or manually set, go back to
        pending (refused, changing nothing, unless every one of them is). A failed
        scattered step keeps its finished items under `kept` so the retry re-runs only
        what failed (SPEC §6)."""
        with self.tx() as conn:
            doc, plan = self._plan_for_write(project)
            chosen = self.select_steps(project, steps, tags)
            state = self.read_state(project)
            was = {s: S.entry_of(state, s) for s in chosen}
            bad = [f"step {s} is {e['status']}" for s, e in was.items()
                   if e["status"] not in ("failed", "stale") and not e.get("manual")]
            if bad:
                raise BadRequest(f"{'; '.join(bad)}; only a failed, stale or manually set step "
                                 "can be retried")
            for sid in chosen:
                e = was[sid]
                if e["status"] == "failed" and plan.steps[sid].scatter \
                        and isinstance(e.get("results"), list):
                    kept = {k: e.get(k) for k in ("inputs_hash", "run_ids", "results")}
                    state["steps"][sid] = S.pending_kept(kept)
                else:
                    state["steps"][sid] = S.pending()
            self.write_state(project, state)
            for sid, e in was.items():
                self._log(conn, project, doc["rev"], author, reason, kind="step.retry", step=sid)
                self._status_change(project, sid, e["status"], "pending")
            self.notify()
        return chosen

    def submit(self, project: str, step: str, outputs: Any,
               run: str | None = None) -> dict[str, Any]:
        """step_submit: the agent of a running step hands over the outputs the step declares
        (SPEC §5). Checked against them: every required one, fitting types, no others. Kept as
        the run's submission (a resubmit replaces it) with its `step.submit` record, in one
        transaction; the runner merges them into the step's outputs when the fn exits."""
        with self.tx() as conn:
            _, plan = self.plan(project)
            s = plan.steps.get(step)
            if s is None:
                raise NotFound(f"the plan of project {project} has no step {step!r}")
            if not s.declared:
                raise BadRequest(f"step {step} declares no outputs to submit")
            e = S.entry_of(self.read_state(project), step)
            if e["status"] != "running":
                raise BadRequest(f"step {step} is {e['status']}; outputs are submitted while "
                                 "it runs")
            runs = e.get("run_ids") or []
            if run is None:
                if len(runs) != 1:
                    raise BadRequest(f"step {step} has {len(runs)} runs; pass run, the run id "
                                     "(SLUICE_RUN_ID) of yours")
                run = runs[0]
            elif run not in runs:
                raise NotFound(f"step {step} has no run {run!r}")
            errs = T.check_value(T.record_of(s.declared), outputs, "outputs")
            if isinstance(outputs, dict):
                errs += [f"outputs.{k}: step {step} declares no output {k}"
                         + (" (the fn returns that one itself)" if k in s.fn.outputs else "")
                         for k in outputs if k not in s.declared]
            if errs:
                raise InvalidPlan(errs, f"outputs do not match what step {step} declares")
            conn.execute("INSERT OR REPLACE INTO submissions (project, run, step, outputs, at) "
                         "VALUES (?, ?, ?, ?, ?)", (project, run, step, _dumps(outputs),
                                                    now_iso()))
            self.append(project, {"kind": "step.submit", "step": step, "run": run,
                                  "outputs": outputs})
        return {"ok": True, "run": run}

    def submission(self, project: str, run: str) -> dict[str, Any] | None:
        """What the agent of a run has submitted (step_submit), or None."""
        return db.submission(self.home, project, run)

    # ---- the inbox (SPEC §8) ----

    def inbox(self, project: str | None = None, status: str = "open") -> list[dict[str, Any]]:
        """inbox_list: the items with this status ("all" for every one) of the project, or of
        every project, oldest first; each carries its `project`."""
        if status not in (*I.STATUSES, "all"):
            raise BadRequest(f"status: expected one of {', '.join(I.STATUSES)} or all, "
                             f"got {status!r}")
        with self.rx() as conn:
            if project is not None:
                self._row(conn, project)
            return I.items(conn, project, None if status == "all" else status)

    def inbox_post(self, project: str, title: str, body: str | None = None,
                   ui: str | None = None, input: str | None = None,
                   sender: str | None = None) -> dict[str, Any]:
        """Post an open item. With `input`, answering it sets that plan input, so the plan
        must declare it; without a body, the item's body is that input's doc."""
        if not isinstance(title, str) or not title.strip():
            raise BadRequest("title: expected a non-empty string")
        with self.tx() as conn:
            self._row(conn, project)
            if input is not None:
                plan = self.plan(project)[1]
                if input not in plan.inputs:
                    raise NotFound(f"the plan of project {project} has no input {input!r}")
                body = plan.input_docs.get(input) if body is None else body
            item = I.post(conn, project, self.log_cap(), title, body, ui, input, sender)
            self.notify()
        return item

    def _open_item(self, conn: Connection, project: str, item_id: str) -> dict[str, Any]:
        """The item, refusing an unknown one (NotFound) or one that is not open (NotOpen)."""
        self._row(conn, project)
        item = I.find(conn, project, item_id)
        if item is None:
            raise NotFound(f"project {project} has no inbox item {item_id!r}")
        if item["status"] != "open":
            raise NotOpen(item_id, item["status"])
        return item

    def inbox_answer(self, project: str, item_id: str, answer: Any,
                     author: str) -> dict[str, Any]:
        """Answer an open item. When it names a plan input, the answer's value (answer_value)
        goes through set_input in the same transaction; a value that does not fit refuses the
        answer and the item stays open. The one write path for MCP and the dashboard."""
        errs = check_answer(answer)
        if errs:
            raise InvalidPlan(errs, "not a valid answer")
        with self.tx() as conn:
            item = self._open_item(conn, project, item_id)
            if item.get("input"):
                name = item["input"]
                value = answer_value(answer)
                if value is None:
                    need = "give values.value, params.value or text"
                    raise InvalidPlan([f"answer: inbox item {item_id} sets plan input {name}; "
                                       + need])
                try:
                    self.set_input(project, name, value, author,
                                   f"inbox item {item_id}: {item['title']}")
                except InvalidPlan as e:
                    raise InvalidPlan(e.errors, f"inbox item {item_id}: the answer does not "
                                      f"fit plan input {name}") from e
            item = I.finish(conn, project, self.log_cap(), item_id,
                            {"status": "answered", "answer": answer, "answered": now_iso()},
                            {"kind": "inbox.answer", "answer": answer, "by": author})
            self.notify()
        return item

    def inbox_close(self, project: str, item_id: str, reason: str | None,
                    author: str) -> dict[str, Any]:
        """Withdraw an open item (the poster no longer needs it)."""
        with self.tx() as conn:
            self._open_item(conn, project, item_id)
            extra = {"reason": reason} if reason else {}
            item = I.finish(conn, project, self.log_cap(), item_id,
                            {"status": "closed", "closed": now_iso(), **extra},
                            {"kind": "inbox.close", **extra, "by": author})
            self.notify()
        return item


def _icon(row: Any) -> dict[str, Any] | None:
    if row["icon_type"] is not None:
        return {"kind": "image", "type": row["icon_type"]}
    if row["icon_text"] is not None:
        return {"kind": "text", "text": row["icon_text"]}
    return None


def check_answer(answer: Any) -> list[str]:
    """An answer is {action: string, params?: object, values?: object, text?: string}."""
    if not isinstance(answer, dict):
        return ["answer: expected an object {action, params?, values?, text?}"]
    errs = [f"answer.{k}: unknown key (answers have action, params, values, text)"
            for k in answer if k not in ANSWER_KEYS]
    if "action" not in answer:
        errs.append("answer.action: missing required field")
    errs += [f"answer.{k}: expected {'a string' if t is str else 'an object'}"
             for k, t in ANSWER_KEYS.items() if k in answer and not isinstance(answer[k], t)]
    return errs


def answer_value(answer: dict[str, Any]) -> Any:
    """The value an answer gives a plan input: the first of `values.value` (a form field named
    value), `params.value` (a button's value) and `text` that is there; None when none is."""
    for where in (answer.get("values") or {}, answer.get("params") or {}):
        if "value" in where:
            return where["value"]
    return answer.get("text")


def _parse_time(text: Any) -> dt.datetime | None:
    """An ISO UTC time as the state writes it (2026-09-26T14:02:11Z); None when it is not."""
    try:
        return dt.datetime.strptime(text, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=dt.UTC)
    except (TypeError, ValueError):
        return None


def _body(doc: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in doc.items() if k != "rev"}


def _unpaused(step: Any) -> Any:
    """A step without its `paused` flag: the one change a running step takes."""
    return {k: v for k, v in step.items() if k != "paused"} if isinstance(step, dict) else step


def apply_ops(doc: dict[str, Any], ops: Any) -> dict[str, Any]:
    """Apply a JSON Patch, turning every failure into InvalidPlan with the op index."""
    if not isinstance(ops, list) or not all(isinstance(o, dict) for o in ops):
        raise InvalidPlan(["ops: expected a list of JSON Patch operations"])
    cur: Any = copy.deepcopy(doc)
    for i, op in enumerate(ops):
        try:
            cur = jsonpatch.JsonPatch([op]).apply(cur)
        except (jsonpatch.JsonPatchException, jsonpointer.JsonPointerException, TypeError,
                KeyError) as e:
            raise InvalidPlan([f"ops[{i}]: {e}"]) from e
    if not isinstance(cur, dict):
        raise InvalidPlan(["plan: expected an object"])
    return cur
