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
- **State:** what actually happened to each node. Written only by the runner.
- **Runner:** the loop that starts ready nodes, supervises their processes, records results,
  applies spawns, and opens inbox items.
- **Inbox:** items that need a decision from an orchestrator or a human: a node that failed for
  good, or an explicit `core.ask` node.
- **Pack:** a directory of fns. The core knows nothing about git or any project; packs do.

## 2. Workspace layout

`SLUICE_HOME` (default `~/.sluice`):

```
config.json
plans/<plan_id>/
  plan.json          # current desired plan (snapshot of the log)
  plan.log.jsonl     # one line per accepted edit (source of truth)
  state.json         # runner-owned observed state
  events.jsonl       # append-only event stream
  inbox/<item_id>.json
  .lock              # flock target for every read-modify-write in this plan dir
runs/<run_id>/       # one dir per fn execution
  cmd.json input.json stdout.log stderr.log output.json exit.json
cache/<sha256>.json  # results of effect-free fns, keyed by fn name+version+input
```

`config.json`:

```json
{
  "packs": ["/abs/path/to/packs/agents", "/abs/path/to/packs/git"],
  "slots": {"default": 8, "agent": 6, "heavy": 2},
  "tick": "2s",
  "http": {"host": "127.0.0.1", "port": 7420}
}
```

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
required fields but allow extra fields.

## 4. Functions

A fn lives in `<pack>/<fn.name>/` with `fn.json` and, unless composite, `main.py`.

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

- `name`: dotted lowercase, unique across all loaded packs (duplicate = load error).
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
- env: `SLUICE_HOME`, `SLUICE_PLAN`, `SLUICE_NODE`, `SLUICE_RUN_ID`, `SLUICE_RUN_DIR`,
  `SLUICE_ATTEMPT` (1-based), `SLUICE_IDEMPOTENCY_KEY` (`<plan>/<node>/<attempt>`),
  `SLUICE_FN_DIR`, and `PYTHONPATH` including sluice's `src` dir so `import sluice.fn` works.
- cwd: the run dir.
- stdout: exactly one JSON object keyed by output port (the helper also writes it to
  `$SLUICE_RUN_DIR/output.json`; the runner prefers that file). Logs go to stderr.
- exit code: `0` success; `75` transient failure (retried per `retry.transient`); anything
  else is a failure.

The output object may carry one reserved key, `"_spawn"`:

```json
{"_spawn": {"reason": "checks red, attempt 2", "nodes": {"<new_id>": { ...node... }}}}
```

The runner removes `_spawn` before type-checking the output, then applies it as a plan patch
authored `node:<node_id>`. An invalid spawn fails the node.

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

Inside a graph, `$in.<port>` refers to the composite's inputs. Composites may nest (max depth
8). `effects`, `timeout`, `slots` and `retry` do not apply to composites.

### 4.3 Built-in fns (native, no process)

- `core.echo`: `in {"value": "any"}`, `out {"value": "any"}`. Passes its input through.
  Effect-free. Useful for joins, renames and tests.
- `core.ask`: `in {"question": "string", "context": "any?", "to": ["orchestrator", "human"]}`
  (`to` defaults to `orchestrator`), `out {"answer": "any"}`. Opens an inbox item; the node
  waits until the item is resolved with an `answer`, which becomes its output.
- `core.fail`: `in {"message": "string"}`. Always fails. For tests and explicit stops.

## 5. Plans

### 5.1 Document

```json
{
  "id": "lash-1-0",
  "rev": 12,
  "title": "lash 1.0",
  "paused": false,
  "resources": {"pg-schema": 1},
  "meta": {},
  "nodes": {
    "s7b": {
      "fn": "lash.task",
      "in": {
        "brief": {"file": "briefs/s7b.md"},
        "base":  {"from": "s7-shared.head"},
        "name":  {"value": "s7b"}
      },
      "after": ["s7a"],
      "when":  [{"from": "gate.ok", "op": "eq", "value": true}],
      "claims": ["pg-schema"],
      "hold": false,
      "timeout": "6h",
      "note": "free text for humans and orchestrators"
    }
  }
}
```

- Plan ids and node ids match `^[a-z0-9][a-z0-9_-]*$`. Expanded composite nodes get ids
  `<outer>/<inner>` (so `/` is reserved).
- `rev` is maintained by the store and cannot be patched.
- Bindings (every input port): `{"value": <json>}`, `{"from": "<node>.<port>[.<field>...]"}`,
  or `{"file": "<path>"}` (read as a UTF-8 string at start time; relative paths resolve
  against the plan dir). Optional input ports may be omitted.
- `after`: ordering-only dependencies (the node waits for them to succeed).
- `when`: all conditions must hold for the node to run, else it is skipped. Each condition is
  `{"from": ref, "op": "eq"|"ne"|"in"|"truthy"|"falsy", "value": <json>?}`. A `when` ref is
  also a dependency.
- `claims`: resource names from `resources`; each claim holds one unit from the node's first
  start until it (or, for a composite, every inner node) is terminal.
