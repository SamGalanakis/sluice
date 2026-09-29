"""The dashboard's HTTP routes (SPEC §8 Views): pages, and one Datastar SSE stream per page.

A page renders completely on first load (usable without JavaScript) and carries the version of
what it shows (`ver`, from the projects' change counters in the database — `projects.ver`,
which every write to a project's rows moves — and the stats of the stderr.log of every running
step's current run, so a progress line moves while an agent works). Its stream polls that
version every `interval` seconds off the event loop; when they change it re-renders the
page's parts and sends a `datastar-patch-elements` event for each part that differs from what
the client has, then the new `ver` (so a reconnecting client resumes from there). An idle page
gets nothing. A step's detail (the project page's drawer, or its own page) streams the same way
under the `sver` signal, versioned by the project and that step's runs.
The log page's stream sends the table when the filter signals changed, and on the newest page
prepends new matching records. The inbox page streams its items the same way, and its answer
route is one of the dashboard's writes: it calls the same Store.inbox_answer as the MCP
tool. The others archive or pause a project (Store.update_project, like project_update) or
pause a step. The settings menu's route writes nothing on the server: it sets the browser's
cookies (theme, value types), which every page reads to render them.
"""

from __future__ import annotations

import functools
import hashlib
import ipaddress
import threading
from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

import anyio
from datastar_py import ServerSentEventGenerator as SSE
from datastar_py.consts import ElementPatchMode
from datastar_py.starlette import DatastarResponse, read_signals
from starlette.requests import Request
from starlette.responses import FileResponse, HTMLResponse, JSONResponse, RedirectResponse, Response

from . import db, views
from . import inbox as I
from . import log as L
from .errors import BadRequest, NotFound, SluiceError
from .store import Store

STATIC = Path(__file__).resolve().parent / "static"
# the dashboard's own scripts, then the vendored ones (from jsDelivr, the versions in their
# names; lang-core's imports rewritten to these files), so no third-party script runs here
STATIC_TYPES = {"dashboard.css": "text/css", "inbox.js": "text/javascript", "openui.json": "application/json",
                "sluice.js": "text/javascript", "nav.js": "text/javascript",
                "logo.svg": "image/svg+xml", "favicon.svg": "image/svg+xml",
                **dict.fromkeys(["datastar-rocket-1.0.4.js", "lang-core-0.3.0.js",
                                 "zod-4.6.5-v4.js", "zod-4.6.5-v4-core.js", "ci-info-4.4.0.js"],
                                "text/javascript")}
AUTHOR = "dashboard"
HTTP_STATUS = {"not_found": 404, "conflict": 409, "busy": 503}
# the Host names this machine answers to on a loopback socket (the SDK's list for /mcp)
LOCAL_HOSTS = ("127.0.0.1", "localhost", "::1")
# the settings menu's cookies: the theme (an id of views.THEMES; none until one is picked,
# when the OS's shows) and "1" to show value types; kept for 400 days, a browser's longest
THEME_COOKIE, TYPES_COOKIE = "sluice_theme", "sluice_types"
COOKIE_AGE = 400 * 24 * 3600


def _image(request: Request, kind: str, data: bytes, digest: str) -> Response:
    """An icon's image: its content type, nosniff and its sha256 as the ETag (a matching
    If-None-Match gets 304); for SVG a CSP keeps any script inside from running even when
    the URL is opened directly."""
    headers = {"X-Content-Type-Options": "nosniff", "ETag": f'"{digest}"'}
    if kind == "image/svg+xml":
        headers["Content-Security-Policy"] = \
            "default-src 'none'; style-src 'unsafe-inline'; img-src data:"
    if request.headers.get("if-none-match") == headers["ETag"]:
        return Response(status_code=304, headers=headers)
    return Response(data, media_type=kind, headers=headers)


