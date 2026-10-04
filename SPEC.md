# sluice: specification

sluice runs plans: graphs of typed function calls. Orchestrators (agents over MCP, people at a
shell or the dashboard) edit a project's plan through typed tools; a per-home coordinator
validates every edit, schedules ready steps and runs each one inside a supervised systemd unit.
The plan document borrows CWL's shapes (`inputs`/`outputs`/`steps`, `run`, `source`/`default`,
`scatter`, CWL type spellings) without aiming for CWL compliance.

This file is the contract of the shipped build. When the code and this file disagree, fix one
of them in the same change. `DESIGN.md` covers how the dashboard looks; `docs/agent/*.md` are
the agent-facing topics the `docs` tool serves.

## 1. Concepts

- **Home:** one directory holding a database and everything a set of projects needs (§2.3). One
  coordinator process owns each home's writes.
- **Project:** a name (renameable), an immutable UUIDv7 id, a description, an optional icon,
  named resources, paused and archived flags. Each project has exactly one plan and its own
  functions and recipes.
- **Function (fn):** a named unit with typed `inputs` and `outputs`. Built-in fns are compiled
  into sluice (§16); user fns are Python (`fn.json` plus `main.py`, §5). An **open** fn (every
  agent fn) also takes extra inputs a step binds and outputs a step declares.
- **Plan:** typed plan `inputs`, named plan `outputs`, and `steps`. Each step runs one fn and
  binds its inputs to plan inputs, other steps' outputs, literals or files. Edited only through
  typed edits; each edit gets a new revision (`rev`) and a history entry.
- **Unit:** the steps sharing one `unit:<name>` tag; an untagged step is a unit of one (§6.7).
- **Run:** one execution of a step (one per scattered item) or of a call, in its own transient
  systemd unit under a guardian (§2.5, §7).
- **Message:** a row on a project thread: a question (`needs_reply`) or a note. Open questions
  to `owner` are the inbox (§8).
- **Log:** each project's ordered records of what happened (edits, manual values, status
  changes, calls, messages, …), each with a home-wide `seq` (§9). History, not truth: `status`
  and `plan_get` are the current state.

## 2. Installation, home and processes

### 2.1 Home resolution

Every mode resolves its home once, at startup: `SLUICE_HOME` when it is set, else the home the
installation has selected (§2.2; installation directory `SLUICE_INSTALL_DIR`, default
`~/.local/share/sluice/install`). When neither exists the command fails with an error saying
so. The installation control directory must lie outside the home.

### 2.2 Installation and releases

An installation lives under a prefix (default `~/.local/share/sluice`):

```
bin/sluice                  the launcher: a small native program (built by build-release)
install/                    the control directory (SLUICE_INSTALL_DIR overrides it)
  install.lock              flock: shared for admission decisions, exclusive for fence/select/unfence
  selection.json            {generation, release_path, home_path}: the selected release and home
  fence.json                {generation, reason, since} while the installation is fenced
  generation.json           the installation generation, advanced by every fence/select
  entry                     symlink to the selected release's bin/sluice
  services.json             what scripts/deploy started: {name: {unit, release}}
releases/<git-sha>-<sha256>/
  bin/sluice                the release binary
  python/                   sluice_fn (the fn helper, §5.4) and inline_python.py
  tmux/                     the private tmux 3.7c build and its tmux-manifest.json
  assets/                   the dashboard assets (the binary also embeds them)
  manifest.json             release_id, git_sha, guardian protocol, sha256 of every file,
                            the private tmux manifest, the build toolchain
```

The **launcher** execs `<install>/entry --installation-entry <install> <args…>`; the release
binary reads `selection.json` and execs the selected release's `bin/sluice` with `SLUICE_HOME`
set to the selected home and `SLUICE_INSTALL_DIR` to the control directory. A release binary
re-execs itself once with `SLUICE_PYTHON_DIR=<release>/python`,
`SLUICE_TMUX_PREFIX=<release>/tmux` and `PYTHONDONTWRITEBYTECODE=1`. Agent docs and dashboard
assets are compiled into the binary.

`sluice install <command>` prints the installation status as JSON
`{generation, selection, fence}`:

| command | effect |
|---|---|
| `install status` | read only |
| `install fence <reason>` | writes `fence.json` under the exclusive lock; while fenced, every coordinator activation and admission write is refused (`maintenance: <reason> (generation n)`), except a coordinator started with `--maintenance` |
| `install select <release_dir> <home>` | verifies the release's manifest (when it has one) and records the selection |
| `install unfence` | verifies the selected release, clears the selected home's `cutover` maintenance mode through its coordinator when one is set, and removes the fence |

A home that is not the installation's selected home refuses admission (`maintenance: stale
selected home`). Without a release and without `SLUICE_INSTALL_DIR` (a source build), the
installation is the sibling directory `<home>.sluice-install`.

**`scripts/build-release <prefix>`** runs `cargo build --workspace --bins --release --locked`
with its target dir under `<prefix>/.build/target`, stages `bin/sluice`, `python/`, the private
tmux (built once by `scripts/build-private-tmux` and cached under `<prefix>/.build`) and
`assets/`, writes `manifest.json`, moves the stage to `releases/<release_id>` (an existing
release id must have identical metadata), compiles the launcher into `<prefix>/bin/sluice` and
prints the release path. It refuses a prefix on `PATH`, `/`, or `~/.local/bin`.

**`scripts/deploy [REF] [--prefix DIR]`** (REF defaults to `origin/main`) installs one commit:

1. `git archive` the commit into a temporary source tree and run `build-release` there;
2. `install fence "deploy <sha>"`;
3. stop the services recorded in `services.json` and the home's auto-started coordinator unit;
4. `install select <release> <home>`;
5. start three transient user units, `sluice-<sha16(install dir)>-coordinator`
   (`coordinator --maintenance`), `-serve` (`serve --no-runner --port 3065`, or
   `SLUICE_DEPLOY_PORT`) and `-loop` (`loop`), each with `SLUICE_HOME`, `SLUICE_INSTALL_DIR`,
   `PATH` and `HOME` set, recording them in `services.json`; after the coordinator it waits
   until the coordinator is ready to serve its socket;
6. check that every unit is active and run `sluice doctor --json`;
7. `install unfence`;
8. prune releases: keep the newest three plus every release a live process runs from or an
   unfinished run in the home records.

Any failure after the fence leaves the installation fenced and says so. Running steps survive a
deploy: each run's guardian stays pinned to its own release, and the new coordinator adopts it
(§7.9).

### 2.3 Home layout

```
config.json                 {"fn_dirs": [], "http": {...}, "log_max": 10000}; written when
                            missing. Only fn_dirs is read: directories (relative to the home)
                            whose fn dirs join the global scope
sluice.db                   the database (§3)
coordinator.sock            the coordinator's Unix socket (owner only)
coordinator.lock            flock held by the running coordinator
fns/                        global user fns; fns/generations/ holds immutable published
                            copies that runs are pinned to
recipes/<name>.json         global recipes (§6.8)
runs/<run id>/              one run's directory: its control socket, invocation and delivery
                            records, messages.json (the messages assigned to it), stderr and
                            the engine's own files
locks/session-<key>.lock    agent session locks
projects/<project id>/
  fns/                      the project's fns
  generations/<n>/          immutable published copies of the project's fns
  recipes/<name>.json       the project's recipes
  .env                      checked for syntax by verify; never loaded (§5.4)
  icons/<generation>        the project's image icon
