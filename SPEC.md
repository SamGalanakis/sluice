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
  status changes, calls, thread messages). It is history; `plan.json` and `state.json` are the
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
runner.lock                 flock held by the one runner of this home (a second one refuses to start)
runner.json                 the runner's heartbeat {pid, started, beat}, refreshed about once
                            a second; stale means the runner is down
.env                        global secrets (KEY=value lines)
fns/                        global user functions
log.jsonl                   the home log: fn_call runs without a project (§6b)
runs/<call_id>/             input.json, output.json, stderr.log, shim.json, child.json,
                            shim.lock and exit.json of those calls (§4)
.lock                       flock target for appends to the home log
projects/<name>/
  project.json              {"name", "description", "archived"?, "paused"?, "icon"?}
  icon.<ext>                the project's image icon (svg, png, webp, jpg or gif), at most one
  plan.json                 the project's plan (current truth)
  state.json                runner-owned: plan input values, step status and outputs (current truth)
  log.jsonl                 the project's log (§6b): edits, manual values, step status changes,
                            calls, thread messages, inbox changes
  inbox.json                {"items": [...]}: the project's inbox items (§8a), current truth
  fns/                      project-local functions
  .env                      project secrets (override global ones)
  runs/<run_id>/            input.json, output.json, stderr.log for one fn execution (a step run,
                            or a call: then run_id is the call id); submitted.json, what its
                            agent submitted (§5); shim.json, child.json, shim.lock and
                            exit.json, the supervising shim's identity, the fn child's pid +
                            start time, liveness lock and exit record (§4)
  .lock                     flock target for read-modify-write in this project and log appends
```

A project's optional **icon** is either an image — an `icon.<ext>` file (SVG, PNG, WebP, JPEG
or GIF, at most 256 KB) written by `project_create`/`project_update`'s `icon` argument, which
sniffs the type from the file's content — or a short text icon (at most 16 characters, no
control characters, typically one emoji) kept as `project.json`'s `"icon"`. A project has at
most one of the two: setting one clears the other; `icon: ""` removes it. A value that looks
like a path (starts with `/` or `~`) but is not a readable image file is an error, never a
text icon. `projects_list` reports it as `{"kind": "image", "type": <content type>}` or
`{"kind": "text", "text": ...}`, absent when none; the dashboard shows it by the project's
name and serves an image icon at `/projects/<name>/icon` (§8).

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

Types are written on fn inputs and outputs, plan inputs and the outputs a step of an open fn
declares (§5). An extra input of such a step has no written type: it takes its source's (a
ref's type; for a list source an array of the refs' type, `Any[]` when they differ; `Any` for a
`default`; the item type when it is the scatter input). Where a type is handed on (the env of
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
  pattern) to select steps by; and `"when": "<ref>"` to run it only if that value is true.
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
  `{"source": ["<ref>", ...]}` fan-in: an array of the values, in order. A ref is a plan input
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
  written to the run dir's `submitted.json` (a resubmit replaces it) and logged
  (`step.submit`). When the fn exits 0, the runner merges them into the step's outputs (the
  fn's returned values win on a name they share); a required declared output never submitted
  fails the step with `declared outputs not submitted: <names> (the agent must call
  step_submit ...)`. An unsubmitted optional one is null.
- A step is **ready** when every plan input and step it reads has a value / has `succeeded`.

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
`rev`, rewrites `plan.json`, and appends a `plan.edit` record `{"rev", "author", "reason",
"ops"}` to the project's log (creation is rev 1, one `add` of the whole plan). Removing or
changing a running step is refused.

## 6. Runner and state

`state.json`:
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

**Staleness.** A result is only valid for the inputs it was computed from. When a step starts
(and so when it succeeds) or is set by hand, its state records `inputs_hash`: a hash of the
canonical JSON of its resolved inputs (the object it runs with, unbound optional inputs null; for
a scattered step the whole array), or null for a step set by hand with `force` while what it
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
   `paused` in `project.json`: it stays `pending`, whatever it would read held, until
   unpaused; pausing never stops a running step). Built-in fns run inline;
   staleness is re-checked after each round of inline results, so nothing starts from a result
   that no longer holds.
4. If anything changed, write `state.json`, then append a `step.status` record per step whose
   status changed in this pass (§6b).
5. Calls: follow the log's `call` records; start `pending` calls and log each status change (§6b). `call_status` reads a call's latest record.

On startup — once, under `runner.lock`, before the first tick — the runner adopts what a
previous one left. For every step entry still `running` and every `running` non-direct call it
looks at each run dir (a step's `run_ids`; a call's is `runs/<call>`): `exit.json` → the run
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
shim pids. Each adopted run appends `run.adopt` `{step or call, run, outcome}`
(`watching`/`finished`/`unknown`/`restarted`), and a run dir whose shim lives — or whose
recorded fn child lives on past it — but which no running step or call references (the old
runner died between spawning it and recording it) is killed and logged `run.orphan` `{run}`
— only when something was actually signalled. A `direct` call (§8 `fn_call`) is
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
  `pending` (refused, changing nothing, unless every one is). A failed scattered step with
  `results` goes back keeping `{inputs_hash, run_ids, results}` under `kept`: when it starts,
  an unchanged inputs hash and one kept result per item mean the runs that already succeeded
  are not re-run (their kept run ids stand in `run_ids`); a different hash or count drops
  `kept` and runs every item as usual. Their succeeded dependents turn
  `stale` when they produce a different result. Record `step.retry` `{step}` per step.
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
and a message; it changes nothing. `where` is a file path (relative to `SLUICE_HOME` when inside
it, e.g. `projects/p/fns/x.y/fn.json`), followed by `#<path in the document>` for JSON
(`projects/p/plan.json#steps.a.run`, `projects/p/state.json#inputs.n`) or `:<line>` for `.env`
files. Without a project it checks the built-in and global scopes and every directory under
`projects/`; with one, the built-in and global scopes and that project. It covers:
- every `fn.json`: shape (`name`, `inputs`, `outputs`, optional `doc`, boolean `open` and,
  for an open fn, `submits`; nothing else), the name
  matching its directory, every type parsing, `main.py` present for non-built-ins;
