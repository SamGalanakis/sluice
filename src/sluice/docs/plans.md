# Plans

Each project has exactly one plan: JSON with typed `inputs`, named `outputs` and `steps`. A new
project starts with `{"inputs": {}, "outputs": {}, "steps": {}}`. Each step runs one function
(`run`) and binds each of the function's inputs (`in`). The `agent.*` functions below come from
the `agents` pack and `git.head` from the `git` pack — install a pack before using it (see
`docs("fns")`).

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
 "steps": {"head": {"run": "core.echo", "doc": "Pass the repo on once approved",
                    "in": {"value": {"source": "repo"}}},
           "gate": {"run": "core.echo", "in": {"value": {"source": "approved"}}}}}
```

`status` returns them (`input_docs`, and `doc` on a step), the diagram and the dashboard show
them, and an inbox item that asks for an input (`inbox_post(..., input="approved")`) without a
body shows that input's doc. The dashboard's Inbox also lists every required input that holds up
a step and has no value yet, with its doc.

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
  (the MCP tool `step_submit`). A submission that does not fit returns `invalid` listing every
  problem, and the agent submits again; the last one counts. Each accepted one is a
  `step.submit` log record.
- When the agent's process ends, the submitted outputs join the step's outputs. A required
  declared output that was never submitted fails the step, naming it.
- Every agent function takes `session` and returns `session`: bind a later step's `session` to
  an earlier step's `session` output to continue that same agent (or `fn_call` it with the
  session to follow up by hand).

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
   "{unit}-work": {"run": "agent.run", "in": {"engine": {"default": "{engine}"},
                   "cwd": {"source": "{unit}-fork/path"}, "spec": {"file": "{spec}"}},
                   "outputs": {"landed": "boolean"}},
   "{unit}-cleanup": {"run": "git.worktree_rm", "after": ["{unit}-work"],
                      "in": {"repo": {"default": "{repo}"}, "path": {"source": "{unit}-fork/path"}}}}}
```

- `recipe_list(project)` lists the recipes the project sees with their params; a broken file
  is listed with its `error`.
- `unit_add(project, "lane", {"unit": "fix-login", "repo": "/src/app", "base": "origin/main",
  "spec": "/specs/fix-login.md", "engine": "devin"})` adds `fix-login-fork`, `fix-login-work`
  and `fix-login-cleanup` in one edit at the current rev (no rev to fetch), each tagged
  `unit:fix-login`, paused unless `start=true`. It returns `{rev, steps}` and refuses an id the
  plan already has.
- Substitution is tiny on purpose: `{param}` in step ids and in every string is replaced by the
  param's value; a string that is exactly `{param}` becomes the value with its type. `{{` and
  `}}` are literal braces; an unknown `{x}` is an error. `unit` (a valid step id) is always a
  param. There are no loops or conditionals: use `when` and `scatter` in the steps.

## Shapes
- **Chain:** B reads A's output.
- **Fan-out:** several steps read the same output.
- **Fan-in:** one step reads a list source; `core.collect` gathers values into one array and
  `core.format` builds text from them (`"{0} and {1}"`, or `{name}` with a record).
- **Scatter:** `"scatter": "<input>"` runs the step once per item of that input's array; each
  output becomes an array in item order. Use it when the number of items is only known at run
  time. An extra input can be the scattered one; each run's agent submits its own outputs.

A step starts once every plan input and step it reads has a value / has succeeded. A failed step
blocks everything downstream until you act.

## Stale results
A result holds only for the inputs it was computed from. When those change (an upstream re-ran
with a different result, you changed a plan input or a step's binding, or an upstream you
bypassed with `force` finished), the step turns `stale`, and so does every succeeded step
downstream of it. A stale step keeps its outputs so you can look at them, but it never re-runs
by itself, and steps reading it wait. You decide:
- `step_retry(project, steps=[...])` runs it again with the current inputs (then retry its stale
  dependents, or they come back by themselves if its new result is the same as before);
- `step_set_output(project, step, outputs)` accepts a result by hand.
If the inputs change back to what the step was computed from, it is `succeeded` again.

## Names
Project names, step ids, plan input and output names: lowercase letters, digits, `-`, `_`;
start with a letter or digit.

## Editing
Every edit carries the plan's current `rev` (from `plan_get` or `status`). If someone edited in
between you get `conflict` with `current_rev`: re-read and retry. `plan_patch` takes RFC 6902
JSON Patch ops against the plan without `rev`:

```json
[{"op": "add", "path": "/inputs/repo", "value": "string"},
 {"op": "add", "path": "/steps/head", "value": {"run": "git.head", "in": {"path": {"source": "repo"}}}},
 {"op": "replace", "path": "/steps/notes/in/engine", "value": {"default": "codex"}},
 {"op": "remove", "path": "/steps/old-step"}]
```