- `hold: true`: never start (existing runs continue). `paused: true` at plan level: start
  nothing.
- `timeout`: overrides the fn's timeout for this node.

### 5.2 Validation (every accepted edit must pass)

1. Document shape and id syntax.
2. Every `fn` exists in the registry.
3. Composite expansion succeeds (depth ≤ 8, inner refs resolve).
4. Every `from`/`when` ref names an existing node and an output port of its fn (fields beyond
   the port are navigated through record/optional types; navigating into `any` is allowed).
5. Every required input port is bound; no unknown ports.
6. Types: `fits(source_type, port_type)` for `from`; `check_value(port_type, value)` for
   `value`; `file` requires a port that accepts `string`.
7. The expanded graph (from + after + when edges) is acyclic.
8. Every claim names a declared resource.
9. No edit may delete or rewire a node whose state is `succeeded` while another node that
   depends on it is `running` (the runner passes current state to validation).

Validation returns every error, each with a JSON path, e.g.
`nodes.s7b.in.base: out type {branch, sha?} does not fit {branch, sha}: sha is optional`.

### 5.3 Edits and the log

`patch(rev, ops, author, reason)`: `ops` is an RFC 6902 JSON Patch against the document
without `rev`. If `rev` is not the current revision, the edit fails with `Conflict(current)`.
Otherwise the patched document is validated; on success `rev += 1`, `plan.json` is rewritten,
and one line is appended to `plan.log.jsonl`:

```json
{"rev": 13, "at": "2026-09-26T14:02:11Z", "author": "orch-s7", "reason": "...", "ops": [...]}
```

The log's first line (rev 1) is the creation, with `ops` = one `add` of the whole document.
`plan_at(rev)` replays the log. `revert(rev, to_rev)` computes a patch from the current
document to the document at `to_rev` and applies it as a normal edit.

## 6. State and events

`state.json`:

```json
{
  "rev": 88,
  "plan_rev": 13,
  "nodes": {
    "s7b/fork": {
      "status": "succeeded",
      "attempt": 1,
      "run_id": "20260926T140211-s7b-fork-1-ab12",
      "pid": null,
      "started": "...", "finished": "...",
      "output": {"path": "...", "sha": "..."},
      "error": null,
      "cache_hit": false,
      "claims_held": []
    }
  }
}
```

Statuses: `pending`, `running`, `waiting` (core.ask), `succeeded`, `failed`, `skipped`,
`cancelled`. Composite nodes also get an entry whose status is derived: `running` if any
inner node started, `failed` if any inner failed, `skipped` if all inner skipped, `succeeded`
when every inner node is terminal and not failed and the graph's `out` bindings resolve.

`events.jsonl` lines: `{"seq": n, "at": ts, "type": ..., "node": id?, "data": {...}}`. Types:
`plan_created`, `plan_patched`, `node_started`, `node_succeeded`, `node_failed`,
`node_retrying`, `node_skipped`, `node_waiting`, `node_cancelled`, `spawn_applied`,
`inbox_opened`, `inbox_resolved`, `runner_started`, `runner_stopped`.

## 7. Runner

One loop per `SLUICE_HOME`, over all plans. Each tick (default every 2 s, and immediately
after an edit made in-process):

1. **Load.** For each plan, read the plan and state; if `plan.rev` changed, re-expand.
2. **Reconcile.** Add `pending` entries for new expanded nodes. A node that is no longer in
   the plan: if running, kill its process group and mark `cancelled`; otherwise drop its
   state entry (events keep the history).
3. **Poll running nodes.** A run is finished when `exit.json` exists. On exit 0: read
   `output.json` (or stdout), strip `_spawn`, `check_value` against the fn's `out`; mismatch is
   a failure. On exit 75 with retries left: `node_retrying`, back to `pending` after backoff,
   `attempt += 1`. Past the timeout: kill the process group, failure. On failure without
   retries: `failed`, open an inbox item (kind `failure`, with error, stderr tail, input,
   run dir).
4. **Apply spawns** as plan edits (author `node:<id>`). If the edit is invalid, the node fails
   with the validation errors.
5. **Start ready nodes.** A `pending` node is ready when every dependency (from/after/when) has
   `succeeded`. If any dependency is `skipped` or `cancelled`, the node is `skipped`. If any
   dependency `failed`, the node stays `pending` (an orchestrator decides). `when` false →
   `skipped`. Then, if not held/paused, its claims and slots are available, it starts:
   native fns run inline; effect-free fns check the cache first; others get a run dir and a
   launched process.
6. **Persist** state (state `rev += 1` when anything changed) and events.

**Launcher.** `python -m sluice.launch <run_dir>` (stdlib only). It reads `cmd.json`
(`argv`, `env`, `cwd`), runs the child with stdin from `input.json`, stdout to `stdout.log`,
stderr to `stderr.log`, and writes `exit.json` (`{"code": int, "at": ts}`). The runner starts
the launcher with `start_new_session=True` and records its pid, so fn processes survive a
runner restart. On restart, a `running` node with `exit.json` is finished normally; with a
live pid it stays running; otherwise it fails as transient (`lost`) and follows its retry
policy.

