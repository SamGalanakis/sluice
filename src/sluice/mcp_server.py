"""The MCP server (SPEC §8): tools, docs for agents, and the dashboard's routes.

Built on the official `mcp` SDK (v2 calls FastMCP `MCPServer`); `sluice serve` serves it over
streamable HTTP next to the runner, and `sluice tool` calls the same tools in-process.
"""

import functools
import inspect
import json
import keyword
import threading
from pathlib import Path
from typing import Annotated, Any, Literal

import anyio
from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.exceptions import ToolError, UnexpectedToolError
from mcp.types import CallToolResult, TextContent
from pydantic import Field, ValidationError

from . import calls, runner, views
from . import log as L
from . import verify as verify_mod
from .dashboard import Dashboard
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


def _kwargs(kwargs: dict[str, Any]) -> dict[str, Any]:
    """An argument named after a Python keyword (`from`) reaches its parameter (`from_`)."""
    return {(k + "_" if keyword.iskeyword(k) else k): v for k, v in kwargs.items()}


def _tool(fn):
    """Wrap a tool body so results become JSON and SluiceErrors become JSON error payloads."""
    if inspect.iscoroutinefunction(fn):
        @functools.wraps(fn)
        async def wrapper(*args, **kwargs):
            try:
                return _result(await fn(*args, **_kwargs(kwargs)))
            except SluiceError as e:
                return _error(e)
    else:
        @functools.wraps(fn)
        def wrapper(*args, **kwargs):
            try:
                return _result(fn(*args, **_kwargs(kwargs)))
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


