# Functions

`fn_list(project)` shows what a project can use and `fn_get(name, project)` one function's full
definition. `fn_call(name, inputs, project, wait)` runs a single function without touching the
plan; poll `call_status(call, project)` if it outlives `wait`, or wait for its `call` records
with `log_wait(project, since_seq, kinds=["call"])`. Each status change of a call is a record
in the project's log (the home log without a project).

## Inline code: `inline.bash` and `inline.python`
For a check or a small transform no function exists for, put the code in the step. Bind any
extra inputs; the code sees them by name. Declare `outputs` to give the step typed results.

```json
{"inputs": {"repo": "string", "sha": "string", "items": "Any[]"},
 "outputs": {},
 "steps": {
   "on-main": {"run": "inline.bash",
               "in": {"code": {"default": "git -C \"$repo\" merge-base --is-ancestor \"$sha\" origin/main && printf '{\"on_main\": true}' > \"$OUT\""},
                      "repo": {"source": "repo"}, "sha": {"source": "sha"}},
               "outputs": {"on_main": "boolean"}},
   "count":   {"run": "inline.python",
               "in": {"code": {"default": "out = {'n': len(items)}"}, "items": {"source": "items"}},
               "outputs": {"n": "int"}}}}
```

`inline.bash` runs with errexit and pipefail, extra inputs as environment variables, and reads
declared outputs from the JSON object the script writes to `$OUT`; it fails on a non-zero exit
unless `check: false` (then read `code`). `inline.python` (standard library only) sees `inp` and
each extra input, and returns what it assigns to `out`. Anything longer or reused belongs in a
function of its own (`fn_save`).

## Scopes
- **builtin**: compiled into sluice — `core.*`, `inline.bash` / `inline.python`,
  `message.ask`, `message.say`, `message.reply` and `message.wait` (`docs("threads")`; the
  retired `message.post` still runs for older plans), `decide.llm`, the agent functions (`agent.claude`,
  `agent.codex`, `agent.devin`, `agent.review`, `agent.run`), `git.*`, `gh.*` and `jev.*`.
- **global**: `$SLUICE_HOME/fns/` and the dirs in the config's `fn_dirs`; every project sees them.
- **project**: the project's own `fns/`; only that project sees them.

Names never collide: a global function may not reuse a built-in name, and a project function may
not reuse a built-in or global one. Two projects may each have a function of the same name. A
project with a colliding (or broken) function refuses plan edits and runs until it is fixed;
`verify` shows where.

## Writing a function
`fn_save(fn, main_py, project)` checks the fn.json and writes `fns/<name>/fn.json` and `main.py`
into the project (leave out `project` for a global function). Saving the same name again in the
same scope replaces it. It returns `{name, scope, path, generation}`. fn.json takes only the keys
`name`, `doc`, `inputs`, `outputs`, `open`, `submits` and `icon`.

`fn`:
```json
{
  "name": "text.upper",
  "doc": "Upper-case a string.",
  "inputs":  {"text": "string"},
  "outputs": {"text": "string"}
}
```

`main_py` (run with `uv`; declare third-party packages in the PEP 723 block):
```python
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice_fn import run, sh, Transient

def main(inp, ctx):
    ctx.log("upper-casing")                 # logs go to stderr
    return {"text": inp["text"].upper()}    # a dict keyed by output name

if __name__ == "__main__":
    run(main)                               # run(main, retries=3, backoff=60) retries Transient
```

- The name is dotted lowercase (`area.verb`); `inputs` and `outputs` map names to types
  (`docs("types")`).
- `"open": true` makes it an agent block (`docs("plans")`): a step may bind extra inputs and
  declare outputs, which whoever does the work submits with `step_submit` while the step runs
  (`ctx.extra_inputs` and `ctx.outputs` below describe them; `ctx.submit(outputs)` submits from
  the fn itself). A run submits once, and an agent's valid submission ends its session at once:
  `ctx.builtin('agent.run', ...)` then returns its result (`session`, `final`, `git`) to the
  fn, and the step completes with what the fn returns, the submission joined in. Tell the agent both, the command, and "Submit only when you are finished:
  submitting ends your session."; the `agent.*` builtins (and `ctx.header`) show how.
