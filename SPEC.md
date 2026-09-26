# sluice: specification (v0, minimal)

sluice runs a plan: a graph of typed function calls. Orchestrators (agents or humans) edit the
plan through typed tools; a runner executes it. This is deliberately small: features get added
when real use asks for them, not before. When the code and this file disagree, fix one of them
in the same change.

## 1. Concepts

- **Function (fn):** a reusable unit with a name, an optional description, typed inputs, typed
  outputs, and a Python implementation (`main.py`, run with `uv`).
- **Plan:** a set of nodes. Each node calls one fn; each input is bound to a literal value or to
  other nodes' outputs. Edited only through `plan_patch`, and every edit is logged.
- **Runner:** starts a node once all its inputs are available, records its output or its
  failure. Nothing else. A failed node shows up in `status`; an orchestrator decides what next.

## 2. Workspace layout

`SLUICE_HOME` (default `~/.sluice`):

```
config.json                 {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420}, "max_parallel": 8}
plans/<plan_id>/
  plan.json                 current plan (snapshot of the log)
  plan.log.jsonl            one line per accepted edit
  state.json                runner-owned: status and output of every node
  .lock                     flock target for read-modify-write in this dir
runs/<run_id>/              input.json, output.json, stderr.log for one fn execution
```

Functions are loaded from the built-in dir `src/sluice/fns/` (shipped in the package) plus every
dir in `config.fn_dirs`. A fn dir is any immediate subdirectory containing `fn.json`; other
entries are ignored. Duplicate names are a load error.

Writes are atomic (`<file>.tmp` then `os.replace`); read-modify-write holds `fcntl.flock`.

## 3. Types

Written inline in `fn.json`, compared structurally. No shared registry.

| Form | Meaning |
|---|---|
| `"string"`, `"int"`, `"float"`, `"bool"`, `"any"` | primitives; `any` accepts anything |
| `"string?"` (any primitive + `?`) | shorthand for `{"optional": "string"}` |
| `["a", "b"]` | enum of strings |
| `{"list": T}` | list of T |
| `{"optional": T}` | T or null; an optional input may be left unbound |
| any other object `{"f": T, ...}` | record with those fields |

`fits(out, inp)`: `any` on either side fits; optional into non-optional does not; same
primitive, or `int` into `float`; enum subset, or enum into `string`; lists covariant; records:
every required field of `inp` exists in `out` and fits, extra fields in `out` are fine.

`check_value(type, value) -> list[str]` validates a runtime value, returning path-bearing
errors (`report.outcome: expected one of [done, blocked], got "ok"`).

## 4. Functions

`<fn_dir>/fn.json` plus `<fn_dir>/main.py`:

```json
{
  "name": "git.head",
  "description": "Current branch and commit of a working tree.",
  "in":  {"path": "string"},
  "out": {"branch": "string", "sha": "string"}
}
```

`name` (dotted lowercase) and `in`/`out` are required; `description` is optional.

**Process contract.** The runner runs `uv run --quiet --script <fn_dir>/main.py` with:
stdin = the input object (unbound optional inputs are `null`); env `SLUICE_HOME`,
`SLUICE_PLAN`, `SLUICE_NODE`, `SLUICE_RUN_ID`, `SLUICE_RUN_DIR`, `SLUICE_FN_DIR`, and
`PYTHONPATH` containing sluice's `src` dir; cwd = the run dir. The fn writes one JSON object
keyed by output name to stdout (logs go to stderr) and exits 0. Any other exit code, or an
output that fails `check_value` against `out`, is a failure. Retries and timeouts, if a fn
needs them, happen inside the fn.

`main.py` uses the stdlib-only helper `sluice.fn` (§7) and declares third-party deps with
PEP 723 inline metadata.

## 5. Plans

```json
{
  "id": "release-2",
  "rev": 7,
  "title": "Release 2",
  "nodes": {
    "api":   {"fn": "agent.run", "in": {"engine": {"value": "devin"}, "cwd": {"value": "/repo"},
                                        "spec": {"value": "Add the /health endpoint"}}},
    "ui":    {"fn": "agent.run", "in": {"engine": {"value": "claude"}, "cwd": {"value": "/repo"},
                                        "spec": {"value": "Show health in the footer"}}},
    "gate":  {"fn": "core.collect", "in": {"items": {"from": ["api.final", "ui.final"]}}},
    "notes": {"fn": "agent.run", "in": {"engine": {"value": "claude"}, "cwd": {"value": "/repo"},
                                        "spec": {"from": "gate.items.0"}}}
  }
}
```

- Plan and node ids match `^[a-z0-9][a-z0-9_-]*$`. `rev` is maintained by the store.
- **Bindings.** `{"value": <json>}` a literal; `{"from": "<node>.<output>[.<field or index>...]"}`
  one upstream value; `{"from": ["<ref>", ...]}` fan-in: a list of upstream values in order.
  Optional inputs may be omitted.