```

### 2.4 Coordinator

The coordinator (`sluice coordinator`) owns the home: it holds `coordinator.lock`, opens the
single SQLite writer, serves `coordinator.sock`, publishes the fn registry, runs reconciliation
and, while some client holds the **scheduler lease**, admits and launches work.

- Any CLI command that needs it, `serve` and `loop` connect to the socket and, when nothing
  answers, start the coordinator as the transient user unit `sluice-coordinator-<sha16(home)>`
  (`systemd-run --user --collect --service-type=exec -p Restart=no`) and wait up to 10 s for
  its socket. Activation is refused while the installation is fenced and while the home's
  maintenance mode is `cutover`.
- `coordinator --maintenance` may start while the installation is fenced; `scripts/deploy` uses
  it.
- The scheduler lease is one per home: `serve` takes it unless `--no-runner`, `loop` takes it,
  and a second holder is refused (`conflict`, "scheduler lease already held"). The lease lasts
  as long as the holder's connection. Without a holder nothing new starts; running work goes
  on.
- Every request on the socket is one length-prefixed JSON frame (§12.1). A request it cannot
  decode, or one whose handler panics, gets a logged error reply; the connection stays usable.

### 2.5 Runs and the guardian

Each run is a transient systemd user unit, `sluice-run-<run id>.service`, started with
`Delegate=yes`, `KillMode=control-group` and `Restart=no`. Inside it the **guardian**
(`sluice guardian --run --attempt --socket`) proves its identity to the coordinator, splits the
unit's cgroup into `control` (itself) and `payload/<invocation>` leaves, starts the payload
through the `payload-exec` launcher, serves the run's control socket (callbacks, engine hooks,
delivery acknowledgements), holds one watch on the coordinator for cancellation and messages,
and reports the start and the completion until the coordinator acknowledges them. A payload is
a Python fn (§5.4), an agent session in the private tmux, or a built-in fn.

Stopping a run sends TERM to its processes, waits five seconds, then kills the payload cgroup
recursively and proves it empty before any resource it held is released. The guardian, not the
coordinator, owns the payload: a coordinator restart or a deploy leaves running payloads alone.

Host prerequisites (cgroup v2, a systemd user manager with delegation, `pidfd_open`, a boot id)
are checked by `sluice doctor`; see `docs/rust/host-prerequisites.md`.

### 2.6 Maintenance modes

The home has one maintenance mode: `normal`, `drain` or `cutover`.

- **drain** (`drain` tool, `sluice drain`): pauses the selected projects (default every
  project not archived) that are not paused already, records them and the drain's author as
  owner, and rejects new plan work and user calls home-wide (`busy`, "drain rejects new plan
  work and user calls"): plan edits, retries, input sets and `fn_call`. Running steps and calls
  finish. Draining again with another author is a `conflict`. `release` unpauses exactly the
  recorded projects and returns to `normal`.
- **cutover**: admission closed and coordinator activation refused. Nothing in this build
  enters it; `install unfence` clears it.

## 3. Storage

`sluice.db` is SQLite in WAL mode, created from `migrations/0001.sql` (23 STRICT tables). Only
the coordinator writes, through one writer task; reads use a pool of read-only connections and
one snapshot per answer. Every logical change (an edit and its records, a status change and
its records, a message and its record) commits in one transaction.

Tables: `home_meta`, `projects`, `plans`, `plan_edits`, `inputs`, `steps`, `attempts`, `runs`,
`submissions`, `calls`, `step_results`, `resources`, `leases`, `messages`,
`question_attachments`, `message_deliveries`, `readers`, `records`, `change_versions`,
`maintenance`, `artifact_jobs`, `sessions`, `notification_attempts`.

Views for agents' queries (§12.4 `query`): `outcomes` (removed steps' results), `log` (each
record as the log tools return it), `step_changes` (`step.status` records as rows), `edits`
(`plan_edits`) and `questions` (messages with `needs_reply`, plus derived `state`
open|answered|closed and `waiting`). Public tables are keyed by the immutable `project_id`,
never by name.

Ids: projects, runs, attempts, results and invocations are UUIDv7; message ids and record
seqs share one increasing integer sequence.

## 4. Types

Written inline, compared structurally:

| form | meaning |
|---|---|
| `"string"`, `"int"`, `"float"`, `"boolean"`, `"Any"` | primitives; `Any` accepts anything |
| `"T?"`, e.g. `"string?"`, `"string[]?"` | optional: T or null; an optional input may be left unbound |
| `["null", T]` | optional form for any T |
| `"T[]"` | array of T |
| `{"type": "array", "items": T}` | array of T |
| `{"type": "enum", "symbols": ["a", "b"]}` | one of these strings |
| `{"type": "record", "fields": {"f": T}}` | object with these fields |

An output **fits** an input when either is `Any`; same primitive, or `int` into `float`; an
enum into `string` or into an enum with every symbol; arrays of fitting items; a record that
has every required field of the input record with fitting types (extra fields are fine). An
optional value does not fit a required input. Runtime values are checked against types with
path-bearing errors (`report.outcome: expected one of [done, blocked], got "ok"`).

Plan inputs and step-declared outputs may be written `{"type": T, "doc": "..."}`. An extra
input of an open fn's step takes its source's type: a ref's type; for a list source, an array
of the refs' type (`Any[]` when they differ); `Any` for a `default`; `string` for a `file`; the
item type for the scatter input.

## 5. Functions

### 5.1 Scopes

- **builtin:** compiled into the binary (§16).
- **global:** `<home>/fns/` and every directory in `config.json`'s `fn_dirs`.
- **project:** `projects/<id>/fns/`.

A project sees builtin, global and its own fns. Names never collide: a global fn may not reuse
a builtin name, and a project fn may not reuse a builtin or global one. A fn dir is an
immediate subdirectory holding `fn.json` whose `name` matches the directory. The registry is
rescanned when fn files change (a watcher on the fn scopes) and republished only when it
changed; each publication is an immutable generation that runs are pinned to.

A project whose own scope has a problem (a collision, a bad `fn.json`) is **blocked**: plan
edits, manual values and new runs of that project fail with `invalid` ("project registry
blocked", listing the problems). Problems elsewhere leave the broken fn out of lookup.
`verify` reports every problem.

### 5.2 fn.json

```json
{"name": "text.upper", "doc": "Upper-case a string.",
 "inputs": {"text": "string"}, "outputs": {"text": "string"}}
```

Keys: `name` (dotted lowercase: two or more `[a-z][a-z0-9_]*` parts), `doc`, `inputs`,
`outputs`, `open` (boolean), `submits`, `icon`; any other key is a problem. `submits` (only
with `open: true`) names outputs every step of the fn declares as if it listed them itself,
each a type or `{"type", "doc"}`, none named like a fn output.

**Icon:** one file `icon.svg`, `icon.png` or `icon.webp` in the fn dir (at most 256 KiB, its
content the type its name says), or `"icon": "<text>"` (at most 16 characters, no control
characters). The file wins. Two icon files, a bad file or a bad text is a problem. An SVG
icon is drawn single-colour in `currentColor` on a 16×16 grid.

### 5.3 Saving

`fn_save(fn, main_py, project?)` validates the manifest, refuses a name that collides, writes
`fns/<name>/fn.json` and `main.py` into the project's (or the global) scope, republishes the
registry and returns `{name, scope, path, generation}`.

### 5.4 Python fn contract

A user fn runs as `uv run --no-project --quiet <bundle>/main.py`, where the bundle is the fn's
published generation. `main.py` declares any dependencies in a PEP 723 block. The process
runs in the run's payload cgroup, in the fn's published directory, with:

- `PYTHONPATH`: the release's `python/` directory (the `sluice_fn` helper) plus the published
  scope directory holding the fn, so it can import modules kept beside it (e.g. `_lib/`);
- environment: the coordinator's environment plus `SLUICE_HOME`, `SLUICE_BIN`,
  `SLUICE_PROJECT_ID`, `SLUICE_PROJECT` (the name at launch), `SLUICE_STEP`, `SLUICE_RUN_ID`,
  `SLUICE_RUN_DIR`, `SLUICE_PROJECT_DIR`, `SLUICE_FN_DIR`, `SLUICE_PREV_RUN`,
  `SLUICE_CONTROL_SOCKET`, `SLUICE_RUN_CAPABILITY`, and `SLUICE_HOST_PATH`,
  `SLUICE_HOST_PYTHONPATH`, `SLUICE_HOST_VIRTUAL_ENV` (the values before sluice changed them).
  `.env` files are not loaded; secrets come from the environment the coordinator was started
  with.
- stdin: one JSON envelope `{"protocol": 1, "inputs": {...}, "context": {...}}`. `context`
  holds `home`, `run_dir`, `project_dir`, `project`, `project_id`, `step`, `run_id`,
  `attempt_id`, `invocation_id`, `fn_dir`, `bin`, `prev_run`, `extra_inputs` (an open fn's
  step: `{name: {"type"}}`), `outputs` (the outputs the step declares: `{name: {"type",
  "doc"}}`), `returns`, `control_socket` and `run_capability`.
- stdout: exactly one JSON document, `{"ok": true, "outputs": {...}}` or `{"ok": false,
  "error": {"kind", "message"}}` (or an `agent_failure` error object), at most 16 MiB. Anything
  else, a non-zero exit without a result, or outputs that fail their types fails the step;
  the error carries the stderr tail (2 KiB).

The helper, `sluice_fn` (standard library only):

```python
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
from sluice_fn import run, sh, Transient

