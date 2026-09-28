"""Agent sessions that run an engine's real interactive TUI in a private tmux server, supervised
until the step is done (supervisor.py), with one adapter per engine (claude.py)."""

from sluice.fn import with_step_notes

from .claude import Claude
from .codex import Codex
from .supervisor import ThreadFeed, required_outputs, supervise, thread_note


def task_text(text, inp, ctx, listen, delivery="pasted"):
    """The task: `text` with the step's inputs and outputs sections (sluice.fn.with_step_notes)
    and, unless listen is false, a step-thread note saying messages arrive in the session."""
    task = with_step_notes(text, inp, ctx, False)
    if ctx.project and ctx.step and listen is not False:
        task += "\n\n" + thread_note(ctx, delivery)
    return task


def run_claude(text, inp, ctx, cwd):
    """Run `text` as a step's task in a supervised Claude session (Opus); `inp` may carry
    `session` (resume) and `listen`. Returns {"final", "session", "cost_usd"}."""
    listen = inp.get("listen")
    feed = ThreadFeed(ctx) if ctx.project and ctx.step and listen is not False else None
    return supervise(Claude(), task_text(text, inp, ctx, listen), cwd, ctx.run_dir,
                     required=required_outputs(ctx), session=inp.get("session"), feed=feed,
                     attempt=ctx.attempt)


def run_codex(text, inp, ctx, cwd):
    """Run one supervised Codex app-server thread and return its final message and id."""
    listen = inp.get("listen")
    feed = ThreadFeed(ctx) if ctx.project and ctx.step and listen is not False else None
    return supervise(Codex(inp.get("model") or "sol", inp.get("effort")),
                     task_text(text, inp, ctx, listen, "delivered"), cwd, ctx.run_dir,
                     required=required_outputs(ctx), session=inp.get("session"), feed=feed,
                     attempt=ctx.attempt)