- name collisions across scopes (see §2);
- `project.json` shape (`name` equal to its directory, optional strings `description` and
  `icon`, optional booleans `archived` and `paused`), `.env`
  files parsing as `KEY=value` lines (blank lines, `#` comments and `export ` allowed);
- the plan: full validation (§5) against the project's functions;
- `state.json` agreeing with the plan (no state for unknown steps or undeclared plan inputs,
  valid statuses, outputs of succeeded steps passing their output types, plan input values
  passing their types).

`{"ok": bool, "problems": [{"where", "message"}]}`. CLI `sluice tool verify '{"project": "P"}'`
prints them and exits non-zero when there are any.

## 6b. The log

Each project has one append-only `log.jsonl`, and `SLUICE_HOME/log.jsonl` holds the calls made
without a project. Every record is `{"seq", "at", "kind", ...}`; `seq` counts up from 1 per log.
Appends hold the directory's `.lock` flock, so writers in any process (the store, the runner, a
fn process posting to a thread) get distinct, increasing seqs; the file is in seq order. Kinds:

| kind | fields | written by |
|---|---|---|
| `plan.edit` | `rev, author, reason, ops` | every accepted edit (§5) |
| `plan.input` | `rev, author, reason, name, value` | `plan_set_input` |
| `step.output` | `rev, author, reason, step, outputs, force?` | `step_set_output` |
| `step.retry` | `rev, author, reason, step` | `step_retry` |
| `step.cancel` | `step, author, reason` | `step_cancel`: the runner then kills the step and fails it with `cancelled: <reason>` |
| `step.submit` | `step, run, outputs` | every accepted `step_submit` (§5) |
| `step.status` | `step, from, to, error?, run_ids?` | every status change of a step: the runner, once per state write (`from` is the status at the previous write, so a built-in finishing inline goes `pending` → `succeeded`; a new step's `from` is null), and the manual tools; `error` when it failed, `run_ids` when it finished |
| `call` | `call, fn, status, inputs?, outputs?, error?, direct?, pid?, pid_start?` | every status change of a `fn_call`; the pending record (a direct call's first) carries the `inputs`; a direct call's running record also its `pid` and `pid_start` |
| `message` | `thread, from, to?, body, data?` | `thread.post` (§10) |
| `inbox.post` | `item, title, from?, input?` | `inbox_post`, `inbox.ask` (§8a) |
| `inbox.answer` | `item, answer, by` | `inbox_answer` and the dashboard's answer route |
| `inbox.close` | `item, reason?, by` | `inbox_close` |
| `run.adopt` | `step or call, run, outcome` | the runner, once per leftover run at startup: what its dir showed (`watching`, `finished`, `unknown`, `restarted`, §6) |
| `run.orphan` | `run` | a live run nothing referenced, killed at startup (§6) |

The log is history, not the source of truth, so it is capped at `config.log_max` records
(default 10000): when an append takes it past the cap, the oldest records are dropped under the
lock, down to 90% of the cap (so a full log is not rewritten on every append), together with the
run dirs that only dropped records referred to (a `call` record refers to `runs/<call>`, a
`step.status` record to its `run_ids`; dirs `state.json` still lists are kept). The latest record
of a call that is still pending or running is never dropped. `plan_history` therefore reaches
back only as far as the log does.

Readers take no lock: a line not yet complete is left out until it is. `log_read` and `log_wait`
(§8), `thread.wait` and `sluice watch` share one filter: `kinds` (exact kinds, or a group name,
`step`, `plan`, `inbox` or `run`, for every kind under it) and `threads` (messages only on these threads; given
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
`sluice loop` runs the steps: the two share nothing but the files (the runner polls about once
a second), so the server can restart without ending running steps. Stopping the runner leaves
its runs going — a later runner adopts them (§6) — unless it was started with `--kill-runs`.
A `--host` that is not loopback warns loudly on stderr: the tools (fn_save, fn_call — running
code) are served without authentication to anyone who can reach the port. Errors are tool
errors whose message is JSON
`{"error": "not_found"|"conflict"|"invalid"|"bad_request", "message", ...}` (`conflict` carries
`current_rev` for a plan edit, or `status` for an inbox item that is no longer open; `invalid`
carries `errors`). `rev` is optional on the convenience tools (they apply
to the current revision under the lock) and required on `plan_patch`.

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
skipped) with its word for assistive technology, never colour
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
  paused, after "Stopped:" when nothing is running (`Stopped: a and b failed, blocking 4
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
  History, the log filtered to the history kinds, Log, and Functions, the functions as the
  project sees them), first whether the work moves: a progress bar by status and one line
  (succeeded of total, skipped, running, stale, failed, blocked, paused, total `cost_usd`, last
  activity; the failed, blocked and paused left to the stuck line when it leads the page; the
  bar's label counts them all) with the Pause
  and Archive switches; then the description (markdown, folded to its opening, then "Show
  more"; a paused or archived project says so), then the **board**. Its **lanes** are the
  steps joined by handoffs (an edge that carries a value; `after` only orders), so independent
  pieces of work stay together. The steps any edge joins (a handoff or an `after`) are one
  independent piece of work, its own quiet box when there are several; the boxes wrap, in
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
  its own edges. The plan's order is kept. Inside a lane, each row is sorted by where its
  neighbours sit, a few sweeps down and up, so
  edges seldom cross. Each step is a compact bubble: its status glyph, its id and, small, how
  long it ran (live while running) and `done/total` for a scattered step; its tooltip is its
  doc and what it says now (a running step's last non-empty stderr line, a failed step's
  error, what a pending step waits on, "its inputs changed" when stale). A running step that
  has written nothing for 15 minutes (the newest stderr.log mtime of its runs that have not
  finished, else their run dirs') is quiet: its card, its index row and its drawer's title
  wear a `quiet 42m` badge in the attention voice, kept current to the minute (and nothing
  more: its tooltip stays its last output, and its progress shows the tail). A blocked step's card says `blocked`. A failed step's
  line (tooltip, log summary, the head of its Error) is its error's last non-empty line,
  where a traceback names the exception, in sluice's words: without a leading exception class
  (`sluice.fn.ShError: `), the home directory as `~`, and an exit code of 128 + n (or -n)
  explained by its signal (`exited 143 (terminated: SIGTERM)`); the Error section keeps the
  whole error as raised under it. Nothing is inferred from other records. A pending step whose
  unfinished upstream steps are all running is next in line and reads at full strength;
  pending steps further off are faint. Everything else is one click away in the step's detail.
  Built-ins that run inline (`core.*`) are dashed bubbles. The server lays out the board, so
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
  `X-Content-Type-Options: nosniff`, stat-based cache validators (ETag, Last-Modified) and,
  for SVG, `Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline';
  img-src data:` so a script inside it cannot run even opened directly; 404 when the project
  has no image icon. Wherever a project's name shows — its index row, the switcher's button
  and menu entries, its page head — an image icon is an `<img>` of this URL (a `?v=` of the
  file's mtime busts a stale cache) and a text icon is escaped text in the same box. The
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
the stats (mtime, size) of the files they read (`project.json`, `plan.json`, `state.json`,
`log.jsonl`, `inbox.json`) and, on the project page, of the `stderr.log` of every running
step's runs, so a progress line moves while an agent works; a step's version adds the step and
the stderr of its runs. The server polls those stats about once a second off the event loop
(never blocking the runner or the MCP tools); when they change it re-renders the page's parts
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
  store's `update_project`, the `project_update` tool's own code path, then redirects (303) to
  the project. An archived project keeps running; it is listed apart. Same refusals as the answer route (404, 403 for another `Origin`).
- `POST /projects/<name>/pause` and `POST /projects/<name>/steps/<id>/pause`: a form `paused`
  ("1" or "0") calls `update_project` or `pause_steps` (the `project_update` and `step_pause`
  tools' code paths), then redirects (303) to the project, with the step's drawer open
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

`plan_view(project, format)` returns the Mermaid text, or the project page as a standalone HTML
document from the same renderer: the summary and the board (cards without links), then every
step's detail in a disclosure (no nav, no drawer, no stream, no script).

Every tool refuses an argument it does not take (`bad_request`, naming it and the arguments
the tool does take) rather than ignore it. A tool that changes one step's contents takes
`step`; a tool that acts on a selection (`step_pause`, `step_retry`, `step_cancel`,
`step_remove`, `status`) takes `steps` (ids; a single id is a list of one) and/or `tags`.

| Tool | Args | Returns |
|---|---|---|
| `docs` | `topic?` | the index, or one page as markdown |
| `projects_list` | – | `[{name, description, rev, counts, archived, paused, icon?}]`; `icon`: `{"kind": "image", "type": <content type>}` or `{"kind": "text", "text": ...}` (§2) |
| `project_create` | `name, description?, icon?` | `{name}` (with an empty plan) |
| `project_update` | `name, description?, archived?, paused?, icon?` | `{name}`; `archived: true` lists the project apart on the dashboard (nothing stops); `paused: true` starts none of its steps until `false` (§6); `icon` is an image path or a short text icon, `""` removes it (§2) |
| `project_delete` | `name` | `{deleted}`: removes the project's directory (plan, state, log, inbox, runs); refused (`bad_request`) unless it is archived, none of its steps is running and no non-direct call on it is pending or running |
| `fn_list` | `project?` | `[{name, doc, inputs, outputs, scope, open?, submits?, error?}]` in lookup order (`scope`: builtin, global or project); `open: true` marks an open fn, `submits` what its agent submits on every step; `error` marks a function with a problem |
| `fn_get` | `name, project?` | the fn.json plus `scope` and `path` |
| `fn_save` | `fn, main_py, project?` | writes `fn.json` + `main.py` into the project's (or, without a project, the global) `fns/<name>/` after validating `fn`; `{scope, path}` |
| `fn_call` | `name, inputs, project?, wait?, direct?` | checks `inputs`, then queues one fn run outside the plan (a `call` record in the log, §6b) for the runner; `{call, status, outputs?, error?}`, waiting up to `wait` s (capped at 3600). `direct: true` runs it in the calling process to the end instead (no runner needed) |
| `call_status` | `call, project?` | `{call, status, outputs?, error?, stderr_tail?}` from the call's latest record |
| `plan_get` | `project` | `{rev, plan}` |
| `plan_patch` | `project, rev, ops, reason, author?, start? = false` | `{rev}`; a step it adds comes in paused unless `start` (§5) |
| `step_add` | `project, step, spec, reason?, start? = false` | `{rev}`: `plan_patch` adding one step at the current rev |
| `step_update` | `project, step, changes, reason?` | `{rev}`: each key of `changes` replaces that field of the step, null removes it; a running step takes only `paused` |
| `step_remove` | `project, steps?, tags?, reason?` | `{rev, steps}`: removes the selected steps in one edit; refused while one runs or something left reads it |
| `step_pause` | `project, steps?, tags?, subtree? = false, paused? = true, reason?` | `{rev, steps}`: one edit setting (to the reason, else true) or clearing `paused` on the steps selected by id and/or tag, with everything downstream of them (what reads from or runs after them, transitively) when `subtree`; an already paused step keeps its reason unless a new one is given |
| `step_cancel` | `project, steps?, tags?, reason?` | `{steps}`: marks the selected running steps for the runner to kill; each fails with `cancelled: <reason>` (`step_retry` runs it again); refused unless every one is running |
| `plan_history` | `project, since_rev?` | the `plan.edit`, `plan.input`, `step.output` and `step.retry` records still in the log (with `rev` > `since_rev`) |
| `plan_set_input` | `project, name, value, reason?` | `{ok}` |
| `step_set_input` | `project, step, input, value, reason?, rev?` | `{rev}` |
| `step_set_output` | `project, step, outputs, reason?, force?` | `{ok}` (§6: refused while what it reads is not ready, unless `force`) |
| `step_retry` | `project, steps?, tags?, reason?` | `{steps}` (each failed, stale or manual); a failed scattered step re-runs only its failed items when its inputs are unchanged (§6) |
| `step_submit` | `project, step, outputs, run?` | `{ok, run}`: the running step's declared outputs, from its agent (§5); `invalid` with every mismatch |
| `log_read` | `project?, since_seq?, kinds?, threads?, limit? = 200` | `{records, last_seq}`: matching records oldest first (§6b filter); after `since_seq` the first `limit` of them (`last_seq` is then the last one returned, else the log's last seq, so passing it back continues); without `since_seq` the last `limit`. No project: the home log |
| `log_wait` | `since_seq, project?, kinds?, threads?, timeout? = 300, limit? = 200, wake? = "any"` | like `log_read` after `since_seq`, but waits (polling the file, without blocking the server or the runner) until at least one matching record exists or `timeout` s pass (then `records` is empty; `timeout` is capped at 3600). `wake: "questions"`: a note (a message with `needs_reply` false) does not end the wait; it comes back with the next record that does, or at the timeout |
| `verify` | `project?` | `{ok, problems: [{where, message}]}` (§6a) |
| `plan_view` | `project, format: "mermaid"\|"html"` | the diagram or page as text |
| `status` | `project, steps?, tags?, brief? = false` | only the steps selected by id and/or tag when given; with `brief`, every string over 200 characters in `inputs`, `outputs` and the steps' `outputs` is cut to its first 200 and `… [n more characters]`; `{rev, paused, inputs: {name: value or null}, input_docs?: {name: doc}, outputs: {name: value or null}, steps: [{id, run, status, started, finished, outputs?, error?, doc?, paused?, tags?, after?, when?, skipped?, waiting?, manual}]}` (status: pending, running, succeeded, failed, stale or skipped, with `skipped` saying why; `input_docs` only when some input has a doc; `paused` is true or the reason; `waiting`, on a pending step, says why it has not started: `paused: <reason>`, `the project is paused`, `step a is pending`, `after step a, which is running`, `plan input n has no value`) |
| `inbox_post` | `project, title, body?, ui?, input?, from?` | `{id}` (§8a); refused (`not_found`) when `input` is not a declared plan input |
| `inbox_list` | `project?, status? = "open"` | the items with that status (`open`, `answered`, `closed` or `all`), each with its `project`, oldest first; every project's without `project` |
| `inbox_answer` | `project, id, answer` | the answered item; `conflict` (with `status`) unless it is open; with `input`, `invalid` when the value does not fit (the item stays open) |
| `inbox_close` | `project, id, reason?` | the closed item; `conflict` unless it is open |

## 8a. The inbox

Each project has an inbox, `inbox.json`: `{"items": [...]}` in posting order, written atomically
under the project's flock (like `state.json`), and not trimmed with the log. An item is `{id,
title, body?, ui?, input?, from?, status, created, answer?, answered?, closed?, reason?}`: `id`
is `i<n>` (one more than the highest in the project), `body` markdown, `ui` an OpenUI Lang
program, `input` a plan input, `from` who asked (a step id, an agent), `status` `open`,
`answered` or `closed`, the times ISO UTC. Only an open item changes, once: answering or closing
anything else is refused (`conflict` with its `status`), which is what makes a stale button or a
second answer harmless. Every change appends one log record (§6b), so `log_wait(project,
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
```

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
  Each takes `session?` and returns `session` (empty when the harness wrote none): binding a
  later step's `session` to an earlier step's `session` output continues that agent.
  Claude always runs Opus. Codex (`agent.codex`, or `agent.run` with engine `codex`) takes
  `model` `sol` (the default) or `astra` and `effort` (`minimal` to `max`), whose default is
  `high`; both always reach the harness. A codex run's echo folds the diff codex prints after
  every patch into one line (the harness log keeps it), and its `final` is the agent's last
  message from the session's rollout. The fns share their engine code through
  `packs/agents/_agents/`.
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
log's. Agents post through `fn_call` (any harness) and read with `log_read`/`log_wait`
(`threads: [name]`); plans use them as ordinary steps.
- `thread.post`: inputs `{thread: string, body: string, from: string, to: string?, data: Any?}`,
  outputs `{seq: int}`. Appends under the project's flock, so concurrent posters get distinct,
  increasing seqs.
- `thread.wait`: inputs `{thread: string, since_seq: int?, to: string?, timeout: int?,
  wake: string?}`, outputs `{messages: Any[], last_seq: int}`: the messages after `since_seq`
  (default 0: all) and, with `to`, only those addressed to it or to nobody; blocks (polling the
  log every 0.5 s) until there is at least one or `timeout` s (default 300) pass, then
  `messages` is empty. With `wake: "questions"` a note (`needs_reply` false) does not end the
  wait: it comes back with the next question, or at the timeout. `last_seq` is the log's last
  seq, to pass back as `since_seq`.

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
(§8a) with `from` = the step (`call <run_id>` for a call) and polls `inbox.json` every 0.5 s
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
reads the file only; it needs no runner or server.

## 11. Conventions

Python ≥ 3.12, `uv` for everything. `src/sluice/fn.py`, `src/sluice/__init__.py`,
`src/sluice/log.py`, `src/sluice/inbox.py` and `src/sluice/util.py` import only the stdlib (fns
import them). Tests in
`tests/`; external tools are faked in tests. Commits: plain sentences, no AI attribution of any
kind; stage exact paths.
