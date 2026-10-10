# Plans

Each project has exactly one plan, stored as rows and exported as JSON with typed `inputs`,
named `outputs` and `steps`. These JSON documents are the export format, not edit requests. A new
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

`plan_get` exports them with the plan. Full `step_get` and `plan_read` replies include them
in `spec`, and the dashboard shows them.

## Titles: how a step is named on the dashboard
Every step has a title the owner reads instead of its id; the id stays beside it in mono. Give
your steps titles by writing what you already write:

- a step's `doc`: its first line is its title, the rest says more ("Port the ledger shard\n
  It owns the write path…");
- else the first heading (or first line) of its `spec`, `prompt` or `task` input: a literal
  string, or the file a `{"file": path}` names (its first 4 KiB is read);
- in a recipe's unit, the recipe's `title` (below) names every step of the unit, each with its
  stage before it ("work · FIG-12: Fix the cron driver").

A step with none of these is shown by its id. Titles are read from the plan when it is drawn,
never stored: edit the doc or the spec file and the title follows. `status` returns each step's
`title` (and `stage` in a unit of several steps) and each unit's `title`, so you can name steps
the way the owner sees them.

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
- Before it is done, a running step may publish its latest values with
  `step_progress(project, step, run, outputs)` (in the run, `sluice tool step_progress
  --outputs '{"red": 3}'`; `ctx.progress(red=3)` in a fn): fields of its outputs, type-checked
  and merged over its earlier progress. Progress is never final: no step reads it, it is not
  a submission and does not end the session, and it writes no log record, so it wakes no
  `next` or `log_wait`. The dashboard and a board's `Output` show it, and `query` reads it
  from `steps.progress` and `steps.progress_at`. The next run of the step clears it.
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
- A Codex network blip never fails a step. Codex retries a dropped connection itself
  (`Reconnecting... 2/5`, one line each in the run's log) while its turn goes on. When Codex gives
  up, the run waits (30 s, then 2 min, then 8 min) and continues the same session with a short
  "continue where you left off" input; only a network still down after that fails the step
  `transient`, with a message naming the cause (`codex: network error
  (responseStreamDisconnected, HTTP 403); Codex gave up after its own retries. Codex said: …`)
  and the `session`. `step_retry` with that `session` bound resumes it once the network is back.
- A Claude or Devin agent that stops before its first turn on a screen nobody can answer
  (Claude's first-run setup, its API-key prompt, updated terms, managed-settings or MCP-server
  approval, a required update; Devin's organization picker or workspace-trust prompt; or any
  screen that stands unchanged for 20 s with no turn) fails the step at once with
  `agent_failure` kind `BlockedScreen`, carrying the `session`. The message names the screen
  and what the owner must do, then quotes it, e.g. ``claude: blocked on its first-run setup
  (theme picker) — Claude Code's first-run setup is unfinished for the account it runs as: run
  `claude` once on this host as that user and finish it (…), then step_retry. Claude showed:
  Let's get started. | …``. Treat it as `AuthFailed`: tell the owner the message as given, do
  not `step_retry` until they say it is answered, then retry with the `session` bound, or run
  the step on another engine.
- An engine CLI that updated itself keeps running: a version newer than the ones sluice was
  tested on runs untested, and the run's result `notes` say so (`codex 0.161.0 is newer than
  the tested 0.160.0, 0.160.1; accepted untested (floor 0.160.0)`). A launch fails with
  `agent_failure` kind `CapabilityMismatch` only when the engine lacks something sluice needs
  (``codex 0.161.0 lacks `turn/steer` that sluice needs …``), is older than its floor, or (Devin)
  is a newer major; and a run on an untested version that fails in a way a protocol change
  explains ends its message with `likely cause: untested <engine> <version> (tested …)`. Retrying
  will not help: tell the owner the message as given (the fix is on the host: update, or pin the
  engine CLI to a tested version), or run the step on another engine. `sluice doctor` shows each
  engine's tested versions, floor, installed version and probe results.
- When an agent run fails before its engine completed a turn, or on a timeout or stall
  (`TurnStartTimeout`, `ReadyTimeout`, `StallCap`, `WallCap`), its message ends with the last
  rows of the engine's screen at the failure, under `pane at failure (last rows; whole screen:
  <path>):`; the whole screen is in `pane-at-failure.txt` in the run's directory (secrets
  masked). Read those rows before retrying: they usually say what the engine was waiting for.
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
`agent.codex` and `agent.devin` return `log`, the path to a readable run record of
turns, completed messages, tool calls and errors. Bind a later step's input to `work/log`
to read or copy it. A nonempty `log` input copies the record to that path. The output is
null if no log file exists. Codex keeps its redacted diagnostic wire transcript separately.
Claude returns its last message in `result` and does not expose a `log` output.

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
  fails at launch with the object to use instead. An edit that adds an agent step with a
  string `model`, or changes a step's model to one (`step_add`, `unit_add` with its recipe's
  params and `inputs`, `step_update`, `step_set_input`, `plan_edit`, or `plan_set_input` on an
  input a model reads), is refused at once with that message. In a recipe whose `model` param
  is a name, pass the object through `unit_add`'s `inputs`:
  `"inputs": {"work": {"model": {"type": "normal", "model": "sol", "effort": "high"}}}`.

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
- **Title and view.** A recipe may say how its units read on the dashboard. `"title"` is a
  template filled with the unit's params: `"title": "{ticket}: {spec}"`. A param bound as
  `{"file": "{spec}"}` stands for that file's title (its first heading), not its path. `"view"`
  is an OpenUI Lang program drawing a unit's one-line summary; the plan draws
  a recipe's live units as one block per band (a row a unit, a column a stage, the units that
  need someone in For you and Stopped first), the view in each unit's row, and the view whole on
  the unit's page:

  ```json
  "title": "{spec}",
  "view": "root = Stack([Output(\"land\", \"landed_sha\"), LastMessage(140)], \"row\")"
  ```

  The view's vocabulary is `Stack(children, direction?)`, `Text(text, tone?)`,
  `Markdown(text)`, `Link(label, href)`, `Param(name)`, `Output(stage, field)`,
  `StepStatus(stage)` and `LastMessage(chars?)`; a stage is a step id without `{unit}-`, and
  a `Param` draws nothing (the unit's Details menu on its row and page lists its params), and
  `{param}` works in any string. `recipe_list` returns each recipe's `stages`, its `title` and
  `view` as written, and `title_error` or `view_error` when one does not check (the recipe still
  adds units; its block then shows a note instead of the summary). Nothing is stored with the
  plan: the dashboard finds a unit's recipe again from its step ids and fns, and reads its
  params back from its steps, so a unit you reshape by hand no longer matches and is drawn as a
  plain unit.
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
are the tags the model itself reads. `cadence:<n>m|h|d` tells the dashboard how long a running
step may write nothing before it reads as quiet (default 2 h): tag a watch loop that speaks only
on news `cadence:1d` so its silence is not flagged.

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

To move a step's work out, use `step_update` to set `run` to `core.external`, declare the
outputs its dependents read and remove `scatter`, then `step_retry` it. Its inputs stay; the
old function's own inputs become extra inputs. For a step `w` whose dependent reads `final`:
`step_update(project, "w", changes={"run": "core.external", "outputs": {"final": "string"},
"scatter": null})`. `outputs` replaces that map, so include any other declarations to keep.

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
output}` for a plan output. When nothing can go it is no edit. `keep: ["ta-*"]` keeps every
unit whose name matches a pattern (`*` any run of characters, `?` one), reported as `{unit,
keep}`.

Rather than remembering to prune, set `prune_done_after` on the project so finished lanes
retire themselves: `project_update(project, prune_done_after=21600)` (seconds; `sluice tool
project_update --name p --prune-done-after-hours 6`; null turns it off), with `prune_keep:
["ta-*"]` for units that must never go, whatever their state. Sluice then runs that prune
itself, looking at each project at most every 5 minutes, as one edit by `sluice` ("retire done
units older than 6h") and only when something qualifies. It removes only done units, keeps
what anything left references, and skips a round when the plan moved under it (it never fights
your edits). Its edit is a plain `plan.edit`: `next` does not wake for it (unless `all`). Read
the setting with `query`: `SELECT prune_done_after, prune_keep FROM projects WHERE project_id =
?1`. The owner can set both on the project settings page.

Removing a step that finished (by `plan_prune`, `step_remove` or any `plan_edit`) keeps what it
ended with in the `outcomes` view: its status, outputs, error, run ids, unit, when it was
recorded and when it was removed. Nothing trims it; read it with `query`, e.g.
`SELECT step_id, status, outputs FROM outcomes WHERE project_id = ?1 AND unit = 'x'` with the
project's id as the parameter.

## Scoped reads
Read one unit with `unit_get(project, unit, compact=false)` or one step with
`step_get(project, step, compact=false)`. Their full steps include the declaration as
`spec` and the normalized `references`; `compact=true` returns only `id`, `unit`, `recipe`,
`position`, `run`, `status`, `paused` and `priority`, without decoding declarations. A missing
step or unit is `not_found`.

`plan_read(project, units?, steps?, status?, recipe?, compact=true, limit=200, cursor?)`
returns `{project, rev, state_epoch, recipe_generation, steps, next_cursor}`. Lists match any
member and filters combine with AND. `units`, `steps` and `status` each accept one string.
Absent filters are unrestricted, an empty list matches nothing and a name matching nothing
returns an empty page. Statuses are the stored words `pending`, `running`, `succeeded`,
`failed`, `stale` and `skipped`, not the dashboard's words. A `recipe` filters units that
match it now. Steps come in position order. Limits are 1 to 1000, larger capped at 1000;
zero is `bad_request`.

Pass `next_cursor` back with the same project and filters. A cursor always binds the plan's
revision, binds execution state only when filtering on status, and binds recipes only when
filtering on recipe. Unfiltered paging therefore survives status changes. An expired cursor
is `cursor_expired`; read again without it. A malformed cursor or one from another query is
`bad_request`. `step_get` returns `{project, rev, state_epoch, step}`. `unit_get` returns
`{project, rev, state_epoch, recipe_generation, unit}`; the unit has `id`, `recipe`,
`entry_steps`, `exit_steps`, `done`, `settled` and its `steps` in position order.

`plan_get(project)` exports the whole plan with its revision and project identity, in authored
section and key order. It does not compile, so a broken fn catalog does not prevent an export.
`plan_view(project, format="mermaid", all=false, units?, steps?, status?, recipe?)` uses the
same filters. Every edge crossing the selection ends at an outside boundary node, and a
comment counts them. `all=true` keeps done units.

## Editing
Read the unit or steps you need with `unit_get`, `step_get` or `plan_read`. Use the typed tools for single changes and `plan_edit` for an atomic batch. Pass `rev` when a change depends on an earlier read. `plan_get` exports the whole plan. A preview describes the edit's affected work; ask for a full dry run explicitly. An edit refused `busy` is retried as is.

`plan_edit(project, ops, reason, rev?, start=true)` applies operations in order to one
candidate. Each sees the preceding operations, then the candidate is validated once and
commits whole or not at all. Pass the revision from a scoped read when the edit depends on
it. If the plan moved on you get `conflict` with `current_rev`: read again before deciding
what to change. `reason` is required for `plan_edit`, `unit_update` and `unit_remove`; other
edit tools default to an empty reason.

```json
{"project": "demo", "rev": 7, "reason": "Replace the old checkout step", "ops": [
  {"op": "input.put", "name": "repo", "declaration": "string"},
  {"op": "step.add", "step": "head", "spec": {"run": "git.head", "in": {"path": {"source": "repo"}}}},
  {"op": "step.update", "step": "notes", "changes": {"in": {"engine": {"default": "codex"}, "cwd": {"source": "repo"}, "spec": {"default": "Write the notes"}}}},
  {"op": "step.remove", "steps": ["old-step"]}
]}
```

Operations are `input.put {name, declaration}`, `input.remove {name}`, `output.put {name,
source}`, `output.remove {name}`, `step.add {step, spec}`, `step.update {step, changes}`,
`step.remove {steps}`, `edge.add` or `edge.remove {step, after}`, `unit.add {recipe, unit,
params?, after?, inputs?, tags?}`, `unit.update {unit, changes}`, `unit.remove {unit}` and
`order.set {collection, ids}`. `collection` is `steps`, `inputs` or `outputs`; `ids` must name
every current member exactly once, and `rev` is required. Pure reordering is an authored edit.
An unknown operation or field, empty `ops`, changes, removal steps or edge entries is
`bad_request`. Operation refusals are collected as `invalid` with `ops[i]` paths; candidate
validation errors use plan paths.

Small edits use `step_add`, `step_update` or `step_remove`. `changes` accepts only `run`,
`in`, `scatter`, `doc`, `outputs`, `paused`, `after`, `tags`, `needs` and `priority`; null
removes a key and `in` replaces the whole map. `unit_update(project, unit, changes, reason)`
changes listed members by exact id without regenerating the recipe; its `steps` report the
changed members. `unit_remove(project, unit, reason)` removes every member; its `steps`
report removed members in position order.

`edge_add` and `edge_remove` append or remove entries without replacing someone else's gate
entries. An edge already there or already absent is a no-op. `step` may be `unit:<name>` to
act on the unit's entry steps. Selection tools use `steps` and/or `tags`; one string is fine.
A tool refuses an argument it does not take.

Edit replies are `{project, rev, preview, steps?, board_warnings?}`. Every edit takes
`dry_run=true`, returning the preview alone without changing anything. The preview is
`{scope, changes, would_start, would_queue, would_skip, would_stale, errors}`. `scope` is
`impact` by default and describes affected work, including resource competitors when needed.
For a whole-plan simulation, pass `dry_run=true, preview_scope="all"`; without `dry_run`,
`all` is `bad_request`. `changes` are resolved row puts and deletes, not request operations.

A no-op writes no record or history and keeps the current revision with empty
`preview.changes`. Typed lowering can produce no operations; supplied empty batches are
still refused. `step_set_input` keeps its `bad_request` when it changes nothing. A running
step can only be paused or tagged. A `busy` refusal with `retryable=true` after three stale
preparations writes nothing: send the same edit again.

`plan_history(project, since_rev?, after_seq?, limit=200)` returns `{project, entries,
next_after_seq}`. Every authored edit is retained from revision 1 with resolved `changes`;
retained `plan.input`, `step.output` and `step.retry` log records join them, oldest by `seq`
first. Both filters apply together. Pass `next_after_seq` as `after_seq` to page; null ends
the history. Limits follow `plan_read`.

## Pausing
Steps you add start as soon as they are ready. To draft first, pass `start=false` to
`plan_edit`, `step_add` or `unit_add`: the steps it adds come in **paused** (unless a step sets
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
