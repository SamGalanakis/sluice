# Functions

Call `fn_list` to see what exists and `fn_get(name)` for one function's full definition.
`fn_call(name, inputs, wait)` runs a single function without writing a plan.

## Writing a new function
A function is a directory with `fn.json` and `main.py`, placed in a directory listed in the
sluice config `fn_dirs` (then `fn_list` shows it).

`fn.json`:
```json
{
  "name": "text.upper",
  "doc": "Upper-case a string.",
  "inputs":  {"text": "string"},
  "outputs": {"text": "string"}
}
```

`main.py` (run with `uv`; declare third-party packages in the PEP 723 block):
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

- `inp` holds the inputs by name (missing optional inputs are `None`). Return every output.
- `sh(argv, cwd=...)` runs a command and raises on a non-zero exit.
- Raise `Transient` for failures worth retrying (rate limits, capacity); `run(main, retries=N)`
  retries them. Any other exception fails the step with its traceback.
- `ctx` has `plan`, `step`, `run_id`, `run_dir` (scratch space), `attempt`.
- Output types are checked after the function exits; a mismatch fails the step.
