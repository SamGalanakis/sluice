"""The dashboard's HTTP routes (SPEC §8 Views): pages, and one Datastar SSE stream per page.

A page renders completely on first load (usable without JavaScript) and carries the version of
what it shows (`ver`, from the stats of the files it reads). Its stream polls those stats every
`interval` seconds off the event loop; when they change it re-renders the page's parts and
sends a `datastar-patch-elements` event for each part that differs from what the client has,
then the new `ver` (so a reconnecting client resumes from there). An idle page gets nothing.
The log page's stream sends the table when the filter signals changed, and on the newest page
prepends new matching records. Everything is read-only.
"""

from __future__ import annotations

import hashlib
import threading
from collections.abc import AsyncIterator, Callable
from pathlib import Path
from typing import Any

import anyio
from datastar_py import ServerSentEventGenerator as SSE
from datastar_py.consts import ElementPatchMode
from datastar_py.starlette import DatastarResponse, read_signals
from starlette.requests import Request
from starlette.responses import HTMLResponse, Response

from . import log as L
from . import views
from .errors import BadRequest, NotFound, SluiceError
from .store import Store

PROJECT_FILES = ("project.json", "plan.json", "state.json", L.FILE)
REPLACED = {"plan-src"}  # replaced, not morphed, so its data-init re-renders the diagram


def _stat(path: Path) -> tuple[int, int] | None:
    try:
        s = path.stat()
    except FileNotFoundError:
        return None
    return s.st_mtime_ns, s.st_size


def _digest(value: Any) -> str:
    return hashlib.sha256(repr(value).encode()).hexdigest()[:16]


def project_ver(store: Store, project: str) -> str:
    """The version of what the project page shows: the stats of the files it reads."""
    d = store.project_dir(project)
    return _digest([_stat(d / f) for f in PROJECT_FILES])


def index_ver(store: Store) -> str:
    return _digest([(n, [_stat(store.project_dir(n) / f) for f in PROJECT_FILES])
                    for n in store.project_names()])


def log_ver(store: Store, project: str | None) -> str:
    return _digest(_stat(store.log_dir(project) / L.FILE))


def _patch(pid: str, html: str) -> str:
    return SSE.patch_elements(html, mode=ElementPatchMode.REPLACE if pid in REPLACED else None)


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
                            parts: Callable[[], dict[str, str]]) -> AsyncIterator[str]:
        """Patch the parts that changed whenever `ver()` moves on (all of them at once when
        the client's version is not the current one)."""
        run = anyio.to_thread.run_sync
        try:
            cur = await run(ver)
            last = await run(parts)
            if client_ver != cur:
                for pid, html in last.items():
                    yield _patch(pid, html)
                yield SSE.patch_signals({"ver": cur})
            while await self._tick():
                new_ver = await run(ver)
                if new_ver == cur:
                    continue
                cur, new = new_ver, await run(parts)
                for pid, html in new.items():
                    if last.get(pid) != html:
                        yield _patch(pid, html)
                last = new
                yield SSE.patch_signals({"ver": cur})
        except (SluiceError, OSError, ValueError):  # e.g. the project is gone: end the stream
            return

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
        d, stamp = self.store.log_dir(project), None
        while True:
            new_stamp = await run(log_ver, self.store, project)
            if new_stamp != stamp:
                stamp = new_stamp
                res = await run(L.read, d, seen, q.kinds, q.threads)
                if res["records"]:
                    rows = views.log_rows(reversed(res["records"]))
                    yield SSE.patch_elements(rows, selector="#log-rows",
                                             mode=ElementPatchMode.PREPEND)
                if res["last_seq"] != seen:
                    seen = res["last_seq"]
                    yield SSE.patch_signals({"seen": seen})
            if not await self._tick():
                return

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
        return views.index(self.store, index_ver(self.store))

    def _project(self, name: str) -> str:
        self.store.project(name)
        return views.project_page(self.store, name, project_ver(self.store, name))

    async def index(self, request: Request) -> Response:
        return await self._page(self._index)

    async def index_stream(self, request: Request) -> Response:
        signals = await _signals(request)
        return DatastarResponse(self._parts_stream(
            signals.get("ver"), lambda: index_ver(self.store),
            lambda: views.index_parts(self.store)))

    async def project(self, request: Request) -> Response:
        return await self._page(self._project, request.path_params["name"])

    async def project_stream(self, request: Request) -> Response:
        name = request.path_params["name"]
        try:
            await anyio.to_thread.run_sync(self.store.project, name)
        except SluiceError as err:
            return Response(err.message, status_code=404)
        signals = await _signals(request)
        return DatastarResponse(self._parts_stream(
            signals.get("ver"), lambda: project_ver(self.store, name),
            lambda: views.project_parts(self.store, name)))

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

    async def fns(self, request: Request) -> Response:
        return await self._page(views.fns_page, self.store,
                                request.query_params.get("project") or None)

    def add_routes(self, server: Any) -> None:
        """Register every route on the MCP server (FastMCP `custom_route`)."""
        for path, handler in (("/", self.index), ("/stream", self.index_stream),
                              ("/projects/{name}", self.project),
                              ("/projects/{name}/stream", self.project_stream),
                              ("/projects/{name}/log", self.log),
                              ("/projects/{name}/log/stream", self.log_stream),
                              ("/log", self.log), ("/log/stream", self.log_stream),
                              ("/fns", self.fns)):
            server.custom_route(path, methods=["GET"])(handler)

