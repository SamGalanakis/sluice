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

## Stale results
A result holds only for the inputs it was computed from. When those change (an upstream re-ran
with a different result, you changed a plan input or a step's binding, or an upstream you
bypassed with `force` finished), the step turns `stale`, and so does every succeeded step
downstream of it. A stale step keeps its outputs so you can look at them, but it never re-runs
by itself, and steps reading it wait. You decide:
- `step_retry(project, step)` runs it again with the current inputs (then retry its stale
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

You cannot remove or change a running step. Every edit needs a short `reason`; it goes into the
plan's history (`plan_history`).

## Manual values
- `plan_set_input(project, name, value)`: provide a plan input the plan is waiting on. Changing
  it later makes the steps that already read it stale.
- `step_set_input(project, step, input, value)`: pin a literal on one step input (an edit; a
  succeeded step turns stale).
- `step_set_output(project, step, outputs)`: mark a step succeeded with outputs you supply (you
  did the work, or you know the result). Type-checked against the function's outputs; for a
  scattered step, each output is an array. Refused while a step it reads has not succeeded or a
  plan input it reads has no value (the error names them); `force: true` sets it anyway, and the
  step turns stale once those values are all there.
- `step_retry(project, step)`: run a failed, stale or manually set step again.

Every edit, manual value and step status change is a record in the project's log:
`plan_history(project)` shows the edits and manual values, `log_read(project)` everything
(`docs("threads")`).

## Validation errors
Every edit is checked: functions exist (as the project sees them), required inputs are bound,
refs point at real inputs or outputs, types fit, no cycles. Errors are a list with paths, e.g.
`steps.notes.in.cwd: repo is int, which does not fit string: int is not string`. Fix each path
and resend. `verify(project)` runs the same checks plus function and state checks.
