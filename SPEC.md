# sluice: specification (v0, minimal)

sluice runs a plan: a graph of typed function calls. Orchestrators (agents or humans) edit the
plan through typed tools; a runner executes it. Deliberately small: features get added when real
use asks for them. The document shapes borrow from CWL (Common Workflow Language) where that
helps: `inputs`/`outputs`/`steps`, `run`, `source`/`default`, `scatter`, CWL type spellings. We do
not aim for CWL compliance. When the code and this file disagree, fix one of them in the same
change.

## 1. Concepts

- **Function (fn):** a reusable unit with a name, an optional `doc`, typed named `inputs`, typed
  named `outputs`, and a Python implementation (`main.py`, run with `uv`).
- **Plan:** typed plan `inputs`, named plan `outputs`, and `steps`. Each step runs one fn; each of
  its inputs comes from a plan input, other steps' outputs, or a literal. Edited only through
  typed edits, every edit logged.
- **Runner:** starts a step once everything it reads is available, records its outputs or its
  failure. A failed step shows up in `status`; an orchestrator decides what next.

## 2. Workspace layout

`SLUICE_HOME` (default `~/.sluice`):

```
config.json                 {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420}, "max_parallel": 8}
plans/<plan_id>/
  plan.json                 current plan (snapshot of the log)
  plan.log.jsonl            one line per accepted edit
  state.json                runner-owned: plan input values, step status and outputs
  .lock                     flock target for read-modify-write in this dir
runs/<run_id>/              input.json, output.json, stderr.log for one fn execution
```

Fns load from the built-in dir `src/sluice/fns/` (shipped in the package) plus every dir in
`config.fn_dirs`. A fn dir is any immediate subdirectory containing `fn.json`; other entries are
ignored. Duplicate names are a load error. Writes are atomic (`<file>.tmp` then `os.replace`);
read-modify-write holds `fcntl.flock`.

## 3. Types

Written inline, compared structurally, no shared registry. CWL spellings:

| Form | Meaning |
|---|---|
| `"string"`, `"int"`, `"float"`, `"boolean"`, `"Any"` | primitives; `Any` accepts anything |
| `"T?"` e.g. `"string?"`, `"string[]?"` | optional: T or null; an optional input may be left unbound |
| `["null", T]` | optional form for any T, e.g. an optional enum |
| `"T[]"` e.g. `"string[]"` | array of T (shorthand) |
| `{"type": "array", "items": T}` | array of T |
| `{"type": "enum", "symbols": ["a", "b"]}` | enum of strings |
| `{"type": "record", "fields": {"f": T, ...}}` | record |

`fits(out, inp)`: `Any` on either side fits; optional into non-optional does not; same primitive,
or `int` into `float`; enum subset, or enum into `string`; arrays covariant; records: every
required field of `inp` exists in `out` and fits, extra fields in `out` are fine.

`check_value(type, value) -> list[str]` validates a runtime value, returning path-bearing errors
(`report.outcome: expected one of [done, blocked], got "ok"`).

## 4. Functions

`<fn_dir>/fn.json` plus `<fn_dir>/main.py`:

```json
{
  "name": "git.head",
  "doc": "Current branch and commit of a working tree.",
  "inputs":  {"path": "string"},
  "outputs": {"branch": "string", "sha": "string"}
}
```

`name` (dotted lowercase), `inputs` and `outputs` are required; `doc` is optional.

**Process contract.** The runner runs `uv run --quiet --script <fn_dir>/main.py` with stdin = an
object keyed by input name (unbound optional inputs are `null`); env `SLUICE_HOME`,
`SLUICE_PLAN`, `SLUICE_STEP`, `SLUICE_RUN_ID`, `SLUICE_RUN_DIR`, `SLUICE_FN_DIR`, and
`PYTHONPATH` containing sluice's `src` dir; cwd = the run dir. The fn writes one JSON object keyed
by output name to stdout (logs go to stderr) and exits 0. Any other exit code, or outputs that
fail `check_value`, is a failure. Retries and timeouts, if a fn needs them, happen inside the fn
(`run(main, retries=N)`, §7).

## 5. Plans

