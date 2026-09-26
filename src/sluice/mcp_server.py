"""The MCP server (SPEC §8): tools, docs for agents, and the live plan pages.

Built on the official `mcp` SDK (v2 calls FastMCP `MCPServer`); `sluice serve` serves it over
streamable HTTP next to the runner.
"""

import functools
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

from . import views
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
    def fn_list() -> Any:
        """List every function: [{name, doc, inputs, outputs}], types as declared in fn.json."""
        return [store.registry.fns[n].summary() for n in store.registry.names()]

    @tool
    def fn_get(name: str) -> Any:
        """Return one function's full fn.json.

        Args:
            name: the function name, e.g. "git.head".
        """
        fn = store.registry.get(name)
        if fn is None:
            raise NotFound(f"no fn {name!r}")
        return fn.raw

    @tool
    async def fn_call(name: str, inputs: dict[str, Any], wait: float = 0) -> Any:
        """Run one function as a one-step plan named call-<timestamp>-<hex>.

        Returns {plan, status, outputs?, error?}. If the call has not finished within `wait`
        seconds, status is "pending" or "running": poll status(plan) later.

        Args:
            name: the function to run.
            inputs: an object keyed by the function's input names; checked against its types
                before anything runs (an `invalid` error lists every mismatch with its path).
            wait: seconds to wait for the result (default 0: return at once).
        """
        pid = await anyio.to_thread.run_sync(store.create_call, name, inputs, AUTHOR)
        deadline = anyio.current_time() + wait
        while True:
            res = await anyio.to_thread.run_sync(store.call_result, pid)
            if res["status"] in ("succeeded", "failed") or anyio.current_time() >= deadline:
                return res
            await anyio.sleep(0.1)

    @tool
    def plans_list(include_calls: bool = False) -> Any:
        """List plans: [{id, label, rev, counts}] where counts maps step status -> number.

        Args:
            include_calls: also list the one-step plans made by fn_call (default false).
        """
        return store.plans(include_calls)

    @tool
    def plan_create(plan: str, doc: dict[str, Any], reason: str) -> Any:
        """Create a plan. Returns {rev} (1). Read docs("plans") for the document shape.

        Args:
            plan: the new plan's id (lowercase letters, digits, - and _).
            doc: the plan: {label?, inputs?: {name: type}, outputs?: {name: {"source": ref}},
                steps: {id: {run, in, scatter?}}}. It is validated; an `invalid` error lists
                every problem with its path.
            reason: why, recorded in the plan's history.
        """
        return {"rev": store.create(plan, doc, AUTHOR, reason)}

    @tool
    def plan_get(plan: str) -> Any:
        """Return {rev, doc}: the current plan document (without rev) and its revision.

        Args:
            plan: the plan id.
        """
        cur = store.get(plan)
        return {"rev": cur["rev"], "doc": {k: v for k, v in cur.items() if k != "rev"}}

    @tool
    def plan_patch(plan: str, rev: int, ops: list[dict[str, Any]], reason: str,
                   author: str = AUTHOR) -> Any:
        """Edit a plan with RFC 6902 JSON Patch ops. Returns {rev}.

        Args:
            plan: the plan id.
            rev: the revision you read; if the plan moved on you get `conflict` with
                current_rev, so re-read and retry.
            ops: JSON Patch operations against the document without rev, e.g.
                [{"op": "add", "path": "/steps/x", "value": {...}}].
            reason: why, recorded in the plan's history.
            author: who is editing (default "mcp").
        """
        return {"rev": store.patch(plan, rev, ops, author, reason)}

    @tool
    def plan_history(plan: str, since_rev: int | None = None) -> Any:
        """Return the plan's log: edits {rev, at, author, reason, ops} and manual values
        {rev, at, author, reason, action, ...}.

        Args:
            plan: the plan id.
            since_rev: only entries after this revision.
        """
        return store.history(plan, since_rev)

    @tool
    def plan_set_input(plan: str, name: str, value: Any, reason: str = "") -> Any:
        """Set a declared plan input; steps reading it can then start. Returns {ok}.

        Args:
            plan: the plan id.
            name: the plan input's name.
            value: its value, checked against the input's type.
            reason: why, recorded in the plan's history.
        """
        store.set_input(plan, name, value, AUTHOR, reason)
        return {"ok": True}

    @tool
    def step_set_input(plan: str, step: str, input: str, value: Any, reason: str = "",
                       rev: int | None = None) -> Any:
        """Pin one step input to a literal ({"default": value}); an edit. Returns {rev}.

        Args:
            plan: the plan id.
            step: the step id.
            input: the step's input name.
            value: the literal, checked against the input's type.
            reason: why, recorded in the plan's history.
            rev: the revision you read (default: the current one).
        """
        return {"rev": store.set_step_input(plan, step, input, value, AUTHOR, reason, rev)}

    @tool
    def step_set_output(plan: str, step: str, outputs: dict[str, Any], reason: str = "") -> Any:
        """Mark a step succeeded with outputs you supply (manual: true); it is not run unless
        retried. Returns {ok}.

        Args:
            plan: the plan id.
            step: a step that is not running.
            outputs: every output of its function, type-checked (arrays for a scattered step).
            reason: why, recorded in the plan's history.
        """
        store.set_output(plan, step, outputs, AUTHOR, reason)
        return {"ok": True}

    @tool
    def step_retry(plan: str, step: str, reason: str = "") -> Any:
        """Set a failed (or manually set) step back to pending so it runs again. Returns {ok}.

        Args:
            plan: the plan id.
            step: the step id.
            reason: why, recorded in the plan's history.
        """
        store.retry(plan, step, AUTHOR, reason)
        return {"ok": True}

    @tool
    def plan_view(plan: str, format: Literal["mermaid", "html"] = "mermaid") -> Any:
        """Draw the plan with each step's status: a Mermaid flowchart or a standalone HTML page.

        Args:
            plan: the plan id.
            format: "mermaid" (default) or "html".
        """
        return views.render(store, plan, format)

    @tool
    def status(plan: str) -> Any:
        """Return {rev, inputs, outputs, steps: [{id, run, status, started, finished,
        outputs?, error?, manual}]}. inputs and outputs map names to values (null if unset).

        Args:
            plan: the plan id.
        """
        return store.status(plan)

    @mcp.custom_route("/plans", methods=["GET"])
    async def plans_page(request: Request) -> Response:
        return HTMLResponse(await anyio.to_thread.run_sync(views.index, store))

    @mcp.custom_route("/plans/{pid}", methods=["GET"])
    async def plan_page(request: Request) -> Response:
        try:
            text = await anyio.to_thread.run_sync(views.render, store,
                                                  request.path_params["pid"], "html", 3)
        except SluiceError as e:
            return HTMLResponse(f"<p>{e.message}</p>", status_code=404)
        return HTMLResponse(text)

    return mcp
