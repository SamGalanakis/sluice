# sluice

Orchestrators (agents or humans) edit a **plan**: a graph of typed function calls. A **runner**
executes it: it starts a step once everything it reads is available and records its outputs or
its failure. An orchestrator decides what happens after a failure.

- A function is `fn.json` (typed `inputs` and `outputs`) plus a `main.py` run with `uv`. The
  built-in ones live in `src/sluice/fns`; `config.fn_dirs` adds more.
- Plans are local JSON (CWL-like `inputs`, `outputs`, `steps`). Every edit goes through typed
  tools (MCP or CLI) at the current revision and is appended to a log.

See [SPEC.md](SPEC.md) for the contract and `src/sluice/docs` for the pages agents read.

```sh
uv sync
uv run sluice init
uv run sluice serve        # runner + MCP at http://127.0.0.1:7420/mcp, plan pages at /plans
```
