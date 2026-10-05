# Plans

Each project has exactly one plan: JSON with typed `inputs`, named `outputs` and `steps`. A new
project starts with `{"inputs": {}, "outputs": {}, "steps": {}}`. Each step runs one function
(`run`) and binds each of the function's inputs (`in`). `agent.*` and `git.*` are built in like
`core.*` — every first-party function is compiled into sluice (see `docs("fns")`).

```json
{
  "inputs":  {"repo": "string", "tasks": "string[]"},
  "outputs": {"notes": {"source": "notes/final"}},
  "steps": {
    "work":  {"run": "agent.run", "scatter": "spec",
              "in": {"engine": {"default": "devin"}, "cwd": {"source": "repo"},
                     "spec": {"source": "tasks"}}},
    "gate":  {"run": "core.collect", "in": {"items": {"source": ["work/final"]}}},
    "notes": {"run": "agent.run",
              "in": {"engine": {"default": "claude"}, "cwd": {"source": "repo"},
                     "spec": {"source": "gate/items.0"}}}
  }
}
```

## Docs
Say what an input or a step is for with an optional `doc`. A plan input takes the object form
`{"type": <type>, "doc": "..."}` instead of a bare type; a step takes `"doc"` next to `run`:

```json
{"inputs": {"repo": {"type": "string", "doc": "Absolute path of the checkout to work in"},
            "approved": {"type": "boolean", "doc": "Whether the owner accepts the change"}},
 "outputs": {},
 "steps": {"head": {"run": "git.head", "doc": "Pass the repo on once approved",
                    "after": ["approved"], "in": {"path": {"source": "repo"}}}}}
```

`plan_get` returns them with the plan, and the dashboard and `plan_view` show them.

## Binding a step input
- `{"default": <json>}`: a literal value.
- `{"source": "repo"}`: a plan input.
- `{"source": "work/final"}`: another step's output. Add `.field` or `.0` to reach inside:
  `"review/report.summary"`, `"gate/items.0"`.
- `{"source": ["a/out", "b/out"]}`: fan-in, the step gets an array of those values in order.
- `{"file": "/abs/path/spec.md"}`: the file's text, a `string`, read when the step starts (each
  start, a retry's too). Keep a long spec in a file instead of pasting it into the plan: edit
  the file until the step starts, and that text runs. A missing file fails the step, naming
  it; `verify` warns about one missing now. Editing the file after the step succeeded does not
  make it stale (only the path counts); the new text runs the next time the step starts.
- Optional function inputs (`T?`) may be left out.

A source binding is a **handoff**: the step waits for its sources to succeed, and is skipped
when a source step is skipped.

## Agent blocks: typed inputs and outputs
An agent function (`agent.claude`, `agent.codex`, `agent.devin`, `agent.run`, `agent.review`)
is **open** (`fn_list` shows `open: true`): a step running it may bind extra inputs of any name
and declare the outputs it will produce. How to shape a plan around them: `docs("composing")`.

```json
{"inputs": {"repo": "string"},
 "outputs": {"branch": {"source": "logic/branch"}},
 "steps": {
   "design": {"run": "agent.claude", "doc": "Write the interface the others build on",
              "in": {"cwd": {"source": "repo"},
                     "prompt": {"default": "Design the scoring interface; commit it on a new branch."}},
              "outputs": {"interface": {"type": "string", "doc": "Path of the interface file"},
                          "branch": "string"}},
   "logic": {"run": "agent.claude",
             "in": {"cwd": {"source": "repo"},
                    "prompt": {"default": "Implement the interface on a branch of your own, based on the given one."},
                    "interface": {"source": "design/interface"},
                    "base": {"source": "design/branch"}},
             "outputs": {"branch": "string"}}}}
```

- **Extra inputs** (`interface`, `base` above) take the type of their source (`Any` for a
  `default`). The agent sees each one, with its type and value, under `## Inputs` in its task.
- **Declared outputs** (`outputs`, next to `run` and `in`) are a type, or `{"type", "doc"}`;
  they join the function's own outputs, so `design/interface` is a ref like any other and is
  type-checked where it is read.