def main(inp, ctx):
    ctx.log("upper-casing")
    return {"text": inp["text"].upper()}

if __name__ == "__main__":
    run(main)
```

- `run(main, retries=0, backoff=30)` reads the envelope, calls `main(inputs, ctx)`, writes the
  result and exits. `Transient` raised by `main` is retried within the same run up to
  `retries` times, `backoff` seconds apart (`SLUICE_BACKOFF` overrides it); `ctx.attempt`
  counts the calls. SIGTERM and SIGINT raise `Cancelled`. `main` returning `None` means `{}`;
  anything but a dict is an error.
- Error kinds written: `Rejected` → `rejected` (refused on purpose; a registered completion
  action may follow, below), `Transient` → `transient` (retries exhausted), `Cancelled` →
  `cancelled`, `AgentFailure(kind, message, session)` → an `agent_failure` error, anything
  else → `fn_failure` with the exception's text. The traceback goes to stderr.
- `log(msg)` / `ctx.log`: stderr. `sh(argv, cwd, check, env, timeout, input)` runs a command
  (raises `ShError` on a non-zero exit when `check`); `stream(argv, on_line, …)` runs one and
  hands each line to `on_line`; `child_env(extra)` is the environment for child tools (host
  `PATH`, `PYTHONPATH`, `VIRTUAL_ENV` restored, uv and agent-nesting variables removed).
- `ctx` attributes: `project_id`, `project`, `step`, `run_id`, `attempt_id`, `invocation_id`,
  `run_dir`, `home`, `fn_dir`, `project_dir`, `prev_run`, `extra_inputs`, `outputs`,
  `attempt`.
- `ctx.callback(command, args)` sends one command to the run's control socket with the run
  capability and returns the reply; an error reply raises `Transient`, `AgentFailure`,
  `Cancelled` or `CallbackError` (`error`, `message`, `errors`, `current_rev`, `retryable`).
- `ctx.tool(name, args)` calls a named tool for the run's own project (`project` defaults to
  it): the reads `status`, `plan_get`, `messages`, `log_read`, `fn_list`, `fn_get` and
  `call_status`; `message.post` and `message.wait` (run as the step); and the project's
  mutations (`project_update`, the edit tools, retry, cancel, manual values). `args` use the command's wire shape (§12.1),
  not the flat MCP arguments. Another project, or any other tool, is refused (`conflict`).
- `ctx.builtin(name, inputs)` runs a builtin fn inside this run (same run and attempt, a new
  invocation) and returns its outputs.
- `ctx.submission()` returns the outputs submitted so far; `ctx.submit(outputs)` submits the
  step's declared outputs (§6.4).
- `ctx.retry_on_failure(step, message)` registers a completion action: if this run ends
  `rejected`, `step` (which must have a completed result) is retried with `message` (≤ 8 KiB)
  as feedback. Registering the same action again is a no-op; a different one is an error.
- `with ctx.acquire(resource, amount=1, timeout=None):` holds a section lease on a project
  resource (§7.6) for the block; `timeout` raises `TimeoutError`. An undeclared resource or an
  amount over a fixed capacity raises `CallbackError` (`bad_request`).
- `ctx.header(text)` appends the step's declared outputs to an agent task.

## 6. Plans

### 6.1 Document

```json
{
  "inputs":  {"repo": "string", "tasks": {"type": "string[]", "doc": "One task per item"}},
  "outputs": {"notes": {"source": "notes/final"}},
  "steps": {
    "work":  {"run": "agent.run", "scatter": "spec", "tags": ["unit:build"],
              "in": {"engine": {"default": "devin"}, "cwd": {"source": "repo"},
                     "spec": {"source": "tasks"}}},
    "gate":  {"run": "core.collect", "tags": ["unit:build", "exit"],
              "in": {"items": {"source": ["work/final"]}}},
    "notes": {"run": "agent.run", "after": ["unit:build"],
              "in": {"engine": {"default": "claude"}, "cwd": {"source": "repo"},
                     "spec": {"source": "gate/items.0"}}}
  }
}
```

A new project's plan is `{"inputs": {}, "outputs": {}, "steps": {}}` at rev 1. Project names,
step ids, plan input and output names match `^[a-z0-9][a-z0-9_-]*$`; a project name may not
look like a UUID; `owner` and `orchestrator` are not step ids. Plan inputs and steps share one
namespace.

Step keys: `run` (the fn), `in` (bindings), `scatter`, `doc`, `outputs` (declared, open fns
only), `paused` (`true` or a reason string), `after` (gate entries), `tags`, `needs`,
`priority`. Plan outputs are `{"source": "<ref>"}`.

### 6.2 Bindings

- `{"default": <json>}`: a literal.
- `{"source": "<ref>"}`: a plan input name, or `<step>/<output>` with optional `.field` or
  `.0` path segments.
- `{"source": ["<ref>", …]}`: fan-in, an array of the values in order.
- `{"file": "/abs/path"}`: the file's UTF-8 text, a `string`, read when the step starts (every
  start); a missing file fails the step and `verify` warns about one missing now. Only the path
  feeds the inputs hash.

Unbound optional fn inputs are null. A source binding is a **handoff**: the step waits for its
sources to succeed and is skipped when a source step is skipped.

### 6.3 Scatter

`"scatter": "<input>"` runs the step once per item of that input's array (each item must fit
the input's type); every output becomes an array in item order. The step succeeds when every
item does and fails if any fails, keeping the per-item results so a retry with unchanged inputs
and item count re-runs only the failed items. A scattered step holds its `needs` once.

### 6.4 Open fns: extra inputs, declared outputs, submission

A step whose fn is open may bind extra inputs (any id-shaped name) and declare `outputs`
(`{name: type | {"type", "doc"}}`, none named like a fn output); `submits` from fn.json join
them. Refs to declared outputs validate like any output. While the step runs, whoever does the
work calls `step_submit(project, step, run, outputs)`: checked against the declared outputs
(every required one, fitting types, no others; `invalid` lists each mismatch), refused unless
the run is current; a resubmission replaces the last. Each accepted submission is a
`step.submit` record. When the run completes, the submission joins the fn's outputs; a required
declared output never submitted fails the step.

### 6.5 Work done outside sluice

A step running `core.external` (open, no inputs or outputs of its own) is never started: once
ready it waits (wait reason `external: set its outputs with step_set_output`) until
`step_set_output` settles it or `step_cancel` fails it. It may not scatter, and `fn_call`
refuses it. Patching a step's `run` to `core.external` and retrying it moves its work out: the
old fn's inputs become extra inputs, and outputs dependents read must be declared.

### 6.6 Gates: `after`

`after` is a list of gate entries. A step is ready when every handoff source has succeeded and
every entry is satisfied.

| entry | satisfied when | skips the step when |
|---|---|---|
| `s` (a step) | `s` succeeded | `s` was skipped |
| `s?` | `s` succeeded or was skipped | never |
| `r` (a boolean ref or plan input) | the value is `true` | the value is `false` or null, or its step was skipped |
| `!r` | the value is `false` | the value is `true` or null, or its step was skipped |
| `unit:u` | every exit step of unit `u` succeeded | any exit step was skipped |
| `unit:u?` | every exit step succeeded or was skipped | never |

- An entry whose step is pending, running, failed or stale is unsatisfied: the step waits.
- A ref entry must be typed `boolean`, `boolean?` or `Any`; an `Any` value that is not a
  boolean fails the step (`after: <ref> is 3, not a boolean`). `?` is refused on refs and `!`
  on step and unit entries. A unit entry naming a unit with no steps is refused.
- A skipped step is decided again whenever its reasons change and goes back to `pending`. Skip
  reasons: `<ref> is false|true|null`, `step <s> was skipped`, `unit <u> was skipped (exit step
  <s>)`. Wait text: `after <entry> (<status>)`.
- Gates decide starts only: they never stop running work, are not evaluated for paused steps,
  and never feed the inputs hash.
- Entries keep their order and are deduplicated. Cycles count every entry; a unit entry counts
  as edges to the unit's exit steps.

### 6.7 Units

A unit is its tag: steps tagged `unit:<name>` form unit `<name>` (at most one `unit:` tag per
step); an untagged step is a unit of one named by its id. Edges may cross units. A unit is
**done** when every step succeeded or was skipped. Its **exit steps** are its steps tagged
`exit`, else its sinks over the edges inside the unit; a unit may not depend on its own exits.
Its **entry steps** (derived, never stored) are its steps with no dependency inside the unit.
`unit:` and `exit` are reserved tags. A unit is **settled** when none of its steps is running
and every pending one is external or not ready.

### 6.8 Recipes

A recipe is `recipes/<name>.json` in the home (global) or in `projects/<id>/` (the project's
wins on a name clash): `{"name", "doc"?, "params"?: {name: type | {"type", "doc"}}, "steps"}`,
`name` matching the file. `unit` (a step id) is always a param. In every step id and string,
`{param}` is replaced by the value (a non-string as JSON); a string that is exactly `{param}`
becomes the value with its type; `{{`/`}}` are literal braces; an unknown `{x}` is an error. A
recipe step with `when` is broken.

`unit_add(project, recipe, unit, params, after?, inputs?, tags?, start?)` checks the params,
expands the recipe, tags every new step `unit:<unit>` (then the recipe's tags, then `tags`;
`unit:` tags in `tags` are refused), and in the same edit appends `after` entries per suffix
(`"*"` means the unit's entry steps) and binds `inputs` per suffix to `{"default": value}`. A
suffix is a recipe step's id without the leading `<unit>-`. An unknown suffix, an input the
step's fn does not declare and the recipe does not bind, or a step id the plan already has is
refused before anything is written.

### 6.9 Validation

Every edit is validated whole: ids valid; every `run` visible to the project; required inputs
bound, no unknown inputs (extra inputs and declared outputs only on open fns); refs naming a
plan input or a step output (paths through record types, anything under `Any`); every source
fitting its input (list sources element-wise, the scatter input item-wise); literals passing
their types; gate entries well formed; `needs` naming declared resources within any fixed
capacity; no cycles. Errors are a list with paths
(`steps.notes.in.cwd: repo is int, which does not fit string: int is not string`).

### 6.10 Edits

Every edit tool produces RFC 6902 ops against the plan document, validates the result, and
commits the new plan, a `plan_edits` row and a `plan.edit` record `{rev, author, reason, ops}`
in one transaction. The reply is the **edit result** `{project: {project_id, name}, rev,
preview, steps?}`, `preview` being `{ops, would_start, would_queue, would_skip, would_stale,
errors}` and `steps` the steps the edit was about (`unit_add`'s new steps, `unit_tag`'s unit,
`step_pause`'s selection, `plan_prune`'s removed steps).
With `dry_run: true` the reply is the preview alone and nothing is written. The simulation uses
cached capacities and never runs fns; `core.external` steps never appear in `would_start`.

- `plan_patch` requires the current `rev`; the other edit tools take an optional `rev` and
  otherwise apply to the current plan. A stale `rev` is `conflict` with `current_rev`.
- A running step may change only `paused` and `tags`.
- `start: false` (`plan_patch`, `step_add`, `unit_add`) adds steps with `"paused": true`
  unless a step sets `paused` itself.
- An edit that changes nothing (an edge already there, tags or pauses as they are, a prune
  that removes nothing, a patch that yields the same plan) commits nothing: no rev, record or
  history row. Its reply is the edit result with the current `rev` and empty `preview.ops`.
  `step_set_input` refuses one instead (`bad_request`).
- Removing a finished step keeps its result as an `outcomes` row; a pending step leaves none.

## 7. Running

### 7.1 Step status

`pending`, `running`, `succeeded`, `failed`, `stale`, `skipped`. A succeeded step may be
`manual` (set by hand). A scattered step also has `done`, `total` and `instances`. A step's
`error` is a structured error object (§12.2).

A step is **ready** when it is pending, not paused, its project is not paused, every plan input
it reads has a value, every handoff source succeeded and every gate is satisfied. A ready step
starts at once unless it has `needs` (§7.6); a ready `core.external` step only waits.

### 7.2 Admission and launch

While the scheduler lease is held the coordinator, on every relevant change, settles skips,
marks staleness, and admits ready steps in priority order (higher first, ties in plan order).
Admission reserves an attempt and its runs in one transaction (status `running`, fresh run ids,
the messages assigned to each run, `prev_run`), then launches each run's unit. A reservation
whose launch fails before the payload starts fails that run with `process_lost`; it is never
started twice. Built-in fns that need no process run inside the coordinator.

### 7.3 Staleness

When a step starts or is set by hand it records `inputs_hash`, a hash of the canonical JSON of
its bound inputs (an unbound optional input is left out; a file binding by path). A succeeded
step becomes `stale` when a step it reads is stale or its inputs, once all available, hash
differently; it becomes `succeeded` again if they hash as recorded. Stale steps keep their
outputs, never re-run by themselves, and block their readers. A step set with `force` while its
inputs were not ready records unknown inputs and turns stale once they are all there. Gates
never cause staleness.

### 7.4 Retry and re-arm

`step_retry(project, steps?, tags?, message?, reason?)` takes steps that are `succeeded`,
`failed` or `stale` (any other selected step refuses the whole call). Each goes back to
`pending` and gets a `step.retry` record with its new work generation. A succeeded step keeps
showing its old outputs until the new run ends; its dependents go stale only if the result
differs. A failed scattered step keeps its succeeded items when its inputs hash and item count
are unchanged.

Retrying also **re-arms** the blocked region: from each retried step it walks dependents
through handoffs and gates, passing through failed, stale and pending steps (failed and stale
ones go back to `pending`, cancelled failures included) and stopping at succeeded, running and
skipped steps. The reply is `{project, steps, rearmed, stopped_at}`. `message` (1 to 65536
bytes) is posted to each retried step's thread in the same transaction, so its next run is
assigned it. Pauses are kept.

### 7.5 Cancel

`step_cancel(project, steps?, tags?, reason?, expected_rev?)` asks running steps to stop (their
guardians stop the payload) and fails a pending `core.external` step at once. A cancelled step
fails with the error `{"error": "cancelled", "message": <reason>}` and a `step.cancel` record.
Reply `{"ok": true}`.

### 7.6 Resources and leases

A project declares resources: `{"lane": 4}` or `{"lane": {"capacity": 4}}` (a fixed integer
≥ 0), or `{"cpu": {"capacity_fn": "<fn>"}}`, a fn the project sees that takes no required input
and returns `{capacity: int}`. While the scheduler lease is held, every capacity fn is called as
a direct call (author `capacity`) every 10 s; a good value replaces the cached one and is
recorded as `project.capacity`, a failure keeps the last good value and records the error.
Before its first value a capacity fn's resource admits only needs of 0.

A step's `needs` (`{resource: n}`) is held while it runs (a scattered step once). A ready step
with `needs` starts only when every named resource has `capacity - held >= need`; otherwise it
stays pending and queued (`step.queued` record when its shortfall changes; wait reason
`queued: needs lane 1 (4/4 held)`). Lowering a capacity never stops running work; a resource
that a step needs or a lease holds cannot be removed.

A **section lease** (`ctx.acquire`, §5.4) waits for and holds an amount of a resource inside a
running step's run; grants follow the step's `priority`, then arrival. Held leases count in
the same totals as `needs`. A run that ends releases its leases. Grants and releases are
`step.lease` records.

### 7.7 Manual values

- `plan_set_input(project, name, value)`: sets a declared plan input (type-checked), a
  `plan.input` record; readers that already ran go stale when it changes.
- `step_set_input(project, steps?, tags?, inputs)`: binds the named inputs of the selected
  steps to `{"default": value}` in one edit, skipping running steps and steps lacking an input;
  a succeeded step whose binding changes goes stale. The reply is the edit result plus
  `changed` (the steps changed), `running` (selected, running, left alone) and `unsupported`
  (`[{step, inputs}]`, selected but lacking those inputs).
- `step_set_output(project, step, outputs, force?, reason?)`: marks a non-running step
  `succeeded` with `manual: true`, outputs checked against the step's outputs (arrays for a
  scattered step). Without `force`, refused (`invalid`, "step gates or inputs are not ready")
  while a gate is unsatisfied or something it reads is not ready. A `step.output` record.

### 7.8 Message delivery and `prev_run`

Each step keeps a delivery cursor: the last message id addressed to it that a run of it was
given. Reserving a run assigns it every message to the step after the cursor and records the
range on the run; the live feed continues from the end of that range, and the cursor advances
once the run has started. Every run of a scattered step gets the step's messages.
`runs/<run>/messages.json` shows the assigned range. Each run records `prev_run`, the step's (or
item's) previous run, seen as `SLUICE_PREV_RUN` and `ctx.prev_run`. Agent fns resume the
previous session when a bound `session` says so, or when `session` is unbound, the run was
assigned messages, `prev_run` recorded a session, and the engine and cwd are unchanged;
`session: ""` always starts fresh.

### 7.9 Adoption and completion

A coordinator that starts finds runs whose guardians are still alive and watches them
(`run.adopt` outcome `watching`), finishes runs that completed while it was away (`finished`),
and fails runs whose guardian and payload are gone without a completion (`lost`, error
`process_lost`). A live run that no step or call references is stopped (`run.orphan`). A
completion is journalled by the guardian and acknowledged durably by the coordinator.

### 7.10 Outcomes

Removing a finished step from the plan (any edit, including `plan_prune`) stamps its result row
`removed_at`; the `outcomes` view shows these rows (`result_id, project_id, step_id,
generation, work_generation, attempt_id, unit, declaration, inputs, inputs_hash, status,
outputs, error, manual, run_ids, recorded_at, removed_at`). They are never trimmed and go with
the project.

## 8. Messages

A message is a row `{id, thread, from, to, title, body, needs_reply, reply_to, answer, ui,
input, data, run, at, claimed_by}` plus a `message` record written in the same transaction. Rows
are never trimmed with the log and are deleted with their project.

`message_post(project, body, thread?, to?, needs_reply?, reply_to?, answer?, title?, ui?,
input?, data?, from?, run?)` → `{id}`:

- `from`: the given `from`, else the author (§12.3).
- `thread`: the parent's for a reply; else the given thread; else `step-<step>` when posted by a
  step's run; else `m<id>`. Thread names use the id alphabet.
- `to`: a step id, `orchestrator`, `owner` or absent (anyone); a reply defaults to the
  parent's `from`. A post that is not a reply, on a `step-<id>` thread naming a step in the
  current plan and not from that step, defaults to that step, so its runs are given it.
- `needs_reply` defaults to true for a new message and false for a reply: true is a question,
  false a note.
- `answer` `{action, params?, values?}` requires `reply_to`, and the parent must be an open
  question, else `conflict` ("question is no longer open").
- `input` names a declared plan input. The first **answering reply** (a reply with
  `needs_reply` false, or one carrying `answer`) resolves the question atomically and sets the
  input when there is one: the value is `answer.values.value`, else `answer.params.value`, else
  the body; it goes through `plan_set_input`'s path with reason `message <id>: <title>`. A
  value that does not fit refuses the reply (`invalid`) and the question stays open. A reply
  that is itself a question answers nothing. `answer.action == "close"` closes the question
  without setting anything.
- An open question to `owner` reserves a notification attempt and writes a `project.notify`
  record.

Question state is derived: `open`, `answered` or `closed`. A question posted by a step's run
is `waiting` while that run is live; otherwise it reports why nobody waits: `asking run is
being cancelled`, `<step> is <status>`, `<step> is cancelled`, `running another run`, `not in
the plan`, `call <run> is <status>`.

`message.post` with `wait: true` (§16) blocks until the first answering reply and returns it as
`reply`; a closed question fails it (`question closed`). A retried step asking with the same
title takes up its own latest earlier question: an open one nobody waits on gets the new run as
asker; an answered one whose answer nobody claimed returns that answer at once. A claimed
answer (`claimed_by`) is never reused.

`messages(project, view, thread?, since?)` → `{project, messages, last_id}`:

| view | shows |
|---|---|
| `inbox` | open questions to `owner`, then notes to `owner` after the reader's read position in their thread |
| `questions` | every open question in the project |
| `history` | every thread with a message to or from `owner` |
| `thread` | one thread in full (`thread` required) |

Read positions are kept per project, reader and thread and advance through `mark_read` (the
dashboard marks what it shows, as `owner`). The MCP tool reads as reader `cli`.

## 9. The log

Every record is `{seq, at, project, kind, …}`; `seq` is home-wide and increasing, so one log's
seqs have gaps. Kinds:

| kind | fields |
|---|---|
| `plan.edit` | `rev, author, reason, ops` |
| `plan.input` | `rev, author, reason, name, value` |
| `step.output` | `rev, author, reason, step, outputs, force` |
| `step.retry` | `rev, author, reason, step, work` |
| `step.cancel` | `step, author, reason` |
| `step.submit` | `step, run, outputs, author` |
| `step.status` | `step, from, to, error, run_ids, needs` |
| `step.lease` | `step, run, lease, resource, amount, state, reason` |
| `step.queued` | `step, needs, resources, reason` |
| `call` | `call, fn, status, inputs, outputs, error, direct, author` |
| `message` | the message's fields, with `posted_at` for its time |
| `project.pause` | `paused, reason, author` |
| `project.archive` | `archived, reason, author` |
| `project.update` | `fields, reason, author` |
| `project.rename` | `old_name, new_name, author` |
| `project.delete` | `project_id, name, author` |
| `project.capacity` | `resource, fn, capacity, error` |
| `project.notify` | `message, outcome, error` |
| `run.adopt` | `run, step, call, outcome` (`watching`, `finished`, `lost`) |
| `run.orphan` | `run` |
| `run.completion_action.register` | `run, target, message, author` |
| `run.completion_action` | `run, outcome, author` |
| `unit.settled` | `unit, work, steps: [{id, status, held, outputs, omitted}]` |

`kinds` filters take exact kinds or the groups `plan`, `step`, `project`, `run`, `unit`.
`threads` keeps only messages on those threads (alone it means messages only). Calls made
without a project go to the home log (`project` null).

Each log keeps at most 10,000 records; past that it is trimmed to 9,000 in the same
transaction. A `since_seq` older than a log's trim floor, or newer than any seq the home has
issued, is `cursor_expired`. `plan_edits` is never trimmed, so `plan_history` reaches rev 1.

## 10. Waiting: `log_wait`, `next`, `watch`

`log_wait(project?, since_seq?, kinds?, threads?, limit=200, timeout=300, wake="any")` returns
`{records, last_seq}` as soon as a matching record exists after `since_seq`, or with no records
after `timeout` seconds (capped at 3600). `wake: "questions"` holds notes until a record that is
not a note arrives or the timeout passes.

`next(projects=[], since_seq=0, me="orchestrator", timeout=300, all=false, settle=20,
settle_max=120, settles="short")` waits across projects (all live projects when empty) for what
an orchestrator acts on and returns `{records, notes, last_seq, timed_out}`. It wakes on:

- a `unit.settled` record (written when a unit settles, once per unit and work generation);
- a `step.status` to `failed`, `stale` or `skipped`;
- a question to `me` or to nobody, not from `me`;
- an answering reply not from `me`;
- `project.pause` or `project.archive` not authored by `me`;
- with `all`, any record.

Notes are held and returned in `notes`. After the first waking record it keeps collecting until
`settle` seconds pass with nothing new, or `settle_max` seconds after the first. Messages come
first in `records`. `settles` sets how much of a settled unit's step outputs are carried:
`short` (booleans, numbers, strings up to 80 characters and a `summary`'s first line; the rest
named in `omitted`), `full` or `none`. Every call records the reader's position and heartbeat in
`readers`.

`sluice next` is the same wait (§14), and `sluice watch` follows the log as JSON lines.

## 11. Verify

`verify(project?)` returns a list of problems `[{where, message}]` (empty when all is well) and
changes nothing. It covers the fn registry (shapes, names, icons, collisions), `.env` syntax,
project directories that belong to no live project (a warning), unfinished attempts, and for
each project its plan (full validation against its fns, missing `file` bindings) and its state
against the plan.

## 12. Tools

### 12.1 Transports

- **MCP**: streamable HTTP at `http://127.0.0.1:<port>/mcp`, served by `sluice serve`. The
  server's instructions are the `instructions` docs topic; every topic is also a resource
  `sluice://docs/<topic>`.
