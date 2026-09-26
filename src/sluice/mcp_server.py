"""The MCP server (SPEC §9): every tool, JSON results, JSON error payloads.

Built on the official `mcp` SDK (v2: `MCPServer`, formerly FastMCP), served over streamable
HTTP by `sluice serve` together with the runner loop.
"""

import functools
import inspect
import json
from typing import Any

import anyio
import jsonpointer
from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.exceptions import ToolError, UnexpectedToolError
from mcp.types import CallToolResult, TextContent
from pydantic import ValidationError

from . import lifecycle as L
from . import views
from .errors import BadRequest, InvalidPlan, NotFound, SluiceError
from .store import Store

INSTRUCTIONS = """\
sluice runs a plan: a graph of typed fn calls. Edit plans with plan_patch/node_add at the
current rev (a stale rev returns a `conflict` error with current_rev); every accepted edit is
validated and logged. Watch progress with status and events_tail; decide inbox items with
inbox_resolve. Errors are JSON: {"error": not_found|conflict|invalid|bad_request, ...}."""


def _ok(result: Any) -> CallToolResult:
    structured = result if isinstance(result, dict) else {"result": result}
    return CallToolResult(content=[TextContent(type="text", text=json.dumps(result))],
                          structured_content=structured)


def _err(e: SluiceError) -> CallToolResult:
    return CallToolResult(content=[TextContent(type="text", text=e.to_json())], is_error=True)


def _tool(fn):
    """Wrap a tool body: results become JSON, SluiceErrors become JSON error payloads."""
    if inspect.iscoroutinefunction(fn):
        @functools.wraps(fn)
        async def wrapper(*args, **kwargs):
            try:
                return _ok(await fn(*args, **kwargs))
            except SluiceError as e:
                return _err(e)
    else:
        @functools.wraps(fn)
        def wrapper(*args, **kwargs):
            try:
                return _ok(fn(*args, **kwargs))
            except SluiceError as e:
                return _err(e)
    wrapper.__signature__ = inspect.signature(fn).replace(return_annotation=CallToolResult)
    wrapper.__annotations__ = {**fn.__annotations__, "return": CallToolResult}
    return wrapper


class _Server(MCPServer):
    async def call_tool(self, name, arguments, context=None):
        try:
            return await super().call_tool(name, arguments, context)
        except UnexpectedToolError:
            raise
        except ToolError as e:
            # Argument validation and unknown tools: report them in the same JSON shape.
            cause = e.__cause__
            if isinstance(cause, ValidationError):
                problems = "; ".join(f"{'.'.join(str(x) for x in er['loc'])}: {er['msg']}"
                                     for er in cause.errors())
                return _err(BadRequest(f"bad arguments for {name}: {problems}"))
            return _err(BadRequest(str(e)))


