# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

One person: Sam, who supervises many AI agents at once. Other Claude sessions (orchestrators)
write plans over MCP; sluice runs them. Sam opens the dashboard between other work, often on a
second screen or a phone, to answer: what is running, what is stuck, what needs me, what did each
agent produce, and is the work moving. Agents read the same state through MCP tools, not the
dashboard.

## Product Purpose

sluice runs typed plans of functions for AI-agent work. A plan is mostly agent blocks: a prompt
plus typed inputs and outputs, run by an engine (Claude, Devin, Codex). The runner handles
dependencies, gates, units, fan-out (scatter), resources, staleness when inputs change, and human
decisions via the Inbox.
The dashboard succeeds when Sam can tell within seconds whether anything needs him and whether the
work is moving, and can drill into any block to see what it was told, what it is doing now and
what it produced.

## Positioning

A plan is a small typed graph of units of work you would hand to a person, not a CI pipeline of
hundreds of jobs. The dashboard shows that graph as the work itself: each block's prompt, its live
progress line and its typed outputs, with the one human decision point (the Inbox) always in view.

## Operating Context

- One Rust binary, installed as a release under `~/.local/share/sluice` and upgraded in place by
  `scripts/deploy`. A per-home coordinator owns the state; `sluice serve` serves the dashboard,
  MCP and the HTTP tools on localhost; `sluice loop` holds the scheduler; every step runs in its
  own systemd user unit and survives coordinator restarts and upgrades.
- State lives in one SQLite database per home (`sluice.db`: projects, plans, steps, runs, calls,
  messages, the log); each run's own files stay under `runs/`.
- Agents post to per-step threads (`step-<id>`) and to `orchestrator`; questions to `owner` are
  the inbox, and Sam answers there.
- Plans are light: a handful to a few dozen blocks, grouped into units by `unit:` tags.

## Capabilities and Constraints

- Mostly read-only. The writes are answering and replying to messages, pausing, retrying (with
  feedback) and cancelling a step, and the project settings (name, description, icon,
  resources, pause, archive, delete); each goes through the same commands as the MCP tools.
- Server-rendered HTML; every page works without JavaScript; Datastar SSE streams patch only the
  parts that changed. Every script is served by sluice itself (vendored); only the fonts come
  from cdn.jsdelivr.net.
- Every value is untrusted and HTML-escaped.
- Statuses: pending, running, succeeded, failed, stale, skipped; a succeeded step may be manual
  (value set by hand). Scatter steps report done/total.
- Agent blocks are typed: a step declares its own `outputs` and binds extra named inputs.
- Terminology: project, plan, unit, step (block), fn, engine, message (question or note),
  thread, log record, rev.

## Brand Commitments

- The logo (the owner's SVG) is the brand: its navy, cream and blue are the dashboard's
  palette, the blue its one accent and the colour of running.
- The dashboard's one coral badge: coral appears only in the logo and the count of open
  questions to the owner. A failure is never coral.
- Plain, factual copy in sluice's own terms; no AI mention in teammate-visible text.

## Evidence on Hand

- `crates/sluice-web/examples/dashboard_fixture.rs` serves a seeded scratch home (tagged
  units, queued work, every gate relation, open questions) for rendering and screenshots.
- No customers, metrics or claims beyond the repository.

## Product Principles

1. One place asks the person: the inbox, with the one coral badge. Nothing else calls for them.
2. The plan is the page: show the graph of work, not a table about it.
3. Every block explains itself: prompt in, progress now, typed outputs out.
4. Calm when healthy: colour marks state, never decoration; coral is reserved.
5. Works as plain HTML first; live updates are an enhancement.

## Accessibility & Inclusion

Status is never colour-only (a text label and shape carry it). Keyboard reachable, visible focus,
light and dark themes, usable at 390px width.