- **HTTP**: `POST /api/tools/<name>` with a JSON object body, the same flat arguments as MCP;
  the reply is the tool's JSON, errors mapped to 400 (`bad_request`, `invalid`), 404, 409
  (`conflict`, `cursor_expired`), 503 (`busy`), 408 (`cancelled`) and 500.
- **CLI**: `sluice tool <name> '<json>'` (§14).
- **Wire**: every tool is a command `{"command": name, "args": {...}}` with a reply `{"reply":
  kind, "data": …}` on the coordinator socket (`sluice tool rpc '<request>'` sends a raw
  request). `docs/rust/schemas.json` (`CommandRequest`, `CommandReply`) is its schema.

The HTTP server binds loopback only. Every route refuses a `Host` that is not `127.0.0.1`,
`localhost` or `[::1]`, and refuses a present `Origin` that is not the same loopback origin.
Bodies are at most 1 MiB and at most 64 requests run at once. A tool call has a deadline of its
wait plus 30 s (`busy`, not retryable, "command deadline exceeded").

### 12.2 Arguments, results and errors

MCP and HTTP arguments are flat; an argument a tool does not take is refused (`bad_request`
naming the ones it takes). `project` is a current name or `id:<uuid>`; `steps`, `tags`,
`projects` and `state` also accept a single string. Waits (`wait`, `timeout`, `settle_max`) are
capped at 3600 s. A reply object is returned as is; a non-object reply is wrapped as
`{"result": …}` in MCP structured content; an acknowledgement is `{"ok": true}`.