def _local_host(request: Request) -> bool:
    """The dashboard's DNS-rebinding guard (/mcp has the SDK's own): a request that arrived
    on a loopback socket must be addressed to this machine. A deliberate non-loopback bind
    lifts the check."""
    server = request.scope.get("server")
    if not server:
        loopback = True  # no socket to ask: treat as local
    else:
        try:
            loopback = ipaddress.ip_address(server[0]).is_loopback
        except ValueError:
            loopback = server[0] == "localhost"
    if not loopback:
        return True
    try:
        host = urlsplit(f"//{request.headers.get('host', '')}").hostname
    except ValueError:
        return False
    return host in LOCAL_HOSTS


def _viewer(request: Request) -> views.Viewer:
    """The browser's settings, from its cookies (anything else is ignored), and the page's
    address."""
    theme = request.cookies.get(THEME_COOKIE)
    query = request.url.query
    return views.Viewer(theme=theme if theme in views.THEMES else None,
                        types=request.cookies.get(TYPES_COOKIE) == "1",
                        path=request.url.path + (f"?{query}" if query else ""))


def _local(handler: Callable) -> Callable:
    """403 a request that reached a loopback socket under a foreign Host; otherwise note the
    viewer's settings for the pages it renders."""
    async def route(request: Request) -> Response:
        if not _local_host(request):
            return Response("requests under a foreign Host are refused", status_code=403)
        views.VIEWER.set(_viewer(request))
        return await handler(request)
    return route


def _next(nxt: str, fallback: str) -> str:
    """A form's `next`, used only when it is a local path: no scheme, no netloc, no
    backslash; otherwise `fallback`."""
    where = urlsplit(nxt)
    return nxt if nxt and not where.scheme and not where.netloc and "\\" not in nxt \
        else fallback


def _foreign(request: Request) -> bool:
    """A write sent from another site's page (DNS rebinding satisfies Origin == Host, which the
    Host check covers)."""
    origin = request.headers.get("origin")
    return bool(origin) and urlsplit(origin).netloc != request.headers.get("host")


def _stat(path: Path) -> tuple[int, int] | None:
    try:
        s = path.stat()
    except FileNotFoundError:
        return None
    return s.st_mtime_ns, s.st_size


def _digest(value: Any) -> str:
    return hashlib.sha256(repr(value).encode()).hexdigest()[:16]


# every running step's run ids, by project (the stderr.log a page's version stats)
RUNNING = ("SELECT s.project, j.value FROM states s, json_each(s.doc, '$.steps') e, "
           "json_each(e.value, '$.run_ids') j WHERE e.value ->> '$.status' = 'running'")


def _running_stderr(store: Store, rows: list[Any]) -> list[tuple[int, int] | None]:
    """The stats of the stderr.log of each (project, run id) row."""
    return [_stat(store.runs_dir(p) / r / "stderr.log") for p, r in rows
            if isinstance(r, str) and L.RUN_ID_RE.match(r)]


def index_ver(store: Store) -> str:
    """The version of the index and inbox pages: the runner's liveness and every project's
    name and change counter."""
    with store.rx() as conn:
        vers = [tuple(r) for r in db.all_rows(conn, "SELECT name, ver FROM projects "
                                                    "ORDER BY name")]
    return _digest([views.runner_state(store.home), vers])


def home_ver(store: Store) -> str:
    """The version of the index: `index_ver` and the running steps' stderr, so a row's
    `quiet 40m` goes when its run writes again."""
    with store.rx() as conn:
        runs = db.all_rows(conn, RUNNING + " ORDER BY s.project")
    return _digest([index_ver(store), _running_stderr(store, runs)])


def project_ver(store: Store, project: str) -> str:
    """The version of what the project page shows: the project's change counter, the open
    inbox items of every project (the nav's badge) and the running steps' stderr (their
    progress lines)."""
    with store.rx() as conn:
        row = db.one(conn, "SELECT ver FROM projects WHERE name = ?", (project,))
        badge = I.open_count(conn)
        runs = db.all_rows(conn, RUNNING + " AND s.project = ?", (project,))
    return _digest([views.runner_state(store.home), row and row[0], badge,
                    _running_stderr(store, runs)])


