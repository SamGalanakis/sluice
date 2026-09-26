"""The MCP server (SPEC §8): the tools, JSON results, JSON error payloads.

Built on the official `mcp` SDK (v2 calls FastMCP `MCPServer`); `sluice serve` serves it over
streamable HTTP next to the runner.
"""

import functools
import inspect
import json
from typing import Any

import anyio
from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.exceptions import ToolError, UnexpectedToolError
from mcp.types import CallToolResult, TextContent
from pydantic import ValidationError

from .errors import BadRequest, NotFound, SluiceError
from .store import Store

AUTHOR = "mcp"
INSTRUCTIONS = """\
sluice runs plans: typed steps that call fns. Create a plan with plan_create, change it with
plan_patch at the current rev (a stale rev returns a `conflict` error with current_rev) or the
convenience tools, and watch it with status. Errors are JSON: {"error": not_found|conflict|
invalid|bad_request, "message": ...}."""


def _result(value: Any) -> CallToolResult:
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
    mcp = _Server("sluice", instructions=INSTRUCTIONS)

    def tool(fn):
        mcp.add_tool(_tool(fn), name=fn.__name__, description=inspect.getdoc(fn))
        return fn

    @tool
    def fn_list() -> Any:
        """Every fn: [{name, doc, inputs, outputs}] (types as declared)."""
        return [store.registry.fns[n].summary() for n in store.registry.names()]

    @tool
    def fn_get(name: str) -> Any:
        """The fn.json of one fn."""
        fn = store.registry.get(name)
        if fn is None:
            raise NotFound(f"no fn {name!r}")
        return fn.raw

    @tool
    async def fn_call(name: str, inputs: dict[str, Any], wait: float = 0) -> Any:
        """Run one fn as a one-step plan (call-<ts>-<short>). Waits up to `wait` seconds and
        returns {plan, status, outputs?, error?}."""
        pid = await anyio.to_thread.run_sync(store.create_call, name, inputs, AUTHOR)
        deadline = anyio.current_time() + wait
        while True:
            res = await anyio.to_thread.run_sync(store.call_result, pid)
            if res["status"] in ("succeeded", "failed") or anyio.current_time() >= deadline:
                return res
            await anyio.sleep(0.1)

    @tool
    def plans_list(include_calls: bool = False) -> Any:
        """[{id, label, rev, counts}]; fn_call plans are hidden unless include_calls."""
        return store.plans(include_calls)

    @tool
    def plan_create(plan: str, doc: dict[str, Any], reason: str) -> Any:
        """Create plan `plan` from `doc` (id?, label?, inputs?, outputs?, steps). {rev}."""
        return {"rev": store.create(plan, doc, AUTHOR, reason)}

    @tool
    def plan_get(plan: str) -> Any:
        """{rev, doc}: the current plan, `doc` without rev."""
        cur = store.get(plan)
        return {"rev": cur["rev"], "doc": {k: v for k, v in cur.items() if k != "rev"}}

    @tool
    def plan_patch(plan: str, rev: int, ops: list[dict[str, Any]], reason: str,
                   author: str = AUTHOR) -> Any:
        """Apply an RFC 6902 JSON Patch (against the plan without rev) at `rev`. {rev}."""
        return {"rev": store.patch(plan, rev, ops, author, reason)}

    @tool
    def plan_history(plan: str, since_rev: int | None = None) -> Any:
        """The plan's log: edits ({rev, at, author, reason, ops}) and manual values."""
        return store.history(plan, since_rev)

    @tool
    def plan_set_input(plan: str, name: str, value: Any, reason: str = "") -> Any:
        """Set a declared plan input (type-checked). {ok}."""
        store.set_input(plan, name, value, AUTHOR, reason)
        return {"ok": True}

    @tool
    def step_set_input(plan: str, step: str, input: str, value: Any, reason: str = "",
                       rev: int | None = None) -> Any:
        """Bind a step input to {"default": value}: an edit, at `rev` or the current one. {rev}."""
        return {"rev": store.set_step_input(plan, step, input, value, AUTHOR, reason, rev)}

    @tool
    def step_set_output(plan: str, step: str, outputs: dict[str, Any], reason: str = "") -> Any:
        """Mark a non-running step succeeded with these outputs (manual: true). {ok}."""
        store.set_output(plan, step, outputs, AUTHOR, reason)
        return {"ok": True}

    @tool
    def step_retry(plan: str, step: str, reason: str = "") -> Any:
        """Set a failed (or manually set) step back to pending. {ok}."""
        store.retry(plan, step, AUTHOR, reason)
        return {"ok": True}

    @tool
    def status(plan: str) -> Any:
        """{rev, inputs, outputs, steps: [{id, run, status, started, finished, outputs?, error?,
        manual}]}."""
        return store.status(plan)

    return mcp