- An open function can require outputs of its agent on every step: `"submits": {"summary":
  {"type": "string", "doc": "What it did"}}` in fn.json. Each step running it declares them
  as if it listed them under its `outputs` (so `step/summary` is a typed ref, and a required
  one never submitted fails the step with `exited_without_submit`); they appear in
  `ctx.outputs` like the rest.
- An icon tells its steps apart on the dashboard: an `icon.svg` (or `icon.png` /
  `icon.webp`, at most 256 KB) next to `fn.json`, or `"icon": "🧪"` in fn.json (at most 16
  characters); the file wins. Draw an SVG single-colour in `currentColor` on a 16×16
  `viewBox`: the dashboard paints it in the theme's ink. A bad icon is a fn.json problem
  (`verify`). `fn_save` writes the fn.json key; add a file to the fn's dir yourself.
- `inp` holds the inputs by name (missing optional inputs are `None`). Return every output.
- `sh(argv, cwd=...)` runs a command and raises on a non-zero exit.
- Raise `Transient` for failures worth retrying (rate limits, capacity); `run(main, retries=N,
  backoff=S)` calls `main` again within the same run, `ctx.attempt` counting up (`SLUICE_BACKOFF`
  overrides the backoff). `Rejected` refuses the work on purpose, `AgentFailure(kind, message,
  session)` reports a failed agent, and any other exception fails the step with its message
  (the traceback goes to stderr).
- `log(msg)` (or `ctx.log`) writes to stderr; `stream(argv, on_line=...)` runs a command and
  hands each output line to `on_line`; `child_env()` is the environment to give child tools.
- `ctx.tool(name, args)` calls a sluice tool for the run's own project: reads (`status`,
  `plan_get`, `messages`, `log_read`, `fn_list`, `fn_get`, `call_status`), `step_wait` (to
  block until other steps reach a status instead of sleeping), `ask`, `say` and
  `reply` (as the step, with its run), `message.ask` / `message.wait`, and the project's edit
  tools. `args` are the tool's MCP arguments
  (`ctx.tool("step_pause", {"steps": "deploy", "reason": "hold"})`; `project` defaults to the
  run's), and it returns what the MCP tool returns (`{"ok": True}` for an acknowledgement).
  Edits it makes are authored `step:<step>`.
  `ctx.builtin(name, inputs)` runs a built-in function (an agent, say) inside this run. A
  function that runs an agent declares `"listen": "boolean?"` and passes it on: its runs then
  take messages live, and `ask`/`say` to its step report `delivered`.
- `ctx.retry_on_failure(step, message)` registers a send-back: if this run then ends rejected,
  `step` is retried with `message` posted to it first.
- `ctx` has `project`, `project_id`, `step`, `run_id`, `run_dir` (scratch space), `project_dir`,
  `fn_dir`, `home`, `attempt`, `prev_run` (the step's previous run, `None` on the first), and,
  for a step of an open function,
  `extra_inputs` (`{name: {"type"}}`; the values are in `inp`) and `outputs` (`{name: {"type",
  "doc"}}`, the outputs the step declares).
- `with ctx.acquire("land", 1):` holds an amount of one of the project's resources
  (`docs("plans")`, Resources) for just that section of a step's work, e.g. a rebase and push
  that must not overlap another step's: it waits until the runner grants it (waiters in their
  step's `priority` order, ties first come), then holds it until the block exits, however it
  exits; a run that ends (crash, kill, cancel) lets go of it too. It counts against the same
  capacity as steps' `needs`. An undeclared resource or an amount over a fixed capacity raises
  `CallbackError` (`bad_request`) at once; `timeout=` seconds raises `TimeoutError` instead of
  waiting on. It needs a run in a project.
- Secrets come from `.env` files: `$SLUICE_HOME/.env`, then the project's
  `$SLUICE_HOME/projects/<id>/.env` (project values win), loaded into every run (fns, built-ins
  and agent sessions) at launch; the run's own `SLUICE_*` variables win over both. `verify`
  checks their syntax. Never put secrets in plans.
- Output types are checked after the function exits; a mismatch fails the step.
