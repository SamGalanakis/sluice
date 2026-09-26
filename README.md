# sluice

Orchestrators (agents or humans) edit a **plan**: a graph of typed function calls. A **runner**
executes it: it starts a step once everything it reads is available and records its outputs or
its failure. An orchestrator decides what happens after a failure.

- Work lives in **projects** under `$SLUICE_HOME/projects/<name>/`: each has one plan, its own
  functions (`fns/`) and secrets (`.env`, layered over `$SLUICE_HOME/.env`).
- A **function** is `fn.json` (typed `inputs` and `outputs`) plus a `main.py` run with `uv`.
  Scopes: built-in (`src/sluice/fns`), global (`$SLUICE_HOME/fns` and `config.fn_dirs`) and
  project. Names never collide across scopes; `fn_save` writes a new one after checking it.
- Optional **packs** live in [`packs/`](packs/README.md): agents, git and jev functions that
  are not loaded by default — copy one into `$SLUICE_HOME/fns/` (or a project's `fns/`), or
  point `config.fn_dirs` at it, to install it.
- Plans are local JSON (CWL-like `inputs`, `outputs`, `steps`). Every edit goes through typed MCP
  tools at the current revision and is appended to a log.
- Each project has one append-only **log** (`log.jsonl`): every edit, manual value, step status
  change, `fn_call` and thread message, with a `seq`. It is capped history; `plan.json` and
  `state.json` are the current truth. `log_read`/`log_wait` read it over MCP and
  `sluice watch -p <project>` follows it from a shell.
- A result is only valid for the inputs it was computed from: when they change, the step and
  everything downstream turn **stale** and wait for `step_retry` (or `step_set_output`).
- **Threads** are named conversations in the log: `thread.post` appends a message,
  `log_wait` (or the `thread.wait` step) waits for one.
- `verify` checks functions, projects, plans and state and says where each problem is.

- `sluice serve` also serves a **dashboard**: projects, each plan as a live Mermaid
  diagram with its steps and history, the functions, a paged, filterable **log viewer**
  (`/projects/<name>/log`, `/log`) and the **Inbox**. Pages work without JavaScript; with it,
  [Datastar](https://data-star.dev) streams only the parts that changed.

## Inbox

Things waiting on a person. An agent posts an item with `inbox_post(project, title, body?, ui?,
input?, from?)`; a plan step does it with the built-in `inbox.ask` and waits for the answer.
The dashboard's Inbox (`/inbox`, `/projects/<name>/inbox`) lists open items, with their count
as the nav's one red badge. Each shows its markdown body and either a text box or, when the
item has a `ui`, a small form drawn from an [OpenUI Lang](https://www.openui.com) program
(buttons, choices, fields; the vocabulary is in `docs("inbox")`). Answering posts `{action,
params?, values?, text?}` through the same code path as the `inbox_answer` tool; an item named
after a plan `input` sets that input (type-checked), so the steps waiting on it start. Answers
and closes are one-shot (a second one is a `conflict`), items live in the project's
`inbox.json`, and every change is an `inbox.*` log record, so `log_wait` or
`sluice watch -p demo --kinds inbox` wakes whoever waits.

```sh
uv run sluice tool inbox_post '{"project": "demo", "title": "Ship v2?", "ui": "root = Button(\"Ship\", \"ship\")"}'
uv run sluice tool inbox_list '{"status": "answered"}'
```

See [SPEC.md](SPEC.md) for the contract and `src/sluice/docs` for the pages agents read.

```sh
uv sync
uv run sluice serve        # runner + MCP at http://127.0.0.1:7420/mcp, dashboard at /
uv run sluice tool         # list the MCP tools
uv run sluice tool project_create '{"name": "demo", "description": "try it"}'
uv run sluice tool verify  # exits 1 when there are problems
```

`sluice tool <name> '<json>'` calls the same tools the MCP server exposes, in-process. Without
`serve` running, start `sluice loop` for the runner, or pass `"direct": true` to `fn_call`.

For long-running work, run the two apart so restarting the server never ends a running step:

```sh
sluice loop                  # the runner: keep it up
sluice serve --no-runner     # MCP + dashboard: restart freely
```

```sh
uv run sluice watch -p demo --kinds step.status,message   # one JSON line per new log record
```

In Claude Code, `Monitor("sluice watch -p demo --kinds step.status,message")` turns each step
change and message into an event.
