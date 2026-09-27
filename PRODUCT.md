# Product

<!-- impeccable:product-schema 1 -->

<!-- Written from the orchestrator's brief for the dashboard redesign (no interview round was
possible in that session). Facts marked (inferred) were read from the brief or the repository,
not confirmed in conversation. -->

## Platform

web

## Users

One person: Sam, who supervises many AI agents at once. Other Claude sessions (orchestrators)
write plans over MCP; sluice runs them. Sam opens the dashboard between other work, often on a
second screen or a phone, to answer: what is running, what is stuck, what needs me, what did each
agent produce, and is the work moving. Agents read the same state through MCP tools, not the
dashboard. (inferred: second screen and phone as the usual scenes)

## Product Purpose

sluice runs typed plans of functions for AI-agent work. A plan is mostly agent blocks: a prompt
plus typed inputs and outputs, run by an engine (Claude, Devin, Codex). The runner handles
dependencies, fan-out (scatter), staleness when inputs change, and human decisions via an Inbox.
The dashboard succeeds when Sam can tell within seconds whether anything needs him and whether the
work is moving, and can drill into any block to see what it was told, what it is doing now and
what it produced.

## Positioning

A plan is a small typed graph of units of work you would hand to a person, not a CI pipeline of
hundreds of jobs. The dashboard shows that graph as the work itself: each block's prompt, its live
progress line and its typed outputs, with the one human decision point (the Inbox) always in view.

## Operating Context

- `sluice serve` serves the dashboard and MCP on localhost; `sluice loop` may run the runner apart.
- State lives in files under SLUICE_HOME (plan.json, state.json, log.jsonl, inbox.json, runs/).
- Agents post to per-step threads (`step-<id>`) and to `orchestrator`; the orchestrator agent
  and Sam answer.
- Plans are light: a handful to a few dozen blocks.

## Capabilities and Constraints

- Read-only except answering inbox items (the one write, shared with the MCP tool).
- Server-rendered HTML; every page works without JavaScript; Datastar SSE streams patch only the
  parts that changed. External assets only from cdn.jsdelivr.net.
- Every value is untrusted and HTML-escaped.
- Statuses: pending, running, succeeded, failed, stale; a succeeded step may be manual (value set
  by hand). Scatter steps report done/total.
- Typed agent blocks are arriving: a step may declare its own `outputs` types and bind extra
  named inputs.
- Terminology: project, plan, step (block), fn, engine, inbox item, thread, log record, rev.

## Brand Commitments

- The dashboard's one red badge: only the count of open inbox items is red.
- Plain, factual copy in sluice's own terms; no AI mention in teammate-visible text.

## Evidence on Hand

- A real sample project (`tetris`: 21 steps, agent lanes, a failed step, a thread message,
  an answered inbox item) under the live SLUICE_HOME, copied for rendering.
- No customers, metrics or claims beyond the repository.

## Product Principles

1. One place asks the person: the inbox, with the one red badge. Nothing else calls for them.
2. The plan is the page: show the graph of work, not a table about it.
3. Every block explains itself: prompt in, progress now, typed outputs out.
4. Calm when healthy: colour marks state, never decoration; red is reserved.
5. Works as plain HTML first; live updates are an enhancement.

## Accessibility & Inclusion

Status is never colour-only (a text label and shape carry it). Keyboard reachable, visible focus,
light and dark themes, usable at 390px width.