def project_stamp(store: Store, project: str) -> str:
    """What the board shows, excluding log records that only move the project counter."""
    with store.rx() as conn:
        row = db.one(conn, "SELECT p.description, p.archived, p.paused, p.icon_text, "
                     "p.icon_hash, p.changed, l.doc, l.rev, s.doc FROM projects p "
                     "JOIN plans l ON l.project = p.name JOIN states s ON s.project = p.name "
                     "WHERE p.name = ?", (project,))
        inbox = [tuple(r) for r in db.all_rows(conn,
                 "SELECT n, sender, run FROM inbox WHERE project = ? AND status = 'open'",
                 (project,))]
        badge = I.open_count(conn)
        runs = db.all_rows(conn, RUNNING + " AND s.project = ?", (project,))
    return _digest([tuple(row) if row else None, inbox, badge, views.runner_state(store.home),
                    _running_stderr(store, runs)])


def step_ver(store: Store, project: str, sid: str) -> str:
    """The version of a step's detail: the step, the project's version, and the stderr of the
    step's runs (the step is part of it: the drawer's `sver` moves on when it shows another)."""
    runs = views.step_run_dirs(store, project, sid)
    return _digest([sid, project_ver(store, project), [_stat(r / "stderr.log") for r in runs]])


def log_ver(store: Store, project: str | None) -> str:
    """The version of a log page: its log's last seq and its size (a trim changes that)."""
    with store.rx() as conn:
        row = db.one(conn, "SELECT max(seq), count(*) FROM records WHERE project IS ?",
                     (project,))
    return _digest(tuple(row))


def _patch(html: str) -> str:
    return SSE.patch_elements(html)


async def _signals(request: Request) -> dict[str, Any]:
    try:
        signals = await read_signals(request)
    except ValueError:  # not JSON
        return {}
    return signals if isinstance(signals, dict) else {}