def build_server(store: Store, stop: threading.Event | None = None,
                 interval: float = 1.0) -> MCPServer:
    """The MCP server with the dashboard's routes; `stop` ends the dashboard's streams and
    `interval` is how often they poll for changes (seconds)."""
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
        is pending, running, succeeded or failed. Poll call_status(call) for a slow one; every
        status change is also a `call` record in the log (log_read).

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
        """Set a declared plan input; steps reading it can then start. Changing it later makes
        the succeeded steps that read it (and their dependents) stale. Returns {ok}.

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
                        reason: str = "", force: bool = False) -> Any:
        """Mark a step succeeded with outputs you supply (manual: true); it is not run unless
        retried. Refused (`invalid`, naming them) while a step it reads has not succeeded or a
        plan input it reads has no value, unless force. Returns {ok}.

        Args:
            project: the project.
            step: a step that is not running.
            outputs: every output of its function, type-checked (arrays for a scattered step).
            reason: why, recorded in the plan's history.
            force: set it anyway although what it reads is not ready (e.g. a broken
                upstream); the step then turns stale once those values are all there.
        """
        store.set_output(project, step, outputs, AUTHOR, reason, force)
        return {"ok": True}

    @tool
    def step_retry(project: str, step: str, reason: str = "") -> Any:
        """Set a failed, stale or manually set step back to pending so it runs again. Its
        succeeded dependents turn stale if it produces a different result. Returns {ok}.

        Args:
            project: the project.
            step: the step id.
            reason: why, recorded in the plan's history.
        """
        store.retry(project, step, AUTHOR, reason)
        return {"ok": True}

    def _log_args(project: str | None, kinds: list[str] | None, limit: int | None) -> Any:
        if project is not None:
            store.project(project)
        errs = L.check_kinds(kinds)
        if errs:
            raise BadRequest("; ".join(errs))
        if limit is not None and limit < 1:
            raise BadRequest("limit: expected a positive int")
        return store.log_dir(project)

    @tool
    def log_read(project: str | None = None, since_seq: int | None = None,
                 kinds: list[str] | None = None, threads: list[str] | None = None,
                 limit: int = 200) -> Any:
        """Read the log: {records, last_seq}. Records are {seq, at, kind, ...} oldest first;
        kinds: plan.edit, plan.input, step.output, step.retry, step.status, call, message,
        inbox.post, inbox.answer, inbox.close.

        Args:
            project: the project's log; leave out for the home log (calls without a project).
            since_seq: only records after this seq (pass the last_seq you got to continue).
                Without it: the last `limit` matching records.
            kinds: only these kinds; "step", "plan" or "inbox" match every kind under them.
            threads: only messages on these threads (and, without kinds, only messages).
            limit: at most this many records (default 200).
        """
        d = _log_args(project, kinds, limit)
        return L.read(d, since_seq, kinds, threads, limit)

    @tool
    async def log_wait(since_seq: int, project: str | None = None,
                       kinds: list[str] | None = None, threads: list[str] | None = None,
                       timeout: int = 300, limit: int = 200) -> Any:
        """Wait for log records after since_seq: returns {records, last_seq} as soon as at least
        one matching record exists, or with no records once `timeout` seconds pass. Call it
        again with the last_seq it returned to keep watching.

        Args:
            since_seq: wait for records after this seq (0 for any; last_seq from log_read).
            project: the project's log; leave out for the home log.
            kinds: only these kinds (as in log_read).
            threads: only messages on these threads (as in log_read).
            timeout: seconds to wait at most (default 300).
            limit: at most this many records (default 200).
        """
        d = _log_args(project, kinds, limit)
        deadline = anyio.current_time() + max(0, timeout)
        while True:
            res = await anyio.to_thread.run_sync(L.read, d, since_seq, kinds, threads, limit)
            if res["records"] or anyio.current_time() >= deadline:
                return res
            await anyio.sleep(min(0.25, max(0.0, deadline - anyio.current_time())))

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
        A step's status is pending, running, succeeded, failed or stale (its result was
        computed from inputs that have changed since; it waits for step_retry or
        step_set_output, and so do the steps reading it).

        Args:
            project: the project.
        """
        return store.status(project)

    @tool
    def inbox_post(project: str, title: str, body: str | None = None, ui: str | None = None,
                   input: str | None = None,
                   from_: Annotated[str | None, Field(alias="from")] = None) -> Any:
        """Ask a person something: post an open item to the project's inbox (the dashboard's
        Inbox shows it). Returns {id}. Wait for the answer with log_wait(project, since_seq,
        kinds=["inbox"]), or read it with inbox_list. Read docs("inbox") first.

        Args:
            project: the project.
            title: the question, one line.
            body: more context, as markdown.
            ui: an OpenUI Lang program with buttons or a form for the answer (docs("inbox")
                lists the components); without it the person gets a text box.
            input: a declared plan input the answer sets (its value: values.value, else
                params.value, else text), type-checked like plan_set_input.
            from: who is asking (a step id, an agent name).
        """
        return {"id": store.inbox_post(project, title, body, ui, input, from_)["id"]}

    @tool
    def inbox_list(project: str | None = None, status: str = "open") -> Any:
        """List inbox items, oldest first: [{project, id, title, body?, ui?, input?, from?,
        status, created, answer?, answered?, closed?, reason?}]. An answer is {action,
        params?, values?, text?}.

        Args:
            project: only this project's items; leave out for every project.
            status: open (default), answered, closed or all.
        """
        return store.inbox(project, status)

    @tool
    def inbox_answer(project: str, id: str, answer: dict[str, Any]) -> Any:
        """Answer an open inbox item, as the person would from the dashboard. An item that is
        already answered or closed is refused (`conflict` with its `status`). When the item
        names a plan input, the answer sets it first; a value that does not fit is refused
        (`invalid`) and the item stays open. Returns the answered item.

        Args:
            project: the project.
            id: the item id, e.g. "i3".
            answer: {action: string, params?: object, values?: object, text?: string}.
        """
        return store.inbox_answer(project, id, answer, AUTHOR)

    @tool
    def inbox_close(project: str, id: str, reason: str | None = None) -> Any:
        """Withdraw an open inbox item you posted (you no longer need the answer). Refused
        (`conflict`) once it is answered or closed. Returns the closed item.

        Args:
            project: the project.
            id: the item id.
            reason: why, shown with the item.
        """
        return store.inbox_close(project, id, reason, AUTHOR)

    Dashboard(store, stop, interval).add_routes(mcp)
    return mcp
