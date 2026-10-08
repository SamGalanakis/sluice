Run every sluice command as plain `sluice` from PATH, never a path under releases/. SLUICE_PROJECT_ID and SLUICE_RUN_ID identify this invocation. This step's project is `id:01a10513-16c5-7742-a8a2-42b9f1812a08`; pass exactly that as `project` to any sluice tool.

## Previous attempt
None: this is the first attempt at this step.
The working directory /workspace/kiln/lash/forks/fig-4996 is clean now.

## Task

You work in the kiln fork /workspace/kiln/lash/forks/fig-4996: run `cd /workspace/kiln/lash/forks/fig-4996 && . ./env.sh` first and never write outside it, except the report file your spec names and the run summary file this task names.
- Build, test, lint and format only through kiln: `kiln check <targets>` (fast, no linking), `kiln build`, `kiln test <targets>`, `kiln clippy`, `kiln fmt`, and `kiln gate lash <fork> -- ...` for anything that needs services or raw cargo. Never run raw cargo or bazel yourself. Python scripts run with python3.
- Lash builds with Buck2. After changing Cargo manifests, dependencies or features, or adding/removing a crate, binary or test file, run `kiln sync` and commit the regenerated BUCK files and tools/buck2/target-inventory.json with your change. Never hand-edit or hand-merge generated files: regenerate them. Do not edit tools/buck2/**, third-party/ or .buckconfig unless your notes grant it.
- A first-party compile killed for memory (remote exit 9): retry that label once with `-c kiln.memory_scale=2`, and name it in your report. Never raise sizes yourself.
- Never git stash (forks share one stash); use a WIP commit. Never dispatch CI workflows. Never restart or reconfigure the shared build pool.
- Your session ends when your turn ends: run builds and tests in the foreground and wait for them. When your shell tool takes a yield or timeout, give kiln at least 300000 ms; never poll a running build at short intervals, and never send progress-only messages ("still compiling"). `pgrep -f <pattern>` matches its own command line; wait on exit or an output marker instead.
- `kiln test|clippy|check` end with one stderr line, `kiln: <op> passed|failed executed=N passed=N failed=N receipt=<path>` (counts for test only): read that line for the result and executed count. A kiln command that must restart the fork's daemon waits for running ones (`kiln: <op> queued behind <pid> ...`); it never cancels them.
- One kiln command per fork at a time: never start a second kiln command (or several at once) while one runs, and never edit sour
[trimmed for the fixture]