- The agent is told the outputs under `## Outputs you must submit`, with the exact command:
  `sluice tool step_submit '{"project": ..., "step": ..., "run": ..., "outputs": {...}}'`
  (the `step_submit` tool; in the run, `sluice tool step_submit --outputs-file out.json` does
  the same with no shell quoting), and: "Submit only when you are finished: submitting ends
  your session." A submission that does not fit returns `invalid` listing every problem and
  changes nothing; the agent fixes them and submits again.
- A valid submission is the agent's done signal (a `step.submit` record): its session is
  stopped at once and the step completes with the fn's outputs (`session`, `final`, `git` for
  the agent fns) and the submitted ones, so its dependents start right away. There is no
  second submission (`conflict`); to send the step more work, `step_retry` it with a message,
  which reopens it as a new run.
- Until its run ends, a step that has submitted is **finishing**: `status`, `step_context` and
  `call_status` show `finishing: {since, submission_seq, release}` (when it submitted, its
  `step.submit` record, the run's pinned release); the units view marks it `▷` and its line
  says `finishing <step>`. Messages to it, a second submission and `step_set_output` are
  refused meanwhile. A run pinned to a release from before the done signal does not stop its
  agent on submit: it waits until the agent has been idle through its grace. Settle such a
  run on its submission with `step_settle(project, step, reason)`: the runner stops the agent
  and the step succeeds with the submission plus the agent fn's own outputs (`session`,
  `final`, `model`, `git`), exactly as the done signal would have. Only a step that runs an
  agent fn itself; a fn that composes an agent returns its own outputs, so for it (and
  whenever `step_settle` cannot derive them) `step_cancel` the step and `step_set_output`
  what it should have.
- A run that ends without a valid submission (its agent exited, or stopped after its
  nudges) fails the step with `exited_without_submit`, carrying the agent's `session`.
- An engine's account problem fails the step at once with `agent_failure`, carrying the
  `session`, and nothing inside the run retries it:
  - kind `QuotaExhausted`: a hard usage cap (Codex, Claude or Devin: a usage, plan or credit
    limit, or any limit resetting more than 15 minutes away);
  - kind `AuthFailed`: the engine cannot authenticate on this host (logged out, a token expired
    or revoked, the account barred).

  The message says what happened, what the owner must do and when it resets if known, then the
  engine's own text (secrets masked), e.g. ``codex: not logged in (token revoked) — run `codex
  login` on this host, then step_retry. Codex said: …`` or `claude: weekly limit reached
  (seven_day); resets 2026-10-08T23:00Z (in 3d) — wait for the reset or buy usage credits at
  …, then step_retry (or run the step on another engine). Claude said: …`. What to do as the
  orchestrator:
  - Do not `step_retry` it until the cause is fixed: a retry before then fails the same way and
    spends a launch. Expect other steps on that engine to fail the same way until then.
  - Tell the owner with the message as given, by `ask(project, to="owner", ...)`
    (`docs("inbox")`) or in your reply in the conversation: only the owner can sign in again on
    the host or add quota.
  - Once the owner says it is fixed (`AuthFailed`), or after the reset or once quota is added
    (`QuotaExhausted`), `step_retry` with the step's `session` bound to it to resume the same
    agent. Or, if the work cannot wait, rerun the step on another engine.

  A short rate limit is not either kind: it stays `transient` and is retried inside the run,
  just after its reset when the engine reported one.
- Each agent's task starts with `## Previous attempt`: "None: this is the first attempt at
  this step.", or which attempt it is, how the one before ended (failed with its error,
  cancelled by whom and why, settled), what it submitted (or output), the git head it left and
  whether it left changes, and the `git status` of its `cwd` now (count and up to 20 paths).
  `step_context` (`sluice me`) carries the same as `attempt: {number, previous?, worktree?,
  note}`. Look at what an earlier attempt left before redoing or undoing it.
- Every agent function takes `session` and returns `session`: bind a later step's `session` to
  an earlier step's `session` output to continue that same agent (or `fn_call` it with the
  session to follow up by hand). An unbound `session` may still resume the previous attempt's
  session after a retry — the agent fns decide from the assigned messages and `prev_run`.

