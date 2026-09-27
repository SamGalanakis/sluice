"""The dashboard's HTTP routes (SPEC §8 Views): pages, and one Datastar SSE stream per page.

A page renders completely on first load (usable without JavaScript) and carries the version of
what it shows (`ver`, from the stats of the files it reads, including the stderr.log of every
running step's current run, so a progress line moves while an agent works). Its stream polls
those stats every `interval` seconds off the event loop; when they change it re-renders the
page's parts and sends a `datastar-patch-elements` event for each part that differs from what
the client has, then the new `ver` (so a reconnecting client resumes from there). An idle page
gets nothing. A step's detail (the project page's drawer, or its own page) streams the same way
under the `sver` signal, versioned by the project and that step's runs.
The log page's stream sends the table when the filter signals changed, and on the newest page
prepends new matching records. The inbox page streams its items the same way, and its answer
route is one of the dashboard's two writes: it calls the same Store.inbox_answer as the MCP
tool. The other is archiving a project (Store.update_project, like project_update).
"""

from __future__ import annotations

import hashlib
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

from . import inbox as I
from . import log as L
from . import views
from .errors import BadRequest, NotFound, SluiceError
from .store import Store
from .util import read_json

PROJECT_FILES = ("project.json", "plan.json", "state.json", L.FILE, I.FILE)
STATIC = Path(__file__).resolve().parent / "static"
# the dashboard's own scripts, then the vendored ones (from jsDelivr, the versions in their
# names; lang-core's imports rewritten to these files), so no third-party script runs here
STATIC_TYPES = {"inbox.js": "text/javascript", "openui.json": "application/json",
                "sluice.js": "text/javascript", "nav.js": "text/javascript",
                **dict.fromkeys(["datastar-rocket-1.0.4.js", "lang-core-0.3.0.js",
                                 "zod-4.6.5-v4.js", "zod-4.6.5-v4-core.js", "ci-info-4.4.0.js"],
                                "text/javascript")}
AUTHOR = "dashboard"
HTTP_STATUS = {"not_found": 404, "conflict": 409}


def _stat(path: Path) -> tuple[int, int] | None:
    try:
        s = path.stat()
    except FileNotFoundError:
        return None
    return s.st_mtime_ns, s.st_size


def _digest(value: Any) -> str:
    return hashlib.sha256(repr(value).encode()).hexdigest()[:16]


def index_ver(store: Store) -> str:
    """The version of the index and inbox pages: the stats of every project's files."""
    return _digest([(n, [_stat(store.project_dir(n) / f) for f in PROJECT_FILES])
                    for n in store.project_names()])


def _running_stderr(store: Store, project: str) -> list[Path]:
    """The stderr.log of every running step's runs (cheap: state.json is small)."""
    try:
        steps = read_json(store.project_dir(project) / "state.json").get("steps") or {}
    except (OSError, ValueError, AttributeError):
        return []
    out = []
    for e in steps.values():
        if isinstance(e, dict) and e.get("status") == "running":
            out += [store.runs_dir(project) / r / "stderr.log" for r in e.get("run_ids") or []
                    if isinstance(r, str) and L.RUN_ID_RE.match(r)]
    return out


def project_ver(store: Store, project: str) -> str:
    """The version of what the project page shows: the stats of the files it reads, of every
    inbox (for the nav's badge) and of the running steps' stderr (their progress lines)."""
    d = store.project_dir(project)
    inboxes = [_stat(store.project_dir(n) / I.FILE) for n in store.project_names()]
    return _digest([[_stat(d / f) for f in PROJECT_FILES], inboxes,
                    [_stat(p) for p in _running_stderr(store, project)]])


def step_ver(store: Store, project: str, sid: str) -> str:
    """The version of a step's detail: the step, the project's version, and the stderr of the
    step's runs (the step is part of it: the drawer's `sver` moves on when it shows another)."""
    runs = views.step_run_dirs(store, project, sid)
    return _digest([sid, project_ver(store, project), [_stat(r / "stderr.log") for r in runs]])


