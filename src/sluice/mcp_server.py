"""The MCP server (SPEC §8): tools, docs for agents, and the live project pages.

Built on the official `mcp` SDK (v2 calls FastMCP `MCPServer`); `sluice serve` serves it over
streamable HTTP next to the runner, and `sluice tool` calls the same tools in-process.
"""

import functools
import html
import inspect
import json
from pathlib import Path
from typing import Any, Literal

import anyio
from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.exceptions import ToolError, UnexpectedToolError
from mcp.types import CallToolResult, TextContent
from pydantic import ValidationError
from starlette.requests import Request
from starlette.responses import HTMLResponse, Response

from . import calls, runner, views
from . import verify as verify_mod
from .errors import BadRequest, NotFound, SluiceError
from .store import Store

AUTHOR = "mcp"
DOCS = Path(__file__).resolve().parent / "docs"


def doc_topics() -> dict[str, str]:
    """topic -> the page's first heading (or first line)."""
    out = {}
    for page in sorted(DOCS.glob("*.md")):
        first = next((ln for ln in page.read_text(encoding="utf-8").splitlines() if ln.strip()),
                     "")
        out[page.stem] = first.lstrip("# ").strip()
    return out


def doc_page(topic: str) -> str:
    if topic not in doc_topics():
        raise NotFound(f"no docs topic {topic!r}; topics: {', '.join(doc_topics())}")
    return (DOCS / f"{topic}.md").read_text(encoding="utf-8")


def _result(value: Any) -> CallToolResult:
    if isinstance(value, str):  # markdown, Mermaid or HTML: returned as plain text
        return CallToolResult(content=[TextContent(type="text", text=value)])
    structured = value if isinstance(value, dict) else {"result": value}
    return CallToolResult(content=[TextContent(type="text", text=json.dumps(value))],
                          structured_content=structured)


def _error(e: SluiceError) -> CallToolResult:
    return CallToolResult(content=[TextContent(type="text", text=e.to_json())], is_error=True)


def _tool(fn):
    """Wrap a tool body so results become JSON and SluiceErrors become JSON error payloads."""
    if inspect.iscoroutinefunction(fn):
        @functools.wraps(fn)
        async def wrapper(*args, **kwargs):
            try:
                return _result(await fn(*args, **kwargs))
            except SluiceError as e:
                return _error(e)
    else:
        @functools.wraps(fn)
        def wrapper(*args, **kwargs):
            try:
                return _result(fn(*args, **kwargs))
            except SluiceError as e:
                return _error(e)
    wrapper.__signature__ = inspect.signature(fn).replace(return_annotation=CallToolResult)
    wrapper.__annotations__ = {**fn.__annotations__, "return": CallToolResult}
    return wrapper


class _Server(MCPServer):
    async def call_tool(self, name, arguments, context=None):
        try:
            return await super().call_tool(name, arguments, context)
        except UnexpectedToolError:
            raise
        except ToolError as e:  # bad arguments or an unknown tool, in the same JSON shape
            if isinstance(e.__cause__, ValidationError):
                problems = "; ".join(f"{'.'.join(map(str, x['loc']))}: {x['msg']}"
                                     for x in e.__cause__.errors())
                return _error(BadRequest(f"bad arguments for {name}: {problems}"))
            return _error(BadRequest(str(e)))


