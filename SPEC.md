# sluice: specification (v1)

sluice runs a plan: a graph of typed function calls. Agents and humans edit the plan through
typed tools; a runner process executes it, reacts to results, and escalates only what a rule
cannot settle. This file is the contract every part of the repo builds against. When the code
and this file disagree, fix one of them in the same change.

## 1. Concepts

- **Function (fn):** a reusable unit with typed input ports, typed output ports, and a Python
  implementation (`main.py`, run with `uv`) or a graph of other fns (a composite).
- **Node:** one call of a fn inside a plan. Its inputs are bound to literals, files, or other
  nodes' outputs.
- **Plan:** the desired graph. Only grows by edits. Stored as JSON, with every edit logged.
- **State:** what actually happened to each node. Written by the runner (and by operator
  actions, under the same lock).
- **Runner:** the loop that starts ready nodes, supervises their processes, records results,
  applies spawns, and opens inbox items.
- **Inbox:** items that need a decision from an orchestrator or a human: a node that failed for
  good, or an explicit `core.ask` node.
- **Pack:** a directory of fns. The core knows nothing about git or any project; packs do. The
  packs in this repo are `agents` and `git`, shipped inside the package (`src/sluice/packs`).

## 2. Workspace layout

`SLUICE_HOME` (default `~/.sluice`):

```
config.json
runner.lock          # flock held by the one runner loop of this home
plans/<plan_id>/
  plan.json          # current desired plan (snapshot of the log)
  plan.log.jsonl     # one line per accepted edit (source of truth)
  state.json         # observed state
  events.jsonl       # append-only event stream
  inbox/<item_id>.json
  outputs/<node_id>.json   # the recorded output of each succeeded node
  .lock              # flock target for every read-modify-write in this plan dir
runs/<run_id>/       # one dir per fn execution
  cmd.json input.json stdout.log stderr.log output.json exit.json
cache/<sha256>.json  # results of effect-free fns, keyed by fn name+version+input
```

`config.json`:

```json
{
  "packs": ["/abs/path/to/extra/pack"],
  "slots": {"default": 8, "agent": 6, "heavy": 2},
  "tick": "2s",
  "http": {"host": "127.0.0.1", "port": 7420}
}
```

The packs shipped in the package (every directory under `src/sluice/packs`) always load;
`packs` lists extra pack directories (default `[]`; relative paths resolve against
`SLUICE_HOME`). A slot name missing from `slots` has capacity 1.

All writes are atomic: write `<file>.tmp`, fsync, `os.replace`. Every read-modify-write of a
plan dir holds `fcntl.flock` on its `.lock`. JSONL appends happen under the same lock.

## 3. Type language

Types are written inline in `fn.json`. There is no shared type registry; compatibility is
structural.

| JSON form | Meaning |
|---|---|
| `"string"`, `"int"`, `"float"`, `"bool"`, `"any"` | primitives; `any` accepts anything |
| `"string?"` (any primitive + `?`) | shorthand for `{"optional": "string"}` |
| `["a", "b"]` | enum of string literals |
| `{"list": T}` | list of T |
| `{"map": T}` | object with string keys and T values |
| `{"optional": T}` | T or null; an input port of this type may be left unbound |
| `{"union": {"tagA": R, "tagB": R}}` | tagged union: an object whose `"kind"` field is the tag, plus the fields of record R |
| `{"record": {...}}` | explicit record (use when a field is named list/map/optional/union/record) |
| any other object `{"f": T, ...}` | record with those fields |

A dict with exactly one key that is one of `list`, `map`, `optional`, `union`, `record` is
that special form; every other dict is a record.

`fits(out, inp)`: can every value of type `out` be used where `inp` is expected?

- `inp` is `any`, or `out` is `any` (checked at runtime instead): true.
- `inp` optional: `fits(out.of, inp.of)` if `out` is optional, else `fits(out, inp.of)`.
  `out` optional and `inp` not optional: false.
- primitives: same name, or `int` into `float`.
- enum: `set(out) <= set(inp)`; enum into `string`: true.
- list / map: covariant in the element.
- record: every non-optional field of `inp` exists in `out` and fits; optional fields of `inp`
  that exist in `out` must fit. Extra fields in `out` are fine.
