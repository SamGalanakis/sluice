# sluice: specification (v0, minimal)

sluice runs a plan: a graph of typed function calls. Orchestrators (agents or humans) edit the
plan through typed tools; a runner executes it. Deliberately small: features get added when real
use asks for them. The document shapes borrow from CWL (Common Workflow Language) where that
helps: `inputs`/`outputs`/`steps`, `run`, `source`/`default`, `scatter`, CWL type spellings. We do
not aim for CWL compliance. When the code and this file disagree, fix one of them in the same
change.

## 1. Concepts

- **Function (fn):** a reusable unit with a name, an optional `doc`, typed named `inputs`, typed
  named `outputs`, and a Python implementation (`main.py`, run with `uv`). An **open** fn (an
  agent) also takes whatever extra inputs a step binds and declares, per step, outputs that
  its agent submits (§5).
- **Project:** a name, an optional description and an optional icon (§2), nothing else (no
  code directory: put whatever context matters in the description). Each project has exactly
  one plan, its own functions and its own `.env`. Every call names the project it acts on.
- **Plan:** typed plan `inputs`, named plan `outputs`, and `steps`. Each step runs one fn; each of
  its inputs comes from a plan input, other steps' outputs, or a literal. Edited only through
  typed edits, every edit logged.
- **Log:** each project has one append-only log of what happened (edits, manual values, step
  status changes, calls, thread messages). It is history; the plan and the state are the
  current truth.
- **Runner:** starts a step once everything it reads is available, records its outputs or its
  failure. A failed step shows up in `status`; an orchestrator decides what next. Runs survive
  a runner restart: the next one adopts them (§6).
- **Inbox:** each project's items waiting on a person (a question, optionally with an OpenUI
  form, optionally setting a plan input). A person answers in the dashboard; agents post, wait
  on the log and read the answer (§8a).

## 2. Workspace layout

`SLUICE_HOME` (default `~/.sluice`):

```
config.json                 {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 7420},
                             "log_max": 10000}
sluice.db                   the home's database (SQLite, WAL): every project, plan, edit, state,
                            call, submission, inbox item, log record and the outcome of every
                            finished step removed from a plan (below)
runner.lock                 flock held by the one runner of this home (a second one refuses to start)
runner.json                 the runner's heartbeat {pid, started, beat}, refreshed about once
                            a second; stale means the runner is down
.env                        global secrets (KEY=value lines)
fns/                        global user functions
recipes/<name>.json         global recipes (§5)
runs/<call_id>/             input.json, output.json, stderr.log, shim.json, child.json,
                            shim.lock and exit.json of the calls without a project (§4)
trash/                      a deleted project's directory on its way out (<name>-<token>)
projects/<name>/            made when something needs it (runs/, fns/); may be prepared with
                            fns/ and .env before the project is created
  fns/                      project-local functions
  recipes/<name>.json       project recipes (§5): a project sees the global ones and its own,
                            and its own wins on a name clash
  .env                      project secrets (override global ones)
  runs/<run_id>/            input.json, output.json, stderr.log for one fn execution (a step run,
                            or a call: then run_id is the call id); shim.json, child.json,
                            shim.lock and exit.json, the supervising shim's identity, the fn
                            child's pid + start time, liveness lock and exit record (§4)
```

**The database** (`src/sluice/db.py`, standard library only; SQLite 3.37 or later, on a local
filesystem) holds, in STRICT tables: `projects` (name, description, `archived`, `paused`, the
icon, `created`, and `ver`, a counter every change to the project's rows moves — kept by
triggers, rolled back with them — plus `changed`, the time of its last state write); `plans`
(the project's plan document and its `rev`, the one authoritative rev); `plan_edits` (every
edit's rev, ops, author, reason, time and the seq of its `plan.edit` record; never trimmed);
`states` (the project's state document, §6); `calls` (§8 `fn_call`); `submissions` (§5);
`inbox` (§8a); `records` (the log, §6b); `outcomes` (what each finished step a plan edit
removed ended with, §6; never trimmed); and `deletions` (a deleted project whose directory is
not gone yet: name, token, time). Deleting a project deletes all of its rows and adds its
`deletions` row in one transaction; once that commits (the outermost transaction, when it is
nested in another), its `projects/<name>/` moves to `trash/<name>-<token>` (in a write
transaction that still finds the row) and is removed, and then the row goes. Until then a
project of that name cannot be created, so the removal never touches a replacement; a removal
that failed or was cut short (a crash after the commit) is finished by the runner's GC. Every
logical change is one write transaction (`BEGIN IMMEDIATE`): a plan edit writes the plan, its
`plan_edits` row and its record; a manual value the state and its record; an inbox answer that
sets a plan input both of those and the item; a submission its row and its record; a runner
state change the state and its `step.status` records; a call's status change its row and its
record. A write that cannot get the database within 5 s fails with `busy` ("the store is busy,
try again"), having written nothing. Reads that must agree (a status, a log page and its
cursor) come from one snapshot. `user_version` is the schema's version, now 2. A new file gets
the whole schema at 2; a version-1 file (before `outcomes`) is upgraded in place on first open,
in one `BEGIN IMMEDIATE` transaction that checks the version again under the lock and creates
the table and its index (`IF NOT EXISTS`: a file may have them already) before setting 2; a
database of any other version is refused, and so is a home from before this database — one with a `log.jsonl` or a
`projects/<name>/project.json` and no `sluice.db` — which is never read or treated as empty: it
must be imported into a new home first. The database also defines views for agents'
queries: `steps` (one row per plan step with its state, absent meaning `pending`), `messages`,
`step_changes`, `edits` and `log` (each record as `log_read` returns it).

A project's optional **icon** is either an image (SVG, PNG, WebP, JPEG or GIF, at most 256 KB)
read by `project_create`/`project_update`'s `icon` argument from a file, its type sniffed from
the content, and kept in the project's row with its sha256 — or a short text icon (at most 16
characters, no control characters, typically one emoji). A project has at most one of the two:
setting one clears the other; `icon: ""` removes it. A value that looks like a path (starts
with `/` or `~`) but is not a readable image file is an error, never a text icon.
`projects_list` reports it as `{"kind": "image", "type": <content type>}` or `{"kind": "text",
"text": ...}`, absent when none; the dashboard shows it by the project's name and serves an
image icon at `/projects/<name>/icon` (§8).

**Function scopes:** built-in (shipped in the package, `src/sluice/fns/`), global
(`SLUICE_HOME/fns/` and every dir in `config.fn_dirs`), and project (`projects/<name>/fns/`). A
project sees built-in + global + its own functions. **Names never collide:** a global function may
not reuse a built-in name, and a project function may not reuse a built-in or global name (or
another name in the same scope). A collision is an error of the later scope: the offending
project (or global dir) reports it from `verify` and `fn_list` (an entry with `error`), and
`fn_save` refuses to create one. Lookup still resolves a colliding name to the earlier scope's
function. Two projects may each have a function of the same name. A fn dir is any immediate
subdirectory containing `fn.json` whose `name` matches the directory; anything else (e.g.
`_lib/`) is ignored.

**Only project problems block.** While one of the project's own functions has a problem (a
collision, or a fn.json that fails the §6a checks), that project refuses plan edits, manual values,
`fn_call` and new runs (`invalid`, listing the problems); steps already running finish. Problems
in the global or built-in scope never block anything: the broken or colliding function is left
out of lookup and reported by `verify` and `fn_list`, and a plan step that uses it fails
validation like any unknown function. Reads (`status`, `plan_get`, `plan_history`, views,
`fn_list`, `verify`) always work. Functions are rescanned when a `fn.json` or `main.py` changes,
so a fix (or `fn_save`) needs no restart.

Files outside the database (`config.json`, `runner.json`, a fn's files, a run's files) are
written atomically (`<file>.tmp` then `os.replace`).

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

Types are written on fn inputs and outputs, plan inputs and the outputs a step of an open fn
declares (§5). An extra input of such a step has no written type: it takes its source's (a
ref's type; for a list source an array of the refs' type, `Any[]` when they differ; `Any` for a
`default`; `string` for a `file`; the item type when it is the scatter input). Where a type is handed on (the env of
an open fn, §4) it is spelled back in the forms above, a string where one exists.

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

`name` (dotted lowercase), `inputs` and `outputs` are required; `doc` is optional, and so is
`"open": true`: a step running an open fn may bind extra inputs and declare outputs of its own
(§5). Agent fns are open; nothing else needs to be. An open fn may also say what its agent
submits on every step: `"submits": {name: type or {"type", "doc"}}`, names apart from its
`outputs`. Every step running it declares those outputs as if it listed them itself (a step
may not declare one again): they are typed for refs, required unless optional, told to the
agent in `SLUICE_STEP_OUTPUTS`, and submitted with `step_submit`.

**Process contract.** The runner runs each fn under a shim,
`python -m sluice.exec <run_dir> -- uv run --quiet --script <fn_dir>/main.py` — every run
carries this small supervising Python process for its whole life. The shim takes an
exclusive flock on the run dir's `shim.lock` — retrying briefly, so a liveness probe
landing in the gap never makes it refuse — and holds it for its whole life (that lock is
the run's liveness — a pid is never trusted, pids get reused), writes `shim.json`
`{pid, started, argv}` before starting the fn and `child.json` `{pid, pid_start}` right
after (so a fn that outlives its shim can still be found and stopped), and once the fn
exits writes `exit.json` `{code, signal, finished}` — `code` 127 with an `error` when the
fn could not even be started — the only evidence a run is done (`output.json` alone never
is) — then exits as the fn did.
Agent fns also record their tmux server, engine and app-server pids with `/proc` start times
in `native-processes.json`, so the runner can reap them after a fn is killed without cleanup.
The fn runs with stdin = an object keyed by input name (unbound optional inputs are `null`),
stdout to `output.json`, stderr to `stderr.log`; env `SLUICE_HOME`,
`SLUICE_PROJECT`, `SLUICE_STEP`, `SLUICE_RUN_ID`, `SLUICE_RUN_DIR`, `SLUICE_FN_DIR`, and
`PYTHONPATH` containing sluice's `src` dir, plus every `KEY=value` line of `SLUICE_HOME/.env` and
then the project's `.env` (project values win). A step of an open fn also gets, when it has
any, `SLUICE_STEP_INPUTS`, JSON `{name: {"type": T}}` of its extra inputs (their values are in
stdin under their names), and `SLUICE_STEP_OUTPUTS`, JSON `{name: {"type": T, "doc": "..."}}`
of the outputs it declares (`doc` empty when there is none). Secrets live there, never in plans. cwd = the run
dir. `SLUICE_PROJECT` names the project (empty for a call without one); for a call,
`SLUICE_STEP` is empty and `SLUICE_RUN_ID` is the call id. The fn writes one JSON object keyed
by output name to stdout (logs go to stderr) and exits 0. Any other exit code, or outputs that
fail `check_value`, is a failure. Retries and timeouts, if a fn needs them, happen inside the fn
(`run(main, retries=N)`, §7). Each run is its own session, led by the shim: signals sent to the
group reach the fn (the shim ignores SIGINT, SIGTERM and SIGHUP itself and still records how the
fn went). When the runner stops a run (a cancelled step, a removed step, a scattered run whose
supervisor died leaving a live fn behind, or shutdown with `--kill-runs`) it sends the whole process group SIGTERM, then
SIGKILL to whatever is left after 5 s, so nothing started under `uv run` outlives it (SIGTERM
first lets an agent CLI stop tool processes it started in sessions of their own). `sluice
serve` and `sluice loop` exit 0 on SIGINT, SIGTERM and SIGHUP (a closed terminal or `tmux
kill-session`), leaving their runs going for the next runner to adopt (§6); with `--kill-runs`
they stop them first.

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
- **Holding and ordering.** A step may carry `"paused": true` or `"paused": "<reason>"` to
  hold it (§6); `"after": ["<step>", ...]` to wait for steps it reads nothing from (an
  ordering edge: it is ready only once they have succeeded or been skipped, it is never stale
  because of them, and it counts for cycles); `"tags": ["<tag>", ...]` (tags match the id
  pattern, optionally after one `<prefix>:`, as in `unit:lane-1`) to select steps by; and `"when": "<ref>"` to run it only if that value is true.
- **Conditions.** `when` names a step output or plan input of type `boolean` (or `boolean?`;
  `Any` is checked when it runs), read like an input: the step waits for it. Once it is known,
  `true` lets the step run; `false` or null makes it `skipped` (state `skipped: "<ref> is
  false"`) instead of running; any other value fails it (`when: <ref> is 3, not a boolean`).
  A step that reads from a skipped step is skipped too (`step a was skipped`); an `after` edge
  counts a skipped step as settled. A skipped step never ran, so it is decided afresh whenever
  its reason changes: if the value turns true, it goes back to `pending`. A paused step is
  held, not skipped. A step that `plan_patch` or `step_add` adds comes in with
  `"paused": true` unless the call passes `start: true` or the step sets `paused` itself;
  that pause is one more op in the edit's history.
- **Docs.** A plan input is declared by its type, or, as in CWL, by `{"type": <type>, "doc":
  "..."}` (both keys only; no type form has just these keys, so the two never clash). A step
  may carry `"doc": "..."` next to `run`, `in` and `scatter`. Docs are optional strings that say
  what a value or a step is for; `status` returns them (`input_docs`, a step's `doc`), the
  Mermaid view puts a step's doc on a second line of its label, the dashboard shows a step's doc
  as its card's title and an input's doc on its node, and an
  inbox item posted for an input without a body takes that input's doc as its body (§8a).
- **Step inputs** (`in`): `{"default": <json>}` a literal; `{"source": "<ref>"}` one value;
  `{"source": ["<ref>", ...]}` fan-in: an array of the values, in order; `{"file": "<absolute
  path>"}` the file's text (UTF-8), a `string` (an input declared otherwise refuses it at
  validation), read when the step **starts** — each start, a retry's too, so a spec edited
  before the step starts is what runs. A file missing or unreadable then fails the step
  (`input spec: cannot read the file /abs/spec.md: No such file or directory`) without
  starting it; `verify` warns about one missing now. The run's `input.json` holds the text;
  anywhere else the binding shows its path. A ref is a plan input
  name (`repo`) or `<step>/<output>`, optionally followed by `.<field or index>...` to reach
  inside a value. Optional fn inputs may be omitted.
- **Fan-out:** several steps read the same output. **Fan-in:** a list `source`, or several inputs
  from different steps (`core.collect` gathers into one array).
- **Scatter** (dynamic fan-out): `"scatter": "<input name>"`. That input must receive an array
  whose items fit the fn's input type; the step runs once per item (the other inputs are the same
  for every run) and each of its outputs becomes an array, in item order. The step succeeds when
  every run succeeds and fails if any fails; a failed scattered step re-runs only its failed
  items when retried with unchanged inputs (§6).
