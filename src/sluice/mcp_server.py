"""The MCP server (SPEC §8): tools, docs for agents, and the dashboard's routes.

Built on the official `mcp` SDK (v2 calls FastMCP `MCPServer`); `sluice serve` serves it over
streamable HTTP next to the runner, and `sluice tool` calls the same tools in-process.
"""

import functools
import inspect
import json
import keyword
import os
import threading
from pathlib import Path
from typing import Annotated, Any, Literal

import anyio
from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.exceptions import ToolError, UnexpectedToolError
from mcp.types import CallToolResult, TextContent
from pydantic import Field, ValidationError

from . import calls, runner, views
from . import drain as drain_mod
from . import log as L
from . import me as me_mod
from . import query as query_mod
from . import verify as verify_mod
from . import watch as watch_mod
from .dashboard import Dashboard
from .errors import BadRequest, NotFound, SluiceError
from .store import Store

AUTHOR = "mcp"  # who a tool call acts for when nothing names anyone (`sluice tool`: "cli")
DOCS = Path(__file__).resolve().parent / "docs"
WAIT_CAP = 3600  # the most a tool waits: log_wait's timeout, fn_call's wait (seconds)


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


def author_of(given: str | None, client: str | None = None, fallback: str = AUTHOR) -> str:
    """Who a tool call acts for, the first there: the explicit `author`; SLUICE_AUTHOR;
    `step:<id>` when SLUICE_STEP is set (an agent inside a step calling `sluice tool`); the
    MCP client's name (its `initialize` clientInfo); else `fallback`."""
    for name in (given, os.environ.get("SLUICE_AUTHOR")):
        if isinstance(name, str) and name.strip():
            return name.strip()
    if step := os.environ.get("SLUICE_STEP", "").strip():
        return f"step:{step}"
    return client or fallback