Without a `rev`: `step_add(project, step, spec)`, `step_update(project, step, changes)` (each
key replaces that field, null removes it) and `step_remove(project, steps=[...])`. They are the
same edit, validated the same way. `edge_add(project, step, after=[...])` and
`edge_remove(project, step, after=[...])` add ids to a step's `after` or take them out, at
the current rev: no rev to read, and edges someone else added are never dropped (a `plan_patch`
of `/steps/<id>/after` replaces the whole list). Adding an edge that is there already, or
removing one that is not, changes nothing.

Tools that change one step's contents take `step` (`step_add`, `step_update`, `step_set_input`,
`step_set_output`, `step_submit`); tools that act on a selection take `steps` (ids; one id is
fine too) and/or `tags`: `step_pause`, `step_retry`, `step_cancel`, `step_remove` and the
`status` filter. A tool refuses an argument it does not take, naming the ones it does.

You cannot remove or change a running step (only pause it). Every edit needs a short `reason`;
it goes into the plan's history (`plan_history`).

## Pausing
Steps you add come in **paused**, so a drafted plan starts nothing until you let it:
`plan_patch(..., start=true)` or `step_add(..., start=true)` lets them start at once.

`step_pause(project, steps=[...], tags=[...], subtree=false, paused=true, reason="")` holds
or releases steps in one edit, by id and/or tag; `subtree=true` takes everything downstream
too, including steps that only become ready later. A paused step does not start, however
ready its inputs; pausing never stops a running one (it finishes, and its next start is
held). The step keeps `"paused": "<reason>"` (or `true`), and `status` lists, for each pending
step, why it is `waiting`. `project_update(name, paused=true)` holds the whole project.
`step_cancel(project, steps=[...], reason=...)` stops running steps; each fails with
`cancelled: <reason>` and `step_retry` runs it again.

## Conditions
`"when": "check/ok"` runs a step only if that value is true: a gate without a gate step. The
ref is read like an input (the step waits for it) and must be a `boolean`. False (or null)
makes the step `skipped`, with the reason (`check/ok is false`), and every step reading from
it is skipped too; steps ordered `after` it still run. If the value changes later (you
retry `check` and it passes), skipped steps go back to pending and run. `status` shows
`when` and `skipped` on each step.

## Ordering and tags
`"after": ["a"]` makes a step wait for `a` to succeed (or be skipped) without reading anything from it: for
two steps that must not overlap (both edit one file) or must happen in order. It is not a
data edge, so `a` turning stale does not make it stale. `"tags": ["e2e", "heavy"]` label
steps so you can pause or release them together. To add an ordering edge to a step that is
already in the plan, use `edge_add`.

## Manual values
- `plan_set_input(project, name, value)`: provide a plan input the plan is waiting on. Changing
  it later makes the steps that already read it stale.
- `step_set_input(project, step, input, value)`: pin a literal on one step input (an edit; a
  succeeded step turns stale).
- `step_set_output(project, step, outputs)`: mark a step succeeded with outputs you supply (you
  did the work, or you know the result). Type-checked against the step's outputs (its
  function's and those it declares); for a
  scattered step, each output is an array. Refused while a step it reads has not succeeded or a
  plan input it reads has no value (the error names them); `force: true` sets it anyway, and the
  step turns stale once those values are all there.
- `step_retry(project, steps=[...])`: run failed, stale or manually set steps again.

## Work done outside sluice
A step whose work happens elsewhere (a person, another orchestrator's workers, a CI run) runs
the built-in `core.external`. The runner never starts it: once ready it waits (`status` says
`external: set its outputs with step_set_output`) until you set its outputs with
`step_set_output`, or `step_cancel` it (it fails `cancelled: <reason>`). Declare the outputs it
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
A **unit** is an independent piece of work: the steps joined by any edge (a handoff, a `when`,
an `after`). It is **done** once every step in it succeeded or was skipped (at least one
succeeded). `status(project)` and `plan_view(project)` leave the done units out, saying how many
(`done_units: {units, steps}` in `status`, one line in the view), so they show what is still
going on; `all=true` shows everything, and `status(steps=[...])` or `tags=[...]` returns what
you ask for, done or not. The dashboard folds a done unit's box instead.

`plan_prune(project, older_than_hours=0)` removes every done unit whose last step finished at
least that long ago, in one edit: nothing else reads from them, and `plan_history` keeps the
removed steps. A unit a plan output reads stays. It returns `{rev, units, steps, outcomes}`.

Removing a step that finished (by `plan_prune`, `step_remove` or any `plan_patch`) keeps what it
ended with in the `outcomes` table: its status, typed outputs, error, times, run ids, unit (its
`unit:` tag, else its unit's first step) and who removed it. Nothing trims it; read it with
`query`, e.g. `SELECT step, status, outputs FROM outcomes WHERE project = 'p' AND unit = 'x'`.

## Validation errors
Every edit is checked: functions exist (as the project sees them), required inputs are bound,
extra inputs and declared outputs only on an open function's step, refs point at real inputs
or outputs, types fit, no cycles. Errors are a list with paths, e.g.
`steps.notes.in.cwd: repo is int, which does not fit string: int is not string`. Fix each path
and resend. `verify(project)` runs the same checks plus function and state checks.