- union: every tag of `out` exists in `inp` and its record fits.

`check_value(type, value) -> list[str]` validates a runtime JSON value and returns error paths
(e.g. `report.outcome: expected one of [done, blocked], got "ok"`). Records reject missing
required fields but allow extra fields. `int` rejects booleans and floats; `float` accepts ints.

## 4. Functions

A fn lives in `<pack>/<fn.name>/` with `fn.json` and, unless composite, `main.py`. Only the
immediate subdirectories of a pack that contain `fn.json` are fns; everything else (`tests/`,
`_lib/`, `examples/`, files) is skipped.

```json
{
  "name": "agent.devin",
  "version": 1,
  "description": "Run a Devin session on a spec file in a working directory.",
  "in":  {"cwd": "string", "spec": "string", "resume": "string?"},
  "out": {"log": "string", "final": "string", "report": "string?"},
  "effects": true,
  "timeout": "4h",
  "slots": {"agent": 1},
  "retry": {"transient": 3, "backoff": "10m"}
}
```

- `name` (required): dotted lowercase, unique across all loaded packs and the built-ins
  (duplicate = load error). `in` and `out` are required (may be `{}`).
- `version` (default `1`), `description` (default `""`).
- `effects` (default `true`): `false` means the result depends only on the input, so the
  runner caches it by `sha256(name, version, canonical input JSON)`.
- `timeout` (default `"1h"`), durations are `<int><s|m|h>`.
- `slots` (default `{"default": 1}`): capacity consumed while running.
- `retry.transient` (default 0): how many times exit code 75 is retried; `retry.backoff`
  (default `"30s"`) between attempts.

### 4.1 Process contract

The runner executes `uv run --quiet --script <fn_dir>/main.py` through the launcher (§7) with:

- stdin: the input object (one JSON object keyed by input port; unbound optional ports are
  `null`).
- env: the runner's environment plus `SLUICE_HOME`, `SLUICE_PLAN`, `SLUICE_NODE`,
  `SLUICE_RUN_ID`, `SLUICE_RUN_DIR`, `SLUICE_ATTEMPT` (1-based), `SLUICE_IDEMPOTENCY_KEY`
  (`<plan>/<node>/<attempt>`), `SLUICE_FN_DIR`, and `PYTHONPATH` starting with sluice's `src`
  dir so `import sluice.fn` works.
- cwd: the run dir.
- stdout: exactly one JSON object keyed by output port (the helper also writes it to
  `$SLUICE_RUN_DIR/output.json`; the runner prefers that file). Logs go to stderr.
- exit code: `0` success; `75` transient failure (retried per `retry.transient`); anything
  else is a failure.

The output object may carry one reserved key, `"_spawn"`:

```json
{"_spawn": {"reason": "checks red, attempt 2", "nodes": {"<new_id>": { ...node... }},
            "forward": "<new_id>"}}
```

The runner removes `_spawn` before type-checking the output, then applies it as a plan patch
authored `node:<node_id>` (its reason ends with `(run <run_id>)`, which makes a spawn that was
applied just before a runner restart idempotent). New ids must not exist yet. An invalid spawn
fails the node. The result of a fn that spawns is not cached.

`forward` (optional) names one of the spawned nodes. Its fn's `out` must `fit` the spawning
fn's `out`, else the spawn is invalid. When it applies, the spawning node's own output is not
type-checked and the node becomes `forwarded` (not terminal) instead of `succeeded`: its
outputs resolve from the target's outputs, transitively (a target may forward again), and it
becomes `succeeded`/`failed`/`skipped`/`cancelled` when its final target does, recording that
target's output. Dependents keep binding to the spawning node and never need rewiring. A
`forwarded` node keeps the claims it (and its composite ancestors) hold until its final target
is terminal, and the forward chain (each target and a target's inner nodes) runs under those
held units without acquiring them again.

`main.py` uses the stdlib-only helper `sluice.fn` (§8) and declares its own third-party deps
with PEP 723 inline metadata.

### 4.2 Composite fns

A composite has `"graph"` instead of `main.py`:

```json
{
  "name": "demo.twice",
  "version": 1,
  "in":  {"x": "int"},
  "out": {"y": "int"},
  "graph": {
    "nodes": {
      "a": {"fn": "core.echo", "in": {"value": {"from": "$in.x"}}},
      "b": {"fn": "core.echo", "in": {"value": {"from": "a.value"}}}
    },
    "out": {"y": {"from": "b.value"}}
  }
}
```

Inside a graph, `$in.<port>` refers to the composite's inputs. Inner nodes take the same keys
as plan nodes (§5.1) except `timeout`-free composites; in particular they may carry `after`
and `when`, whose refs (like those of `from`) are local to the graph (a sibling id) or
`$in.<port>`. Claims on inner nodes name the plan's resources. Composites may nest (max depth
8). `effects`, `timeout`, `slots` and `retry` do not apply to composites, and a plan node
calling a composite may not set `timeout`.