## Choosing a model
`agent.run`, `agent.devin`, `agent.codex` and `agent.claude` take `model`, a JSON object. Leave
it out for the engine's default (devin swe-2 high, codex sol high, claude opus high). There is
no separate `effort` input: the effort is in the object.

```json
{"inputs": {"repo": "string"},
 "steps": {
   "build": {"run": "agent.devin",
     "in": {"cwd": {"source": "repo"},
            "model": {"default": {"type": "fusion",
                                  "main": {"model": "claude-opus-5-5", "effort": "high", "fast": false},
                                  "sidekick": {"model": "swe-2", "effort": "high"}}},
            "spec": {"default": "Build the feature on a branch of your own and commit there."}},
     "outputs": {"branch": "string"}},
   "check": {"run": "agent.codex",
     "in": {"cwd": {"source": "repo"}, "branch": {"source": "build/branch"},
            "model": {"default": {"type": "normal", "model": "sol", "effort": "xhigh"}},
            "spec": {"default": "Check out the branch under Inputs and review it."}}}}}
```

- **Normal**, any engine: `{"type": "normal", "model": M, "effort": E?, "fast": bool?}`.
  Devin runs `M[-E][-fast]` (`swe-2-high`, `claude-opus-5-5-high-fast`; leave `effort` out for
  a model without one, such as `adaptive`). Codex runs `M` at reasoning effort `E` (`sol` and
  `astra` name `gpt-6.1-sol` and `gpt-6-astra`; other names are `codex debug models` slugs).
  Claude runs Opus (`opus` or `claude-opus-5-5`) at `low`, `medium`, `high`, `xhigh` or `max`.
  Codex and Claude cannot run fast.
- **Fusion**, Devin only: `{"type": "fusion", "main": {"model": M, "effort": E?, "fast":
  bool?}, "sidekick": {"model": S, "effort": F?, "priority": bool?}}` runs
  `fusion-M[-E][-fast]-sidekick-S[-F][-priority]`, so the object above runs
  `fusion-claude-opus-5-5-high-sidekick-swe-2-high`; `"fast": true` on main runs
  `fusion-claude-opus-5-5-high-fast-sidekick-swe-2-high`.
- Unknown keys, empty strings and non-boolean `fast` or `priority` are refused. The id is
  checked against the engine's own list (`devin models list`, `codex debug models`) before
  the session starts. An unknown id fails the run (`agent_failure`, kind `Invalid`) with the
  nearest ids it lists; nothing else runs instead. The result's `model` is the id that ran.
- A string `model` (`"sol"`, `"fusion"`) or an `effort` input is the retired form: the run
  fails at launch with the object to use instead.

## Units: the tag is the unit
Steps tagged `unit:<name>` form **unit** `<name>`; a step carries at most one `unit:` tag. An
untagged step is a unit of one, addressed by its step id. Edges may cross units freely. A unit
is **done** when every step in it succeeded or was skipped.

A unit's **exit steps** are its steps tagged `exit` (the tag is reserved); when none is tagged,
they are its sinks over the unit's own edges. Exits mark delivery — what `unit:<name>` gate
entries and `next` settling look at — not the definition of done. A unit may not depend on its
own exits.

`unit_add` tags every step it adds `unit:<unit>`; recipes tag their delivery step `exit` so
cleanup stays out of the dependency.