Errors are `{"error": kind, "message", …}`:

| kind | extra fields | meaning |
|---|---|---|
| `bad_request` | | malformed or unknown argument |
| `not_found` | | unknown project, step, fn, call, unit |
| `conflict` | `current_rev?` | stale revision, state changed, question no longer open |
| `invalid` | `errors` | validation failed; each error has a path |
| `busy` | `retryable` | drain, maintenance, deadline |
| `storage` | | database or I/O failure |
| `cursor_expired` | | `since_seq` outside the log |
| `process_lost` | | a run's process vanished without a result |
| `cancelled` | | cancelled work |
| `fn_failure` | | a fn failed |
| `transient` | | a retryable failure that exhausted its retries |
| `rejected` | | a fn refused the work on purpose |
| `agent_failure` | `kind, session?` | an agent session failed |

### 12.3 Authors

Every write records an author: the tool's `author` argument when given; else `SLUICE_AUTHOR`;
else `step:<SLUICE_STEP>` when set; else the MCP client's name; else `mcp` (MCP) or `cli`
(`sluice tool`). The dashboard writes as `owner`, capacity calls as `capacity`.

### 12.4 Tool reference

Edit tools share `rev?` (`plan_patch`: required), `dry_run=false`, `reason=""` and `author?`
and return the edit result (§6.10) or, with `dry_run`, the preview. Selection tools take
`steps?` and/or `tags?` (a `unit:` tag selects the unit); naming neither is `bad_request`, an
unknown step `not_found`.

