# sluice

A controlled firehose for agent work. Orchestrators (agents or humans) edit a **plan**: a graph
of typed function calls. A **runner** executes it: it starts ready nodes, supervises their
processes, records results, and escalates only what a rule cannot settle to an **inbox**.

- Everything is a function: `fn.json` (typed inputs and outputs) plus a `main.py` run with `uv`,
  or a composite graph of other functions.
- Plans are local JSON files. Every edit goes through typed tools (MCP or CLI) with the current
  revision and is appended to a log.
- The core knows nothing about git or any project. Packs supply functions: the built-in packs are
  `agents` and `git`, shipped inside the package (`src/sluice/packs`).

See [SPEC.md](SPEC.md) for the full contract.

```sh
uv sync
uv run sluice init
uv run sluice serve        # runner + MCP at http://127.0.0.1:7420/mcp
```