- **Plan outputs** name the plan's results: `{"source": "<ref>"}`. `fn_call` and `status`
  report them.
- **Typed agent blocks.** A step whose fn is open (§4) may bind **extra inputs** in `in`
  besides the fn's own (`{"source": ...}` or `{"default": ...}`; names follow the id pattern;
  their types come from their sources, §3), and may declare its own **`outputs`**: `{"name":
  <type> or {"type": <type>, "doc": "..."}}`, which join the fn's outputs (a name the fn
  already has is an error). Refs to them validate like any output (field paths included; arrays
  for a scattered step). A non-open fn given either is a validation error. While the step
  runs, its agent submits the declared outputs with `step_submit(project, step, outputs,
  run?)`: checked against the declared outputs (every required one, types fitting, no others;
  `invalid` lists every mismatch with its path), refused unless the step is `running`; `run`
  (a run id) is needed only when the step has several runs (scatter). Accepted outputs are
  kept as the run's submission (a resubmit replaces it), in the same transaction as their
  `step.submit` record. When the fn exits 0, the runner merges them into the step's outputs (the
  fn's returned values win on a name they share); a required declared output never submitted
  fails the step with `declared outputs not submitted: <names> (the agent must call
  step_submit ...)`. An unsubmitted optional one is null.
- **Work done outside sluice.** A step running the built-in `core.external` (open, no
  inputs or outputs of its own) stands for work that happens elsewhere: a person, another
  orchestrator's workers, a CI pipeline. The runner never starts it: once ready it stays
  `pending` (`waiting`: `external: set its outputs with step_set_output`, §8) until its
  outputs are set by hand (`step_set_output`, §6) or it is cancelled (`step_cancel` fails a
  pending `core.external` step at once). It declares the outputs it will get and may bind
  extra inputs, which order it after their sources (as any open fn's step). It does not
  scatter (a validation error: it is one piece of outside work), and `fn_call` refuses it.
  Its doc says who is doing the work and where; the dashboard shows it as live outside work
  (§8). **Moving a step's work out** needs no tool of its own: patch the step's `run` to
  `core.external` and retry it. Its bindings stay valid (the old fn's own inputs, now unknown
  to `core.external`, become extra inputs, typed by their sources: a default is `Any`), and so
  do its declared outputs; a step that reads an output of the old fn needs that output
  declared too (`steps.b.in.x: step w (fn core.external) has no output results` otherwise),
  and a scattered step drops its `scatter`. For a failed `agent.run` step `w` whose
  dependents read `w/final`:

  ```json
  [{"op": "replace", "path": "/steps/w/run", "value": "core.external"},
   {"op": "add", "path": "/steps/w/outputs/final", "value": "string"},
   {"op": "replace", "path": "/steps/w/doc", "value": "Fanned out to five lash workers; the orchestrator sets final when they land"}]
  ```

  then `step_retry(project, ["w"])`: `w` is pending again and waits, and
  `step_set_output(project, "w", {"final": ...})` settles it once the work lands, so its
  dependents run.
- A step is **ready** when every plan input and step it reads has a value / has `succeeded`.
- **Units.** The plan's units are the connected components of its steps over every edge
  (handoffs, `when`, `after`; a plan input shared by two steps is no edge), in plan order (by
  their first step), each its steps in plan order (`plan.units`): the independent pieces of
  work. No edge joins two units. A unit is **done** when every step in it succeeded (set by
  hand too) or was skipped, with at least one success. `status` and `plan_view` leave the done
  units out by default (§8), the dashboard groups its board's boxes by unit and folds a done
  one, and `plan_prune` removes them (their outcomes stay, §6).

**Recipes.** A step shape used again and again (a lane: a worktree, an agent working in it,
the worktree removed) is a recipe: `recipes/<name>.json` in `SLUICE_HOME` or in the project's
directory (§2; the project's wins on a name clash), shaped `{"name", "doc"?, "params"?: {<name>:
<type or {"type", "doc"}>}, "steps": {<step id>: <step>}}` with `name` the file's name. Params
are typed like plan inputs (§3); `unit` (a string matching the id pattern) is always a param,
implicit. Substitution is deliberately tiny: in every step id and every string anywhere in the
steps (object keys too), `{param}` is replaced by the param's value (a non-string as its JSON);
a string that is exactly `{param}` becomes the value itself, so a non-string param keeps its type
(an optional param left out is null). `{{` and `}}` are literal braces. An unknown `{x}` or a
lone brace is an error naming where it is. Nothing else: no loops and no conditionals (`when`
and `scatter` already exist in steps). `unit_add(project, recipe, params, start?)` checks the
params against their types (every required one, no others), expands the recipe, tags every new
step `unit:<unit>` before its own tags, refuses ids the plan already has, and adds the steps in
one edit at the current rev (no rev argument: it is an add, like `step_add`); unless `start`,
they come in paused. `recipe_list(project)` lists the recipes the project sees; a broken recipe
file (bad JSON or shape, a name that is not the file's, a bad param type, an unknown `{x}`) is
listed with its `error` and never stops the others. For example, `recipes/lane.json`:

```json
{"name": "lane",
 "doc": "One unit of work: a worktree, an agent working in it on a spec read from a file, then the worktree removed",
 "params": {"repo": "string", "base": "string", "spec": {"type": "string", "doc": "Absolute path of the spec file"},
            "engine": {"type": "enum", "symbols": ["devin", "codex", "claude"]}},
 "steps": {
   "{unit}-fork": {"run": "git.worktree", "doc": "Cut a worktree for {unit}",
                   "in": {"repo": {"default": "{repo}"}, "base": {"default": "{base}"},
                          "branch": {"default": "work/{unit}"}}},
   "{unit}-work": {"run": "agent.run", "doc": "Do {unit} in its worktree",
                   "in": {"engine": {"default": "{engine}"}, "cwd": {"source": "{unit}-fork/path"},
                          "spec": {"file": "{spec}"}},
                   "outputs": {"landed": {"type": "boolean", "doc": "Whether the change landed"}}},
   "{unit}-cleanup": {"run": "git.worktree_rm", "doc": "Remove {unit}'s worktree",
                      "in": {"repo": {"default": "{repo}"}, "path": {"source": "{unit}-fork/path"}},
                      "after": ["{unit}-work"]}}}
