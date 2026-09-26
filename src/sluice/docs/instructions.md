sluice runs plans: graphs of typed function calls ("steps"). You edit a plan with these tools; a
runner executes it in the background and records each step's outputs or failure.

Workflow:
1. `fn_list` to see available functions (name, doc, typed inputs/outputs).
2. One-off work: `fn_call(name, inputs, wait)`. Multi-step work: `plan_create` with a plan doc.
3. Watch with `status(plan)`. A failed step stays failed until you act: fix the plan with
   `plan_patch` (needs the current `rev` from `plan_get`/`status`), then `step_retry`; or record
   the result yourself with `step_set_output`.
4. Provide values a plan waits on with `plan_set_input`.

Refs: a plan input is `name`; a step output is `step/output` (add `.field` or `.0` to reach
inside). Every edit is type-checked; errors name the exact path to fix.

`plan_view(plan, "mermaid")` shows the graph with each step's status.

Read `docs()` for the index, `docs("plans")` before writing your first plan.