**Projects and fns**

| tool | arguments | result |
|---|---|---|
| `docs` | `topic?` | the topic's markdown, or the index |
| `projects_list` | | `[{project_id, name, description, rev, settings_rev, counts, paused, archived, resources?, icon?}]` (live projects, by name); `counts` maps step status to the plan's steps in it, `resources` each declared resource to `{capacity}` or `{capacity_fn}`, `icon` is `{kind: "image", type}` or `{kind: "text", text}` |
| `project_create` | `name`, `description=""`, `icon?`, `resources={}`, `author?` | `{project_id, name}` |
| `project_update` | `project`, `new_name?`, `description?`, `icon?` (`""` removes), `resources?` (each key set, null removes), `paused?`, `archived?`, `expected_settings_rev?`, `reason?`, `author?` | `{project_id, name}`; changes write `project.rename`, `project.pause`, `project.archive`, `project.update` |
| `project_delete` | `project`, `confirm_name`, `expected_settings_rev` (from `projects_list`), `author?` | `{project_id, name, deleted}`; the project must be archived, and nothing of it live |
| `fn_list` | `project?` | `[{name, doc, inputs, outputs, scope, submits?, icon?, open?}]` |
| `fn_get` | `name`, `project?` | the fn.json plus `scope` and `path` (null for a builtin) |
| `fn_save` | `fn`, `main_py`, `project?` | `{name, scope, path, generation}` |
| `fn_call` | `name`, `inputs={}`, `project?`, `wait?` (seconds, default 0), `direct=false`, `author?` | `{call, project_id, status, inputs, outputs, error, direct}`; `direct` runs it now through a guardian and ignores `wait` |
| `call_status` | `call`, `project?` | as `fn_call` |
| `recipe_list` | `project` | `[{name, doc, params, scope}]`, a broken file as `{name, scope, error}` |

An `icon` is a text icon (at most 16 characters), an absolute or `~/` path to an SVG, PNG,
WebP, JPEG or GIF file of at most 256 KiB, which the coordinator reads once and stores, or
`{media_type, bytes_base64}`. A missing or unreadable path is `invalid`.

Each call's status changes are `call` records. `core.external` cannot be called.

**Plan edits**

| tool | specific arguments |
|---|---|
| `plan_patch` | `project`, `rev`, `ops` (RFC 6902), `start=true` |
| `step_add` | `project`, `step`, `spec`, `start=true` |
| `step_update` | `project`, `step`, `changes` (each key replaces that field, null removes it) |
| `step_remove` | `project`, `steps?`, `tags?` |
| `step_pause` | `project`, `steps?`, `tags?`, `subtree=false`, `paused=true` (sets `"paused"` to the `reason`, or `true` without one, a step already paused keeping its own; `false` removes it) |
| `unit_add` | `project`, `recipe`, `unit`, `params={}`, `after={}`, `inputs={}`, `tags=[]`, `start=true` |
| `unit_tag` | `project`, `unit`, `add=[]`, `remove=[]` |
| `edge_add`, `edge_remove` | `project`, `step` (a step or `unit:<name>`: its entry steps), `after` (entries) |
| `step_set_input` | `project`, `steps?`, `tags?`, `inputs` |
| `plan_prune` | `project`, `units?`, `tags?`, `older_than=0` (seconds) |

`step_pause` with `subtree` also selects every step downstream of the selection (reading from
or gated on one, transitively). `unit_add` reports the steps it added in `steps`, `unit_tag`
the unit's steps and `step_pause` the steps selected.

`plan_prune` selects done units (all, or those named or tagged) whose last step finished at
least `older_than` seconds ago, keeps any unit that a surviving step or plan output references
(computed as a closure), and removes the rest in one edit; naming a unit that does not exist or
is not done is `invalid`. The reply is the edit result plus `units` (removed) and `kept`:
`[{unit, step}]` or `[{unit, output}]`, each kept unit with the step or plan output that holds
it; `steps` lists the removed steps.

**State and values**