def _client(context: Any) -> str | None:
    """The MCP client's name from the session's `initialize`; None in-process."""
    try:
        info = context.session.client_params.client_info
    except (AttributeError, ValueError):  # no session or no initialize (sluice tool)
        return None
    return info.name if info is not None and info.name else None


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
    params: dict[str, list[str]]  # each tool's argument names, as callers spell them
    author: str  # the last rung of author_of: "mcp", or "cli" for `sluice tool`

    async def call_tool(self, name, arguments, context=None):
        known = self.params.get(name)
        unknown = [k for k in arguments or {} if known is not None and k not in known]
        if unknown:  # every tool refuses what it does not take, rather than drop it
            return _error(BadRequest(
                f"{name} takes no argument {', '.join(map(repr, unknown))}; its arguments "
                f"are {', '.join(known) or 'none'}"))
        if known is not None and "author" in known:  # every write names who made it
            arguments = {**(arguments or {}), "author": author_of(
                (arguments or {}).get("author"), _client(context), self.author)}
        if name == "inbox_post":  # an item names who asks by the same rule
            arguments = {**(arguments or {}), "from": author_of(
                (arguments or {}).get("from"), _client(context), self.author)}
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
                 interval: float = 1.0, author: str = AUTHOR) -> MCPServer:
    """The MCP server with the dashboard's routes; `stop` ends the dashboard's streams and
    `interval` is how often they poll for changes (seconds); `author` is who a write acts for
    when nothing else says (author_of)."""
    mcp = _Server("sluice", instructions=doc_page("instructions"))
    mcp.params, mcp.author = {}, author

    def tool(fn):
        mcp.add_tool(_tool(fn), name=fn.__name__, description=inspect.getdoc(fn))
        mcp.params[fn.__name__] = [p.rstrip("_") if keyword.iskeyword(p.rstrip("_")) else p
                                   for p in inspect.signature(fn).parameters]
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
        """List projects: [{name, description, rev, counts, archived, paused, icon?}]; counts
        maps step status -> number of steps in the project's plan; icon, when the project has
        one, is {"kind": "image", "type": <content type>} or {"kind": "text", "text": <text>}.
        """
        return store.projects()

    @tool
    def project_create(name: str, description: str = "", icon: str | None = None,
                       author: str | None = None) -> Any:
        """Create a project with an empty plan (rev 1). Returns {name}.

        Args:
            name: lowercase letters, digits, - and _ (starting with a letter or digit).
            description: what the project is for; put any context an orchestrator needs here.
            icon: the project's icon: an absolute path to an image file (SVG, PNG, WebP, JPEG
                or GIF, at most 256 KB, copied into the project's row), or a short
                text icon (an emoji; at most 16 characters).
            author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return store.create_project(name, description, author, icon=icon)

    @tool
    def project_update(name: str, description: str | None = None,
                       archived: bool | None = None, paused: bool | None = None,
                       icon: str | None = None, reason: str = "",
                       author: str | None = None) -> Any:
        """Replace a project's description, archive it, pause it and/or set its icon; each
        change is a record in the project's log (project.pause, project.archive,
        project.update) with the author and reason. Returns {name}.

        Args:
            name: the project.
            description: the new description (leave out to keep it).
            archived: true to archive (the dashboard lists it apart; nothing stops or
                changes), false to bring it back.
            paused: true to pause the whole project: no step of it starts, however ready,
                until false again; running steps finish.
            icon: an image path or a short text icon, as in project_create; "" removes the
                icon (leave out to keep it). A project has at most one of the two kinds.
            reason: why, in the records (say why a project is paused).
            author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return store.update_project(name, description, archived, paused, icon=icon,
                                    author=author, reason=reason)

    @tool
    def project_delete(name: str) -> Any:
        """Delete a project and everything it holds: plan, state, log, inbox and runs. Cannot
        be undone. Refused unless it is archived first, none of its steps is running and no
        non-direct call on it is pending or running. Returns {deleted}. The name can be used
        for a new project once the old files are gone (project_create says when it is not yet).

        Args:
            name: the project.
        """
        return store.delete_project(name)

    @tool
    def fn_list(project: str | None = None) -> Any:
        """List the functions a project sees (built-in, global, then the project's own; without
        a project: built-in and global): [{name, doc, inputs, outputs, scope, icon?}]. A function
        with a problem (bad fn.json, name collision) carries `error`; see verify.

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
                      wait: float = 0, direct: bool = False, author: str | None = None) -> Any:
        """Run one function outside the plan. Returns {call, status, outputs?, error?}; status
        is pending, running, succeeded or failed. Poll call_status(call) for a slow one; every
        status change is also a `call` record in the log (log_read).

        Args:
            name: the function to run.
            inputs: an object keyed by the function's input names; checked against its types
                before anything runs (an `invalid` error lists every mismatch with its path).
            project: run it in this project (its functions and .env); leave out for none.
            wait: how many seconds to wait for the result, a number such as 60 (default 0:
                return at once; capped at 3600).
            direct: run it here and now, to the end, instead of queueing it for the runner
                (for use without a runner, e.g. from the command line); ignores `wait`.
            author: who is calling (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        call = await anyio.to_thread.run_sync(calls.create, store, name, inputs, project,
                                              direct, author)
        store.notify()
        if direct:
            return await anyio.to_thread.run_sync(runner.run_call_direct, store, call, project)
        deadline = anyio.current_time() + min(wait, WAIT_CAP)
        while True:
            rec = await anyio.to_thread.run_sync(calls.latest, store, call, project)
            if rec["status"] in calls.DONE or anyio.current_time() >= deadline:
                return calls.result(rec)
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
                   author: str | None = None, start: bool = False) -> Any:
        """Edit a project's plan with RFC 6902 JSON Patch ops. A step it adds comes in
        paused (so a drafted plan starts nothing) unless start is true or the step sets
        `paused` itself; unpause with step_pause. Returns {rev}.

        Args:
            project: the project.
            rev: the revision you read; if the plan moved on you get `conflict` with
                current_rev, so re-read and retry.
            ops: JSON Patch operations against the plan without rev, e.g.
                [{"op": "add", "path": "/steps/x", "value": {...}}]. The result is validated;
                an `invalid` error lists every problem with its path.
            reason: why, recorded in the plan's history.
            author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
            start: let the steps it adds start as soon as they are ready.
        """
        return {"rev": store.patch(project, rev, ops, author, reason, start)}

    @tool
    def step_add(project: str, step: str, spec: dict[str, Any], reason: str = "",
                 start: bool = False, author: str | None = None) -> Any:
        """Add one step to a plan: plan_patch for a single step, at the current rev. It
        comes in paused unless start is true (or the spec sets `paused`). Returns {rev}.

        Args:
            project: the project.
            step: the new step's id.
            spec: the step, {run, in, scatter?, doc?, outputs?, paused?, after?, tags?}.
            reason: why, recorded in the plan's history.
            start: let it start as soon as it is ready.
            author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return {"rev": store.add_step(project, step, spec, author, reason, start)}

    @tool
    def recipe_list(project: str) -> Any:
        """List the recipes a project sees: [{name, doc, params, scope}] by name (scope global:
        SLUICE_HOME/recipes/, or project: the project's own recipes/, which wins on a name
        clash); a broken recipe file is listed as {name, scope, error}. params maps each param
        to its type, `unit` (always there) included. Read docs("plans") on recipes.

        Args:
            project: the project.
        """
        return store.recipes(project)

    @tool
    def unit_add(project: str, recipe: str, params: dict[str, Any], start: bool = False,
                 author: str | None = None, reason: str = "") -> Any:
        """Add one unit of work from a recipe: its steps with `{param}` filled in, each tagged
        `unit:<unit>`, in one plan edit at the current rev. They come in paused unless start
        is true. Refused (`bad_request`) when an id it would add is already in the plan;
        `invalid` lists every param or expansion problem. Returns {rev, steps}.

        Args:
            project: the project.
            recipe: the recipe's name (recipe_list).
            params: {unit: "<name of the unit, a valid step id>", <param>: value, ...}, each
                checked against the recipe's param types.
            start: let the new steps start as soon as they are ready.
            author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
            reason: why, recorded in the plan's history (default "add unit <unit> (recipe
                <recipe>)").
        """
        return store.unit_add(project, recipe, params, start, author, reason)

    @tool
    def step_update(project: str, step: str, changes: dict[str, Any], reason: str = "",
                    author: str | None = None) -> Any:
        """Change fields of one step: each key of `changes` replaces that field (`in` is
        replaced whole), null removes it. A running step only takes `paused`. Returns {rev}.

        Args:
            project: the project.
            step: the step id.
            changes: e.g. {"doc": "...", "in": {...}}.
            reason: why, recorded in the plan's history.
            author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return {"rev": store.update_step(project, step, changes, author, reason)}

    @tool
    def step_remove(project: str, steps: list[str] | str | None = None,
                    tags: list[str] | str | None = None, reason: str = "",
                    author: str | None = None) -> Any:
        """Remove steps from a plan in one edit, selected by ids and/or tags. Refused
        (`invalid`) while a step left or a plan output still reads one, or while one runs.
        Each one that finished keeps its outcome (the `outcomes` table, see query). Returns
        {rev, steps, outcomes}: outcomes is how many were kept.

        Args:
            project: the project.
            steps: step ids (one id is fine too).
            tags: every step carrying any of these tags.
            reason: why, recorded in the plan's history.
            author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return store.remove_steps(project, steps, tags, author, reason)

    @tool
    def step_pause(project: str, steps: list[str] | str | None = None,
                   tags: list[str] | str | None = None,
                   subtree: bool = False, paused: bool = True, reason: str = "",
                   author: str | None = None) -> Any:
        """Pause or unpause steps in one edit. A paused step does not start, however ready
        its inputs, until unpaused; a running one finishes (pausing never stops it: see
        step_cancel). Select by ids and/or tags; with subtree, also everything downstream
        (steps that read from or run after them, transitively), including those that become
        ready later. Returns {rev, steps}: the steps selected.

        Args:
            project: the project.
            steps: step ids (one id is fine too).
            tags: select every step carrying any of these tags.
            subtree: include everything downstream of the selected steps.
            paused: true to pause, false to let them start.
            reason: why; kept on each paused step (status shows it) and in the history.
            author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return store.pause_steps(project, steps, tags, subtree, paused, author, reason)

    @tool
    def step_cancel(project: str, steps: list[str] | str | None = None,
                    tags: list[str] | str | None = None, reason: str = "",
                    author: str | None = None) -> Any:
        """Stop running steps, selected by ids and/or tags: the runner kills their processes
        and fails each with `cancelled: <reason>`; step_retry runs them again. A pending
        core.external step (work done outside sluice) fails the same way at once. Refused
        unless every selected step is running or a pending core.external one. Returns {steps}.

        Args:
            project: the project.
            steps: step ids (one id is fine too).
            tags: every step carrying any of these tags.
            reason: why, in their errors and `step.cancel` log records.
            author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return {"steps": store.cancel_steps(project, steps, tags, author, reason)}

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
    def plan_set_input(project: str, name: str, value: Any, reason: str = "",
                       author: str | None = None) -> Any:
        """Set a declared plan input; steps reading it can then start. Changing it later makes
        the succeeded steps that read it (and their dependents) stale. Returns {ok}.

        Args:
            project: the project.
            name: the plan input's name.
            value: its value, checked against the input's type.
            reason: why, recorded in the plan's history.
            author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        store.set_input(project, name, value, author, reason)
        return {"ok": True}

    @tool
    def step_set_input(project: str, step: str, input: str, value: Any, reason: str = "",
                       rev: int | None = None, author: str | None = None) -> Any:
        """Pin one step input to a literal ({"default": value}); an edit. Returns {rev}.

        Args:
            project: the project.
            step: the step id.
            input: the step's input name.
            value: the literal, checked against the input's type.
            reason: why, recorded in the plan's history.
            rev: the revision you read (default: the current one).
            author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return {"rev": store.set_step_input(project, step, input, value, author, reason, rev)}

    @tool
    def step_set_output(project: str, step: str, outputs: dict[str, Any],
                        reason: str = "", force: bool = False,
                        author: str | None = None) -> Any:
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
            author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        store.set_output(project, step, outputs, author, reason, force)
        return {"ok": True}

    @tool
    def step_retry(project: str, steps: list[str] | str | None = None,
                   tags: list[str] | str | None = None, reason: str = "",
                   author: str | None = None) -> Any:
        """Set failed, stale or manually set steps back to pending so they run again,
        selected by ids and/or tags (refused, changing nothing, unless every one is failed,
        stale or manual). Their succeeded dependents turn stale if they produce a different
        result. Returns {steps}.

        Args:
            project: the project.
            steps: step ids (one id is fine too).
            tags: every step carrying any of these tags.
            reason: why, recorded in the plan's history.
            author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return {"steps": store.retry(project, steps, tags, author, reason)}

    @tool
    def step_submit(project: str, step: str, outputs: dict[str, Any],
                    run: str | None = None, author: str | None = None) -> Any:
        """Submit the outputs a running step declares (its `outputs`), from the agent doing
        the step's work. Checked against the declared outputs: every required one present,
        types fitting, no others; `invalid` lists every mismatch with its path, so fix them
        and submit again. Submitting again replaces what was sent. When the step's fn exits,
        these join its outputs; a required one never submitted fails the step. Returns
        {ok, run}.

        Args:
            project: the project.
            step: the running step.
            outputs: an object keyed by declared output name.
            run: the run id (SLUICE_RUN_ID); needed only when the step runs several times at
                once (scatter).
            author: who is submitting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return store.submit(project, step, outputs, run, author)

    def _log_args(project: str | None, kinds: list[str] | None, limit: int | None) -> None:
        if project is not None:
            store.project(project)
        errs = L.check_kinds(kinds)
        if errs:
            raise BadRequest("; ".join(errs))
        if limit is not None and limit < 1:
            raise BadRequest("limit: expected a positive int")

    @tool
    def log_read(project: str | None = None, since_seq: int | None = None,
                 kinds: list[str] | None = None, threads: list[str] | None = None,
                 limit: int = 200) -> Any:
        """Read the log: {records, last_seq}. Records are {seq, at, kind, ...} oldest first
        (seqs increase across the whole home, so one log's have gaps);
        kinds: plan.edit, plan.input, step.output, step.retry, step.status, step.submit,
        step.cancel, call, message, inbox.post, inbox.answer, inbox.close, inbox.adopt,
        run.adopt, run.orphan, project.pause, project.archive, project.update.

        Args:
            project: the project's log; leave out for the home log (calls without a project).
            since_seq: only records after this seq (pass the last_seq you got to continue).
                Without it: the last `limit` matching records.
            kinds: only these kinds; "step", "plan", "inbox", "run" or "project" match every
                kind under them.
            threads: only messages on these threads (and, without kinds, only messages).
            limit: at most this many records (default 200).
        """
        _log_args(project, kinds, limit)
        return L.read(store.home, project, since_seq, kinds, threads, limit)

    @tool
    async def log_wait(since_seq: int, project: str | None = None,
                       kinds: list[str] | None = None, threads: list[str] | None = None,
                       timeout: int = 300, limit: int = 200,
                       wake: Literal["any", "questions"] = "any") -> Any:
        """Wait for log records after since_seq: returns {records, last_seq} as soon as at least
        one matching record exists, or with no records once `timeout` seconds pass. Call it
        again with the last_seq it returned to keep watching.

        Args:
            since_seq: wait for records after this seq (0 for any; last_seq from log_read).
            project: the project's log; leave out for the home log.
            kinds: only these kinds (as in log_read).
            threads: only messages on these threads (as in log_read).
            timeout: seconds to wait at most (default 300; capped at 3600).
            limit: at most this many records (default 200).
            wake: "any" (default) or "questions": a note (a message posted with needs_reply
                false) does not end the wait; it comes back with the next record that does,
                or once `timeout` passes.
        """
        await anyio.to_thread.run_sync(_log_args, project, kinds, limit)
        res = await anyio.to_thread.run_sync(
            functools.partial(L.wait, store.home, project, since_seq, kinds, threads, wake,
                              min(max(0, timeout), WAIT_CAP), 0.25, limit))
        return {"records": res["records"] + res["held"], "last_seq": res["last_seq"]}

    @tool
    async def next(projects: list[str] | str, since_seq: int, me: str = "orchestrator",
                   timeout: float = 300, all: bool = False, settle: float = 20,
                   settle_max: float = 120) -> Any:
        """Wait for the records an orchestrator should act on across the projects, then
        return {records, notes, last_seq, timed_out}. Wakes at once on a step that failed,
        went stale or was skipped (inside a unit too); a message needing a reply, not from
        you, addressed to `me` or to nobody; an inbox post or answer. A unit (the steps
        tagged `unit:<name>`, else steps joined by handoffs or `after`) wakes once, when it
        settles — none of its steps running or pending and startable — never on its steps'
        own successes; its record carries `unit: {name, settled, steps: [{id, status,
        held?, outputs}]}` with the succeeded steps' outputs, on the failure itself when a
        failure settled it. A standalone step wakes on a success when its fn is open. After
        the first waking record it keeps collecting until `settle` seconds pass with no new
        one, or `settle_max` seconds after the first. `notes` are the notes (messages with
        needs_reply false) held on the way — read them before the records. Pass `last_seq`
        back as `since_seq` to continue; nothing is missed or repeated. The command-line
        form is `sluice next`.

        Args:
            projects: the projects to watch (one or several).
            since_seq: records after this seq (the last_seq you last got).
            me: your name; your own messages never wake it (default "orchestrator").
            timeout: seconds to wait at most for the first waking record (default 300;
                capped at 3600); on a timeout records is empty and timed_out is true.
            all: every record wakes it (default false).
            settle: seconds with no new waking record that end the batch (default 20; 0
                returns at the first).
            settle_max: seconds after the first waking record that end the batch at the
                latest (default 120; capped at 3600).
        """
        names = [projects] if isinstance(projects, str) else list(projects)
        if not names:
            raise BadRequest("projects: expected at least one project")
        for p in names:
            store.project(p)
        return await anyio.to_thread.run_sync(functools.partial(
            watch_mod.next_up, store, names, since_seq, me,
            min(max(0, timeout), WAIT_CAP), all, settle=max(0, settle),
            settle_max=min(max(0, settle_max), WAIT_CAP)))

    @tool
    def drain(projects: list[str] | str | None = None, author: str | None = None) -> Any:
        """Pause the projects (default: every project not archived) that are not already
        paused, recording which ones in SQLite so `release` lets exactly those go
        again. Returns {paused, pending}: `pending` is the running steps and live
        non-direct calls still to finish — the CLI's `sluice drain` waits for them.

        Args:
            projects: the projects to drain; leave out for every project not archived.
            author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        names = drain_mod.targets(store, [projects] if isinstance(projects, str)
                                     else projects)
        return {"paused": drain_mod.pause(store, names, author),
                "pending": drain_mod.pending(store, names)}

    @tool
    def release(author: str | None = None) -> Any:
        """Unpause exactly the projects the maintenance ledger lists — what `sluice drain --release`
        does — and clear ownership. Projects paused otherwise stay paused. Returns {released}.

        Args:
            author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return {"released": drain_mod.release(store, author)}

    @tool
    def step_context(project: str, step: str) -> Any:
        """Where a step stands, for the agent doing it — the same as `sluice me` inside the
        step: its fn, doc, status and running time, inputs, the status and short outputs of
        every step it reads or runs after, the unanswered messages on its thread
        (step-<id>), the outputs it must submit with the exact step_submit command, and the
        thread with the command to ask a question.

        Args:
            project: the project.
            step: the step id.
        """
        return me_mod.context(store, project, step)

    def query(sql: str, params: list | None = None, limit: int = 200) -> Any:
        return query_mod.run(store.home, sql, params, limit)

    query.__doc__ = query_mod.doc()
    tool(query)

    @tool
    def verify(project: str | None = None) -> Any:
        """Check functions (fn.json shape and types, name collisions), .env files, each
        project's plan and its state. Returns {ok, problems: [{where, message}], warnings?}
        (warnings: directories under projects/ of no project); changes nothing.

        Args:
            project: check this project (and the built-in and global functions it sees);
                leave out to check everything.
        """
        return verify_mod.verify(store, project)

    @tool
    def plan_view(project: str, format: Literal["mermaid", "html"] = "mermaid",
                  all: bool = False) -> Any:
        """Draw the plan with each step's status: a Mermaid flowchart or a standalone HTML page.
        Done units (independent pieces of work whose every step succeeded or was skipped) are
        left out, with one line saying how many, unless all is true.

        Args:
            project: the project.
            format: "mermaid" (default) or "html".
            all: include the done units too.
        """
        return views.render(store, project, format, all)

    @tool
    def status(project: str, steps: list[str] | str | None = None,
               tags: list[str] | str | None = None, brief: bool = False,
               all: bool = False) -> Any:
        """Return {rev, paused, inputs, outputs, steps: [{id, run, status, started, finished,
        outputs?, error?, doc?, paused?, tags?, after?, when?, skipped?, waiting?, manual}],
        done_units?}. Without steps or tags, the done units (independent pieces of work,
        steps joined by any edge, whose every step succeeded or was skipped) are left out and
        counted in done_units {units, steps}, unless all is true.
        inputs and outputs map names to values (null if unset). A step's status is pending,
        running, succeeded, failed, stale (its result was computed from inputs that have
        changed since; it waits for step_retry or step_set_output, and so do the steps reading
        it) or skipped (its `when` was false, or it reads a skipped step; `skipped` says
        which). `waiting`, on a pending step, says why it has not started.

        Args:
            project: the project.
            steps: only these steps (ids).
            tags: only steps carrying any of these tags (with steps: either).
            brief: cut every string value over 200 characters in inputs and outputs (an
                agent's `final`, a report) to its start and how much more there is; step_get
                or a status without brief has them whole.
            all: include the done units too (steps or tags always return what they select).
        """
        return store.status(project, steps, tags, brief, all)

    @tool
    def plan_prune(project: str, older_than_hours: float = 0, author: str | None = None,
                   reason: str = "") -> Any:
        """Remove every step of every done unit (an independent piece of work whose every
        step succeeded or was skipped) whose last step finished at least older_than_hours
        ago, in one edit; plan_history keeps them, and each step's outcome stays in the
        `outcomes` table (see query). A unit a plan output reads stays. Returns {rev, units,
        steps, outcomes}: how many units, which step ids went and how many outcomes were kept
        (no edit when none).

        Args:
            project: the project.
            older_than_hours: only units finished at least this many hours ago (default 0:
                every done unit).
            author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
            reason: why, recorded in the plan's history (default "prune <n> done units").
        """
        return store.prune(project, older_than_hours, author, reason)

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
            from: who is asking (a step id, an agent name; default: SLUICE_AUTHOR,
                step:<SLUICE_STEP>, the MCP client's name, else "mcp"; "cli" from
                `sluice tool`).
        """
        # asked from inside a step, the item keeps its run: it says when nobody waits for it
        step, run = os.environ.get("SLUICE_STEP", "").strip(), os.environ.get("SLUICE_RUN_ID")
        run = run if step and from_ in (step, f"step:{step}") else None
        return {"id": store.inbox_post(project, title, body, ui, input, from_, run)["id"]}

    @tool
    def inbox_list(project: str | None = None, status: str = "open") -> Any:
        """List inbox items, oldest first: [{project, id, title, body?, ui?, input?, from?,
        run?, seq?, status, created, answer?, answered?, closed?, reason?, waiting?,
        stopped?}]. `seq`: on an item from sluice, the log record it is about. An answer is
        {action, params?, values?, text?}. An open item a step (or a call) asked
        carries `waiting`: false once nothing waits for its answer, with `stopped` saying why
        ("build is failed"); retrying the step takes the item (and an answer given
        meanwhile) up again.

        Args:
            project: only this project's items; leave out for every project.
            status: open (default), answered, closed or all.
        """
        return store.inbox(project, status)

    @tool
    def inbox_answer(project: str, id: str, answer: dict[str, Any],
                     author: str | None = None) -> Any:
        """Answer an open inbox item, as the person would from the dashboard. An item that is
        already answered or closed is refused (`conflict` with its `status`). When the item
        names a plan input, the answer sets it first; a value that does not fit is refused
        (`invalid`) and the item stays open. Returns the answered item.

        Args:
            project: the project.
            id: the item id, e.g. "i3".
            answer: {action: string, params?: object, values?: object, text?: string}.
            author: who is answering (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return store.inbox_answer(project, id, answer, author)

    @tool
    def inbox_close(project: str, id: str, reason: str | None = None,
                    author: str | None = None) -> Any:
        """Withdraw an open inbox item you posted (you no longer need the answer). Refused
        (`conflict`) once it is answered or closed. Returns the closed item.

        Args:
            project: the project.
            id: the item id.
            reason: why, shown with the item.
            author: who is closing it (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP
                client's name, else "mcp"; "cli" from `sluice tool`).
        """
        return store.inbox_close(project, id, reason, author)

    Dashboard(store, stop, interval).add_routes(mcp)
    return mcp
