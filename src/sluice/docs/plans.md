# Plans

Each project has exactly one plan: JSON with typed `inputs`, named `outputs` and `steps`. A new
project starts with `{"inputs": {}, "outputs": {}, "steps": {}}`. Each step runs one function
(`run`) and binds each of the function's inputs (`in`).

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

## Binding a step input
- `{"default": <json>}`: a literal value.
- `{"source": "repo"}`: a plan input.
- `{"source": "work/final"}`: another step's output. Add `.field` or `.0` to reach inside:
  `"review/report.summary"`, `"gate/items.0"`.
- `{"source": ["a/out", "b/out"]}`: fan-in, the step gets an array of those values in order.
- Optional function inputs (`T?`) may be left out.

## Shapes
- **Chain:** B reads A's output.
- **Fan-out:** several steps read the same output.
- **Fan-in:** one step reads a list source; `core.collect` gathers values into one array and
  `core.format` builds text from them (`"{0} and {1}"`, or `{name}` with a record).
- **Scatter:** `"scatter": "<input>"` runs the step once per item of that input's array; each
  output becomes an array in item order. Use it when the number of items is only known at run
  time.

A step starts once every plan input and step it reads has a value / has succeeded. A failed step
blocks everything downstream until you act.

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

You cannot remove or change a running step. Every edit needs a short `reason`; it goes into the
plan's history (`plan_history`).

## Manual values
- `plan_set_input(project, name, value)`: provide a plan input the plan is waiting on. Changing
  it later only affects steps that have not started.
- `step_set_input(project, step, input, value)`: pin a literal on one step input (an edit).
- `step_set_output(project, step, outputs)`: mark a step succeeded with outputs you supply (you
  did the work, or you know the result). Type-checked against the function's outputs; for a
  scattered step, each output is an array.
- `step_retry(project, step)`: run a failed (or manually set) step again.

## Validation errors
Every edit is checked: functions exist (as the project sees them), required inputs are bound,
refs point at real inputs or outputs, types fit, no cycles. Errors are a list with paths, e.g.
`steps.notes.in.cwd: repo is int, which does not fit string: int is not string`. Fix each path
and resend. `verify(project)` runs the same checks plus function and state checks.