| tool | arguments | result |
|---|---|---|
| `plan_get` | `project` | `{project, rev, plan}` |
| `plan_history` | `project`, `since_rev?` | records: every edit (`plan.edit`, from rev 1) and the log's `plan.input`, `step.output`, `step.retry` |
| `plan_set_input` | `project`, `name`, `value`, `rev?`, `dry_run`, `reason`, `author?` | `{ok: true}`, or the preview |
| `step_set_output` | `project`, `step`, `outputs`, `force=false`, `reason`, `author?` | `{ok: true}` |
| `step_retry` | `project`, `steps?`, `tags?`, `message?`, `reason`, `expected_rev?`, `author?` | `{project, steps, rearmed, stopped_at}` |
| `step_cancel` | `project`, `steps?`, `tags?`, `reason`, `expected_rev?`, `author?` | `{ok: true}` |
| `step_submit` | `project`, `step`, `run`, `outputs`, `author?` | `{ok: true}` |
| `status` | `project`, `steps?`, `tags?`, `brief=false`, `all=false`, `view="steps"`, `state?` | below |
| `step_context` | `project`, `step` | below |
| `plan_view` | `project`, `format="mermaid"`, `all=false` | text |
| `verify` | `project?` | `[{where, message}]` |

`status`, steps view: `{project, rev, paused, inputs, outputs, resources, steps: {id: {status,
outputs, error, run_ids, done, total, instances, manual, paused?, queued?, waiting?}},
done_units?}`.
Without a selection, done units are left out and counted in `done_units: {units, steps}` unless
`all`. `brief` cuts strings over 200 characters (`… [n more characters]`). `resources` maps
each to `{capacity, held, queued, error}`. `paused` is `true` or the pause's reason. Every
pending step that is not about to start has `waiting`, the reasons in order: `paused` or
`paused: <reason>`, `project paused`, each handoff not ready (`step <id> is <status>`, `plan
input <name> has no value`), each gate not satisfied (`after <entry> (<why>)`), the resource
shortfall (`queued: needs lane 1 (4/4 held)`, with `queued` listing the resources), and for a
`core.external` step with nothing else, `external: set its outputs with step_set_output`. Units view (`view: "units"`, `brief` refused): `{project, rev, paused, resources?,
units: [{unit, state, age, engine, steps, blocked, last, line}], done_units?}`, `state` one of
`running`, `failed`, `settled`, `blocked`, `queued`, `pending`, filterable with `state`
(`state` is refused in the steps view).

`step_context` (also `sluice me`): `{project, project_id, step, fn, doc, status, started,
finished, elapsed, run, inputs, upstream: [{step, fn, status, outputs, error}], messages (open
questions on its thread), submit: {outputs, command}, thread, ask, needs?, queued?, leases?}`;
`submit.command` and `ask` are ready-to-run `sluice tool` lines.

`plan_view`: Mermaid `flowchart LR` with one subgraph per unit, nodes labelled `id / fn /
status [done/total] / doc`, a class per status, done units left out with a `%% n done units (m
steps) left out` comment unless `all`; or `format: "html"`, a standalone page with an SVG of the
same graph.

**Messages and the log**

| tool | arguments | result |
|---|---|---|
| `message_post` | §8 | `{id}` |
| `messages` | `project`, `view`, `thread?`, `since?` | `{project, messages, last_id}` |
| `log_read` | `project?`, `since_seq?`, `kinds?`, `threads?`, `limit=200` | `{records, last_seq}`; without `since_seq` the latest records |
| `log_wait` | as `log_read`, plus `timeout=300`, `wake="any"` | `{records, last_seq}` |
| `next` | §10 | `{records, notes, last_seq, timed_out}` |
| `query` | `sql`, `params=[]`, `limit=200` | `{columns, rows, truncated}` |

`query` runs one read-only statement on a fresh connection: at most 100,000 bytes of SQL, `limit`
1 to 1000 rows, a 2 s deadline, a 1 MiB response; writes, `ATTACH`, extensions and unsafe
functions are refused. JSON columns come back as JSON text.

**Maintenance**

| tool | arguments | result |
|---|---|---|
| `drain` | `projects?`, `author?` | `{paused, status: {mode, owner, paused, blockers, pending_calls, open_questions, drained}}` |
| `release` | `author?` | `{released}` |

### 12.5 Commands outside MCP