```

`unit_add("p", "lane", {"unit": "fix-login", "repo": "/src/app", "base": "origin/main", "spec":
"/specs/fix-login.md", "engine": "devin"})` adds `fix-login-fork`, `fix-login-work` and
`fix-login-cleanup`, tagged `unit:fix-login` and paused.

**Validation** (every edit must pass; all errors returned with paths): ids valid; docs are
strings and an input's object form has a `type`; every `run`
exists (in the project's lookup order); every required fn input bound, no unknown inputs (extra
inputs and declared outputs only on an open fn's step, no declared output named like one of
the fn's); every ref names a declared plan input
or an existing step and one of its outputs (its fn's or those it declares; fields navigated
through record types, anything under `Any` allowed; a scattered step's outputs are arrays); `fits` holds for each source (for a
list source, the target must be an array or `Any` and each element must fit its item type; for
the scatter input, each item must fit the fn's input type); defaults and plan input values pass
`check_value`; the graph is acyclic.

**Edits.** `patch(rev, ops, reason, author)`: `ops` is RFC 6902 JSON Patch against the plan
without `rev`. A stale `rev` fails with `conflict` (and the current rev). A valid edit bumps
`rev`, replaces the plan, and appends a `plan.edit` record `{"rev", "author", "reason",
"ops"}` to the project's log, also kept (with the record's seq) in the edit history,
`plan_edits` (creation is rev 1, one `add` of the whole plan). Removing or changing a running
step is refused. An edit that removes steps (`plan_patch`, `step_remove`, `plan_prune`: any
edit whose plan no longer has them) keeps the outcome of each one that finished in `outcomes`
(§6), in its own transaction.

## 6. Runner and state

A project's state (its `states` row):
`{"inputs": {"<name>": <value>}, "steps": {"<id>": {"status", "run_ids", "started", "finished",
"outputs", "error", "manual", "inputs_hash", "skipped", "results", "kept"}}}` with status `pending`, `running`,
`succeeded`, `failed`, `stale`, `skipped` (`skipped` holds why). A
scattered step also records `done` (the runs that have ended) and `total` runs. There is no limit on how many run at
once: every ready step starts, and a scattered step starts all its runs; a scattered run that
fails does not stop the others — the step ends once every run has ended: `succeeded` if all
did, else `failed` with `run <i>: <err>` for one failed run, `<n> of <total> runs failed:
run <i>: <err>; ...` for several (each `<err>` cut to one line of at most 200 characters),
and keeps `run_ids` (index-aligned: `run_ids[i]` is item i's run) plus `results` — a list
holding each item's outputs where it succeeded and null where it failed — so a retry can
re-run only what failed.

**Outcomes.** A step's entry leaves the state once the step leaves the plan (the runner drops
it at its next tick), and its run dirs go once nothing refers to them (§6b). So the plan edit
that removes a step whose status is `succeeded`, `failed`, `skipped` or `stale` writes, in the
same transaction, one `outcomes` row: `project, step, rev` (the rev that edit made; the key),
`unit`, `fn` (its `run`), `status`, `outputs` (JSON), `error` (a failure's error, or why it was
skipped), `started`, `finished`, `run_ids` (JSON), `manual` (0 or 1), `removed` (when), and the
edit's `author` and `reason`. `unit` is the name in the step's `unit:<name>` tag when it has
one, else the first step of its unit (§5 `plan.units`) in the plan before the edit when that
unit has more than one step, else null. A step still pending (it never ran) leaves no row, and
a running one cannot be removed. The rows are never trimmed; deleting the project deletes
them. Read them with `query` (§8).

**Staleness.** A result is only valid for the inputs it was computed from. When a step starts
(and so when it succeeds) or is set by hand, its state records `inputs_hash`: a hash of the
canonical JSON of the inputs it binds (for a scattered step the whole array; an optional input
it leaves unbound is not in it, so a fn gaining an optional input leaves the steps that already
ran alone; a `file` binding as `{"file": <path>}`: its path, not its content, so editing a file
after its step succeeded does not make the step stale, and the new text runs only when the step
runs again), or null for a step set by hand with `force` while what it
reads was not ready ("inputs unknown"). Each tick, in dependency order, a `succeeded` step becomes
`stale` when a step it reads is stale, or when its inputs are all available and hash differently
(an upstream re-ran with a different result, a plan input changed, its bindings were edited, or
a null hash once everything it reads is there). While an upstream is re-running the step keeps
its status: it turns stale only if the new result differs. Stale steps keep their outputs for
inspection but never re-run by themselves, and steps reading them wait (they are not
`succeeded`). A stale step whose inputs hash as recorded again (the upstream came back to the
same value) is `succeeded` again. `step_retry` re-runs a stale step; `step_set_output` accepts
its result by hand again. `status` and the views show stale steps distinctly. A succeeded step
with no `inputs_hash` at all (state written before hashes existed) adopts the current hash.

Loop (every ~1 s, and right after an in-process edit), over all projects:
1. New steps get `pending`. State entries of steps removed from the plan, and values of plan
   inputs removed from it, are dropped.
2. Finished runs — `exit.json` written (§4): code 0 with valid outputs → `succeeded` with
   `outputs` (for a step that declares outputs, merged with what its agent submitted, §5);
   otherwise `failed` with `error` (exit code, type errors or declared outputs not submitted,
   plus the stderr tail). A scattered step collects its runs as they finish; a run's failure
   is its item's and does not stop the others (above).
3. Mark stale steps (above), settle `when` (§5: skip what its condition or a skipped input
   rules out, and put back to `pending` a skipped step whose reason no longer holds), then
   start every ready `pending` step (what it reads is there, what it runs `after` has
   succeeded or been skipped) that is not paused (a step's `paused`, or its project's
   `paused`: it stays `pending`, whatever it would read held, until
   unpaused; pausing never stops a running step). A ready `core.external` step is never
   started (§5): it stays `pending`, with no process, run dir or record, and counts as no
   work to start. Built-in fns run inline;
   staleness is re-checked after each round of inline results, so nothing starts from a result
   that no longer holds.
4. If anything changed, write the state and a `step.status` record per step whose status
   changed in this pass (§6b), together.
5. Calls: start the `pending` calls and collect the `running` ones (the `calls` rows), each
   status change written to the call's row with its `call` record (§6b).

Processes are never started, stopped or waited on inside a write transaction: a tick (1) reads
what finished (`exit.json`, `output.json`, stderr, the run's submission) outside any
transaction; (2) in one short write transaction per project applies it, drops what left the
plan, settles steps, runs built-ins inline and **reserves** each ready step's launch — `running`
with fresh run ids in `run_ids` (a kept scattered item keeping its own) and its `step.status`
records, committed before any of its processes exists; (3) outside the transaction stops the
steps `step_cancel` flagged (the flag stays until the stop is done) and, for each reserved
run, makes its dir and starts its shim. Just before each run's start (each step, each
scattered item) its entry is read again: a step cancelled since its reservation — or a run
its entry no longer lists — starts nothing more. A cancel committed between that read and the
start is not seen by it: the run starts, and the next tick stops it, as the flag stays until
the stop is done; (4) records in a second short transaction what (3) did: a cancelled step fails
`cancelled[: <reason>]`, and a start that raised fails its step — or, scattered, just that
item — with `could not start the fn: <error>`. A call is reserved the same way (its row
`running`, committed) before its process starts. A `busy` database skips that project until
the next tick; nothing that has started a process is ever retried.

On startup — under `runner.lock`, as its first tick reaches each project — the runner adopts
what a previous one left. For every step entry still `running` and every `running` non-direct call it
looks at each run dir (a step's `run_ids`; a call's is `runs/<call>`): no dir at all → the run
was reserved but never started (its dir is made before its process) and fails `not started
(the runner stopped before it started the run)` — the step, or just that item of a scattered
one (a retry then re-runs it alone); it is never started automatically, so no agent runs
twice; `exit.json` → the run
is finished from it (its `code`, then §6 step 2 decides outputs or error); a held `shim.lock`
→ the run lives on and is watched; an entry `running` with no `run_ids` at all, or a run dir
without `shim.json` (both from before this contract) → the step fails `runner restarted`; a
free lock with no `exit.json` → `run outcome unknown (its supervisor died)` — never an
invented exit code — and a `child.json` that still names a live fn process then has its
process group killed, so a retry never runs two agents. For a scattered step a finished-but-failed
or unknown run is that item's failure: adoption rebuilds the same per-item picture (finished
items keep their results, live ones are watched) and the step still ends once every run has
ended. A run a `--kill-runs` shutdown
stopped is adopted the same way: usually `finished` off the `exit.json` its shim still
wrote (`exit code -15`), `unknown` when the shim went down with it — never `runner
restarted`. Adoption kills use the recorded shim pid only while its lock is held and it
still leads a live process group, so a reused or tampered pid's group is never signalled;
`step_cancel` and a step removed from the plan stop an adopted run like one this runner
started — and a running entry no Active could be built for is still killed by its recorded
shim pids. The runner kills a native session's recorded tmux server and engine process trees
when it stops, adopts or finishes the run, checking each pid's `/proc` start time before
signalling it. Each adopted run appends `run.adopt` `{step or call, run, outcome}`
(`watching`/`finished`/`unknown`/`restarted`/`not started`), and a run dir whose shim lives —
or whose recorded fn child lives on past it — but which no running step or call references is
killed and logged `run.orphan` `{run}` — only when something was actually signalled.

**Run-dir GC.** At startup and about once a minute the runner removes every run dir nothing
references and nothing runs in: a dir stays while a retained record names it (its `run` or
`call`, or a `step.status` record's `run_ids`), a state entry lists it (`run_ids` or
`kept.run_ids`), a `calls` row or a submission is its, its `shim.lock` is held or a recorded
fn child still lives. Each log's dirs are listed before its references are read; since a run
is referenced before its dir exists, a listed dir nothing references never gains a reference
again. The pass also finishes every deleted project's directory removal still pending (each
`deletions` row: its directory moved to `trash/`, removed, then the row deleted; idempotent)
and removes whatever else `trash/` holds. A `direct` call (§8 `fn_call`) is
run by the process that made it, never by the runner; if that process dies before logging the
end, the runner logs the call `failed` with `error: "the process running this direct call is
gone"` — the pid is checked with its recorded start time, so a reused pid does not pass for it.

**Manual values** (recorded in state and as log records with the current `rev`, `author` and
`reason`, so the history shows who set what; a manual status change also gets its `step.status`
record):
- `plan_set_input(name, value)`: sets a declared plan input (type-checked). Steps reading it
  become ready. Changing it later makes steps that already read it `stale`. Record `plan.input`
  `{name, value}`.
- `step_set_output(step, outputs, force?)`: marks a non-running step `succeeded` with the given
  outputs (type-checked against its outputs, its fn's and those it declares; arrays for a
  scattered step), `manual: true`.
  For manual work, a failed step whose result is known, or a stand-in. Refused (`invalid`, its
  `errors` naming each: `step a is pending`, `plan input n has no value`) while any step it reads
  from has not succeeded or any plan input it reads has no value, unless `force: true`
  (deliberately bypassing a broken upstream; the step then records unknown inputs and turns
  `stale` once those values are all there). It is never run afterwards unless retried. Record
  `step.output` `{step, outputs, force?}`.
- `step_retry(steps?, tags?)`: sets the selected steps, each `failed`, `stale` or manual, back to
  `pending` (refused, changing nothing, unless every one is); a `core.external` step goes
  back to waiting to be settled. A failed scattered step with
  `results` goes back keeping `{inputs_hash, run_ids, results}` under `kept`: when it starts,
  an unchanged inputs hash and one kept result per item mean the runs that already succeeded
  are not re-run (their kept run ids stand in `run_ids`); a different hash or count drops
  `kept` and runs every item as usual. Their succeeded dependents turn
  `stale` when they produce a different result. Record `step.retry` `{step}` per step.
- Setting a step's input by hand is an edit: `step_set_input(step, input, value)` patches its
  binding to `{"default": value}`.
- `step_cancel(steps?, tags?, reason?)` on a pending `core.external` step (§5) fails it at
  once with `cancelled: <reason>` (`cancelled` without one) and appends its `step.cancel`
  and `step.status` records, so an abandoned outside job reads like any cancelled step (a
  running step is failed so by the runner, which stops it first). `step_retry` puts it back.

**Built-in fns** (in `src/sluice/fns/`, run inline):
- `core.echo`: inputs `{"value": "Any"}`, outputs `{"value": "Any"}`.
- `core.collect`: inputs `{"items": "Any[]"}`, outputs `{"items": "Any[]"}`. The fan-in join.
- `core.format`: inputs `{"template": "string", "values": "Any"}`, outputs `{"text": "string"}`.
  Python `str.format`: an array fills `{0}`, `{1}`...; a record fills `{name}`. Non-string values are
  rendered as JSON. Builds prompts from upstream outputs.
- `core.external`: open, inputs `{}`, outputs `{}`, no `main.py`; never run, inline or
  otherwise: a step of work done outside sluice (§5).

## 6a. Verify

`verify(project?)` checks everything and returns every problem it finds, each with a location
and a message; it changes nothing. `where` is a file path (relative to `SLUICE_HOME` when inside
it, e.g. `projects/p/fns/x.y/fn.json`, followed by `#<path in the document>` for JSON or
`:<line>` for `.env` files), or a project's plan or state with the path in it
(`project p: plan#steps.a.run`, `project p: state#inputs.n`). Without a project it checks the
built-in and global scopes and every project; with one, the built-in and global scopes and
that project. It covers:
- every `fn.json`: shape (`name`, `inputs`, `outputs`, optional `doc`, boolean `open` and,
  for an open fn, `submits`; nothing else), the name
  matching its directory, every type parsing, `main.py` present for non-built-ins;
- name collisions across scopes (see §2);
- `.env` files parsing as `KEY=value` lines (blank lines, `#` comments and `export `
  allowed);
- the plan: full validation (§5) against the project's functions (a plan can stop validating
  when a function it uses changes), and a warning for each `file` binding whose file is not
  there (or not readable) now;
- the state agreeing with the plan (no state for unknown steps or undeclared plan inputs,
  valid statuses, outputs of succeeded steps passing their output types, plan input values
  passing their types).

The database's own constraints (§2) keep the shapes of its rows. Without a project, a directory
under `projects/` that is no project's is a warning, not a problem (left over, or prepared with
`fns/` and `.env` before its project is created).

`{"ok": bool, "problems": [{"where", "message"}], "warnings"?: [{"where", "message"}]}`. CLI
`sluice tool verify '{"project": "P"}'` prints them and exits non-zero when there are problems.

## 6b. The log

Each project has one append-only log, and the home has one more for the calls made without a
project: the database's `records` (§2). Every record is `{"seq", "at", "kind", ...}`. `seq` is
the home's: it increases across every log and a committed one is never reused, so a log's seqs
increase but have gaps (another log's records). Writers in any process (the store, the
runner, a fn process posting to a thread) get distinct, increasing seqs. Kinds:

| kind | fields | written by |
|---|---|---|
| `plan.edit` | `rev, author, reason, ops` | every accepted edit (§5) |
| `plan.input` | `rev, author, reason, name, value` | `plan_set_input` |
| `step.output` | `rev, author, reason, step, outputs, force?` | `step_set_output` |
| `step.retry` | `rev, author, reason, step` | `step_retry` |
| `step.cancel` | `step, author, reason` | `step_cancel`: the runner then kills the step and fails it with `cancelled: <reason>` (a pending `core.external` step fails at once) |
| `step.submit` | `step, run, outputs, author?` | every accepted `step_submit` (§5) |
| `step.status` | `step, from, to, error?, run_ids?` | every status change of a step: the runner, once per state write (`from` is the status at the previous write, so a built-in finishing inline goes `pending` → `succeeded`; a new step's `from` is null), and the manual tools; `error` when it failed, `run_ids` when it finished |
| `call` | `call, fn, status, inputs?, outputs?, error?, direct?, pid?, pid_start?, author?` | every status change of a `fn_call`; the pending record (a direct call's first) carries the `inputs` and the `author`; a direct call's running record also its `pid` and `pid_start` |
| `message` | `thread, from, to?, body, data?` | `thread.post` (§10) |
| `inbox.post` | `item, title, from?, input?` | `inbox_post`, `inbox.ask` (§8a) |
| `inbox.answer` | `item, answer, by` | `inbox_answer` and the dashboard's answer route |
| `inbox.close` | `item, reason?, by` | `inbox_close` (`by`, like `inbox.answer`'s, is the author) |
| `project.pause` | `paused, reason?, author` | `project_update` (or `drain`, `release`, the dashboard's Pause) that changes `paused` |
| `project.archive` | `archived, reason?, author` | `project_update` (or the dashboard's Archive) that changes `archived` |
| `project.update` | `fields, reason?, author` | `project_update` that changes the description and/or the icon: `fields` names them |
| `run.adopt` | `step or call, run, outcome` | the runner, once per leftover run: what its dir showed (`watching`, `finished`, `unknown`, `restarted`, `not started`, §6) |
| `run.orphan` | `run` | a live run nothing referenced, killed at startup (§6) |

**Authors.** Every write tool (§8) that leaves a record names who made the change, in the
record's `author` (`by` in the inbox's): the tool's `author` argument when given (not blank);
else the `SLUICE_AUTHOR` environment variable of the process running the tool; else
`step:<id>` when `SLUICE_STEP` is set (an agent inside a step calling `sluice tool`); else the
MCP client's name from the session's `initialize` (`clientInfo.name`); else `mcp` over MCP and
`cli` from `sluice tool`. The dashboard writes as `dashboard`, `sluice drain` as `drain`.
`update_project` records what it changed in the same transaction and nothing when nothing did.

The log is history, not the source of truth, so each log is capped at `config.log_max`
records (default 10000, counted per log): when an append takes one past the cap, its oldest
records are dropped in the same transaction, down to 90% of the cap (so a full log is not
trimmed on every append), together with its finished calls and the submissions that no
remaining record or state entry refers to any more. A pending or running call is never
dropped: its row, not its records, is its truth. The trim removes rows only; the runner's GC
(§6) removes the run dirs nothing references. The plan's edits are kept whole in `plan_edits`,
so `plan_history` reaches back to rev 1 (to the earliest rev a home imported from older
storage had); its manual values (`plan.input`, `step.output`, `step.retry`) as far as the log
does.

`last_seq` is a log's high-water mark (its greatest seq, from the same snapshot as the records),
never moved back past a `since_seq` given. `log_read` and `log_wait`
(§8), `thread.wait` and `sluice watch` share one filter: `kinds` (exact kinds, or a group name,
`step`, `plan`, `inbox`, `run` or `project`, for every kind under it) and `threads` (messages only on these threads; given
without `kinds`, only messages at all).

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

`run(main, retries=0, backoff=30)` reads stdin, calls `main(inp, ctx)` (`ctx`: `project`, `step`,
`run_id`, `run_dir`, `home`, `fn_dir`, `attempt`, `log(msg)`, and for an open fn's step
`extra_inputs` `{name: {type}}` and `outputs` `{name: {type, doc}}` from the env of §4, else
empty) with stdout redirected to stderr,
prints the result as JSON. On `Transient` it sleeps `backoff` s (env `SLUICE_BACKOFF` overrides)
and calls `main` again, up to `retries` times; any other exception, or running out of retries,
prints the traceback and exits 1. `sh(argv, cwd=None, check=True, env=None, timeout=None,
input=None)` runs a command and raises `ShError` on a non-zero exit when `check`.
`sh_stream(argv, on_line=echo_line, cwd=None, check=True, env=None, follow=None, input=None)`
does the same (`input` is written to the command's stdin) but calls `on_line(line, source)` for each line as it arrives (`source` `stdout`, `stderr`, or
`follow` for lines appended to the file `follow`); the default echoes each line to stderr, cut
to 200 chars, so a long-running tool shows live progress in the run's `stderr.log`.
Both run the command in `child_env(env)`: the fn's environment with `PATH`, `PYTHONPATH` and
`VIRTUAL_ENV` as they were before `uv run` and sluice set them up for the fn's own interpreter
(the runner passes the originals as `SLUICE_HOST_*`; without them the fn's environment is
stripped out), so a tool the fn starts runs the host's `python3`, not the fn's isolated one.

## 8. MCP server

`sluice serve` runs the runner and an MCP server (official `mcp` SDK, streamable HTTP) at
`http://<host>:<port>/mcp` in one process. With `--no-runner` it serves only, and a separate
`sluice loop` runs the steps: the two share nothing but the home (the database and the run
dirs; the runner polls about once a second), so the server can restart without ending running
steps. Stopping the runner leaves
its runs going — a later runner adopts them (§6) — unless it was started with `--kill-runs`.
A `--host` that is not loopback warns loudly on stderr: the tools (fn_save, fn_call — running
code) are served without authentication to anyone who can reach the port. Errors are tool
errors whose message is JSON
`{"error": "not_found"|"conflict"|"invalid"|"bad_request"|"busy", "message", ...}` (`conflict`
carries `current_rev` for a plan edit, or `status` for an inbox item that is no longer open;
`invalid` carries `errors`; `busy`: the database stayed locked past its timeout and nothing was
written, try again). `rev` is optional on the convenience tools (they apply to the current
revision in the same transaction) and required on `plan_patch`.

**Docs for agents.** The server sets MCP `instructions` from `src/sluice/docs/instructions.md`
(short: what sluice is, the workflow, where to read more). A `docs(topic?)` tool returns the index
(topic names with their first heading) without a topic, or `src/sluice/docs/<topic>.md`. The same
pages are MCP resources at `sluice://docs/<topic>`. Tool docstrings describe every argument.
Validation errors carry the path and what was expected.

**Views.** A dashboard that only reads, with three exceptions: answering an inbox item (§8a),
archiving a project, and pausing or resuming a project or a step. On a loopback bind every
route refuses (403, plain text) a request whose `Host` does not name this machine
(`127.0.0.1`, `localhost`, `[::1]`); a deliberate non-loopback bind lifts that, and the
write routes still refuse a foreign `Origin` (DNS rebinding satisfies `Origin == Host`).
Server-rendered HTML with inline CSS (`static/dashboard.css`; Sluice Light and Sluice Dark
via `prefers-color-scheme` unless the settings menu chose a theme, usable at phone width,
keyboard reachable), every page on one centred
column that the top nav's content shares, one nav and no second row: a project switcher whose
button is the chosen project's name ("All projects" when none; its menu lists the projects, the
archived ones last), each name led by the project's icon when it has one (§2), then that scope's
sections (a project's Plan · Log · History · Functions,
or Projects · Log · Functions), the current one marked (`aria-current` and a bar, not colour
alone; a step's page is inside Plan), then Inbox and, at the right end, the settings cog, every value HTML-escaped (plans, logs, run output and inbox items are
untrusted). The Inbox link carries the count of open items across all projects as the
dashboard's one coral badge (none when nothing waits); coral, the logo's, is spent on nothing
else, and a failure is never coral or red. A step's status is a
drawn glyph (dashed ring pending, spinning ring running, check succeeded, ring and dot set by
hand, circular arrow stale, cross failed, ring with two bars paused, dashed ring with a slash
skipped, an arrow leaving a box external) with its word for assistive technology, never colour
alone. Every script the dashboard runs is served by sluice from `static/`: its own, and
vendored copies (the version in each name) of Datastar v1.0.4 (`datastar-rocket-1.0.4.js`)
and, on inbox pages, `@openuidev/lang-core@0.3.0` (jsDelivr's ESM build, with its imports of
`zod@4.6.5` and `ci-info@4.4.0` rewritten to the vendored files next to it), so no third-party
script runs with the dashboard's origin, which can reach `/mcp`. The only external assets are
two font stylesheets from cdn.jsdelivr.net (`@fontsource-variable/archivo@5.3.0/wdth.css` for
display, `@fontsource-variable/public-sans@5.3.0` for text; the system sans without them). The
nav's brand is the owner's mark (`static/logo.svg`) beside the wordmark "sluice" as live text,
one link to the index; every page links the same mark as its icon (`static/favicon.svg`). Markdown bodies are rendered on the server by `markdown-it-py` (CommonMark plus tables,
raw HTML escaped, unsafe link schemes refused).
- Mermaid (`flowchart LR`, `plan_view`'s text format for agents; the dashboard does not use it):
  plan inputs as rounded nodes, steps as boxes labelled
  `id / fn / status` (a scattered step shows `done/total`; a step's doc, one line of at most 60
  characters, below it), plan outputs as rounded nodes, an edge
  per source ref labelled with the output name, one colour class per status (pending grey,
  running blue, succeeded green, failed navy ink with a heavy border, stale gold, manual
  outlined; a stale manual step
  shows as stale).
- **What waits on a person is the inbox alone** (§8a): its open items, counted by the nav's
  coral badge. The orchestrator posts there whatever it needs from a person. Failed steps,
  missing inputs and messages between agents are the orchestrator's: they show on the board,
  in the summary line and on the Threads tab, never as a call to the person.
- `GET /projects/<name>/threads` (**Threads**, a tab of the project): every conversation of
  the project, one per thread, the latest first, each a `<sluice-thread>`. A thread shows its
  step (glyph, id, doc) or its name, how many messages, when the last came and a line of it; it
  opens to the messages, each with sender → recipient, when, and its body (markdown rendered, a
  long one folded); all but the last three fold under "n earlier messages" (none from its first
  open question on). A step's messages sit on the left, everyone else's indented. A message that
  asks for a reply (`needs_reply`, true unless the sender marked a note) from someone other
  than a step of the plan, with no later message from that addressee on the thread, is marked
  "Awaiting reply" (gold, not coral) and keeps its thread open, but on a step's thread only while
  that step is in the plan and has not succeeded, failed or been skipped (then nobody waits on
  the answer); a note is marked "note". A thread whose step has left the plan shows its id and
  "no longer in the plan". The
  component counts the messages this browser has not seen ("n new", from localStorage; a
  thread never seen counts as read), marks them while the thread is open, and opens the thread
  the address names (`#th-<thread>`). A step's detail links to its thread (`step-<id>`) with
  its count of messages and of open questions. Markdown anywhere on the dashboard (a spec, a
  message, a value) has its top heading shifted to an h4, under the page's own headings.
- **What is stuck.** A pending step that a failed step holds up, directly or through other
  pending steps, is **blocked** (a paused one counts as paused instead). A project with failed
  steps leads its page and its index row with one line: the failed steps (the first two, each
  a link to its detail, then "n more"), "failed", how many steps they block and how many are
  paused, after "Stopped:" when nothing is running, in sluice or outside it (a ready
  `core.external` step, which is never named there) (`Stopped: a and b failed, blocking 4
  steps · 11 paused`). It reports; it does not ask (not red, not the inbox). The browser tab's
  title leads with `n failed ·` and then `n quiet ·` (running steps gone quiet; the
  project's, or on the index every active project's), kept current as the page's parts
  update and as runs age. A Log or History tab is titled `Log · <project>` or
  `History · <project>`.
- `GET /`: one row per active project, and the archived ones folded under
  "Archived (n)"; each row: the project's status glyph, icon and name, the line above when steps
  failed, description (two lines), a progress bar by status with "n of m"
  succeeded, what is running now (each running step's title and running time, and its
  `quiet 40m` badge as on its card) or why nothing is, and the last activity (the later of the last log record and the last state write).
  When the runner's heartbeat (`SLUICE_HOME/runner.json`'s `beat`) is older than 15 s, the
  index and each project page's summary line say so in the attention voice ("Runner stopped ·
  last seen …"); no heartbeat file says nothing (a runner from before it writes none). The
  streams carry the liveness, not the beat.
- `GET /projects/<name>`: under the nav naming the project (its sections: Plan, Threads,
  History — the log page filtered to the history kinds, which reads the plan's whole edit
  history, every edit back to rev 1, with the manual values the log still has, in seq order —
  Log, and Functions, the functions as the project sees them), first whether the work moves: a progress bar by status and one line
  (succeeded of total, skipped, running, stale, failed, blocked, paused, total `cost_usd`, last
  activity; the failed, blocked and paused left to the stuck line when it leads the page; the
  bar's label counts them all) with the Pause
  and Archive switches; then the description (markdown, folded to its opening, then "Show
  more"; a paused or archived project says so), then the **board**. Its **lanes** are the
  steps joined by handoffs (an edge that carries a value; `after` only orders), so independent
  pieces of work stay together. The steps any edge joins (a handoff, a `when` or an `after`: a
  unit, §5) are one independent piece of work, its own quiet box when there are several; the boxes wrap, in
  the plan's order, and no edge crosses between them. A box is rows by dependency depth
  (`after` counts) from its first step; in a row its cards stand lane by lane, and a row too
  wide wraps within itself. A lane's cards stay together: a lane that would crowd a row it
  shares past the box's width starts below the lanes before it (never above a step it runs
  after), and a lane keeps its side of the box from row to row. On a phone a box stacks its lanes one after another, and the
  board draws no edges. A box of several steps that have all succeeded (skipped ones count
  when the rest succeeded) folds to one line (`<details>`: the success glyph, the first step's id, "… last step
  · n steps", with "n succeeded, m skipped" when some were skipped; on a phone the first id in full and the count under it,
  without the last id), which opens to its cards; open, it stays open through live
  updates and, per browser tab, a reload. No edge joins two boxes, so a folded box hides only
  its own edges. With several boxes, a toolbar above the board orders and filters them, a
  plain GET form whose choices live in the query, so a reload, Back and a shared link keep
  them (the defaults leave the address clean; any other spelling of a choice is sent on, 303,
  to its clean query; an unknown `order`, `show` or `steps` is a 400). The order (`order`) is live
  first by default: each box ranks by its most urgent step, 1 attention (failed; running but
  quiet; asking in an open inbox item, whose `from` is the step; pending on a plan input
  with no value), 2 running (or its work going on outside sluice, a ready `core.external`
  step), 3 ready (pending or paused), 4 held (blocked by a failure, or
  stale), 5 done (succeeded or skipped), the plan's order within a rank, so a box moves only
  when its rank changes; `?order=plan` is the plan's order. `?show=` filters by rank:
  `active` (not done), `attention` or `done` (default all); `?tag=<tag>` (a select, shown
  when the plan tags steps) keeps the boxes with any step so tagged. Each choice of what
  shows counts the boxes it would show within the tag and the steps shown. **What can't run**
  is hidden by default (`?steps=all` shows every step; a segmented Runnable · All steps,
  shown when some step can't run): every skipped step (it never runs), and each step that has
  not run (pending or stale) with a step upstream of it, through handoffs and `after`, that
  failed, that is stale (the runner re-runs a stale step only on `step_retry`), that is
  pending and paused (in the plan; a project's pause does not count), that is pending on a
  plan input with no value, or that can't run itself. The failed, stale, paused or waiting
  step is where a person acts, so it stays unless something above it holds it too;
  and a step behind one that can't run can't run, so no step left waits on a hidden one. A
  ready `core.external` step is none of these: its work goes on outside sluice and the
  steps behind it run once it is settled, so they stay. The
  boxes stay the plan's pieces of work, their cards laid out again without the hidden ones
  (rows, lanes, wrapping) and their edges dropped; a box left with none goes; a finished box's
  folded line still counts all its steps. A step that hidden steps wait behind says how many
  in its small line (`+12 behind`). The stuck sentence, the bar and the index count every
  step. Boxes a filter hides and steps it hides are said in one quiet line at the toolbar's
  end ("9 done boxes hidden · show", "14 steps that can't run hidden · show", "1 done box and
  6 steps that can't run hidden · show", its link showing them); their edges go with them;
  with none left, the board says so ("No step can run." when only steps went). Without
  JavaScript the form
  has an Apply button; with it a choice applies at once, keeping the open step's `#step:`.
  The page's `board` signal holds the query, so its stream renders the board the same way;
  each box's id is its first step's (`box-<id>`), so a live update moves a box whole (open,
  folded, with the drawer's ring). A board of one box ignores `order`, `show` and `tag`; its
  toolbar, only when some step can't run, holds the steps choice alone. A `#step:` address of
  a hidden step still opens its drawer. The standalone page (`plan_view` html) shows every
  step.
  Inside a lane, each row is sorted by where its
  neighbours sit, a few sweeps down and up, so
  edges seldom cross. Each step is a compact bubble: its status glyph, its id and, small, how
  long it ran (live while running) and `done/total` for a scattered step; its tooltip is its
  doc and what it says now (a running step's last non-empty stderr line, a failed step's
  error, what a pending step waits on, "its inputs changed" when stale). A running step that
  has written nothing for 15 minutes (the newest stderr.log mtime of its runs that have not
  finished, else their run dirs') is quiet: its card, its index row and its drawer's title
  wear a `quiet 42m` badge in the attention voice, kept current to the minute (and nothing
  more: its tooltip stays its last output, and its progress shows the tail). A blocked step's card says `blocked`.
  A ready `core.external` step (§5) is live work outside sluice, not idle pending: its own
  glyph in the running blue, the running card's blue border, and `outside · 2h 5m` where
  its time would be, the time since it became ready (the latest `finished` among the steps
  it waits for, kept current; none: `outside` alone); before it is ready it is a pending
  step like any other, and it counts as pending in the summary line and the bar. The index
  row of a project with nothing running says `Waiting on n steps done outside sluice.` A failed step's
  line (tooltip, log summary, the head of its Error) is its error's last non-empty line,
  where a traceback names the exception, in sluice's words: without a leading exception class
  (`sluice.fn.ShError: `), the home directory as `~`, and an exit code of 128 + n (or -n)
  explained by its signal (`exited 143 (terminated: SIGTERM)`); the Error section keeps the
  whole error as raised under it. Nothing is inferred from other records. A pending step whose
  unfinished upstream steps are all running is next in line and reads at full strength;
  pending steps further off are faint. Everything else is one click away in the step's detail.
  Built-ins that run inline (`core.echo`, `core.collect`, `core.format`) are dashed
  bubbles. The server lays out the board, so
  the order reads without JavaScript; the `<sluice-board>` component draws an edge per handoff
  from the bottom of a bubble to the top of the one it feeds, with an arrowhead, several edges
  on one side spread along it (from its `edges` attribute: `[from, to, "output → input"]`); an
  edge that passes rows of bubbles runs through the nearest gap in each, never behind a bubble.
  A legend under the board names the solid (hands on a value) and dashed (runs after) lines.
  Hovering (not a card a reflow brings under a still pointer) or keyboard-focusing a bubble
  traces it: its edges light up and name their ports,
  each name by the bubble at the other end, and the rest recede (their text stays at least
  3:1); the arrow keys move between bubbles (Down and Up to the next row that way, a bubble
  joined to this one by an edge first, else the nearest across). A bubble's accessible name is
  its status, id and time (`failed, a, 1h 14m`). A
  bubble links to the step's page; with JavaScript it opens the step in a drawer (the
  `<sluice-drawer>`) instead (the address becomes `#step:<id>`, so Back and a shared link work;
  Escape, the close button, the scrim or a click on the page around the board (not on a card,
  link, control or the switcher) close it, and focus returns to the card). From 1200px the
  drawer stands beside the page, which makes room for it (the board reflows, the opened card
  scrolls into view); from 721px to 1199px it is over the page on a scrim, below 720px a
  full-screen sheet, and below 1200px it is a modal dialog with the page behind it inert. The
  page's first tab stop is "Skip to plan", which moves focus to the board, and a polite live region announces status changes the stream brings
  (`a failed`). Under the board: the **Result** (the plan's outputs that have a value; a
  long text folds to its first lines, markdown rendered) and the plan's inputs (name, value,
  doc).
- `GET /projects/<name>/icon`: the project's image icon (§2), served with its content type,
  `X-Content-Type-Options: nosniff`, its sha256 as the `ETag` (a matching `If-None-Match`
  gets 304) and,
  for SVG, `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline';
  img-src data:` so a script inside it cannot run even opened directly; 404 when the project
  has no image icon. Wherever a project's name shows — its index row, the switcher's button
  and menu entries, its page head — an image icon is an `<img>` of this URL (its sha256 as
  `?v=`: a new image busts a stale cache, an unchanged one stays cached whatever else changes)
  and a text icon is escaped text in the same box. The
  page's favicon stays sluice's mark.
- `GET /projects/<name>/steps/<id>`: one step (the drawer's content, or a page of its own),
  read like a run history: its id with its state as badges beside it (the status glyph and
  word, `blocked` for a blocked step; runs done of total for a scattered step; how long it
  ran, live while running, with when it started and ended as the time's tooltip; the quiet
  badge; "ended 1h ago" once finished), its doc, one line of meta (fn, cost as money,
  session, tags), then a row each for what a pending step waits on, what it runs after, its
  `when`, and for a failed step the steps it blocks, each a link to that step led by its
  status glyph, the Pause switch where pausing acts (a pending, failed or stale step; Resume
  on any paused step; pausing never stops a running one) and a link to its thread on the
  Threads tab; its error (its last line, then the whole text scrolled to its end); its
  progress (the tail of the current run's stderr, while
  running); its outputs (while running, what the agent has submitted so far), its prompt (the binding
  named `prompt`, `spec`, `task`, `instructions` or `brief`) and its other inputs (the run's
  own `input.json`, else what the binding resolves to now), as a list of fields (a `<dl>`):
  each name in a narrow column, its value beside it with its doc and, for an input, where it
  comes from as a small chip linking to the step (`step/output`, or `plan input name`;
  nothing for a value set in the plan; for the prompt, in its section's head). A value reads
  by its kind: short text as text, a number as a number, a boolean as a small `true`/`false`
  pill, null as "none", a short list of scalars as a comma list, an inbox answer as what was
  chosen, and an identifier (one token with a slash, colon or `@`, or letters and digits: a
  path, URL, sha, session or ticket) in the data face, giving way in the middle when it does
  not fit, whole in its title, with a copy button (with script; it selects the text where the
  clipboard is not allowed). Long or multi-line text and long structures take the full width
  below their name (markdown rendered, other multi-line text and structures as code) and fold
  past a few lines ("Show all"). Types show on demand: in the name's title always, after every
  name with the one Types switch (the "Show value types" setting). Then the
  stderr of a finished run ("Log output", folded past six lines) and, when it ran more than
  once, its attempts from the log, oldest first so the current one closes the list: each its
  outcome (glyph and word), when it started (from the run id's stamp when the log no longer
  holds its start; when it ended if neither says) and how long it took (the current run: its
  live time alone, the start in its tooltip) and,
  for a failure, its headline with the whole error under "Show error".
  A pending `core.external` step (§5) shows its doc up front instead of in the head (an
  "Outside sluice" section, markdown rendered: who is doing the work and where) with one
  line, "Done outside sluice. Set its outputs with step_set_output when the work lands, or
  cancel it.", then its declared outputs as fields ("not set yet", each with its type and
  doc); once ready, its state badges add `outside 2h 5m`.
- `GET /projects/<name>/log` (and `GET /log` for the home log): the log viewer. Newest first, 50
  records per page; `?before=<seq>` shows the 50 matching records below that seq, `?after=<seq>`
  the 50 above it, with newest / newer / older links. Filters are query parameters, so a URL is
  shareable: `kind` (repeated or comma-separated; exact kinds or the `step`/`plan` groups) and
  `thread` (comma-separated), the §6b filter `log_read` uses. A row shows seq, time, kind and a
  one-line summary (`s2 succeeded → stale`, `rev 7 by orch: reason (2 ops)`, `questions from
  e2e: body…`, `e2e → orchestrator: body…` on a step's own thread, `<call> <fn> <status>`,
  `logic submitted interface, branch`, `a: run r1 kept through a runner restart`,
  `run r9 stopped: no step or call claimed it`) and expands to the full record as JSON. On a
  phone the kind filter folds behind its summary (`Filter: all kinds`, `Filter: 3 kinds`). A
  `thread.post` call's `call` records are hidden unless the filter selects `call` (a failed
  one still shows); the tools list everything. Unknown kinds or a
  bad seq are a 400 page.
- `GET /fns?project=<name>` (project optional): every function that context sees, grouped by
  scope, with doc and typed inputs and outputs (`string[]`, `enum(a|b)`, `{field: type}`,
  `T?`); a function with a problem (e.g. a collision) is shown in red with the verify message.
  A scope with more than three functions opens with an index of their names, each a link to
  the function (`#fn-<name>`).

**Live updates.** Every page renders completely on first load and works without JavaScript
(the log filter is a plain GET form; a card is a link to its step's page). The index, project
and log pages then open one Datastar SSE stream each (`GET /stream`, `/projects/<name>/stream`,
`/projects/<name>/log/stream`, `/log/stream`), and a step's detail one of its own
(`/projects/<name>/steps/<id>/stream`, under the `sver` signal; the drawer ends the previous
one when it shows another step). The index and project pages carry a `ver` signal, a hash of
what they show, read from the database in one snapshot: the index (and the inbox page) the
runner's liveness and every project's name and change counter (`projects.ver`, §2) — plus, on
the index, the stats (mtime, size) of the `stderr.log` of every running step's runs; a project
page the runner's liveness, its own counter, the number of open inbox items (the nav's badge)
and its running steps' `stderr.log` stats, so a progress line moves while an agent works; a
step's version adds the step and the stderr of its runs; a log page's, its log's last seq and
size. The server polls those versions about once a second off the event loop, each poll a
short read that holds nothing between polls (never blocking the runner or the MCP tools); when
they change it re-renders the page's parts
(each an element with an id: the project page's summary, graph, result and nav badge; the
Threads tab's threads and nav badge) and sends
a `datastar-patch-elements` event for each part that differs, then the new version. An idle
page receives nothing; a client whose version is not current (e.g. reconnecting) first gets
every part. Parts are morphed, so an expanded disclosure stays open. The page loads Datastar
from its Rocket bundle (`datastar-rocket.js`, which adds web components) and
`/static/sluice.js`, which keeps relative and running times current and defines three
components in the light DOM around what the server rendered (their hosts keep Rocket's
attributes through a morph with `data-preserve-attr`): `<sluice-board>` draws and traces the
edges (redrawn when its `edges` or the board changes or it resizes; the SVG it draws into is
`data-ignore-morph`), moves between cards with the arrow keys and flips a glyph whose status
changes; `<sluice-drawer>` opens and closes the drawer; `<sluice-thread>` marks unseen
messages. On the log page, changing the filter updates the `kinds`/`thread` signals and
reconnects the stream, which sends the new table and rewrites the address bar to the filter's
query string; on the newest page, new matching records are prepended as they are appended.
Streams end when the server shuts down; the client reconnects with backoff.

- `GET /inbox` (every project) and `GET /projects/<name>/inbox`: the items, filtered by
  `?status=open|answered|closed|all` (default open; open oldest first, the others newest first).
  An item shows its title, project, id, `from`, age, the input it sets, and its body as markdown.
  An open item has an answer box that works without JavaScript (a form POST of `text`, then a
  303 back); with it, `/static/inbox.js` draws the item's `ui` (§8a) above the box, and folds
  the box away when the program has buttons. An answered item shows its answer, a closed one
  its reason. The page streams like
  the index (`/inbox/stream`, `/projects/<name>/inbox/stream`,
  parts: the items and the nav badge); each open item's answer area carries
  `data-ignore-morph`, so a patch never resets what a person is typing. An answer the server
  took shows at once, without waiting on the stream (which may be reconnecting after a
  restart; streams retry at most 3 s apart): on the open view the item leaves the list and the
  badge drops.
- `POST /projects/<name>/inbox/<id>/answer`: the first write. A JSON body is the answer object;
  a form body (`text`, `next`) becomes `{"action": "answer", "text"}` and redirects to `next` (a
  local path: no scheme, no netloc, no backslash) on success. Both call the store's `inbox_answer`, the tool's own code path, with
  author `dashboard`. Refusals map to 404 (`not_found`), 409 (`conflict`: already answered or
  closed) and 400 (`invalid`, `bad_request`); JSON gets the error payload, a form an HTML page. A
  request whose `Origin` is not this host is refused (403).
- `POST /projects/<name>/archive`: the other write. A form `archived` ("1" or "0") calls the
  store's `update_project`, the `project_update` tool's own code path (author `dashboard`),
  then redirects (303) to the project. An archived project keeps running; it is listed apart. Same refusals as the answer route (404, 403 for another `Origin`).
- `POST /projects/<name>/pause` and `POST /projects/<name>/steps/<id>/pause`: a form `paused`
  ("1" or "0") calls `update_project` or `pause_steps` (the `project_update` and `step_pause`
  tools' code paths, author `dashboard`), then redirects (303) to the project, with the step's drawer open
  (`#step:<id>`) for a step. Same refusals. A paused step that has not started shows a pause
  glyph (its reason in its tooltip and the drawer's Status); a paused project says so under
  its name with a Resume switch next to Archive. An `after` edge is drawn dashed; the drawer
  lists a step's After and Tags.
- **Settings**: the nav's cog (named "Settings", on every page and at every width) opens a
  menu (a `<details>`; a click elsewhere or Escape closes it) holding a form: the theme, a
  radio group (legend "Theme") of every preset in `views.THEMES`, each row its name and its
  swatch (the theme's canvas with "Aa" in its ink, and its nav band and stripes across the
  corner),
  the chosen one ticked: Sluice Light (`light`), Sluice Dark (`dark`), Canyon (`canyon`),
  Ranger (`ranger`), Diner (`diner`), Night Sky (`night-sky`) and Wood Panel (`wood-panel`)
  (until one is picked, the page follows the OS's `prefers-color-scheme` between the two
  sluice presets and the menu marks that one; once picked it stays); then
  "Show value types" (the drawer's Types switch, as a setting), with a Save button that only
  a page without JavaScript shows. The choices live in the browser's cookies, `sluice_theme`
  (a preset's id; none until one is picked) and `sluice_types` (`1`), which every route reads
  (anything else in them is ignored): a page renders the theme on `<html>` as `data-theme`
  and value types as its `show-types` class, so it never shows the wrong theme first. With
  JavaScript (`static/nav.js`) a choice applies at once and is posted to the route in the
  background; the Types switch posts the same way. Cookies are per host, not port, so
  dashboards on other ports of this machine share them.
- `POST /settings`: a form `theme` (a preset's id) and `types` (`0` or `1`; of
  several, the last: the menu sends a hidden `0` before its checkbox) each set their cookie
  (`Path=/`, `SameSite=Lax`, `HttpOnly`, `Max-Age` 400 days) or clear it (`0`) when
  present; anything else is 400 and changes nothing. It then redirects (303) to `next` under
  the answer route's rule (a local path, else `/`), or answers 204 without one. It writes
  nothing on the server; like the writes it is refused under a foreign `Host` and from a
  foreign `Origin` (403).
- `GET /static/inbox.js`, `GET /static/openui.json`: the renderer and its vocabulary;
  `GET /static/sluice.js`: the dashboard's script and components; `GET /static/nav.js`: the
nav's menus and settings; `GET /static/logo.svg`,
  `GET /static/favicon.svg`: the mark (`image/svg+xml`).

`plan_view(project, format, all?)` returns the Mermaid text, or the project page as a standalone
HTML document from the same renderer: the summary and the board (cards without links), then every
step's detail in a disclosure (no nav, no drawer, no stream, no script). Unless `all`, both
leave out the done units (§5) — their steps, and the edges to them and from them to the plan's
outputs — and say so in one line (`3 done units (7 steps) left out; plan_view with all: true
shows them`: a `%%` comment after `flowchart LR`, a line under the page's summary, whose bar and
counts still cover every step).

The `query` tool gives trusted agents one SELECT (or WITH) against the database itself — for
questions the other tools do not answer: joins, aggregates, looking across projects. Each call
opens its own read-only connection (`PRAGMA query_only`), lets an authorizer allow only reads —
no writes, ATTACH, PRAGMA or load_extension — and turns SQLite's limits down (100 KB of SQL,
200 columns and expression depth, 50 compound selects, 1 MB values, 250,000 VM operations); a
statement still running after 2 s is interrupted (a cooperative check between VM instructions,
so one huge scalar can run past it, bounded by the value limit). It returns {columns, rows,
truncated}: at most `limit` rows (an int in 1–1000, default 200), fetched as `limit + 1` so a
full page is marked `truncated`, and stopping early once the rows' JSON passes ~1 MB. A BLOB
cell is refused with the hint to select `hex(col)` or `length(col)`; `params` binds `?`
placeholders. The `outcomes` table (§6) holds what finished steps removed from plans ended
with. The `steps`, `messages`, `step_changes`, `edits` and `log` views (§2) join the
raw tables into readable shapes, and the tool's description names every table and view with
its columns.

Every tool refuses an argument it does not take (`bad_request`, naming it and the arguments
the tool does take) rather than ignore it. Every tool whose write leaves a record takes
`author?`, resolved by the rule in §6b; `inbox_post` names its asker with `from?` instead, and
`project_delete` and `fn_save` leave no record to carry one. A tool that changes one step's contents takes
`step`; a tool that acts on a selection (`step_pause`, `step_retry`, `step_cancel`,
`step_remove`, `status`) takes `steps` (ids; a single id is a list of one) and/or `tags`.

| Tool | Args | Returns |
|---|---|---|
| `docs` | `topic?` | the index, or one page as markdown |
| `projects_list` | – | `[{name, description, rev, counts, archived, paused, icon?}]`; `icon`: `{"kind": "image", "type": <content type>}` or `{"kind": "text", "text": ...}` (§2) |
| `project_create` | `name, description?, icon?, author?` | `{name}` (with an empty plan); refused (`bad_request`) while a deleted project of the name is still being removed (§2), or when a leftover `projects/<name>/` holds more than `fns/` and `.env` |
| `project_update` | `name, description?, archived?, paused?, icon?, reason?, author?` | `{name}`; each change is a `project.pause`, `project.archive` or `project.update` record with the reason and author (§6b); `archived: true` lists the project apart on the dashboard (nothing stops); `paused: true` starts none of its steps until `false` (§6); `icon` is an image path or a short text icon, `""` removes it (§2) |
| `project_delete` | `name` | `{deleted}`: removes the project (its plan, edits, state, log, inbox, calls, submissions and outcomes in one transaction, then its directory: runs, fns, .env); refused (`bad_request`) unless it is archived, none of its steps is running and no non-direct call on it is pending or running. A direct call that ends after it records nothing; a new project of the same name can be created once the old directory is gone, and starts clean |
| `fn_list` | `project?` | `[{name, doc, inputs, outputs, scope, open?, submits?, error?}]` in lookup order (`scope`: builtin, global or project); `open: true` marks an open fn, `submits` what its agent submits on every step; `error` marks a function with a problem |
| `fn_get` | `name, project?` | the fn.json plus `scope` and `path` |
| `fn_save` | `fn, main_py, project?` | writes `fn.json` + `main.py` into the project's (or, without a project, the global) `fns/<name>/` after validating `fn`; `{scope, path}` |
| `fn_call` | `name, inputs, project?, wait?, direct?, author?` | checks `inputs`, then queues one fn run outside the plan (a `calls` row, the call's truth, its inputs kept for its whole life; each status change also a `call` record in the log, §6b) for the runner; `{call, status, outputs?, error?}`, waiting up to `wait` s (capped at 3600). `direct: true` runs it in the calling process to the end instead (no runner needed); refused (`bad_request`) for `core.external`, which never runs |
| `call_status` | `call, project?` | `{call, status, outputs?, error?, stderr_tail?}` from the call's row (a finished call's row goes with its last record, §6b) |
| `plan_get` | `project` | `{rev, plan}` |
| `plan_patch` | `project, rev, ops, reason, author?, start? = false` | `{rev}`; a step it adds comes in paused unless `start` (§5) |
| `step_add` | `project, step, spec, reason?, start? = false, author?` | `{rev}`: `plan_patch` adding one step at the current rev |
| `recipe_list` | `project` | `[{name, doc, params, scope}]` by name: the recipes the project sees (§5; `scope` global or project, the project's winning a name clash), `params` with `unit` first; a broken recipe file as `{name, scope, error}` |
| `unit_add` | `project, recipe, params, start? = false, author?, reason?` | `{rev, steps}`: the recipe's steps expanded with `params` (`unit` among them), tagged `unit:<unit>`, added in one edit at the current rev, paused unless `start` (§5); `invalid` lists every param or expansion problem, `bad_request` names the ids the plan already has |
| `step_update` | `project, step, changes, reason?, author?` | `{rev}`: each key of `changes` replaces that field of the step, null removes it; a running step takes only `paused` |
| `step_remove` | `project, steps?, tags?, reason?, author?` | `{rev, steps, outcomes}`: removes the selected steps in one edit; refused while one runs or something left reads it; `outcomes` is how many of them finished and kept their outcome (§6) |
| `step_pause` | `project, steps?, tags?, subtree? = false, paused? = true, reason?, author?` | `{rev, steps}`: one edit setting (to the reason, else true) or clearing `paused` on the steps selected by id and/or tag, with everything downstream of them (what reads from or runs after them, transitively) when `subtree`; an already paused step keeps its reason unless a new one is given |
| `step_cancel` | `project, steps?, tags?, reason?, author?` | `{steps}`: marks the selected running steps for the runner to kill; each fails with `cancelled: <reason>` (`step_retry` runs it again); a selected pending `core.external` step fails so at once (§6); refused, changing nothing, unless every one is running or a pending `core.external` step |
| `plan_history` | `project, since_rev?` | every edit (`plan.edit` records from `plan_edits`, back to rev 1) and the `plan.input`, `step.output` and `step.retry` records still in the log, in seq order, each with its `seq` (with `rev` > `since_rev`) |
| `plan_set_input` | `project, name, value, reason?, author?` | `{ok}` |
| `step_set_input` | `project, step, input, value, reason?, rev?, author?` | `{rev}` |
| `step_set_output` | `project, step, outputs, reason?, force?, author?` | `{ok}` (§6: refused while what it reads is not ready, unless `force`) |
| `step_retry` | `project, steps?, tags?, reason?, author?` | `{steps}` (each failed, stale or manual); a failed scattered step re-runs only its failed items when its inputs are unchanged (§6) |
| `step_submit` | `project, step, outputs, run?, author?` | `{ok, run}`: the running step's declared outputs, from its agent (§5); `invalid` with every mismatch |
| `log_read` | `project?, since_seq?, kinds?, threads?, limit? = 200` | `{records, last_seq}`: matching records oldest first (§6b filter); after `since_seq` the first `limit` of them (`last_seq` is then the last one returned, else the log's last seq, so passing it back continues); without `since_seq` the last `limit`. No project: the home log |
| `log_wait` | `since_seq, project?, kinds?, threads?, timeout? = 300, limit? = 200, wake? = "any"` | like `log_read` after `since_seq`, but waits (polling the database with a short read each time, holding nothing in between, without blocking the server or the runner) until at least one matching record exists or `timeout` s pass (then `records` is empty; `timeout` is capped at 3600). `wake: "questions"`: a note (a message with `needs_reply` false) does not end the wait; it comes back with the next record that does, or at the timeout |
| `next` | `projects, since_seq, me? = "orchestrator", timeout? = 300, all? = false, settle? = 20, settle_max? = 120` | `{records, notes, last_seq, timed_out}`: waits with the same short polls (nothing held) until a record one of the projects' logs should wake an orchestrator for — the `sluice next` wake rule (§9): a step failed, stale or skipped (inside a unit too); a unit settling, once (its record carries `unit: {name, settled, steps: [{id, status, held?, outputs}]}`); a standalone step's success when its fn is open; a question addressed to `me` or to nobody; an inbox post or answer (`all`: every record) — then keeps collecting until `settle` s pass with no new waking record, or `settle_max` s after the first (`settle` 0: returns at the first), and returns them all. `notes` are the notes held on the way — read them before the records. `last_seq` is the seq of the last record read, waking or not: pass it back as `since_seq` and nothing is missed or repeated. A timeout (on the wait for the first waking record) returns `records` empty and `timed_out` true (`timeout` and `settle_max` capped at 3600) |
| `drain` | `projects?, author?` | pauses the projects (default: every project not archived) that are not already paused, recording which ones in `drain.json` so `release` lets exactly those go again; `{paused, pending}`, `pending` the running steps and live non-direct calls still to finish — `sluice drain` (§9) is the one that waits for them |
| `release` | `author?` | unpauses exactly the projects `drain.json` lists and deletes it; `{released}`. Projects paused otherwise stay paused |
| `step_context` | `project, step` | where the step stands, for the agent doing it — `sluice me` as JSON (§10): `{project, step, fn, doc, status, started, finished, elapsed, run, inputs, upstream, messages, submit, thread, ask}` |
| `query` | `sql, params?, limit? = 200` | `{columns, rows, truncated}`: one read-only SELECT against the database, on a fresh read-only connection per call (see above) |
| `verify` | `project?` | `{ok, problems: [{where, message}], warnings?}` (§6a) |
| `plan_view` | `project, format: "mermaid"\|"html", all? = false` | the diagram or page as text, without the done units unless `all` (above) |
| `plan_prune` | `project, older_than_hours? = 0, author?, reason?` | `{rev, units, steps, outcomes}`: removes every step of every done unit (§5) whose last step finished at least `older_than_hours` ago, in one edit (so `plan_history` keeps them); `units` is how many, `steps` the ids removed, `outcomes` how many outcomes they kept (§6). A done unit has no edge to anything else, so removing it breaks no step; one a plan output reads is kept (removing it would break the plan). Nothing to remove: no edit, the current rev |
| `status` | `project, steps?, tags?, brief? = false, all? = false` | only the steps selected by id and/or tag when given (done or not); else, unless `all`, every step but those of the done units (§5), which `done_units: {units, steps}` counts (only when some were left out); with `brief`, every string over 200 characters in `inputs`, `outputs` and the steps' `outputs` is cut to its first 200 and `… [n more characters]`; `{rev, paused, inputs: {name: value or null}, input_docs?: {name: doc}, outputs: {name: value or null}, steps: [{id, run, status, started, finished, outputs?, error?, doc?, paused?, tags?, after?, when?, skipped?, waiting?, manual}], done_units?: {units, steps}}` (status: pending, running, succeeded, failed, stale or skipped, with `skipped` saying why; `input_docs` only when some input has a doc; `paused` is true or the reason; `waiting`, on a pending step, says why it has not started: `paused: <reason>`, `the project is paused`, `step a is pending`, `after step a, which is running`, `plan input n has no value`; a ready `core.external` step, §5: `external: set its outputs with step_set_output`) |
| `inbox_post` | `project, title, body?, ui?, input?, from?` | `{id}` (§8a); refused (`not_found`) when `input` is not a declared plan input |
| `inbox_list` | `project?, status? = "open"` | the items with that status (`open`, `answered`, `closed` or `all`), each with its `project`, oldest first; every project's without `project` |
| `inbox_answer` | `project, id, answer, author?` | the answered item; `conflict` (with `status`) unless it is open; with `input`, `invalid` when the value does not fit (the item stays open) |
| `inbox_close` | `project, id, reason?, author?` | the closed item; `conflict` unless it is open |

## 8a. The inbox

Each project has an inbox: its `inbox` rows (§2) in posting order, not trimmed with the log.
An item is `{id,
title, body?, ui?, input?, from?, status, created, answer?, answered?, closed?, reason?}`: `id`
is `i<n>` (one more than the highest in the project), `body` markdown, `ui` an OpenUI Lang
program, `input` a plan input, `from` who asked (a step id, an agent), `status` `open`,
`answered` or `closed`, the times ISO UTC. Only an open item changes, once: answering or closing
anything else is refused (`conflict` with its `status`), which is what makes a stale button or a
second answer harmless. Every change appends one log record (§6b) in the same transaction as
the item's change (an answer that sets a plan input, that input's too), so `log_wait(project,
since_seq, kinds=["inbox"])` and `sluice watch --kinds inbox` wake whoever waits on it.

An **answer** is `{action: string, params?: object, values?: object, text?: string}` (nothing
else). With `input`, answering sets that plan input through `plan_set_input`'s own path (type
check, `plan.input` record by the answering author, reason `inbox item <id>: <title>`) before
the item is marked answered; the value is the first present of `values.value`, `params.value`
and `text`. None present, or a value that does not fit, refuses the answer (`invalid`) and the
item stays open. `inbox_post` refuses an `input` the plan does not declare; without a `body`,
the item's body is that input's doc (§5), if it has one.

**The ui.** `ui` is OpenUI Lang (openui.com), drawn in the browser by a small vanilla-DOM
renderer (`src/sluice/static/inbox.js`, no build step) around lang-core's parser. The vocabulary
is closed and lives in one file, `src/sluice/static/openui.json` (each component's positional
props with their types, and a description): the renderer builds the parser's JSON Schema from
it and has one renderer per component; `docs("inbox")` lists the same signatures (a test
checks both). Components: Stack, Heading, Text, Callout, Table, Separator, Form, Input,
Textarea, Select, Radio, Checkbox, Button (prose goes in `body`, so there is no Markdown
component). Program text is only ever set as DOM text, never as HTML. A statement with an
unknown component or a bad prop, a reference to nothing, an unused statement or a line that is
not a statement is dropped, and the item shows `n lines dropped` with the reasons; if nothing is
left to draw, it says so. The text box always remains. A Button answers with `{action (default
"submit"), params (default {}), values}`, where `values` maps each field of its Form (or, outside
a form, each field outside any form) to its value (Input text, or a number for `type: "number"`;
Checkbox a boolean; Select and Radio the chosen option or ""); a primary button first checks the
fields' `rules` with lang-core's validators and shows what fails.

## 9. CLI

MCP is the interface; the CLI only starts it and reaches the same tools from a shell:

```
sluice serve [--host H] [--port P] [--no-runner] [--kill-runs]
                                      runner + MCP server + dashboard (with the inbox);
                                      --kill-runs stops runs on exit instead of leaving them
sluice loop [--kill-runs]             runner only
sluice tool                           list the MCP tools with one-line descriptions
sluice tool <name> '<json args>'      call that tool in-process and print its result
sluice watch [-p P] [--kinds k1,k2] [--threads a,b] [--since-seq N]
                                      print new log records as JSON lines (§10)
sluice next [-p P …] [--since-seq N | --cursor FILE] [--me NAME] [--timeout S]
            [--settle S] [--settle-max S] [--all] [--json]
                                      print the next records an orchestrator acts on, exit
sluice drain [-p P …] [--no-wait] [--release]
                                      pause projects for maintenance and wait out their
                                      running work; --release unpauses what it paused
sluice me [--project P] [--step S]      where this step stands, for its agent (§10)
sluice query [SQL [PARAM …]] [--limit N] [--table [--width N]]
                                      one read-only SELECT: its rows as JSON, or a table
```

`sluice next` blocks until the projects' logs (the given ones, or every project not archived)
hold a record an orchestrator acts on, collects what follows within a settle window, prints
each record compactly and exits. Every record is judged **as of its own seq**, so reading a
stretch of the log late gives the same wakes as reading it live. A step's status at seq S is
the `to` of its last `step.status` record up to S; with none up to S, the `from` of its first
one after S (null: pending); with no `step.status` record at all, its status now. The plan's
shape (steps, edges, pauses, fns) is the current plan's.

A step's **unit** is the steps sharing its `unit:<name>` tag (a recipe unit, §5, named
`<name>`); a step without one belongs to its component among the steps without one
(`plan.units` over just those, so an untagged step after a recipe unit never joins it) when
that has more than one step (named by its first step); otherwise it is standalone. A unit is
**settled** when at least one of its steps has finished and none of its steps is running or
pending and startable: each is succeeded, skipped, failed or stale, or pending and **held** —
paused, its project paused, a `core.external` step, reading a plan input with no value, or
waiting (through reads or `after`) on a step that is failed, stale or itself held (a step
outside the unit counts by the same rule). **Wakes:**

- a `step.status` to `failed`, `stale` or `skipped`, inside a unit too; a `message` needing a
  reply, not from `--me` (default `orchestrator`), addressed to `--me` or to nobody; an
  `inbox.post` or `inbox.answer`; a `project.pause` or `project.archive` not by `--me`
  (`PROJECT lash paused by dashboard: <reason>`);
- a unit **once, when it settles**: the `step.status` record (any `to`) of one of its steps at
  which it is settled while it was not at its previous `step.status` record. A step inside a
  unit never wakes on its own success. When that record is itself a failure (or stale or
  skipped) it wakes once, with the unit attached; a retry that runs and settles again is a new
  settling and wakes again;
- a standalone step's success when its fn is open;
- with `--all`, every record.

Notes (`needs_reply: false`) not from `--me` are held and printed first, like `sluice watch
--wake questions`. A record that settles its unit carries `unit: {name, settled: true, steps:
[{id, status, held?, outputs}]}` (steps in plan order, status as of the record, `held: true`
on a held pending step, `outputs` a step's outputs now if it succeeded as of the record — the
ones the step declares when it declares any (its contract, e.g. `landed`, `summary`), else its
fn's — leaving out null and empty values; else `{}`). Each record prints as one block: `STEP fix-x running -> failed: <last line
of the error>`, `MSG step-fix-x fix-x -> orchestrator: <body>`, `NOTE …` for a held note,
`INBOX post i3 <title>`; a unit settled by a success as

```
UNIT fig-3984 settled: fork succeeded · work succeeded · close succeeded · rm succeeded
  work.landed: true
  work.summary: <whitespace collapsed, cut to 600 characters and "…">
```

(step names without the `<unit>-` prefix; one line per output: strings as they are, anything
else as compact JSON, cut to 600); a unit settled by a failure as its `STEP` line, then `  unit
fig-3984: fork succeeded · work failed · close pending (held) · rm pending (held)` and the
outputs. The last line is `seq <N>`. After the first waking record it keeps reading until
`--settle S` seconds (default 20) pass with no new waking record, or `--settle-max S` (default
120) after the first; `--settle 0` returns at the first waking record. Without `--since-seq` or
`--cursor` it starts from now; `--cursor FILE` reads the start seq from it (missing: from now)
and writes back, once and atomically, the seq of the last record consumed — read, waking or not
— so a relaunch never misses or repeats one. `--timeout S` bounds the wait for the first waking
record, then exits 0 printing `timeout seq <N>` (and writes the cursor); `--json` prints the
records as JSON lines, `unit` included with its outputs whole, and a final `{"seq": N,
"timed_out": …}`. Exit 0 on a wake or a timeout.

`sluice drain` pauses the given projects (default: every project not archived) that are not
already paused, records which ones in `SLUICE_HOME/drain.json` (`{"paused": […], "at": …}`,
written atomically, merged with an existing file), then waits — one line whenever the count
changes (`running: lash 1 (fix-x), sluice 0; calls 0`) — until none of them has a running
step or a pending or running non-direct call, and exits 0 printing `drained`. `--no-wait`
pauses and exits. `--release` unpauses exactly the projects `drain.json` lists — not ones
paused otherwise — deletes the file and prints what it released. Both write their
`project.pause` records (§6b) with author `drain` and reason `drain: paused for maintenance`
or `drain released`.

`sluice me` reads `SLUICE_PROJECT`, `SLUICE_STEP` and `SLUICE_RUN_ID` from the environment
(the runner sets them for every run, and the native agent packs pass them through to the
engine); outside a step it says so and exits 1, and `--project`/`--step` work anywhere. It
prints, compactly: the step, its fn, doc, status and running time; its inputs (cut like
`status`'s `brief`); each step it reads or runs after, with its status and short outputs
(an output named `summary` whole, else `final` cut to ~300 characters, plus any output whose
name ends in `report` or `path`); the messages on its `step-<id>` thread still unanswered,
newest last; the outputs it must submit (required first) and the exact `sluice tool
step_submit '{…}'` with its run id; and its thread with the command to ask a question.

`sluice query` runs one SELECT through the `query` tool's guards and limits (§8) and prints the
tool's `{columns, rows, truncated}` as JSON, one row per line. Each PARAM binds a `?` in order:
as JSON when it parses (`42`, `null`), else as its text. `--table` prints an aligned table for
people instead — NULL blank, whitespace collapsed, each cell cut to `--width` characters
(default 60, 0 never) — then `(n rows)`, `, truncated` when the limit or size cap cut it.
Without SQL it lists the tables and views with their columns. A refused or failed query exits 1
with the error on stderr.

Every command creates `SLUICE_HOME` with the default `config.json` on first use. `sluice tool`
builds the same MCP server object `serve` exposes and calls its tool (same argument validation,
same code path). It prints JSON results (text for `docs` and `plan_view`); a tool error goes to
stderr as the error JSON with exit 1, and a result with `"ok": false` (`verify` with problems)
also exits 1.

## 10. Built-in fns and first-party packs

`src/sluice/fns/` holds only what sluice itself needs: `core.*` (§6), `thread.*`, `inbox.ask`
and `inline.*` (below), plus shared helper code for built-in fns in `src/sluice/fns/_lib/`. Their `fn.json` files are
the reference for their types.

Every other fn in this repo is a **first-party pack** under `packs/`, not loaded by default:

- `packs/agents/`: `agent.devin`, `agent.codex`, `agent.claude`, `agent.run`, `agent.review`,
  `decide.llm` (run Devin, Codex, Claude, the review agent, decisions). The agent fns are open
  (§5): as a plan step they add to their task text an `## Inputs` section (each extra input
  with its type and value), an `## Outputs you must submit` section (each declared output with
  its type and doc, and the exact `sluice tool step_submit` command) and the step-thread note.
  Each takes `session?` and returns `session` (empty when none was recorded): binding a
  later step's `session` to an earlier step's `session` output continues that agent.
  All five agent fns use `packs/agents/_agents/native/` to supervise a live session on the
  owner's engine login. Each run has a private tmux server (`attach:` in the step's stderr).
  Long tasks go through `<run_dir>/task.md`. A step with required declared outputs completes
  after the engine submits all of them; an idle turn with missing outputs gets a bounded
  series of nudges, then a clear failure. Claude's background shell and scheduled-wakeup
  signals keep its session waiting. Messages addressed to the step arrive in the live session
  without polling. A `session` resumes only in its original directory. A delivered message
  must start a turn within 60 seconds or it is retried once, then fails; waiting background
  work is nudged after 90 minutes by default. Cancel and runner adoption end the tmux server
  and its process tree (`packs/README.md`, "Agent functions and live sessions").
  In a git worktree each agent fn also returns the optional record `git`: `head_before`
  (HEAD when the run started, read once per run and kept in `native.json` across its
  Transient retries), `head_after`, `commits` (in `head_before..head_after`: how far HEAD
  moved, so a lane that pulls before it pushes counts upstream commits too) and `dirty`
  (tracked changes left uncommitted); `git` is absent outside a git worktree.
  While a session is busy the supervisor samples the worktree every few minutes; after
  `SLUICE_AGENT_QUIET_MIN` (45) minutes busy with no change it posts one note (`needs_reply`
  false) to the orchestrator on the step's thread, and again after each further quiet period;
  it never steers or stops the session over it.
  When the step is otherwise done, the supervisor first waits, up to `SLUICE_AGENT_WORK_MIN`
  (10) minutes, while the session's background work runs (what the engine reports, and
  processes it started and let go in its private cgroup), then, once per run, when tracked
  files are changed but not committed, sends the agent one reminder turn; it never commits
  for the agent.
  After a context compaction the session gets the step's own context again (`sluice me`, or
  the path of `task.md` when that fails): Claude through its SessionStart hook (source
  `compact`), Codex and Devin as a message typed in when they report one.
  Git never prompts in a session (`GIT_TERMINAL_PROMPT=0`, `GIT_EDITOR=true`,
  `GIT_MERGE_AUTOEDIT=no`).
  Claude (`agent.claude`, `agent.review`, or `agent.run` with engine `claude`) always runs Opus.
  Codex (`agent.codex`, or
  `agent.run` with engine `codex`) uses the same supervisor with `codex app-server` JSON-RPC
  and a TUI attached to it in tmux. It takes `model` `sol` (the default) or `astra` and
  `effort` (`minimal` to `max`), whose default is `high`. Its final message and turn state come
  from app-server notifications; it waits a configurable grace period before the first nudge
  because Codex reports no reliable pending background-work state. Devin (`agent.devin`, or
  `agent.run` with engine `devin`) runs its TUI in the private tmux server with a per-run
  config that adds lifecycle hooks and an exported transcript. It defaults to `swe-2-high`,
  retains the owner's Devin config, resumes by session id in the same cwd, and uses the same
  grace period because its `Stop` hook does not report pending background shell work. The fns
  share engine code through `packs/agents/_agents/`.
- `packs/git/`: `git.worktree`, `git.worktree_rm`, `git.head`, `git.merge`, `git.rebase`,
  `git.push`, `gh.pr` (worktrees, merge, rebase, push, pull requests)
- `packs/jev/`: `jev.ask`, `jev.choice`, `jev.score`, `jev.noul` (Jev, TypeSafe's System One
  model; needs `TYPESAFE_API_KEY`), with its shared client in `packs/jev/_jev/`

A pack is installed by copying `packs/<pack>/*` into `SLUICE_HOME/fns/` (or a project's `fns/`),
or by adding the pack's absolute path to `config.fn_dirs`; its fns then load in the global (or
project) scope. Packs are self-contained: a fn finds its helpers relative to its own directory,
so a copy anywhere works (see `packs/README.md`).

**Threads** (`thread.*`, plain functions, no engine support): a thread is the project's log
filtered to `message` records with that `thread` name (the project comes from `SLUICE_PROJECT`;
without one they fail with a clear error; thread names use the id pattern). A message is
`{"seq", "at", "kind": "message", "thread", "from", "to"?, "body", "data"?}`; its `seq` is the
log's. Agents post through `fn_call`. Direct callers can read with `log_read`/`log_wait`
(`threads: [name]`); the agent functions above deliver addressed messages into their live
sessions. Plans use thread functions as ordinary steps.
- `thread.post`: inputs `{thread: string, body: string, from: string, to: string?, data: Any?}`,
  outputs `{seq: int}`. Appends in its own write transaction, so concurrent posters get
  distinct, increasing seqs.
- `thread.wait`: inputs `{thread: string, since_seq: int?, to: string?, timeout: int?,
  wake: string?}`, outputs `{messages: Any[], last_seq: int}`: the messages after `since_seq`
  (default 0: all) and, with `to`, only those addressed to it or to nobody; blocks (reading the
  log every 0.5 s) until there is at least one or `timeout` s (default 300) pass, then
  `messages` is empty. With `wake: "questions"` a note (`needs_reply` false) does not end the
  wait: it comes back with the next question, or at the timeout. `last_seq` is the log's last
  seq (§6b: seqs are the home's, so a log's have gaps), to pass back as `since_seq`.

**`inline.bash`** and **`inline.python`**: code given as a string, for a check or a small
transform no fn exists for. Both are open (§5): each extra input the step binds is visible to the
code by its name (`-` becomes `_`), and a step that declares outputs gets them from the code
itself, not from `step_submit` (a declared output the fn returns needs no submitting).
`inline.bash` inputs `{code: string, cwd: string?, check: boolean?}`, outputs `{stdout: string,
stderr: string, code: int}`: runs `bash -e -o pipefail -c code`; extra inputs are environment
variables (strings as they are, anything else as JSON); declared outputs come from one JSON
object the script writes to the file `$OUT`; a non-zero exit fails the step unless `check` is
false. `inline.python` inputs `{code: string, cwd: string?}`, outputs `{value: Any?, stdout:
string}`: runs the code (standard library only) with `inp`, `ctx` and each extra input as a
variable; `value` is what it assigns to `out`, declared outputs come from `out` as a dict, and
`stdout` is what it printed (also streamed as progress).

**`inbox.ask`**: inputs `{title: string, body: string?, ui: string?}`, outputs `{answer: {action:
string, params: Any?, values: Any?, text: string?}}`. It posts an item to the project's inbox
(§8a) with `from` = the step (`call <run_id>` for a call) and reads the item every 0.5 s
until the item is answered (the answer is the output) or closed (the step fails with `inbox item
<id> was closed without an answer: <reason>`). It waits as long as it takes. If this step
already has an open item with the same title, body and ui (a run killed by a runner restart,
then retried), it waits on that one instead of posting again. Like the thread fns, it needs a
project.

**Watching.** Agents watch through MCP: `log_wait` in a loop, passing back `last_seq`. For
harnesses with monitors (e.g. Claude Code's Monitor tool) the shell form is `sluice watch [-p P]
[--kinds k1,k2] [--threads a,b] [--since-seq N] [--wake questions]`: it follows the project's log (the home log
without `-p`) with the same filter as `log_read`, from the end of the log (or after `--since-seq`),
printing each matching record as one JSON line (flushed) as it is appended, and never exits
(`--wake questions` holds notes and prints them with the next record that is not one). It
reads the home's database only; it needs no runner or server. When the next thing to act on
is what an orchestrator wants — not a stream — `next` (the tool) and `sluice next` (§9) wait
for the waking records (a unit once, when it settles) and return them in one batch; a worker agent asks where its own step stands with
`step_context` or `sluice me` inside the step (§9).

## 11. Conventions

Python ≥ 3.12, `uv` for everything. `src/sluice/fn.py`, `src/sluice/__init__.py`,
`src/sluice/log.py`, `src/sluice/inbox.py` and `src/sluice/util.py` import only the stdlib (fns
import them). Tests in
`tests/`; external tools are faked in tests. Commits: plain sentences, no AI attribution of any
kind; stage exact paths.