- **Fan-out:** several nodes bind the same output. **Fan-in:** a list binding, or several
  inputs bound to different nodes.
- A node is **ready** when every node it references has `succeeded`.

**Validation** (every edit must pass; all errors returned with paths): ids valid; every fn
exists; every required input bound and no unknown inputs; every ref names an existing node and
one of its fn's outputs (fields navigated through record types, anything under `any` allowed);
`fits` holds for single refs (for a list binding, the input must be a list or `any`, and each
element must fit the list's item type); literals pass `check_value`; the graph is acyclic.

**Edits.** `patch(rev, ops, reason, author)`: `ops` is RFC 6902 JSON Patch against the plan
without `rev`. A stale `rev` fails with `conflict` (and the current rev). A valid edit bumps
`rev`, rewrites `plan.json`, and appends `{"rev", "at", "author", "reason", "ops"}` to
`plan.log.jsonl` (the creation is rev 1, one `add` of the whole plan). Removing or changing a
running node is refused.

## 6. Runner and state

`state.json`: `{"nodes": {"<id>": {"status", "run_id", "started", "finished", "output",
"error"}}}` with status `pending`, `running`, `succeeded`, `failed`.

Loop (every ~1 s, and right after an in-process edit), over all plans:
1. New nodes get `pending`. State entries of nodes removed from the plan are dropped.
2. Finished processes: exit 0 with a valid output → `succeeded` with `output`; otherwise
   `failed` with `error` (exit code or type errors, plus the stderr tail).
3. Start ready `pending` nodes, at most `max_parallel` running at once across all plans.
   Built-in fns (below) run inline.
4. Write `state.json` if anything changed.

On startup, nodes left `running` by a previous runner are marked `failed` with
`error: "runner restarted"`. `node_retry` sets a `failed` node back to `pending`.

**Built-in fns** (in `src/sluice/fns/`, native, no process):
- `core.echo`: in `{"value": "any"}`, out `{"value": "any"}`.
- `core.collect`: in `{"items": {"list": "any"}}`, out `{"items": {"list": "any"}}`. The
  fan-in join.

## 7. Helper library `sluice.fn` (stdlib only)

```python
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import run, sh

def main(inp, ctx):
    return {"sha": sh(["git", "rev-parse", "HEAD"], cwd=inp["path"]).stdout.strip()}

if __name__ == "__main__":
    run(main)
```

`run(main)` reads stdin, calls `main(inp, ctx)` (`ctx`: `plan`, `node`, `run_id`, `run_dir`,
`home`, `fn_dir`, `log(msg)`) with stdout redirected to stderr, prints the result as JSON, and
on an exception prints the traceback and exits 1. `sh(argv, cwd=None, check=True, env=None,
timeout=None, input=None)` runs a command and raises `ShError` on a non-zero exit when `check`.

## 8. MCP server

`sluice serve` runs the runner and an MCP server (official `mcp` SDK, streamable HTTP) at
`http://<host>:<port>/mcp` in one process. Errors are tool errors whose message is JSON
`{"error": "not_found"|"conflict"|"invalid"|"bad_request", "message", ...}` (`conflict` carries
`current_rev`, `invalid` carries `errors`).

| Tool | Args | Returns |
|---|---|---|
| `fn_list` | – | `[{name, description, in, out}]` |
| `fn_get` | `name` | the fn.json |
| `fn_call` | `name, input, wait?` | runs one fn as a one-node plan (`call-<ts>-<short>`); returns `{plan, status, output?, error?}`, waiting up to `wait` seconds |
| `plans_list` | `include_calls?` | `[{id, title, rev, counts}]` |
| `plan_create` | `plan, doc, reason` | `{rev}` |
| `plan_get` | `plan` | `{rev, doc}` |
| `plan_patch` | `plan, rev, ops, reason, author?` | `{rev}` |
| `plan_history` | `plan, since_rev?` | log entries |
| `status` | `plan` | `{rev, nodes: [{id, fn, status, started, finished, output?, error?}]}` |
| `node_retry` | `plan, node` | `{ok}` |

## 9. CLI

```
sluice init | serve | loop
sluice fn list | show <name> | call <name> '<json>' [--wait S]
sluice plan create <id> <file.json> | show <id> | patch <id> --rev N --reason R <ops.json> | history <id>
sluice status <id>
sluice retry <plan> <node>
```

## 10. Built-in fns in this repo

`src/sluice/fns/` holds the built-ins plus two families: `agent.*`/`decide.*` (run Devin,
Codex, Claude, the review agent, decisions) and `git.*`/`gh.pr` (worktrees, merge, rebase,
push, pull requests). Their `fn.json` files are the reference for their types.

## 11. Conventions

Python ≥ 3.12, `uv` for everything. `src/sluice/fn.py` and `src/sluice/__init__.py` import only
the stdlib. Tests in `tests/` and next to fns; external tools are faked in tests. Commits:
plain sentences, no AI attribution of any kind; stage exact paths.
