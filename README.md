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

```sh
uv run sluice watch -p demo --kinds step.status,message   # one JSON line per new log record
```

In Claude Code, `Monitor("sluice watch -p demo --kinds step.status,message")` turns each step
change and message into an event.