```json
{
  "id": "release-2",
  "rev": 7,
  "label": "Release 2",
  "inputs":  {"repo": "string", "tasks": "string[]"},
  "outputs": {"notes": {"source": "notes/final"}},
  "steps": {
    "work":  {"run": "agent.run", "scatter": "spec",
              "in": {"engine": {"default": "devin"}, "cwd": {"source": "repo"},
                     "spec": {"source": "tasks"}}},
    "check": {"run": "agent.run",
              "in": {"engine": {"default": "claude"}, "cwd": {"source": "repo"},
                     "spec": {"default": "Run the test suite and summarise failures"}}},
    "gate":  {"run": "core.collect", "in": {"items": {"source": ["work/final", "check/final"]}}},
    "notes": {"run": "agent.run",
              "in": {"engine": {"default": "claude"}, "cwd": {"source": "repo"},
                     "spec": {"source": "gate/items.0"}}}
  }
}
```

- Ids (plan, steps, plan inputs/outputs) match `^[a-z0-9][a-z0-9_-]*$`. `rev` is store-maintained.
- **Step inputs** (`in`): `{"default": <json>}` a literal; `{"source": "<ref>"}` one value;
  `{"source": ["<ref>", ...]}` fan-in: an array of the values, in order. A ref is a plan input
  name (`repo`) or `<step>/<output>`, optionally followed by `.<field or index>...` to reach
  inside a value. Optional fn inputs may be omitted.
- **Fan-out:** several steps read the same output. **Fan-in:** a list `source`, or several inputs
  from different steps (`core.collect` gathers into one array).
- **Scatter** (dynamic fan-out): `"scatter": "<input name>"`. That input must receive an array
  whose items fit the fn's input type; the step runs once per item (the other inputs are the same
  for every run) and each of its outputs becomes an array, in item order. The step succeeds when
  every run succeeds and fails if any fails.
- **Plan outputs** name the plan's results: `{"source": "<ref>"}`. `fn_call` and `status`
  report them.
- A step is **ready** when every plan input and step it reads has a value / has `succeeded`.

**Validation** (every edit must pass; all errors returned with paths): ids valid; every `run`
exists; every required fn input bound, no unknown inputs; every ref names a declared plan input
or an existing step and one of its fn's outputs (fields navigated through record types, anything
under `Any` allowed; a scattered step's outputs are arrays); `fits` holds for each source (for a
list source, the target must be an array or `Any` and each element must fit its item type; for
the scatter input, each item must fit the fn's input type); defaults and plan input values pass
`check_value`; the graph is acyclic.

**Edits.** `patch(rev, ops, reason, author)`: `ops` is RFC 6902 JSON Patch against the plan
without `rev`. A stale `rev` fails with `conflict` (and the current rev). A valid edit bumps
`rev`, rewrites `plan.json`, and appends `{"rev", "at", "author", "reason", "ops"}` to
`plan.log.jsonl` (creation is rev 1, one `add` of the whole plan). Removing or changing a running
step is refused.

## 6. Runner and state

`state.json`:
`{"inputs": {"<name>": <value>}, "steps": {"<id>": {"status", "run_ids", "started", "finished",
"outputs", "error", "manual"}}}` with status `pending`, `running`, `succeeded`, `failed`.

Loop (every ~1 s, and right after an in-process edit), over all plans:
1. New steps get `pending`. State entries of steps removed from the plan are dropped.
2. Finished processes: exit 0 with valid outputs → `succeeded` with `outputs`; otherwise
   `failed` with `error` (exit code or type errors, plus the stderr tail). A scattered step
   collects its runs as they finish.
3. Start ready `pending` steps, at most `max_parallel` processes across all plans. Built-in fns
   run inline.
4. Write `state.json` if anything changed.

On startup, steps left `running` by a previous runner are marked `failed` with
`error: "runner restarted"`.

**Manual values** (recorded in state and in `plan.log.jsonl` as author/reason entries without
ops, so the history shows who set what):
- `plan_set_input(name, value)`: sets a declared plan input (type-checked). Steps reading it
  become ready. Changing a value already read by a started step is refused.