Expansion: a plan node `x` calling a composite becomes a composite node `x` plus inner nodes
`x/<inner>` (nested: `x/<inner>/<inner2>`). Every inner node also waits for the composite
node's own dependencies (from/after/when) and inherits its `hold` and `when` conditions. A ref
to `x.<port>` (and a dependency on `x`) means the composite as a whole (§6).

### 4.3 Built-in fns (native, no process)

- `core.echo`: `in {"value": "any"}`, `out {"value": "any"}`. Passes its input through.
  Effect-free. Useful for joins, renames and tests.
- `core.ask`: `in {"question": "string", "context": "any?", "to": {"optional":
  ["orchestrator", "human"]}}` (`to` defaults to `orchestrator`), `out {"answer": "any"}`.
  Opens an inbox item; the node waits until the item is resolved with an `answer`, which
  becomes its output.
- `core.fail`: `in {"message": "string"}`, `out {}`. Always fails. For tests and explicit stops.

## 5. Plans

### 5.1 Document

```json
{
  "id": "release-2",
  "rev": 12,
  "title": "release 2",
  "paused": false,
  "resources": {"db-schema": 1},
  "meta": {},
  "nodes": {
    "api": {
      "fn": "build.task",
      "in": {
        "brief": {"file": "briefs/api.md"},
        "base":  {"from": "shared.head"},
        "name":  {"value": "api"}
      },
      "after": ["schema"],
      "when":  [{"from": "gate.ok", "op": "eq", "value": true}],
      "claims": ["db-schema"],
      "hold": false,
      "timeout": "6h",
      "note": "free text for humans and orchestrators"
    }
  }
}
```

- Plan ids and node ids match `^[a-z0-9][a-z0-9_-]*$`. Expanded composite nodes get ids
  `<outer>/<inner>` (so `/` is reserved); plan refs and `after` may name expanded ids.
- `rev` is maintained by the store and cannot be patched (a document containing it is
  invalid; `plan_create` drops it). Unknown keys are invalid.
- Bindings (every input port): `{"value": <json>}`, `{"from": "<node>.<port>[.<field>...]"}`,
  or `{"file": "<path>"}` (read as a UTF-8 string at start time; relative paths resolve
  against the plan dir). Optional input ports may be omitted.
- `after`: ordering-only dependencies (the node waits for them to succeed).
- `when`: all conditions must hold for the node to run, else it is skipped. Each condition is
  `{"from": ref, "op": "eq"|"ne"|"in"|"truthy"|"falsy", "value": <json>?}` (`value` is
  required for eq/ne/in, a list for in, absent for truthy/falsy; eq/ne/in compare JSON
  values exactly). A `when` ref is also a dependency.
- `claims`: resource names from `resources`; each claim holds one unit from the node's first
  start until it (or, for a composite, every inner node) is terminal.
- `hold: true`: never start (existing runs continue). `paused: true` at plan level: start
  nothing.
- `timeout`: overrides the fn's timeout for this node.

### 5.2 Validation (every accepted edit must pass)

1. Document shape and id syntax.
2. Every `fn` exists in the registry.
3. Composite expansion succeeds (depth ≤ 8, inner refs resolve, inner types fit, every
   required composite `out` port is bound, the inner graph is acyclic).
4. Every `from`/`when` ref names an existing node and an output port of its fn (fields beyond
   the port are navigated through record/optional/map types and a union's `kind`; navigating
   into `any` is allowed). Every `after` entry names an existing node.
