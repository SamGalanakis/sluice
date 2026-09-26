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
- **Project:** a name and an optional description, nothing else (no code directory: put whatever
  context matters in the description). Each project has exactly one plan, its own functions and
  its own `.env`. Every call names the project it acts on.
- **Plan:** typed plan `inputs`, named plan `outputs`, and `steps`. Each step runs one fn; each of
  its inputs comes from a plan input, other steps' outputs, or a literal. Edited only through
  typed edits, every edit logged.
- **Runner:** starts a step once everything it reads is available, records its outputs or its
  failure. A failed step shows up in `status`; an orchestrator decides what next.

## 2. Workspace layout

`SLUICE_HOME` (default `~/.sluice`):

```
config.json                 {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420}, "max_parallel": 8}
runner.lock                 flock held by the one runner of this home (a second one refuses to start)
.env                        global secrets (KEY=value lines)
fns/                        global user functions
calls/<call_id>/            one-off fn_call runs without a project
projects/<name>/
  project.json              {"name", "description"}
  plan.json                 the project's plan (snapshot of the log)
  plan.log.jsonl            one line per accepted edit or manual value
  state.json                runner-owned: plan input values, step status and outputs
  fns/                      project-local functions
  .env                      project secrets (override global ones)
  runs/<run_id>/            input.json, output.json, stderr.log for one fn execution
  calls/<call_id>/          one-off fn_call runs in this project
  .lock                     flock target for read-modify-write in this project
```

**Function scopes:** built-in (shipped in the package, `src/sluice/fns/`), global
(`SLUICE_HOME/fns/` and every dir in `config.fn_dirs`), and project (`projects/<name>/fns/`). A
project sees built-in + global + its own functions. **Names never collide:** a global function may
not reuse a built-in name, and a project function may not reuse a built-in or global name (or
another name in the same scope). A collision is an error of the later scope: the offending
project (or global dir) reports it from `verify` and `fn_list`, its plan edits and runs are
refused until it is fixed, and `fn_save` refuses to create one. Two projects may each have a
function of the same name. A fn dir is any immediate subdirectory containing `fn.json`;
anything else (e.g. `_lib/`) is ignored.

Writes are atomic (`<file>.tmp` then `os.replace`); read-modify-write holds `fcntl.flock`.

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
`SLUICE_PROJECT`, `SLUICE_STEP`, `SLUICE_RUN_ID`, `SLUICE_RUN_DIR`, `SLUICE_FN_DIR`, and
`PYTHONPATH` containing sluice's `src` dir, plus every `KEY=value` line of `SLUICE_HOME/.env` and
then the project's `.env` (project values win). Secrets live there, never in plans. cwd = the run
dir. `SLUICE_PROJECT` names the project (empty for a call without one). The fn writes one JSON object keyed
by output name to stdout (logs go to stderr) and exits 0. Any other exit code, or outputs that
fail `check_value`, is a failure. Retries and timeouts, if a fn needs them, happen inside the fn
(`run(main, retries=N)`, §7).

## 5. Plans

```json
{
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

A new project starts with the empty plan `{"inputs": {}, "outputs": {}, "steps": {}}`.

- Project names, step ids, plan input and output names match `^[a-z0-9][a-z0-9_-]*$`. The plan's
  `rev` is store-maintained and returned by `plan_get`/`status`.
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
exists (in the project's lookup order); every required fn input bound, no unknown inputs; every ref names a declared plan input
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
"outputs", "error", "manual"}}}` with status `pending`, `running`, `succeeded`, `failed`. A
scattered step also records `done` and `total` runs. A step waiting for a process slot stays
`pending`; a scattered step whose runs fail stops its other runs and fails with `run <i>: ...`.

Loop (every ~1 s, and right after an in-process edit), over all projects:
1. New steps get `pending`. State entries of steps removed from the plan are dropped.
2. Finished processes: exit 0 with valid outputs → `succeeded` with `outputs`; otherwise
   `failed` with `error` (exit code or type errors, plus the stderr tail). A scattered step
   collects its runs as they finish.
3. Start ready `pending` steps, at most `max_parallel` processes across all projects. Built-in fns
   run inline.
4. Write `state.json` if anything changed.

On startup, steps left `running` by a previous runner are marked `failed` with
`error: "runner restarted"`.

**Manual values** (recorded in state and in `plan.log.jsonl` as author/reason entries without
ops, `{"rev", "at", "author", "reason", "action": "<tool name>", ...its arguments}`, so the
history shows who set what):
- `plan_set_input(name, value)`: sets a declared plan input (type-checked). Steps reading it
  become ready. Changing a value later affects only steps that have not started yet.
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

## 6a. Verify

`verify(project?)` checks everything and returns every problem it finds, each with a location
(`project`, `fn`, file path or plan path) and a message; it changes nothing. Without a project it
checks the global scope and every project. It covers:
- every `fn.json`: shape (`name`, `inputs`, `outputs`, optional `doc`, nothing else), the name
  matching its directory, every type parsing, `main.py` present for non-built-ins;