- `step_set_output(step, outputs)`: marks a non-running step `succeeded` with the given outputs
  (type-checked against its fn's outputs, arrays for a scattered step), `manual: true`. For
  manual work, a failed step whose result is known, or a stand-in. It is never run afterwards
  unless retried.
- `step_retry(step)`: sets a `failed` (or manual) step back to `pending`.
- Setting a step's input by hand is an edit: `step_set_input(step, input, value)` patches its
  binding to `{"default": value}`.

**Built-in fns** (in `src/sluice/fns/`, run inline):
- `core.echo`: inputs `{"value": "Any"}`, outputs `{"value": "Any"}`.
- `core.collect`: inputs `{"items": "Any[]"}`, outputs `{"items": "Any[]"}`. The fan-in join.
- `core.format`: inputs `{"template": "string", "values": "Any"}`, outputs `{"text": "string"}`.
  Python `str.format`: an array fills `{0}`, `{1}`...; a record fills `{name}`. Non-string values are
  rendered as JSON. Builds prompts from upstream outputs.

## 7. Helper library `sluice.fn` (stdlib only)

```python
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice.fn import run, sh, Transient

def main(inp, ctx):
    return {"sha": sh(["git", "rev-parse", "HEAD"], cwd=inp["path"]).stdout.strip()}

if __name__ == "__main__":
    run(main)            # run(main, retries=3, backoff=600) retries on Transient
```

`run(main, retries=0, backoff=30)` reads stdin, calls `main(inp, ctx)` (`ctx`: `plan`, `step`,
`run_id`, `run_dir`, `home`, `fn_dir`, `attempt`, `log(msg)`) with stdout redirected to stderr,
prints the result as JSON. On `Transient` it sleeps `backoff` s (env `SLUICE_BACKOFF` overrides)
and calls `main` again, up to `retries` times; any other exception, or running out of retries,
prints the traceback and exits 1. `sh(argv, cwd=None, check=True, env=None, timeout=None,
input=None)` runs a command and raises `ShError` on a non-zero exit when `check`.

## 8. MCP server

`sluice serve` runs the runner and an MCP server (official `mcp` SDK, streamable HTTP) at
`http://<host>:<port>/mcp` in one process. Errors are tool errors whose message is JSON
`{"error": "not_found"|"conflict"|"invalid"|"bad_request", "message", ...}` (`conflict` carries
`current_rev`, `invalid` carries `errors`). `rev` is optional on the convenience tools (they apply
to the current revision under the lock) and required on `plan_patch`.

| Tool | Args | Returns |
|---|---|---|
| `docs` | `topic?` | the index, or one page as markdown |
| `fn_list` | – | `[{name, doc, inputs, outputs}]` |
| `fn_get` | `name` | the fn.json |
| `fn_call` | `name, inputs, wait?` | runs one fn as a one-step plan (`call-<ts>-<short>`); `{plan, status, outputs?, error?}`, waiting up to `wait` s |
| `plans_list` | `include_calls?` | `[{id, label, rev, counts}]` |
| `plan_create` | `plan, doc, reason` | `{rev}` |
| `plan_get` | `plan` | `{rev, doc}` |
| `plan_patch` | `plan, rev, ops, reason, author?` | `{rev}` |
| `plan_history` | `plan, since_rev?` | log entries |
| `plan_set_input` | `plan, name, value, reason?` | `{ok}` |
| `step_set_input` | `plan, step, input, value, reason?, rev?` | `{rev}` |
| `step_set_output` | `plan, step, outputs, reason?` | `{ok}` |
| `step_retry` | `plan, step, reason?` | `{ok}` |
| `status` | `plan` | `{rev, inputs: {name: value or null}, outputs: {name: value or null}, steps: [{id, run, status, started, finished, outputs?, error?, manual}]}` |

## 9. CLI

```
sluice init | serve | loop
sluice fn list | show <name> | call <name> '<json>' [--wait S]
sluice plan create <id> <file.json> | show <id> | patch <id> --rev N --reason R <ops.json> | history <id>
sluice set-input <plan> <name> '<json>'
sluice set-output <plan> <step> '<json>'
sluice retry <plan> <step>
sluice status <id>
```

## 10. Built-in fns in this repo

`src/sluice/fns/` holds the built-ins plus two families: `agent.*`/`decide.*` (run Devin, Codex,
Claude, the review agent, decisions) and `git.*`/`gh.pr` (worktrees, merge, rebase, push, pull
requests). Their `fn.json` files are the reference for their types.

## 11. Conventions

Python ≥ 3.12, `uv` for everything. `src/sluice/fn.py` and `src/sluice/__init__.py` import only
the stdlib. Tests in `tests/`; external tools are faked in tests. Commits: plain sentences, no AI
attribution of any kind; stage exact paths.