The wire also carries `mark_read` (advance a reader's position on a thread), `backup`, `builtin`
(guardian-authenticated only), `submission`, `acquire_lease`, `release_lease` and
`register_completion_action` (run callbacks only).

## 13. Dashboard

`sluice serve` serves the dashboard on the same port. Pages (look: `DESIGN.md`):

| route | page |
|---|---|
| `/` | projects: each with its status glyph, progress and what stops it; archived ones folded |
| `/projects/<name>` | redirects (307) to `/projects/id/<uuid>` |
| `/projects/id/<p>` | the board; query `order=live\|plan`, `show=all\|active\|attention\|done`, `tag=`, `format=mermaid` (with `all=true`) |
| `/projects/id/<p>/units/<u>` | one unit's board |
| `/projects/id/<p>/steps/<s>` | one step: status, actions, error, outputs, inputs, runs |
| `POST /projects/id/<p>/steps/<s>/actions` | `action=pause\|unpause\|retry\|cancel`, `revision`, `message` (retry feedback) |
| `/inbox`, `/questions`, `/history` | the message views across projects |
| `/projects/id/<p>/{inbox,questions,history,thread}` | the same for one project; `thread?thread=<name>` |
| `POST /projects/id/<p>/messages` | post an answer or reply as `owner` |
| `POST /projects/id/<p>/messages/read` | mark messages read |
| `/log`, `/projects/id/<p>/log` | the log, 50 records a page, filtered by kinds and threads |
| `/fns` | the functions a project (`?project=`) or the home sees |
| `/projects/id/<p>/settings` (GET, POST), `/preview`, `/icon`, `/delete` | project settings |
| `/projects/id/<p>/icon` | the project's image icon |
| `POST /settings` | display preferences (theme, value types) |
| `/static/<name>` | assets |

Every page has a `…/stream` twin that patches the page live over Datastar SSE. Pages render
fully without JavaScript; every value is HTML-escaped and markdown bodies are rendered on the
server with unsafe link schemes refused. The only external assets are two font stylesheets from
cdn.jsdelivr.net; every script is served from `/static/` (Datastar 1.0.4, OpenUI lang-core
0.3.0, zod 4.6.5 and sluice's own).

**Questions with a ui.** A question's `ui` is an OpenUI Lang program drawn by the inbox page:
one statement per line, the first drawn, components `Stack`, `Heading`, `Text`, `Callout`,
`Table`, `Separator`, `Form`, `Input`, `Textarea`, `Select`, `Radio`, `Checkbox`, `Button`
(signatures in `docs("inbox")`). Unparseable lines are dropped and counted; the text box always
remains. A Button answers `{action, params, values}`; the text box answers with action
`answer` and its text as the body; "Close" answers with action `close`.

## 14. CLI

`sluice <mode>`; errors print as JSON on stderr with a non-zero exit.

| mode | |
|---|---|
| `serve [--no-runner] [--port 3065] [--host 127.0.0.1]` | dashboard, MCP and HTTP tools; takes the scheduler lease unless `--no-runner`; loopback only |
| `loop` | takes the scheduler lease and holds it until SIGINT/SIGTERM |
| `coordinator [--maintenance]` | runs the home's coordinator in the foreground |
| `install fence <reason> \| unfence \| select <release_dir> <home> \| status` | §2.2 |
| `tool [name] [json]` | without a name, lists the tools; with one, runs it (JSON from the argument or stdin) and prints the reply |
| `tool rpc '<request>'` | sends a raw wire request |
| `next [-p P]… [--since-seq N \| --cursor FILE] [--me NAME] [--timeout 300] [--settle 20] [--settle-max 120] [--all] [--settles short\|full\|none] [--cut 600] [--json]` | the `next` wait; without a since it starts at the top of the selected logs; `--cursor` reads and writes the seq in a file |
| `watch [-p P] [--kinds K,…] [--threads T,…] [--since-seq N] [--wake any\|questions]` | follows the log, one JSON record per line, until killed |
| `drain [-p P]… [--no-wait] [--release]` | drains (waits until drained unless `--no-wait`) or releases |
| `me [--project P] [--step S] [--json]` | `step_context` for the current step (from `SLUICE_PROJECT_ID`/`SLUICE_PROJECT` and `SLUICE_STEP`) |
| `doctor [--json]` | host prerequisites, engine profiles and the selected release's manifest check |
| `query [SQL [PARAM…]] [--limit N] [--table [--width 60]]` | the `query` tool, read directly from the database; without SQL, every public table and view with its columns |
| `backup PATH [--force]` | an online copy of `sluice.db` |
| `docs [topic]` | the agent docs |
| `agent hook --engine codex\|claude\|devin --event E [--run R]` | engine hook entry (internal) |
| `guardian`, `payload-exec` | internal |

`sluice next` prints one line per event: `MSG|NOTE <thread> <from> -> <to>: <body>`, `STEP <id>
<from> -> <to>: <error tail>`, `UNIT <u> settled: <outputs>` (long outputs named with a hint to
read them with `sluice query` or `--settles full`), `PROJECT <id> paused by <author>`, and last
`seq N` or `timeout seq N`; `--json` prints the reply.

`sluice tool` takes the wire argument names with these conveniences: `steps`/`tags` (and
`after`, `projects`, `state`) as plain values or lists, `expected`/`dry_run`/`reason`/`author`
flat on edit tools, `wait` for `fn_call`, `timeout` and `wake` for `log_wait`, `timeout`,
`settle` and `settle_max` for `next`, `fn` for `fn_save`, `name` for `project_update` and
`project_delete` (which also fills `confirm_name` and the current `expected_settings_rev`),
`params.unit` for `unit_add`, and `step`/`input`/`value` for `step_set_input`.

## 15. Agent engines

`agent.claude`, `agent.codex`, `agent.devin`, `agent.review` and `agent.run` run a supervised
interactive session of the engine CLI in the run's private tmux, in `cwd`. The supervisor
writes the task (the prompt or spec, the step's inputs under `## Inputs`, the outputs to submit
with the exact `step_submit` command under `## Outputs you must submit`, and, unless `listen:
false`, how to use the step's thread), watches the session through the engine's hooks, nudges a
stalled session, and ends it when the agent is done. The result carries `session` (pass it back
to resume) and `git` facts `{head_before, head_after, commits, dirty}` of `cwd`. A failed
session is an `agent_failure` error with its `kind` and `session`. Agent fns retry up to 3
times, 600 s apart.

Limits (minutes unless noted), overridable through environment variables: `SLUICE_AGENT_MAX_MIN`
(600, the wall cap), `SLUICE_AGENT_STALL_MIN` (30), `SLUICE_AGENT_SETTLE_S` (10),
`SLUICE_AGENT_GRACE_MIN` (10), `SLUICE_AGENT_POLL_S`, `SLUICE_AGENT_READY_S` (180),
`SLUICE_AGENT_TURN_START_S` (60), `SLUICE_AGENT_WAIT_MIN` (90), `SLUICE_AGENT_DIALOG_S` (60),
`SLUICE_AGENT_QUIET_MIN` (45), `SLUICE_AGENT_WORK_MIN` (10), `SLUICE_AGENT_NUDGES`.

## 16. Built-in fns

| fn | inputs | outputs | notes |
|---|---|---|---|
| `core.echo` | `value: Any` | `value: Any` | inline |
| `core.collect` | `items: Any[]` | `items: Any[]` | the fan-in join; inline |
| `core.format` | `template: string`, `values: Any` | `text: string` | `{0}` from an array, `{name}` from a record, `{{`/`}}` literal; non-strings as JSON; inline |
| `core.external` | | | open; never runs (§6.5) |
| `inline.bash` | `code: string`, `cwd: string?`, `check: boolean?` | `stdout, stderr: string`, `code: int` | open; errexit and pipefail; extra inputs as environment variables (`-` → `_`); declared outputs from the JSON object written to `$OUT`; fails on a non-zero exit unless `check: false` |
| `inline.python` | `code: string`, `cwd: string?` | `value: Any?`, `stdout: string` | open; standard library; sees `inp` and each extra input; `out` is the result |
| `message.post` | `body`, `thread?`, `to?`, `needs_reply?`, `reply_to?`, `answer?`, `title?`, `ui?`, `input?`, `data?`, `from?`, `wait?` | `id: int`, `reply: Any?` | §8 |
| `message.wait` | `thread`, `since: int?`, `to?`, `timeout: int?` (300), `wake?` | `messages: Any[]`, `last_seq: int` | waits for messages on a thread after `since`; `wake: "questions"` holds notes |
| `agent.claude` | `cwd`, `prompt`, `session?`, `listen?` | `result`, `session`, `git` | open; Opus |
| `agent.codex` | `cwd`, `spec`, `model?` (`sol` default, `astra`), `effort?` (`minimal`…`max`, default `high`), `log?`, `session?`, `report_path?`, `listen?` | `log`, `final`, `report?`, `session`, `git` | open |
| `agent.devin` | `cwd`, `spec`, `model?` (default `swe-2-high`; `fusion`), `log?`, `session?`, `report_path?`, `listen?` | `log`, `final`, `report?`, `session`, `git` | open |
| `agent.review` | `cwd`, `base`, `standards`, `notes?`, `session?`, `listen?` | `summary`, `sha`, `commits: int`, `session`, `git` | open; reviews and fixes a branch diff with Claude |
| `agent.run` | `engine` (`devin`, `codex`, `claude`), `cwd`, `spec`, `model?`, `effort?`, `session?`, `report_path?`, `listen?` | `final`, `report?`, `session`, `git` | open |
| `decide.llm` | `question`, `context: Any?`, `options: string[]`, `threshold: float?` | `choice`, `p: float`, `confident: boolean` | 2 retries, 30 s apart |
| `git.head` | `path` | `branch`, `sha` | |
| `git.merge` | `repo`, `source`, `target`, `message?`, `push: boolean?` | `merged: boolean`, `sha?`, `conflicts: string[]` | in a temporary worktree; conflicts are data |
| `git.push` | `path`, `branch`, `remote?`, `force_with_lease: boolean?` | `sha` | |
| `git.rebase` | `path`, `onto` | `ok: boolean`, `sha`, `conflicts: string[]` | conflicts abort and are data |
| `git.worktree` | `repo`, `base`, `branch`, `path?` | `path`, `branch`, `sha` | |
| `git.worktree_rm` | `repo`, `path`, `force: boolean?` | `removed: boolean` | |
| `gh.pr` | `path`, `base`, `head`, `title`, `body`, `draft: boolean?` | `number: int`, `url` | creates or updates the open PR |
| `gh.pr_wait` | `path`, `pr`, `until` (`checks`, `merged`), `interval: int?`, `timeout: int?` | `state` (`green`, `red`, `conflicting`, `merged`, `closed`, `timeout`), `sha`, `url`, `failed: string[]` | 3 retries, 30 s apart |
| `gh.run_cancel` | `path`, `run_id: int` | `cancelled: boolean` | |
| `gh.run_latest` | `path`, `branch?`, `workflow?` | `run_id: int`, `sha`, `status`, `conclusion?`, `url`, `workflow`, `failed_jobs: string[]` | |
| `jev.ask` | `state: Any`, `questions: Any`, `model?` | `answers: Any`, `model`, `usage: Any` | TypeSafe System One; needs `TYPESAFE_API_KEY`; 3 retries, 5 s apart (all `jev.*`) |
| `jev.choice` | `state`, `instructions`, `options`, `min_confidence: float?`, `model?` | `choice`, `probabilities`, `confidence: float`, `confident: boolean`, `model` | |
| `jev.score` | `state`, `instructions`, `levels: Any[]`, `model?` | `score: float`, `probabilities`, `confidence: float`, `legend`, `model` | |
| `jev.noul` | `state`, `instructions`, `yes?`, `no?`, `model?` | `noul: float`, `model` | |

Unmarked inputs and outputs are `string`; `?` marks optional ones. Unless noted, a builtin does
not retry. Builtin icons: a spark for the agent fns, a review mark, a fork for `decide.llm`, a
branch for `git.*`, a PR mark for `gh.*`, an arrow leaving a box for `core.external`, a bubble
for `message.post` and an envelope for `message.wait`.