- name collisions across scopes (see §2);
- `project.json` shape, `.env` files parsing as `KEY=value` lines;
- the plan: full validation (§5) against the project's functions;
- `state.json` agreeing with the plan (no state for unknown steps, outputs of succeeded steps
  passing their fn's output types, plan input values passing their types).

`{"ok": bool, "problems": [{"where", "message"}]}`. CLI `sluice verify [-p P]` prints them and exits
non-zero when there are any.

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

**Docs for agents.** The server sets MCP `instructions` from `src/sluice/docs/instructions.md`
(short: what sluice is, the workflow, where to read more). A `docs(topic?)` tool returns the index
(topic names with their first heading) without a topic, or `src/sluice/docs/<topic>.md`. The same
pages are MCP resources at `sluice://docs/<topic>`. Tool docstrings describe every argument.
Validation errors carry the path and what was expected.

**Views.** One renderer turns a plan plus its state into a picture:
- Mermaid (`flowchart LR`): plan inputs as rounded nodes, steps as boxes labelled
  `id / fn / status` (a scattered step shows `done/total`), plan outputs as rounded nodes, an edge
  per source ref labelled with the output name, one colour class per status (pending grey,
  running blue, succeeded green, failed red, manual outlined).
- HTML: a standalone page with that Mermaid diagram (mermaid from cdn.jsdelivr.net), a table of
  steps (status, started, finished, error first line), and the plan's input and output values.
`plan_view(plan, format)` returns either as text. `sluice serve` also serves the HTML live at
`GET /projects` (an index) and `GET /projects/<name>` (refreshes every 3 s).

| Tool | Args | Returns |
|---|---|---|
| `docs` | `topic?` | the index, or one page as markdown |
| `projects_list` | – | `[{name, description, rev, counts}]` |
| `project_create` | `name, description?` | `{name}` (with an empty plan) |
| `project_update` | `name, description` | `{name}` |
| `fn_list` | `project?` | `[{name, doc, inputs, outputs, scope}]` (`scope`: project, global or builtin) |
| `fn_get` | `name, project?` | the fn.json plus `scope` and `path` |
| `fn_save` | `fn, main_py, project?` | writes `fn.json` + `main.py` into the project's (or, without a project, the global) `fns/<name>/` after validating `fn`; `{scope, path}` |
| `fn_call` | `name, inputs, project?, wait?` | runs one fn outside the plan (under `calls/`); `{call, status, outputs?, error?}`, waiting up to `wait` s |
| `call_status` | `call, project?` | `{call, status, outputs?, error?, stderr_tail?}` |
| `plan_get` | `project` | `{rev, plan}` |
| `plan_patch` | `project, rev, ops, reason, author?` | `{rev}` |
| `plan_history` | `project, since_rev?` | log entries |
| `plan_set_input` | `project, name, value, reason?` | `{ok}` |
| `step_set_input` | `project, step, input, value, reason?, rev?` | `{rev}` |
| `step_set_output` | `project, step, outputs, reason?` | `{ok}` |
| `step_retry` | `project, step, reason?` | `{ok}` |
| `verify` | `project?` | `{ok, problems: [{where, message}]}` (§6a) |
| `plan_view` | `project, format: "mermaid"\|"html"` | the diagram or page as text |
| `status` | `project` | `{rev, inputs: {name: value or null}, outputs: {name: value or null}, steps: [{id, run, status, started, finished, outputs?, error?, manual}]}` |

## 9. CLI

```
sluice init | serve | loop
sluice project list | create <name> [--description D] | show <name>
sluice fn list [-p P] | show <name> [-p P] | call <name> '<json>' [-p P]   (runs it directly, no runner needed)
sluice plan show -p P | patch -p P --rev N --reason R <ops.json> | history -p P
sluice set-input -p P <name> '<json>'
sluice set-output -p P <step> '<json>'
sluice retry -p P <step>
sluice status -p P
sluice verify [-p P]
sluice view -p P [--html out.html]      Mermaid to stdout, or write the HTML page
```

## 10. Built-in fns in this repo

`src/sluice/fns/` holds the built-ins plus two families: `agent.*`/`decide.*` (run Devin, Codex,
Claude, the review agent, decisions), `jev.*` (Jev, TypeSafe's System One model: `jev.ask`,
`jev.choice`, `jev.score`, `jev.noul`; needs `TYPESAFE_API_KEY`) and `git.*`/`gh.pr` (worktrees,
merge, rebase, push, pull requests). Shared helper code for fns lives in `src/sluice/fns/_lib/`. Their `fn.json` files are the reference for their types.

## 11. Conventions

Python ≥ 3.12, `uv` for everything. `src/sluice/fn.py` and `src/sluice/__init__.py` import only
the stdlib. Tests in `tests/`; external tools are faked in tests. Commits: plain sentences, no AI
attribution of any kind; stage exact paths.
