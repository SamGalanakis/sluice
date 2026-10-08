sluice runs plans: graphs of typed function calls ("steps"). Work is organised in projects; each
project has one plan and its own functions. You edit a plan with these tools; a
runner executes it in the background and records each step's outputs or failure.

The point: the plan carries the routine, so your attention goes to judgment: failures,
questions, decisions. Keep plans light. A step is a unit of work you would hand to a person,
usually an agent block (an agent with a prompt and typed inputs and outputs); an edge is a
real handoff. Agents do their own mechanics (branches, merges, formatting). Change the plan as
you learn. Read `docs("composing")` before your first plan.

Workflow:
1. `projects_list`, or `project_create(name, description)` (it starts with an empty plan). Every
   plan tool takes `project`.
2. `fn_list(project)` to see the functions it can use (built-in, global, its own). Missing one?
   `fn_save(fn, main_py, project)` writes it (see `docs("fns")`).
3. One-off work: `fn_call(name, inputs, project, wait)`. Multi-step work: `plan_get(project)`,
   then `plan_patch(project, rev, ops, reason)` to add steps. Many units of one shape: write
   a recipe once and add each unit with `unit_add(project, recipe, unit, params)`; the same call
   takes its gate entries, input overrides and tags (`docs("plans")`). `edge_add` adds a gate at
   any time. To cap how many steps run at once, declare resources on the project
   (`project_update(project, resources={"lane": 4})`) and give steps `needs` (`docs("plans")`,
   Resources). Every edit takes `dry_run: true` to see what would change before it does.
4. Look with `status(project)`; wait for changes with `step_wait(project, steps, until)`,
   `log_wait(project, since_seq)` or `next(projects)` (every step status change, call and
   message is a log record). A failed step stays failed until you
   act: fix the plan with `plan_patch` (needs the current `rev`), then `step_retry`; or record
   the result yourself with `step_set_output`. A `stale` step was computed from inputs that have
   changed since: `step_retry` it (or accept it with `step_set_output`). Provide values a plan
   waits on with `plan_set_input`. `plan_prune(project)` removes done units from the plan;
   set `project_update(project, prune_done_after=<seconds>)` so finished lanes retire
   themselves.
5. `verify(project)` lists every problem (bad fn.json, name collisions, plan, state) with where
   it is. A project with function problems refuses edits and runs until they are fixed.

Wait, don't poll: never sleep and re-check `status` or the log, and never run your own
watcher. `step_wait(project, steps or tags, until="succeeded"|"settled"|{"any_of": [...]})`
blocks until steps get there; `log_wait` with `kinds`, `statuses` and `recipients` blocks until
a matching record (`log_wait(project, since_seq, kinds=["step.status", "message"],
statuses=["failed", "stale"], recipients=["orchestrator"])` wakes only on failures, stale steps
and messages to you); `next` blocks across projects. See `docs("threads")`.

Refs: a plan input is `name`; a step output is `step/output` (add `.field` or `.0` to reach
inside). Every edit is type-checked; errors name the exact path to fix.

`plan_view(project, "mermaid")` shows the graph with each step's status. Agents working on the
same project talk through messages (`ask`, `say` and `reply`, then `log_wait`); see
`docs("threads")`.

When you need a person (a decision, an approval, a missing value), ask them
(`ask(project, to="owner", title=..., body=...)`, or a `message.ask` step with `wait: true` in
a plan) and wait with `log_wait(project, since_seq, wake="questions")`; see `docs("inbox")`.
Open questions to the owner are the inbox: the only place the person looks for what needs
them — failed steps and workers' questions are yours to handle, not theirs. Answer a worker's
question with `reply(project, to_message=<its id>, body=...)`; a note (`say`) needs no answer.

To give the owner a live view of a project (a lane overview, a few numbers, a chart, a button
that asks you for something), set its board: `board_set(project, program)`, drawn beside the
plan on the dashboard, with a `Doc()` where its document goes. The document is the board's
hand-written part, plain prose for the owner: write it with `board_doc_write`, and whenever
anything it says changes, read it (`board_doc_read`) and edit the lines that changed
(`board_doc_edit`) or rewrite it. Name steps on the board as `tag:<tag>` so a rename changes
nothing, and act on the `warnings` board_set and plan edits return; see `docs("board")`.

Tool arguments are forgiving: an unknown tool or argument names the nearest valid ones, and an
integer argument (a message id, a seq, a limit) may be given as a string of digits. From a
shell, `sluice tool <name> --help` lists a tool's fields, and `sluice tool <name> --field
value` (or `--field-file PATH`, `-` for stdin) takes them as flags instead of JSON.

Read `docs()` for the index, `docs("composing")` and `docs("plans")` before writing your first
plan.