## Recipes: the same unit again and again
When every unit of work has the same shape (a worktree, an agent working in it, the worktree
removed), write it once as a recipe and add each unit with one call. A recipe is a file
`recipes/<name>.json` under `SLUICE_HOME` (every project sees it) or under the project's
directory `projects/<p>/recipes/` (the project's wins on a name clash):

```json
{"name": "lane",
 "doc": "One unit of work: a worktree, an agent working in it on a spec read from a file, then the worktree removed",
 "params": {"repo": "string", "base": "string", "spec": "string",
            "engine": {"type": "enum", "symbols": ["devin", "codex", "claude"]}},
 "steps": {
   "{unit}-fork": {"run": "git.worktree", "in": {"repo": {"default": "{repo}"},
                   "base": {"default": "{base}"}, "branch": {"default": "work/{unit}"}}},
   "{unit}-work": {"run": "agent.run", "tags": ["exit"],
                   "in": {"engine": {"default": "{engine}"},
                          "cwd": {"source": "{unit}-fork/path"}, "spec": {"file": "{spec}"}},
                   "outputs": {"landed": "boolean"}},
   "{unit}-cleanup": {"run": "git.worktree_rm", "after": ["{unit}-work?"],
                      "in": {"repo": {"default": "{repo}"}, "path": {"source": "{unit}-fork/path"}}}}}
```

- `unit_add(project, "lane", "fix-login", {"repo": "/src/app", "base": "origin/main",
  "spec": "/specs/fix-login.md", "engine": "devin"})` adds `fix-login-fork`, `fix-login-work`
  and `fix-login-cleanup` in one edit at the current rev (no rev to fetch), each tagged
  `unit:fix-login`; they start when ready (`start=false` adds them paused). It returns the
  edit result `{project, rev, preview, steps}` (`steps` the ids it added) and refuses an id
  the plan already has.
- The same call stages the whole lane, in that one edit. `after` and `inputs` are keyed by the
  recipe step's suffix (its id without `<unit>-`: `fork`, `work`, `cleanup`); `"*"` means the
  unit's entry steps:

  ```
  unit_add(project, "lane", "fix-login", {...},
           after={"*": ["unit:auth"]},
           inputs={"work": {"model": {"type": "normal", "model": "swe-2", "effort": "max"}}},
           tags=["arc:auth"])
  ```

  `after` entries are added to the step's own, and each input is bound to
  `{"default": value}`. An unknown suffix, or an input the step's fn does not declare and the
  recipe does not bind, is refused before anything is written.
- `unit_add` takes `tags` too, for every step of the unit next to `unit:<unit>`: group units
  into an **arc** (`arc:auth`) and act on it with `status`, `step_pause`, `step_cancel`,
  `step_retry`, `step_remove` or `plan_prune` by `tags=["arc:auth"]`. `unit_tag(project, unit,
  add=[...], remove=[...])` retags a unit later and returns the edit result with the unit's
  `steps`; tags already as asked are no edit. `unit:` tags are reserved.
- Substitution is tiny on purpose: `{param}` in step ids and in every string is replaced by the
  param's value; a string that is exactly `{param}` becomes the value with its type. `{{` and
  `}}` are literal braces; an unknown `{x}` is an error. `unit` (a valid step id) is always a
  param. There are no loops or conditionals: use `after` gate entries and `scatter` in the
  steps.

## Shapes
- **Chain:** B reads A's output.
- **Fan-out:** several steps read the same output.
- **Fan-in:** one step reads a list source; `core.collect` gathers values into one array and
  `core.format` builds text from them (`"{0} and {1}"`, or `{name}` with a record).
- **Scatter:** `"scatter": "<input>"` runs the step once per item of that input's array; each
  output becomes an array in item order. Use it when the number of items is only known at run
  time. An extra input can be the scattered one; each run's agent submits its own outputs.

A step starts once every plan input and step it reads has a value / has succeeded and every
`after` entry is satisfied. A failed step blocks everything downstream until you act.

## Gates: `after`
`"after": [...]` is a list of **gate entries**: the step waits until every entry is satisfied,
and is skipped when one says so.

| entry | satisfied when | skips the step when |
|---|---|---|
| `"a"` (a step id) | `a` succeeded | `a` was skipped |
| `"a?"` | `a` succeeded or was skipped | never |
| `"check/ok"` (a boolean ref) | the value is `true` | the value is `false` or `null`, or `check` was skipped |
| `"!check/ok"` | the value is `false` | the value is `true` or `null`, or `check` was skipped |
| `"enabled"` (a boolean plan input) | the value is `true` | the value is `false` or `null` |
| `"unit:up"` | every exit step of unit `up` succeeded | any exit step was skipped |
| `"unit:up?"` | every exit step succeeded or was skipped | never |

Rules:
- A step is skipped when any handoff source or any gate entry says so, and it is decided again
  whenever its reasons change — a value turning true, an upstream retried and succeeding, a
  gate edited — going back to `pending`.
- An entry whose step is pending, running, failed or stale is unsatisfied: the step waits.
  Cleanup that must run behind a skipped step uses the `?` form.
- A ref entry must be typed `boolean`, `boolean?` or `Any`; `?` may not go on a ref, `!` may
  not go on a step or unit.
- Gates decide starts only. They never stop running work, they are not evaluated for paused
  steps, and a gate's producer never makes a step stale — only handoff bindings feed the
  inputs hash.
- A bare entry names a step or a boolean plan input; the two share one namespace.

`edge_add(project, step, after=[...])` and `edge_remove(project, step, after=[...])` edit a
step's gate list at the current rev. Entries keep their order and are deduplicated; a unit
entry counts as edges to the unit's exit steps for cycles.

## Ordering and tags
`"after": ["a"]` makes a step wait for `a` without reading anything from it: for two steps
that must not overlap (both edit one file) or must happen in order. It is not a data edge, so
`a` re-running with a different result does not make the step stale. `"tags": ["e2e", "heavy"]`
label steps so you can pause, cancel, retry or select them together; `unit:<name>` and `exit`
are the tags the model itself reads.

## Manual values
- `plan_set_input(project, name, value)`: provide a plan input the plan is waiting on. Changing
  it later makes the steps that already read it stale.
- `step_set_input(project, steps=[...], tags=[...], inputs={name: value})`: pin literals on
  step inputs, across a selection in one edit. Only steps that have the inputs are changed
  (running steps are left alone), and a succeeded step turns stale. It returns the edit result
  `{project, rev, preview}` with `changed` (the steps changed), `running` (selected but
  running, left alone) and `unsupported` (`[{step, inputs}]`: selected but lacking those
  inputs); an edit that would change no step is refused (`bad_request`).
- `step_set_output(project, step, outputs)`: mark a step succeeded with outputs you supply (you
  did the work, or you know the result). Type-checked against the step's outputs (its
  function's and those it declares); for a
  scattered step, each output is an array. Refused while a step it reads has not succeeded or a
  plan input or gate it waits on is unsatisfied (the error names them); `force: true` sets it
  anyway, and the step turns stale once those values are all there.
- `step_retry(project, steps=[...])`: run failed, stale, succeeded or manually set steps again.
  Retrying also re-arms the failed and stale steps directly blocked behind it (`{steps,
  rearmed, stopped_at}`); a skipped step is decided by its gates, never retried. Pass
  `message="..."` to post that message to each retried step — the send-back: its next run
  starts with the message first.

## Work done outside sluice
A step whose work happens elsewhere (a person, another orchestrator's workers, a CI run) runs
the built-in `core.external`. The runner never starts it: once ready it waits (its wait reason
reads `external: set its outputs with step_set_output`) until you set its outputs with
`step_set_output`, or `step_cancel` it (it fails with the error `{"error": "cancelled", "message": <reason>}`). Declare the outputs it
will get, bind extra inputs to order it after their sources, and say in its `doc` who is doing
the work and where; the dashboard shows it as live outside work, and the steps behind it wait
for it. It does not scatter.

```json
{"inputs": {"spec": "string"}, "outputs": {},
 "steps": {"work":  {"run": "core.external", "doc": "Five workers in wt-a..wt-e; the orchestrator sets final when they land",
                     "in": {"spec": {"source": "spec"}}, "outputs": {"final": "string"}},
           "notes": {"run": "core.format", "in": {"template": {"default": "Landed: {0}"},
                                                  "values": {"source": ["work/final"]}}}}}
```

To move a step's work out (say a long agent step you fan out yourself), patch its `run` to
`core.external` and `step_retry` it. Its inputs stay (the old function's own inputs become
extra inputs) and so do its declared outputs; declare any output of the old function a
dependent reads (`{"op": "add", "path": "/steps/w/outputs/final", "value": "string"}`), and
drop its `scatter`.

Every edit, manual value and step status change is a record in the project's log:
`plan_history(project)` shows every edit (back to rev 1) and the manual values the log still
has, `log_read(project)` everything (`docs("threads")`).

## Keeping the plan short
`status(project)` and `plan_view(project)` leave the done units out, saying how many
(`done_units: {units, steps}` in `status`, one line in the view), so they show what is still
going on; `status(steps=[...])` or `tags=[...]` returns what you ask for, done or not. The
dashboard folds a done unit's box instead.

`plan_prune(project, units?, tags?, older_than=0)` removes done units — every unit by
default, or the ones you name or tag — whose last step finished at least `older_than` seconds
ago, in one edit. A unit that any surviving step or plan output still references — a handoff, a
gate, a `unit:` entry — is kept. Naming a unit that is not done is `invalid`. It returns the
edit result `{project, rev, preview, steps}` (`steps` the removed steps) with `units` (the
removed units) and `kept`: each unit kept with what holds it, `{unit, step}` or `{unit,
output}` for a plan output. When nothing can go it is no edit.

Removing a step that finished (by `plan_prune`, `step_remove` or any `plan_patch`) keeps what it
ended with in the `outcomes` view: its status, outputs, error, run ids, unit, when it was
recorded and when it was removed. Nothing trims it; read it with `query`, e.g.
`SELECT step_id, status, outputs FROM outcomes WHERE project_id = ?1 AND unit = 'x'` with the
project's id as the parameter.

## Editing
`plan_patch` takes the plan's current `rev` (from `plan_get` or `status`) and RFC 6902 JSON
Patch ops against the plan document. If someone edited in between you get `conflict` with
`current_rev`: re-read and retry.

```json
[{"op": "add", "path": "/inputs/repo", "value": "string"},
 {"op": "add", "path": "/steps/head", "value": {"run": "git.head", "in": {"path": {"source": "repo"}}}},
 {"op": "replace", "path": "/steps/notes/in/engine", "value": {"default": "codex"}},
 {"op": "remove", "path": "/steps/old-step"}]
```

The other edit tools take an optional `rev`: leave it out to edit the current plan, or pass it
to be refused with `conflict` if the plan moved on.

Small edits: `step_add(project, step, spec)`, `step_update(project, step, changes)` (each
key replaces that field, null removes it) and `step_remove(project, steps=[...])`. They are the
same edit, validated the same way. `edge_add` and `edge_remove` are the only verbs for gates:
no rev to read, and edges someone else added are never dropped (a `plan_patch` of
`/steps/<id>/after` replaces the whole list). Adding an edge that is there already, or
removing one that is not, leaves the plan as it was: no edit, the current `rev`. `step`
may also be `unit:<name>`: the entries then go on every entry step of that unit.

Tools that change one step's contents take `step` (`step_add`, `step_update`, `step_set_output`,
`step_submit`); tools that act on a selection take `steps` (ids; one id is fine too) and/or
`tags`: `step_pause`, `step_retry`, `step_cancel`, `step_remove`, `step_set_input` and the
`status` filter (`plan_prune` takes `units` and `tags`). A tool refuses an argument it does not
take, naming the ones it does.

Every edit tool takes `dry_run: true`, which returns `{ops, would_start, would_queue,
would_skip, would_stale, errors}` from one simulation without changing anything.

An edit that changes nothing is no edit: it commits nothing and returns the current `rev`
with empty `preview.ops` (`step_set_input` refuses one instead).

You cannot remove or change a running step (only pause or tag it). Every edit needs a short
`reason`; it goes into the plan's history (`plan_history`).

## Pausing
Steps you add start as soon as they are ready. To draft first, pass `start=false` to
`plan_patch`, `step_add` or `unit_add`: the steps it adds come in **paused** (unless a step sets
`paused` itself) and start nothing until you release them. To cap how many run at once, use
resources and `needs` (Resources, below), not pauses.

`step_pause(project, steps=[...], tags=[...], subtree=false, paused=true, reason="")` holds or
releases steps in one edit, by id and/or tag — a `unit:` tag selects the whole unit; with
`subtree=true` also every step downstream (reading from or gated on one, transitively). A
paused step does not start, however ready its inputs; pausing never stops a running one (it
finishes, and its next start is held). The step gets `"paused": "<reason>"`, or `true` without
a reason (a step already paused keeps its own); `status` shows it as `paused` and in
`waiting`, and so does the dashboard. It returns the edit result with the selected `steps`.
A plan may also set `"paused": "<reason>"` on a step directly. `project_update(project, paused=true)` holds the whole project.
`step_cancel(project, steps=[...], reason=...)` stops running steps; each fails with the error
`{"error": "cancelled", "message": <reason>}` and `step_retry` runs it again.
`step_settle(project, step, reason)` stops a finishing step's agent instead and succeeds it on
its submission (agent steps, above).

## Resources: limiting what runs at once
Every ready step starts at once, unless it asks for a project's **resources**. Declare them on
the project, each with a fixed capacity or a function that reports one:

```json
{"lane": 56, "codex": {"capacity": 12}, "cpu": {"capacity_fn": "ops.cpu-free"}}
```

`project_create(name, description, resources={...})` or `project_update(project,
resources={...})` (each key set, null removes one; one a step needs cannot be removed). A
`capacity_fn` is a fn the project sees that takes no required input and returns
`{"capacity": <int>}`; the runner calls it about every 10 s, keeps the last good value when a
call fails or times out, and admits nothing on it until the first value comes.

A step asks with `needs` and, among the queued steps, `priority` (default 0, higher first; ties
in plan order):

```json
{"run": "agent.run", "needs": {"lane": 1, "codex": 1}, "priority": 5,
 "in": {"engine": {"default": "codex"}, "cwd": {"default": "/src/app"},
        "spec": {"default": "Fix the flaky login test"}}}
```

It starts only when every resource it names has room (`capacity - held >= need`); while it
runs it holds those amounts (a scattered step once, whatever its item count), and they free
when it succeeds, fails or is cancelled. Until then it stays `pending`, queued — not paused —
and `status` says why in its `waiting`: `queued: needs lane 1 (56/56 held)`. Every pending step
that is not about to start has `waiting`: its pause, `project paused`, each handoff or gate
not ready (`step a is running`, `plan input n has no value`, `after b (failed)`), the resource
shortfall, or for `core.external` the external wait. `status` also
returns `resources`: each one's `capacity`, `held` and how many steps are `queued` on it.
Pausing is still the hold you put on by hand; a step without `needs` is never held back. An
edit naming a resource the project does not declare, or asking for more than a fixed capacity,
is refused. In a recipe, a whole-string `{param}` keeps its type: `"needs": {"lane":
"{lanes}"}`, `"priority": "{prio}"`.

## Stale results
A result holds only for the inputs it was computed from. When those change (an upstream re-ran
with a different result, you changed a plan input or a step's binding, or an upstream you
bypassed with `force` finished), the step turns `stale`, and so does every succeeded step
downstream of it. A stale step keeps its outputs so you can look at them, but it never re-runs
by itself, and steps reading it wait. You decide:
- `step_retry(project, steps=[...])` runs it again with the current inputs — and re-arms the
  failed and stale steps blocked behind it, so a chain comes back in one call;
- `step_set_output(project, step, outputs)` accepts a result by hand.
If the inputs change back to what the step was computed from, it is `succeeded` again.

## Names
Project names, step ids, plan input and output names: lowercase letters, digits, `-`, `_`;
start with a letter or digit. Thread names use the same alphabet.

## Validation errors
Every edit is checked: functions exist (as the project sees them), required inputs are bound,
extra inputs and declared outputs only on an open function's step, refs point at real inputs
or outputs, gate entries name a step, a boolean ref or a unit, types fit, no cycles. Errors are
a list with paths, e.g.
`steps.notes.in.cwd: repo is int, which does not fit string: int is not string`. Fix each path
and resend. `verify(project)` runs the same checks plus function and state checks.