**Operator actions** (tools and CLI): `node_retry` (failed/cancelled/skipped → pending,
`attempt += 1`, closes a related open inbox item), `node_skip` (→ skipped), `node_cancel`
(kill if running → cancelled). Each emits an event.

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
`idempotency_key`, `home`, `fn_dir`, `log(msg)`, `spawn(nodes, reason)`), redirects
`sys.stdout` to stderr while `main` runs, writes the result to stdout and `output.json`,
exits 75 on `Transient`, and on any other exception prints the traceback to stderr, writes
`error.json` (`{"type", "message"}`), and exits 1. `sh(argv, cwd=None, check=True, env=None,
timeout=None, input=None)` wraps `subprocess.run` (text mode, captured output), echoes the
command and a tail of its output to stderr, and raises `ShError` (with stdout/stderr) on a
non-zero exit when `check`.

## 9. MCP server

`sluice serve` runs the runner loop and an MCP server (official `mcp` Python SDK, FastMCP,
streamable HTTP) in one process at `http://<host>:<port>/mcp`. Every tool returns JSON. Errors
raise a tool error whose message is a JSON object `{"error": code, "message": ..., ...}` with
codes `not_found`, `conflict` (includes `current_rev`), `invalid` (includes `errors`),
`bad_request`.

| Tool | Args | Returns |
|---|---|---|
| `plans_list` | – | `[{id, title, rev, paused, counts}]` |
| `plan_create` | `plan, doc, reason, author?` | `{rev}` |
| `plan_get` | `plan, path?` | `{rev, doc}` (`path` is a JSON Pointer into the doc) |
| `plan_patch` | `plan, rev, ops, reason, author?` | `{rev}` |
| `plan_validate` | `plan, doc` | `{ok, errors}` without writing |
| `plan_history` | `plan, since_rev?` | log entries |
| `plan_at` | `plan, rev` | `{doc}` |
| `plan_revert` | `plan, rev, to_rev, reason, author?` | `{rev}` |
| `node_add` | `plan, rev, id, node, reason, author?` | `{rev}` (sugar for an `add` op) |
| `status` | `plan` | `{rev, state_rev, counts, nodes: [{id, fn, status, attempt, started, finished, error?}], ready: [...]}` |
| `node_get` | `plan, node` | `{definition, expanded_ids, state, stderr_tail}` |
| `node_retry` / `node_skip` / `node_cancel` | `plan, node, reason` | `{ok}` |
| `dry_run` | `plan` | the nodes the next tick would start, skip, or leave blocked, with reasons |
| `events_tail` | `plan, since_seq?, limit?` | events |
| `inbox_list` | `plan?, open_only?` | items |
| `inbox_resolve` | `item, resolution, author?` | `{ok}` (`resolution` is `{"answer": ...}` for `core.ask`, or `{"action": "retry"|"skip"|"ack"}` for failures) |
| `fn_list` | – | `[{name, version, description, in, out, composite}]` |
| `fn_get` | `name` | the full fn.json |

`author` defaults to `"mcp"`.

## 10. CLI

`sluice` (argparse), operating directly on `SLUICE_HOME` through the same store:

```
sluice init                         write a default config.json
sluice serve [--host H --port P]    runner + MCP server
sluice loop                         runner only
sluice plan create <id> <file.json> [--reason R]
sluice plan show <id> [--json]
sluice plan patch <id> --rev N --reason R <ops.json>
sluice plan history <id>
sluice status <id> [--watch]
sluice dry-run <id>
sluice events <id> [--follow]
sluice inbox [--plan P] [--all]
sluice inbox resolve <item> '<resolution json>'
sluice node retry|skip|cancel <plan> <node> [--reason R]
sluice fn list | sluice fn show <name> | sluice fn test <name> <input.json>
```

`sluice fn test` runs one fn outside any plan (through the launcher, in a temp run dir) and
prints its output. It is how pack authors test functions.

## 11. Packs in this repo

`packs/agents` and `packs/git` are generic; `packs/lash` is lash-only. Signatures below are
the contract other packs and plans bind to.

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

### lash

Defined in `packs/lash/README.md` by that pack. It uses kiln forks, the L0–L3 check ladder with
test receipts, lane-rules-based briefs, the task composite (fork → agent → L1 → review → L1 →
merge into the arc branch), arc landing (L2 → PR), the known-red ledger, and Linear.

## 12. Repo conventions

- Python ≥ 3.12, `uv` for everything (`uv run pytest`, `uv run ruff check`).
- `src/sluice/fn.py` and `src/sluice/launch.py` import only the stdlib, and
  `src/sluice/__init__.py` imports nothing, so fn processes can import them without deps.
- Tests live in `tests/` (core) and `packs/<pack>/tests/` (packs). Pack tests fake external
  binaries (devin-harness-run, codex, claude, gh, kiln) with scripts on `PATH`.
- Commits: plain sentences, no AI attribution of any kind (no Co-Authored-By, no "Generated
  with"). Stage exact paths.