def build_server(store: Store) -> MCPServer:
    mcp = _Server("sluice", instructions=INSTRUCTIONS)

    def tool(fn):
        mcp.add_tool(_tool(fn), name=fn.__name__, description=inspect.getdoc(fn))
        return fn

    @tool
    def plans_list(include_adhoc: bool = False) -> Any:
        """List plans: [{id, title, rev, paused, counts}]. Ad-hoc fn_call plans are hidden
        unless include_adhoc is true."""
        return views.plans_list(store, include_adhoc)

    @tool
    def plan_create(plan: str, doc: dict[str, Any], reason: str, author: str = "mcp") -> Any:
        """Create plan `plan` from `doc` (the document without rev). Returns {rev}."""
        return {"rev": store.create(plan, doc, author, reason)}

    @tool
    def plan_get(plan: str, path: str | None = None) -> Any:
        """The current document (without rev) and its rev: {rev, doc}. `path` is an optional
        JSON Pointer into the document, e.g. /nodes/build."""
        cur = store.get(plan)
        doc = {k: v for k, v in cur.items() if k != "rev"}
        if path:
            try:
                doc = jsonpointer.resolve_pointer(doc, path)
            except jsonpointer.JsonPointerException as e:
                raise NotFound(f"{path}: {e}") from e
        return {"rev": cur["rev"], "doc": doc}

    @tool
    def plan_patch(plan: str, rev: int, ops: list[dict[str, Any]], reason: str,
                   author: str = "mcp") -> Any:
        """Apply an RFC 6902 JSON Patch (against the document without rev) at `rev`.
        Returns {rev}. Stale rev: `conflict` with current_rev. Invalid result: `invalid`."""
        return {"rev": store.patch(plan, rev, ops, author, reason)}

    @tool
    def plan_validate(plan: str, doc: dict[str, Any]) -> Any:
        """Validate a candidate document for `plan` without writing: {ok, errors}."""
        errs = store.validate({"id": plan, **doc}, plan if plan in store.plan_ids() else None)
        return {"ok": not errs, "errors": errs}

    @tool
    def plan_history(plan: str, since_rev: int | None = None) -> Any:
        """The edit log entries after `since_rev`: [{rev, at, author, reason, ops}]."""
        return store.history(plan, since_rev)

    @tool
    def plan_at(plan: str, rev: int) -> Any:
        """The document as it was at `rev`: {doc}."""
        return {"doc": store.plan_at(plan, rev)}

    @tool
    def plan_revert(plan: str, rev: int, to_rev: int, reason: str, author: str = "mcp") -> Any:
        """Make the document equal to its state at `to_rev`, as a normal edit. Returns {rev}."""
        return {"rev": store.revert(plan, rev, to_rev, author, reason)}

    @tool
    def node_add(plan: str, rev: int, id: str, node: dict[str, Any], reason: str,
                 author: str = "mcp") -> Any:
        """Add one node (sugar for an add op). Returns {rev}."""
        if id in store.get(plan).get("nodes", {}):
            raise InvalidPlan([f"nodes.{id}: a node with this id already exists"])
        if "/" in id or "~" in id:
            raise InvalidPlan([f"nodes.{id}: node ids match [a-z0-9][a-z0-9_-]*"])
        ops = [{"op": "add", "path": f"/nodes/{id}", "value": node}]
        return {"rev": store.patch(plan, rev, ops, author, reason)}

    @tool
    def status(plan: str) -> Any:
        """{rev, state_rev, counts, nodes: [{id, fn, status, attempt, started, finished,
        error?}], ready: [ids the next tick would start]}."""
        return views.status(store, plan)

    @tool
    def node_get(plan: str, node: str) -> Any:
        """{definition, expanded_ids, state, output, stderr_tail} of one node."""
        return views.node_get(store, plan, node)

    @tool
    def node_retry(plan: str, node: str, reason: str, author: str = "mcp") -> Any:
        """failed/cancelled/skipped -> pending with attempt + 1; closes its open inbox item.
        On a composite, applies to each such inner node. Returns {ok}."""
        return L.node_action(store, plan, node, "retry", reason, author)

    @tool
    def node_skip(plan: str, node: str, reason: str, author: str = "mcp") -> Any:
        """pending/waiting/failed -> skipped. Returns {ok}."""
        return L.node_action(store, plan, node, "skip", reason, author)

    @tool
    def node_cancel(plan: str, node: str, reason: str, author: str = "mcp") -> Any:
        """Kill it if running; -> cancelled. Returns {ok}."""
        return L.node_action(store, plan, node, "cancel", reason, author)

    @tool
    def dry_run(plan: str) -> Any:
        """What the next tick would start, skip, or leave blocked, with reasons:
        {start: [{id, reason}], skip: [...], blocked: [...]}."""
        return views.dry_run(store, plan)

    @tool
    def events_tail(plan: str, since_seq: int | None = None, limit: int | None = 100) -> Any:
        """Events after `since_seq` (oldest first), or the last `limit` events."""
        return store.events(plan, since_seq, limit)

    @tool
    def inbox_list(plan: str | None = None, open_only: bool = True) -> Any:
        """Inbox items (failures and core.ask questions), optionally for one plan."""
        return store.inbox_list(plan, open_only)

    @tool
    def inbox_resolve(item: str, resolution: dict[str, Any], author: str = "mcp") -> Any:
        """Resolve an item: {"answer": ...} for core.ask, {"action": "retry"|"skip"|"ack"}
        for a failure. Returns {ok}."""
        return L.inbox_resolve(store, item, resolution, author)

    @tool
    def fn_list() -> Any:
        """Every loaded fn: [{name, description, version, in, out, composite, effects}]."""
        return [store.registry.fns[n].summary() for n in store.registry.names()]

    @tool
    def fn_get(name: str) -> Any:
        """The full fn.json of one fn."""
        fn = store.registry.get(name)
        if fn is None:
            raise NotFound(f"no fn {name!r}")
        return fn.raw

    @tool
    async def fn_call(name: str, input: dict[str, Any], wait: float = 0,
                      author: str = "mcp") -> Any:
        """Run one fn as an ad-hoc one-node plan with the normal machinery (retries, timeout,
        logs, events, inbox on failure). With wait > 0, waits up to that many seconds and
        returns {call, status, output?, error?}; otherwise {call, status: "pending"}."""
        call = await anyio.to_thread.run_sync(views.fn_call, store, name, input, author)
        if wait <= 0:
            return {"call": call, "status": "pending"}
        deadline = anyio.current_time() + wait
        while True:
            res = await anyio.to_thread.run_sync(views.fn_result, store, call)
            if views.call_settled(res) or anyio.current_time() >= deadline:
                res.pop("stderr_tail", None)
                return res
            await anyio.sleep(0.1)

    @tool
    def fn_result(call: str) -> Any:
        """The result of an ad-hoc call: {call, status, output?, error?, stderr_tail?}."""
        return views.fn_result(store, call)

    return mcp