def log_ver(store: Store, project: str | None) -> str:
    return _digest(_stat(store.log_dir(project) / L.FILE))


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
            cur = await run(ver)
            last = await run(parts)
            if client_ver != cur:
                for html in last.values():
                    yield _patch(html)
                yield SSE.patch_signals({signal: cur})
            while await self._tick():
                new_ver = await run(ver)
                if new_ver == cur:
                    continue
                cur, new = new_ver, await run(parts)
                for pid, html in new.items():
                    if last.get(pid) != html:
                        yield _patch(html)
                last = new
                yield SSE.patch_signals({signal: cur})
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
        return views.index(self.store, index_ver(self.store))

    def _project(self, name: str) -> str:
        self.store.project(name)
        return views.project_page(self.store, name, project_ver(self.store, name))

    async def index(self, request: Request) -> Response:
        return await self._page(self._index)

    async def index_stream(self, request: Request) -> Response:
        return await self._stream(request, lambda: index_ver(self.store),
                                  lambda: views.index_parts(self.store))

    async def project(self, request: Request) -> Response:
        return await self._page(self._project, request.path_params["name"])

    async def project_stream(self, request: Request) -> Response:
        name = request.path_params["name"]
        return await self._stream(request, lambda: project_ver(self.store, name),
                                  lambda: views.project_parts(self.store, name),
                                  exists=lambda: self.store.project(name))

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
        origin = request.headers.get("origin")
        if origin and urlsplit(origin).netloc != request.headers.get("host"):
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
                nxt = str(form.get("next") or "")
                back = nxt if nxt.startswith("/") and not nxt.startswith("//") else back
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
        origin = request.headers.get("origin")
        if origin and urlsplit(origin).netloc != request.headers.get("host"):
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
                                  lambda on: self.store.update_project(name, None, on),
                                  f"/projects/{views.quote(name)}")

    async def pause(self, request: Request) -> Response:
        """Pause a project, or resume it, like the project_update tool."""
        name = request.path_params["name"]
        return await self._switch(request, "paused",
                                  lambda on: self.store.update_project(name, paused=on),
                                  f"/projects/{views.quote(name)}")

    async def pause_step(self, request: Request) -> Response:
        """Pause a step, or resume it, like the step_pause tool; back to its drawer."""
        name, sid = request.path_params["name"], request.path_params["sid"]
        return await self._switch(
            request, "paused",
            lambda on: self.store.pause_steps(name, [sid], paused=on, author=AUTHOR),
            f"/projects/{views.quote(name)}#step:{views.quote(sid)}")

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
        """Register every route on the MCP server (FastMCP `custom_route`)."""
        for path, handler in (("/", self.index), ("/stream", self.index_stream),
                              ("/projects/{name}", self.project),
                              ("/projects/{name}/stream", self.project_stream),
                              ("/projects/{name}/threads", self.threads),
                              ("/projects/{name}/threads/stream", self.threads_stream),
                              ("/projects/{name}/steps/{sid}", self.step),
                              ("/projects/{name}/steps/{sid}/stream", self.step_stream),
                              ("/projects/{name}/log", self.log),
                              ("/projects/{name}/log/stream", self.log_stream),
                              ("/log", self.log), ("/log/stream", self.log_stream),
                              ("/fns", self.fns), ("/inbox", self.inbox),
                              ("/inbox/stream", self.inbox_stream),
                              ("/projects/{name}/inbox", self.inbox),
                              ("/projects/{name}/inbox/stream", self.inbox_stream),
                              ("/static/{file}", self.static)):
            server.custom_route(path, methods=["GET"])(handler)
        server.custom_route("/projects/{name}/inbox/{id}/answer", methods=["POST"])(self.answer)
        server.custom_route("/projects/{name}/archive", methods=["POST"])(self.archive)
        server.custom_route("/projects/{name}/pause", methods=["POST"])(self.pause)
        server.custom_route("/projects/{name}/steps/{sid}/pause",
                            methods=["POST"])(self.pause_step)

