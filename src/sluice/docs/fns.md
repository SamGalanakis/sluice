# Functions

`fn_list(project)` shows what a project can use and `fn_get(name, project)` one function's full
definition. `fn_call(name, inputs, project, wait)` runs a single function without touching the
plan; poll `call_status(call, project)` if it outlives `wait`, or wait for its `call` records
with `log_wait(project, since_seq, kinds=["call"])`. Each status change of a call is a record
in the project's log (the home log without a project).

## Scopes
- **builtin**: shipped with sluice (`core.*`, `thread.*`, `inbox.ask`). Other first-party
  functions come in packs (`agents`, `git`, `jev` in the repo's `packs/`): install one by copying
  `packs/<pack>/*` into `$SLUICE_HOME/fns/` or a project's `fns/`, or by adding its path to
  `fn_dirs`.
- **global**: `$SLUICE_HOME/fns/` and the dirs in the config's `fn_dirs`; every project sees them.
- **project**: the project's own `fns/`; only that project sees them.

Names never collide: a global function may not reuse a built-in name, and a project function may
not reuse a built-in or global one. Two projects may each have a function of the same name. A
project with a colliding (or broken) function refuses plan edits and runs until it is fixed;
`verify` and `fn_list` (`error`) show it.

## Writing a function
`fn_save(fn, main_py, project)` checks the fn.json and writes `fns/<name>/fn.json` and `main.py`
into the project (leave out `project` for a global function). Saving the same name again in the
same scope replaces it.

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
from sluice.fn import run, sh, Transient

def main(inp, ctx):
    ctx.log("upper-casing")                 # logs go to stderr
    return {"text": inp["text"].upper()}    # a dict keyed by output name

if __name__ == "__main__":
    run(main)                               # run(main, retries=3, backoff=60) retries Transient
```

- The name is dotted lowercase (`area.verb`); `inputs` and `outputs` map names to types
  (`docs("types")`).
- `inp` holds the inputs by name (missing optional inputs are `None`). Return every output.
- `sh(argv, cwd=...)` runs a command and raises on a non-zero exit.
- Raise `Transient` for failures worth retrying (rate limits, capacity); `run(main, retries=N)`
  retries them. Any other exception fails the step with its traceback.
- `ctx` has `project`, `step`, `run_id`, `run_dir` (scratch space), `attempt`.
- Secrets come from the environment: `$SLUICE_HOME/.env`, then the project's `.env` (project
  values win). Never put them in plans.
- Output types are checked after the function exits; a mismatch fails the step.