5. Every required input port is bound; no unknown ports.
6. Types: `fits(source_type, port_type)` for `from`; `check_value(port_type, value)` for
   `value`; `file` requires a port that accepts `string`; `when` values must be values of the
   ref's type.
7. The expanded graph (from + after + when edges) is acyclic.
8. Every claim (including inner nodes' claims) names a declared resource.
9. No edit may delete or rewire (change the `fn`, `in`, `after` or `when` of) a node whose
   state is `succeeded` while another node that depends on it is `running` or `waiting` (the
   runner passes current state to validation).

Validation returns every error, each with a JSON path, e.g.
`nodes.api.in.base: out type {branch, sha?} does not fit {branch, sha}: sha is optional`.
Errors inside a composite name the fn and the path in its graph, e.g.
`nodes.x: fn demo.twice: graph.nodes.a.in.value: unknown node zz`.

### 5.3 Edits and the log

`patch(rev, ops, author, reason)`: `ops` is an RFC 6902 JSON Patch against the document
without `rev`. If `rev` is not the current revision, the edit fails with `Conflict(current)`.
Otherwise the patched document is validated (a patch that cannot be applied is invalid, with
an `ops[i]` path); on success `rev += 1`, `plan.json` is rewritten, and one line is appended
to `plan.log.jsonl`:

```json
{"rev": 13, "at": "2026-09-26T14:02:11Z", "author": "orch-api", "reason": "...", "ops": [...]}
```

The log's first line (rev 1) is the creation, with `ops` = one `add` of the whole document
(path `""`). `plan_at(rev)` replays the log. `revert(rev, to_rev)` computes a patch from the
current document to the document at `to_rev` and applies it as a normal edit.

## 6. State and events

`state.json`:

```json
{
  "rev": 88,
  "plan_rev": 13,
  "nodes": {
    "api/fork": {
      "status": "succeeded",
      "attempt": 1,
      "run_id": "20260926T140211-api-fork-1-ab12",
      "pid": null,
      "started": "...", "finished": "...",
      "output": {"path": ".../plans/release-2/outputs/api/fork.json", "sha": "..."},
      "error": null,
      "cache_hit": false,
      "claims_held": []
    }
  }
}
```

Leaf entries may also carry runner bookkeeping: `retries` (transient retries used),
`not_before` and `deadline` (epoch seconds), `timeout_s`, `slots`, `def_sha` (the definition
the last decision was made for), `skipped_by` (`dep`, `when` or `operator`), `item` (the
node's inbox item) and `forward` (§4.1). `attempt` starts at 1 and grows by one on each
transient retry and each operator retry.

Statuses: `pending`, `running`, `waiting` (core.ask), `forwarded` (§4.1), `succeeded`,
`failed`, `skipped`, `cancelled`. The last four are terminal.

Composite nodes also get an entry (`"composite": true`) whose status is derived from its inner
nodes, in this order: `failed` if any inner node failed; `skipped` if every inner node is
skipped; once every inner node is terminal, `succeeded` if the graph's `out` bindings resolve,
else `skipped` (an unresolved binding's source was skipped or cancelled); otherwise `running`
if any inner node started, else `pending`. A composite's outputs are its `out` bindings,
resolved on demand.

`events.jsonl` lines: `{"seq": n, "at": ts, "type": ..., "node": id?, "data": {...}}`. Types:
`plan_created`, `plan_patched`, `node_started`, `node_succeeded`, `node_failed`,
`node_retrying`, `node_skipped`, `node_waiting`, `node_forwarded`, `node_cancelled`,
`spawn_applied`, `inbox_opened`, `inbox_resolved`, `runner_started`, `runner_stopped`.
Composite nodes emit `node_started` and their terminal event with `data.composite: true`;
operator actions put `by` and `reason` in `data`.

Inbox items (`inbox/<item_id>.json`, id `<plan>.<nnnn>`): `{id, plan, node, kind: "failure" |
"ask", status: "open" | "resolved", opened, resolution, resolved_by, resolved_at, ...}`; a
failure item adds `fn, attempt, error, stderr_tail, input, run_dir`, an ask item `question,
context, to`.

## 7. Runner

One loop per `SLUICE_HOME` (it holds `runner.lock`; a second loop refuses to start), over all
plans. Each tick (default every 2 s, and immediately after an edit made in-process):

1. **Load.** For each plan, read the plan and state; if `plan.rev` changed, re-expand.
2. **Reconcile.** Add `pending` entries for new expanded nodes. A node that is no longer in
   the plan: if running (or waiting), kill its process group and mark `cancelled`; otherwise
   drop its state entry (events keep the history). A terminal node whose definition (`fn`,
   `in`, `after`, `when`) changed since it was decided goes back to `pending` (`attempt += 1`
   if it had run), so state converges to the plan.
3. **Poll running nodes.** A run is finished when `exit.json` exists. On exit 0: read
   `output.json` (or stdout), strip `_spawn`, `check_value` against the fn's `out`; mismatch is
   a failure. On exit 75 with retries left: `node_retrying`, back to `pending` after backoff,
   `attempt += 1`. Past the timeout: kill the process group, failure. On failure without
   retries: `failed`, open an inbox item (kind `failure`, with error, stderr tail, input,
   run dir).
4. **Apply spawns** as plan edits (author `node:<id>`). If the edit is invalid, the node fails
   with the validation errors.
5. **Start ready nodes.** A `pending` node is ready when every dependency (from/after/when) has
   `succeeded`. If any dependency `failed`, the node stays `pending` (an orchestrator decides).
   Otherwise, if any dependency is `skipped` or `cancelled`, the node is `skipped`. `when`
   false → `skipped`. Then, if not held/paused and past any retry backoff, its claims and
   slots are available, it starts: native fns run inline; effect-free fns check the cache
   first; others get a run dir and a launched process. Natives use no slots. Skips and inline
   results propagate within the same tick.
6. **Persist** state (state `rev += 1` when anything changed) and events.

**Launcher.** `python -m sluice.launch <run_dir>` (stdlib only). It reads `cmd.json`
(`argv`, `env` merged over its own environment, `cwd`), runs the child with stdin from
`input.json`, stdout to `stdout.log`, stderr to `stderr.log`, and writes `exit.json`
(`{"code": int, "at": ts}`; a child killed by signal n exits `128 + n`, one that cannot start
exits `127`). The runner starts the launcher with `start_new_session=True` and records its
pid, so fn processes survive a runner restart. On restart, a `running` node with `exit.json` is
finished normally; with a live pid it stays running; otherwise it fails as transient (`lost`)
and follows its retry policy.

**Operator actions** (tools and CLI): `node_retry` (failed/cancelled/skipped → pending,
`attempt += 1`, closes a related open inbox item; nodes that were skipped because of it, by
dependency or `when`, go back to `pending` too), `node_skip` (pending/waiting/failed →
skipped), `node_cancel` (kill if running; pending/waiting/running/failed → cancelled). On a
composite they apply to each eligible inner node; on a forwarded node, to its final target.
Each emits an event; acting on a node in another status is a `bad_request`.

## 8. Helper library `sluice.fn` (stdlib only)

```python
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import run, Transient, sh

def main(inp, ctx):
    out = sh(["git", "rev-parse", "HEAD"], cwd=inp["path"]).stdout.strip()
    if ctx.attempt < 2 and flaky():
        raise Transient("capacity")             # exit 75
    ctx.spawn({"fix-1": {...}}, reason="...")   # optional
    return {"sha": out}

if __name__ == "__main__":
    run(main)
```

`run(main)` reads stdin, builds `ctx` (`plan`, `node`, `run_id`, `run_dir`, `attempt`,
`idempotency_key`, `home`, `fn_dir`, `log(msg)`, `spawn(nodes, reason, forward=None)`),
redirects `sys.stdout` to stderr while `main` runs, writes the result to stdout and
`output.json`, exits 75 on `Transient`, and on any other exception prints the traceback to
stderr, writes `error.json` (`{"type", "message"}`), and exits 1. `spawn` may be called more
than once (ids must not repeat); at most one call passes `forward`, naming one of its nodes.
`sh(argv, cwd=None, check=True, env=None, timeout=None, input=None)` wraps `subprocess.run`
(text mode, captured output), echoes the command and a tail of its output to stderr, and
raises `ShError` (with stdout/stderr) on a non-zero exit when `check`.

## 9. MCP server

`sluice serve` runs the runner loop and an MCP server (official `mcp` Python SDK: `MCPServer`,
called FastMCP before mcp 2; streamable HTTP) in one process at `http://<host>:<port>/mcp`.
Every tool returns JSON (a text block, plus structured content). Errors return a tool error
whose text is a JSON object `{"error": code, "message": ..., ...}` with codes `not_found`,
`conflict` (includes `current_rev`), `invalid` (includes `errors`), `bad_request` (also used
for arguments that do not match a tool's schema).

| Tool | Args | Returns |
|---|---|---|
| `plans_list` | `include_adhoc?` (default false) | `[{id, title, rev, paused, counts}]` |
| `plan_create` | `plan, doc, reason, author?` | `{rev}` |
| `plan_get` | `plan, path?` | `{rev, doc}` (`doc` without `rev`; `path` is a JSON Pointer into it) |
| `plan_patch` | `plan, rev, ops, reason, author?` | `{rev}` |
| `plan_validate` | `plan, doc` | `{ok, errors}` without writing |
| `plan_history` | `plan, since_rev?` | log entries |
| `plan_at` | `plan, rev` | `{doc}` |
| `plan_revert` | `plan, rev, to_rev, reason, author?` | `{rev}` |
| `node_add` | `plan, rev, id, node, reason, author?` | `{rev}` (sugar for an `add` op; an existing id is `invalid`) |
| `status` | `plan` | `{rev, state_rev, counts, nodes: [{id, fn, status, attempt, started, finished, error?}], ready: [...]}` |
| `node_get` | `plan, node` | `{definition, expanded_ids, state, output, stderr_tail}` |
| `node_retry` / `node_skip` / `node_cancel` | `plan, node, reason, author?` | `{ok, nodes}` (`nodes`: the leaf nodes acted on) |
| `dry_run` | `plan` | `{start: [{id, reason}], skip: [...], blocked: [...]}`: what the next tick would do |
| `events_tail` | `plan, since_seq?, limit?` (default 100) | events (after `since_seq`, else the last `limit`) |
| `inbox_list` | `plan?, open_only?` (default true) | items |
| `inbox_resolve` | `item, resolution, author?` | `{ok}` (`resolution` is `{"answer": ...}` for `core.ask`, or `{"action": "retry"|"skip"|"ack"}` for failures) |
| `fn_list` | – | `[{name, description, version, in, out, composite, effects}]` (`in`/`out` exactly as declared) |
| `fn_get` | `name` | the full fn.json |
| `fn_call` | `name, input, wait?` (seconds, default 0), `author?` | `{call, status, output?, error?}`; `{call, status: "pending"}` when `wait` is 0 |
| `fn_result` | `call` | `{call, status, output?, error?, stderr_tail?}` |

`author` defaults to `"mcp"`.

`fn_call` checks `input` against the fn's `in` types (`invalid` with `input.<port>` paths on a
mismatch), then creates an ad-hoc one-node plan `call-<yyyymmdd>t<hhmmss>-<hex>` with
`meta: {"adhoc": true, "fn": <name>}` and a node `call`, so the runner executes it with the
normal machinery (retries, timeout, logs, events, an inbox item on failure). With `wait > 0` it
polls, without blocking the server, until the call is terminal or waiting, or `wait` elapses.
`plans_list` hides ad-hoc plans unless `include_adhoc`.

## 10. CLI

`sluice` (argparse), operating directly on `SLUICE_HOME` through the same store. Errors print
the §9 JSON payload to stderr and exit 1.

```
sluice init [--pack DIR ...] [--force]   write a default config.json (extra packs: none)
sluice serve [--host H --port P]    runner + MCP server
sluice loop                         runner only (SIGINT/SIGTERM stop it cleanly)
sluice plan create <id> <file.json> [--reason R]
sluice plan show <id> [--json]
sluice plan patch <id> --rev N --reason R <ops.json>
sluice plan history <id>
sluice status <id> [--watch] [--json]
sluice dry-run <id> [--json]
sluice events <id> [--follow] [--limit N]
sluice inbox [--plan P] [--all] [--json]
sluice inbox resolve <item> '<resolution json>'
sluice node retry|skip|cancel <plan> <node> [--reason R]
sluice fn list | sluice fn show <name> | sluice fn test <name> <input.json>
sluice fn call <name> '<input json>' [--wait S] | sluice fn result <call>
```

Every JSON argument may be a file path, inline JSON, or `-` for stdin. `fn list` prints each
fn's name, a one-line `(in) -> (out)` summary, and its description.

`sluice fn test` runs one fn outside any plan (through the real runner and launcher, in a
throwaway `SLUICE_HOME` with the same packs) and prints its output; on failure it prints the
error and stderr tail and exits 1. It is how pack authors test functions. `sluice fn call`
runs a fn as an ad-hoc plan of this home (§9) for a running `serve`/`loop` to execute.

## 11. Packs in this repo

The built-in packs live in `src/sluice/packs/agents` and `src/sluice/packs/git`. Signatures
below are the contract other packs and plans bind to.

### agents

| fn | in | out |
|---|---|---|
| `agent.devin` | `cwd: string, spec: string (spec text), log: string?, resume: string?, report_path: string?` | `log: string, final: string, report: string?` |
| `agent.codex` | `cwd, spec, model: ["sol","astra"]?, log?, resume?, report_path?` | same as devin |
| `agent.claude` | `cwd: string, prompt: string, model: string? (default "opus"), session: string?` | `result: string, session: string, cost_usd: float?` |
| `agent.run` | `engine: ["devin","codex","claude"], cwd: string, spec: string, model: string?, resume: string?, report_path: string?` | `final: string, report: string?, session: string?` |
| `agent.review` | `cwd: string, base: string (git ref), standards: string (path), notes: string?` | `summary: string, sha: string, commits: int` |
| `decide.llm` | `question: string, context: any?, options: {list: string}, threshold: float?` | `choice: string, p: float, confident: bool` |
| `decide.jev` | same as `decide.llm` | same as `decide.llm` |

`agent.run` dispatches to the engine named in its input (the same invocation the engine's own fn
uses), so a composite can pick the engine from data. Agent fns fail as transient on capacity/rate-limit errors. `agent.review` commits fixes itself
(no comments) and never adds AI attribution to commits. `decide.jev` reads `SLUICE_JEV_URL` and
`SLUICE_JEV_KEY`; its request mapping is isolated in one function and marked PENDING until
checked against the real API. `decide.llm` uses `claude -p` with a JSON answer; its `p` is
self-reported and uncalibrated.

### git

| fn | in | out |
|---|---|---|
| `git.worktree` | `repo: string, base: string, branch: string, path: string?` | `path: string, branch: string, sha: string` |
| `git.worktree_rm` | `repo: string, path: string, force: bool?` | `removed: bool` |
| `git.head` | `path: string` | `branch: string, sha: string` |
| `git.merge` | `repo: string, source: string, target: string, message: string?, push: bool?` | `merged: bool, sha: string?, conflicts: {list: string}` |
| `git.rebase` | `path: string, onto: string` | `ok: bool, sha: string, conflicts: {list: string}` |
| `git.push` | `path: string, branch: string, remote: string?, force_with_lease: bool?` | `sha: string` |
| `gh.pr` | `path: string, base: string, head: string, title: string, body: string, draft: bool?` | `number: int, url: string` |

Conflicts are data, not failures: `git.merge`/`git.rebase` abort and return the conflicting
paths so a plan can route on them.

## 12. Repo conventions

- Python ≥ 3.12, `uv` for everything (`uv run pytest`, `uv run ruff check`).
- `src/sluice/fn.py` and `src/sluice/launch.py` import only the stdlib, and
  `src/sluice/__init__.py` imports nothing, so fn processes can import them without deps.
- Tests live in `tests/` (core, with a pack of fake fns in `tests/testpack`) and
  `src/sluice/packs/<pack>/tests/` (packs). Pack tests fake external binaries
  (devin-harness-run, codex, claude, gh) with scripts on `PATH`.
- Commits: plain sentences, no AI attribution of any kind (no Co-Authored-By, no "Generated
  with"). Stage exact paths.