class Dashboard:
    """The routes; `stop` ends every open stream (set when the server shuts down)."""

    def __init__(self, store: Store, stop: threading.Event | None = None,
                 interval: float = 1.0):
        self.store = store
        self.stop = stop or threading.Event()
        self.interval = interval

    async def _tick(self) -> bool:
        """Wait one poll interval; False once the server is stopping."""
        await anyio.sleep(self.interval)
        return not self.stop.is_set()

    async def _parts_stream(self, client_ver: Any, ver: Callable[[], str],
                            parts: Callable[[], dict[str, str]],
                            signal: str = "ver") -> AsyncIterator[str]:
        """Patch the parts that changed whenever `ver()` moves on (all of them at once when
        the client's version is not the current one), then set the `signal` to it."""
        run = anyio.to_thread.run_sync
        try:
            cur, last = None, {}
            while not self.stop.is_set():
                observed = await run(self._parts_snapshot, ver, parts, cur)
                if observed is not None:
                    new_ver, new = observed
                    if new is not None:
                        if cur is not None or client_ver != new_ver:
                            for pid, html in new.items():
                                if cur is None or last.get(pid) != html:
                                    yield _patch(html)
                            yield SSE.patch_signals({signal: new_ver})
                        cur, last = new_ver, new
                if not await self._tick():
                    break
        except (SluiceError, OSError, ValueError):  # e.g. the project is gone: end the stream
            return

    def _parts_snapshot(self, ver: Callable[[], str],
                        parts: Callable[[], dict[str, str]],
                        known: str | None) -> tuple[str, dict[str, str] | None] | None:
        with self.store.rx():
            before = ver()
            if before == known:
                return before, None
            rendered = parts()
            # SQLite stays in one snapshot; files can move independently. A changed
            # observation gets another attempt at the next poll, without holding a reader.
            if ver() != before:
                return None
            return before, rendered

    async def _log_stream(self, project: str | None,
                          signals: dict[str, Any]) -> AsyncIterator[str]:
        run = anyio.to_thread.run_sync
        try:
            q = views.LogQuery.from_signals(signals)
        except BadRequest as err:
            yield SSE.patch_elements(f'<div id="log-view" class="log-view"><p class="bad">'
                                     f"{views.e(err.message)}</p></div>")
            while await self._tick():
                pass
            return
        seen = signals.get("seen")
        if signals.get("view") != q.query() or not isinstance(seen, int):
            html, seen = await run(views.log_view, self.store, project, q)
            yield SSE.patch_elements(html)
            yield SSE.patch_signals({"view": q.query(), "seen": seen})
        if not q.newest:  # an older page stays as it is
            while await self._tick():
                pass
            return
        stamp, history = None, q.history(project)
        while True:
            new_stamp = await run(log_ver, self.store, project)
            if new_stamp != stamp:
                stamp = new_stamp
                res = await run(functools.partial(L.read, self.store.home, project, seen,
                                                  q.kinds, q.threads, history=history))
                recs = [r for r in res["records"] if views.log_shown(r, q)]
                if recs:
                    rows = views.log_rows(reversed(recs))
                    yield SSE.patch_elements(rows, selector="#log-rows",
                                             mode=ElementPatchMode.PREPEND)
                if res["last_seq"] != seen:
                    seen = res["last_seq"]
                    yield SSE.patch_signals({"seen": seen})
            if not await self._tick():
                return

    async def _stream(self, request: Request, ver: Callable[[], str],
                      parts: Callable[[], dict[str, str]], signal: str = "ver",
                      exists: Callable[[], Any] | None = None) -> Response:
        """One page's stream route: `exists` checked off the event loop (-> 404), the
        client's signals, then the SSE response patching `parts` as `ver` moves."""
        if exists is not None:
            try:
                await anyio.to_thread.run_sync(exists)
            except SluiceError as err:
                return Response(err.message, status_code=404)
        signals = await _signals(request)
        return DatastarResponse(self._parts_stream(signals.get(signal), ver, parts,
                                                   signal=signal))

    def _has_step(self, name: str, sid: str) -> None:
        """The step stream's existence probe: the step in the plan (not a full render)."""
        if sid not in self.store.plan(name)[1].steps:
            raise NotFound(f"the plan of project {name} has no step {sid!r}")

    # ---- routes ----

    async def _page(self, render: Callable[..., str], *args: Any) -> Response:
        try:
            return HTMLResponse(await anyio.to_thread.run_sync(render, *args))
        except NotFound as err:
            return HTMLResponse(views.not_found(err.message), status_code=404)
        except SluiceError as err:
            return HTMLResponse(views.layout("bad request", f"<p>{views.e(err.message)}</p>"),
                                status_code=400)

    def _index(self) -> str:
        return views.index(self.store, home_ver(self.store))

    def _project(self, name: str, view: views.BoardView) -> str:
        self.store.project(name)
        return views.project_page(self.store, name, project_ver(self.store, name), view)

    async def index(self, request: Request) -> Response:
        return await self._page(self._index)

    async def index_stream(self, request: Request) -> Response:
        return await self._stream(request, lambda: home_ver(self.store),
                                  lambda: views.index_parts(self.store))

    async def project(self, request: Request) -> Response:
        """The project page, its board ordered and filtered by the query (`order`, `show`,
        `tag`); a query that is not the view's canonical one (the toolbar's form sends the
        defaults too) goes to the canonical address, so the defaults leave it clean."""
        params: dict[str, list[str]] = {}
        for k, v in request.query_params.multi_items():
            params.setdefault(k, []).append(v)
        try:
            view = views.BoardView.parse(params)
        except BadRequest as err:
            return HTMLResponse(views.layout("bad request", f"<p>{views.e(err.message)}</p>"),
                                status_code=400)
        if request.url.query != view.query():
            q = view.query()
            return RedirectResponse(request.url.path + (f"?{q}" if q else ""), status_code=303)
        return await self._page(self._project, request.path_params["name"], view)

    async def project_stream(self, request: Request) -> Response:
        """The project page's stream: its parts in the order and filters of the page's
        `board` signal."""
        name = request.path_params["name"]
        view = views.BoardView.from_signals(await _signals(request))
        return await self._stream(request, lambda: project_ver(self.store, name),
                                  self._project_parts(name, view),
                                  exists=lambda: self.store.project(name))

    def _project_parts(self, name: str, view: views.BoardView) -> Callable[[], dict[str, str]]:
        stamp, last = None, {}

        def parts() -> dict[str, str]:
            nonlocal stamp, last
            key = project_stamp(self.store, name)
            if key != stamp:
                last = views.project_parts(self.store, name, view)
                stamp = key
            return last

        return parts

    async def box(self, request: Request) -> Response:
        def render() -> str:
            params: dict[str, list[str]] = {}
            for k, v in request.query_params.multi_items():
                params.setdefault(k, []).append(v)
            name, sid = request.path_params["name"], request.path_params["sid"]
            body = views.box_content(self.store, name, sid, views.BoardView.parse(params))
            if request.headers.get("sec-fetch-dest") == "empty":
                return body
            return views.layout(name, f'<sluice-board>{body}</sluice-board>', board=True,
                                store=self.store, project=name, tab="plan")
        return await self._page(render)

    def _threads(self, name: str) -> str:
        self.store.project(name)
        return views.threads_page(self.store, name, project_ver(self.store, name))

    async def threads(self, request: Request) -> Response:
        return await self._page(self._threads, request.path_params["name"])

    async def threads_stream(self, request: Request) -> Response:
        name = request.path_params["name"]
        return await self._stream(request, lambda: project_ver(self.store, name),
                                  lambda: views.threads_parts(self.store, name),
                                  exists=lambda: self.store.project(name))

    def _step(self, name: str, sid: str) -> str:
        return views.step_page(self.store, name, sid, step_ver(self.store, name, sid))

    async def step(self, request: Request) -> Response:
        return await self._page(self._step, request.path_params["name"],
                                request.path_params["sid"])

    async def step_stream(self, request: Request) -> Response:
        name, sid = request.path_params["name"], request.path_params["sid"]
        return await self._stream(request, lambda: step_ver(self.store, name, sid),
                                  lambda: views.step_parts(self.store, name, sid), "sver",
                                  exists=lambda: self._has_step(name, sid))

    def _log(self, project: str | None, params: dict[str, list[str]]) -> str:
        return views.log_page(self.store, project, views.LogQuery.parse(params))

    async def log(self, request: Request) -> Response:
        params: dict[str, list[str]] = {}
        for k, v in request.query_params.multi_items():
            params.setdefault(k, []).append(v)
        return await self._page(self._log, request.path_params.get("name"), params)

    async def log_stream(self, request: Request) -> Response:
        name = request.path_params.get("name")
        if name is not None:
            try:
                await anyio.to_thread.run_sync(self.store.project, name)
            except SluiceError as err:
                return Response(err.message, status_code=404)
        return DatastarResponse(self._log_stream(name, await _signals(request)))

    def _inbox(self, project: str | None, status: str) -> str:
        return views.inbox_page(self.store, project, status, index_ver(self.store))

    async def inbox(self, request: Request) -> Response:
        return await self._page(self._inbox, request.path_params.get("name"),
                                request.query_params.get("status") or "open")

    async def inbox_stream(self, request: Request) -> Response:
        name = request.path_params.get("name")
        signals = await _signals(request)
        status = signals.get("status") if signals.get("status") in views.INBOX_FILTERS \
            else "open"
        return await self._stream(
            request, lambda: index_ver(self.store),
            lambda: views.inbox_parts(self.store, name, status),
            exists=(lambda: self.store.project(name)) if name is not None else None)

    async def answer(self, request: Request) -> Response:
        """Answer an inbox item: a JSON answer {action, params?, values?, text?} (from
        inbox.js), or the no-JS text box's form (its text, then a redirect back). Both go
        through Store.inbox_answer, like the inbox_answer tool."""
        if _foreign(request):
            return Response("answers from other sites are refused", status_code=403)
        name, item_id = request.path_params["name"], request.path_params["id"]
        is_json = request.headers.get("content-type", "").startswith("application/json")
        back = "/inbox"
        try:
            if is_json:
                try:
                    answer = await request.json()
                except ValueError:
                    raise BadRequest("the body is not JSON") from None
            else:
                form = await request.form()
                answer = {"action": "answer", "text": str(form.get("text") or "")}
                back = _next(str(form.get("next") or ""), back)
            item = await anyio.to_thread.run_sync(self.store.inbox_answer, name, item_id,
                                                  answer, AUTHOR)
        except SluiceError as err:
            code = HTTP_STATUS.get(err.code, 400)
            if is_json:
                return JSONResponse(err.payload(), status_code=code)
            errs = "".join(f"<li>{views.e(x)}</li>" for x in err.extra.get("errors", []))
            return HTMLResponse(views.layout(
                "not answered", f"<p>{views.e(err.message)}</p><ul>{errs}</ul>"
                f'<p><a href="{views.e(back)}">back to the inbox</a></p>'), status_code=code)
        if is_json:
            return JSONResponse(item)
        return RedirectResponse(back, status_code=303)

    async def _switch(self, request: Request, field: str, change: Callable[[bool], Any],
                      back: str) -> Response:
        """A switch form: `field` "1" or "0" calls `change(on)` (the tool's own code path),
        then back (303). Refused from another site's page."""
        if _foreign(request):
            return Response("changes from other sites are refused", status_code=403)
        on = str((await request.form()).get(field)) == "1"
        try:
            await anyio.to_thread.run_sync(change, on)
        except SluiceError as err:
            return Response(err.message, status_code=HTTP_STATUS.get(err.code, 400))
        return RedirectResponse(back, status_code=303)

    async def archive(self, request: Request) -> Response:
        """Archive a project, or bring it back, like the project_update tool."""
        name = request.path_params["name"]
        return await self._switch(request, "archived",
                                  lambda on: self.store.update_project(name, None, on,
                                                                       author=AUTHOR),
                                  f"/projects/{views.quote(name)}")

    async def pause(self, request: Request) -> Response:
        """Pause a project, or resume it, like the project_update tool."""
        name = request.path_params["name"]
        return await self._switch(request, "paused",
                                  lambda on: self.store.update_project(name, paused=on,
                                                                       author=AUTHOR),
                                  f"/projects/{views.quote(name)}")

    async def pause_step(self, request: Request) -> Response:
        """Pause a step, or resume it, like the step_pause tool; back to its drawer."""
        name, sid = request.path_params["name"], request.path_params["sid"]
        return await self._switch(
            request, "paused",
            lambda on: self.store.pause_steps(name, [sid], paused=on, author=AUTHOR),
            f"/projects/{views.quote(name)}#step:{views.quote(sid)}")

    async def icon(self, request: Request) -> Response:
        """The project's image icon (`_image`); 404 when it has none."""
        try:
            got = await anyio.to_thread.run_sync(self.store.icon_image,
                                                 request.path_params["name"])
        except SluiceError as err:
            return Response(err.message, status_code=404)
        if got is None:
            return Response("not found", status_code=404)
        return _image(request, *got)

    async def fn_icon(self, request: Request) -> Response:
        """A fn's image icon (`_image`), the fn as `?project=` sees it (none: the built-in and
        global ones); 404 when it has none (a text icon is shown as text)."""
        try:
            fn = await anyio.to_thread.run_sync(self.store.fn, request.path_params["name"],
                                                request.query_params.get("project") or None)
        except SluiceError as err:
            return Response(err.message, status_code=404)
        if fn.icon is None or not fn.icon.data:
            return Response("not found", status_code=404)
        return _image(request, fn.icon.type, fn.icon.data, fn.icon.hash)

    async def settings(self, request: Request) -> Response:
        """The settings menu's form: `theme` (an id of `views.THEMES`) and `types` ("0" or "1";
        the last one given wins, so an unticked box after its hidden "0" says off) each set or
        clear their cookie when present. Back (303) to `next` when it is a local path, else to
        the index; with no `next` (the menu's script), 204. Refused from another site's
        page."""
        if _foreign(request):
            return Response("changes from other sites are refused", status_code=403)
        form = await request.form()
        theme = form.get("theme")
        types = [str(t) for t in form.getlist("types")]
        if theme is not None and theme not in views.THEMES:
            return Response(f"theme must be one of {', '.join(views.THEMES)}", status_code=400)
        if types and types[-1] not in ("0", "1"):
            return Response('types must be "0" or "1"', status_code=400)
        nxt = form.get("next")
        response = RedirectResponse(_next(str(nxt), "/"), status_code=303) \
            if nxt is not None else Response(status_code=204)
        changes: dict[str, str] = {}
        if theme is not None:
            changes[THEME_COOKIE] = str(theme)
        if types:
            changes[TYPES_COOKIE] = "1" if types[-1] == "1" else ""
        for name, value in changes.items():
            if value:
                response.set_cookie(name, value, max_age=COOKIE_AGE, path="/",
                                    httponly=True, samesite="lax")
            else:
                response.delete_cookie(name, path="/", httponly=True, samesite="lax")
        return response

    async def static(self, request: Request) -> Response:
        name = request.path_params["file"]
        if name not in STATIC_TYPES:
            return Response("not found", status_code=404)
        return FileResponse(STATIC / name, media_type=STATIC_TYPES[name],
                            headers={"cache-control": "no-cache"})

    async def fns(self, request: Request) -> Response:
        return await self._page(views.fns_page, self.store,
                                request.query_params.get("project") or None)

    def add_routes(self, server: Any) -> None:
        """Register every route on the MCP server (FastMCP `custom_route`), each behind the
        Host allowlist."""
        for path, handler in (("/", self.index), ("/stream", self.index_stream),
                              ("/projects/{name}", self.project),
                              ("/projects/{name}/icon", self.icon),
                              ("/projects/{name}/stream", self.project_stream),
                              ("/projects/{name}/boxes/{sid}", self.box),
                              ("/projects/{name}/threads", self.threads),
                              ("/projects/{name}/threads/stream", self.threads_stream),
                              ("/projects/{name}/steps/{sid}", self.step),
                              ("/projects/{name}/steps/{sid}/stream", self.step_stream),
                              ("/projects/{name}/log", self.log),
                              ("/projects/{name}/log/stream", self.log_stream),
                              ("/log", self.log), ("/log/stream", self.log_stream),
                              ("/fns", self.fns), ("/fns/{name}/icon", self.fn_icon),
                              ("/inbox", self.inbox),
                              ("/inbox/stream", self.inbox_stream),
                              ("/projects/{name}/inbox", self.inbox),
                              ("/projects/{name}/inbox/stream", self.inbox_stream),
                              ("/static/{file}", self.static)):
            server.custom_route(path, methods=["GET"])(_local(handler))
        server.custom_route("/projects/{name}/inbox/{id}/answer",
                            methods=["POST"])(_local(self.answer))
        server.custom_route("/projects/{name}/archive",
                            methods=["POST"])(_local(self.archive))
        server.custom_route("/projects/{name}/pause",
                            methods=["POST"])(_local(self.pause))
        server.custom_route("/projects/{name}/steps/{sid}/pause",
                            methods=["POST"])(_local(self.pause_step))
        server.custom_route("/settings", methods=["POST"])(_local(self.settings))