def build_server(store: Store) -> MCPServer:
    mcp = _Server("sluice", instructions=doc_page("instructions"))

    def tool(fn):
        mcp.add_tool(_tool(fn), name=fn.__name__, description=inspect.getdoc(fn))
        return fn

    for topic, title in doc_topics().items():
        mcp.resource(f"sluice://docs/{topic}", name=topic, description=title,
                     mime_type="text/markdown")(functools.partial(doc_page, topic))

    @tool
    def docs(topic: str | None = None) -> Any:
        """Read sluice's documentation for agents.

        Args:
            topic: a page name such as "plans", "types", "fns" or "examples". Leave it out to
                get the index: {topic: first heading} for every page.
        """
        return doc_topics() if topic is None else doc_page(topic)

    @tool
    def projects_list() -> Any:
        """List projects: [{name, description, rev, counts}]; counts maps step status -> number
        of steps in the project's plan."""
        return store.projects()

    @tool
    def project_create(name: str, description: str = "") -> Any:
        """Create a project with an empty plan (rev 1). Returns {name}.

        Args:
            name: lowercase letters, digits, - and _ (starting with a letter or digit).
            description: what the project is for; put any context an orchestrator needs here.
        """
        return store.create_project(name, description, AUTHOR)

    @tool
    def project_update(name: str, description: str) -> Any:
        """Replace a project's description. Returns {name}.

        Args:
            name: the project.
            description: the new description.
        """
        return store.update_project(name, description)

    @tool
    def fn_list(project: str | None = None) -> Any:
        """List the functions a project sees (built-in, global, then the project's own; without
        a project: built-in and global): [{name, doc, inputs, outputs, scope}]. A function with a
        problem (bad fn.json, name collision) carries `error`; see verify.

        Args:
            project: the project whose functions to list.
        """
        return store.registry(project).listing()

    @tool
    def fn_get(name: str, project: str | None = None) -> Any:
        """Return one function's fn.json plus its `scope` and `path` (its directory).

        Args:
            name: the function name, e.g. "git.head".
            project: look it up as this project sees it (project functions included).
        """
        fn = store.fn(name, project)
        return {**fn.raw, "scope": fn.scope, "path": str(fn.dir)}

    @tool
    def fn_save(fn: dict[str, Any], main_py: str, project: str | None = None) -> Any:
        """Create or replace a function: writes fns/<name>/fn.json and main.py into the project
        (or, without one, the global fns dir). Returns {scope, path}. Read docs("fns") first.

        Args:
            fn: the fn.json: {name, doc?, inputs: {name: type}, outputs: {name: type}}. Checked
                before anything is written; the name may not collide with a built-in or global
                function (nor, for a global function, with any project's own).
            main_py: the Python source of main.py (a uv script calling sluice.fn.run).
            project: the project that owns it; leave out for a global function.
        """
        return store.fn_save(fn, main_py, project)

    @tool
    async def fn_call(name: str, inputs: dict[str, Any], project: str | None = None,
                      wait: float = 0, direct: bool = False) -> Any:
        """Run one function outside the plan. Returns {call, status, outputs?, error?}; status
        is pending, running, succeeded or failed. Poll call_status(call) for a slow one.

        Args:
            name: the function to run.
            inputs: an object keyed by the function's input names; checked against its types
                before anything runs (an `invalid` error lists every mismatch with its path).
            project: run it in this project (its functions and .env); leave out for none.
            wait: seconds to wait for the result (default 0: return at once).
            direct: run it here and now, to the end, instead of queueing it for the runner
                (for use without a runner, e.g. from the command line); ignores `wait`.
        """
        call = await anyio.to_thread.run_sync(calls.create, store, name, inputs, project,
                                              direct)
        store.notify()
        if direct:
            return await anyio.to_thread.run_sync(runner.run_call_direct, store, call, project)
        deadline = anyio.current_time() + wait
        while True:
            res = await anyio.to_thread.run_sync(calls.status, store, call, project)
            res.pop("stderr_tail", None)
            if res["status"] in calls.DONE or anyio.current_time() >= deadline:
                return res
            await anyio.sleep(0.1)

    @tool
    def call_status(call: str, project: str | None = None) -> Any:
        """Return {call, status, outputs?, error?, stderr_tail?} for a fn_call.

        Args:
            call: the id fn_call returned.
            project: the project it ran in, if any.
        """
        return calls.status(store, call, project)

    @tool
    def plan_get(project: str) -> Any:
        """Return {rev, plan}: the project's plan (without rev) and its revision.

        Args:
            project: the project.
        """
        cur = store.get(project)
        return {"rev": cur["rev"], "plan": {k: v for k, v in cur.items() if k != "rev"}}

    @tool
    def plan_patch(project: str, rev: int, ops: list[dict[str, Any]], reason: str,
                   author: str = AUTHOR) -> Any:
        """Edit a project's plan with RFC 6902 JSON Patch ops. Returns {rev}.

        Args:
            project: the project.
            rev: the revision you read; if the plan moved on you get `conflict` with
                current_rev, so re-read and retry.
            ops: JSON Patch operations against the plan without rev, e.g.
                [{"op": "add", "path": "/steps/x", "value": {...}}]. The result is validated;
                an `invalid` error lists every problem with its path.
            reason: why, recorded in the plan's history.
            author: who is editing (default "mcp").
        """
        return {"rev": store.patch(project, rev, ops, author, reason)}

    @tool
    def plan_history(project: str, since_rev: int | None = None) -> Any:
        """Return the plan's log: edits {rev, at, author, reason, ops} and manual values
        {rev, at, author, reason, action, ...}.

        Args:
            project: the project.
            since_rev: only entries after this revision.
        """
        return store.history(project, since_rev)

    @tool
    def plan_set_input(project: str, name: str, value: Any, reason: str = "") -> Any:
        """Set a declared plan input; steps reading it can then start. A later change only
        affects steps that have not started. Returns {ok}.

        Args:
            project: the project.
            name: the plan input's name.
            value: its value, checked against the input's type.
            reason: why, recorded in the plan's history.
        """
        store.set_input(project, name, value, AUTHOR, reason)
        return {"ok": True}

    @tool
    def step_set_input(project: str, step: str, input: str, value: Any, reason: str = "",
                       rev: int | None = None) -> Any:
        """Pin one step input to a literal ({"default": value}); an edit. Returns {rev}.

        Args:
            project: the project.
            step: the step id.
            input: the step's input name.
            value: the literal, checked against the input's type.
            reason: why, recorded in the plan's history.
            rev: the revision you read (default: the current one).
        """
        return {"rev": store.set_step_input(project, step, input, value, AUTHOR, reason, rev)}

    @tool
    def step_set_output(project: str, step: str, outputs: dict[str, Any],
                        reason: str = "") -> Any:
        """Mark a step succeeded with outputs you supply (manual: true); it is not run unless
        retried. Returns {ok}.

        Args:
            project: the project.
            step: a step that is not running.
            outputs: every output of its function, type-checked (arrays for a scattered step).
            reason: why, recorded in the plan's history.
        """
        store.set_output(project, step, outputs, AUTHOR, reason)
        return {"ok": True}

    @tool
    def step_retry(project: str, step: str, reason: str = "") -> Any:
        """Set a failed (or manually set) step back to pending so it runs again. Returns {ok}.

        Args:
            project: the project.
            step: the step id.
            reason: why, recorded in the plan's history.
        """
        store.retry(project, step, AUTHOR, reason)
        return {"ok": True}

    @tool
    def verify(project: str | None = None) -> Any:
        """Check functions (fn.json shape and types, name collisions), project.json, .env
        files, the plan and state.json. Returns {ok, problems: [{where, message}]}; changes
        nothing.

        Args:
            project: check this project (and the built-in and global functions it sees);
                leave out to check everything.
        """
        return verify_mod.verify(store, project)

    @tool
    def plan_view(project: str, format: Literal["mermaid", "html"] = "mermaid") -> Any:
        """Draw the plan with each step's status: a Mermaid flowchart or a standalone HTML page.

        Args:
            project: the project.
            format: "mermaid" (default) or "html".
        """
        return views.render(store, project, format)

    @tool
    def status(project: str) -> Any:
        """Return {rev, inputs, outputs, steps: [{id, run, status, started, finished,
        outputs?, error?, manual}]}. inputs and outputs map names to values (null if unset).

        Args:
            project: the project.
        """
        return store.status(project)

    async def page(render, *args) -> Response:
        try:
            return HTMLResponse(await anyio.to_thread.run_sync(render, *args))
        except SluiceError as e:
            return HTMLResponse(views.layout("not found", f"<p>{html.escape(e.message)}</p>"),
                                status_code=404)

    @mcp.custom_route("/", methods=["GET"])
    async def index_page(request: Request) -> Response:
        return await page(views.index, store)

    @mcp.custom_route("/projects/{name}", methods=["GET"])
    async def project_page(request: Request) -> Response:
        return await page(views.render, store, request.path_params["name"], "html", 3)

    @mcp.custom_route("/fns", methods=["GET"])
    async def fns_page(request: Request) -> Response:
        return await page(views.fns_page, store, request.query_params.get("project") or None)

    return mcp
