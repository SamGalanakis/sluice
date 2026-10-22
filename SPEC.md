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
  named resources, paused and archived flags, an optional board (an OpenUI Lang program the
  dashboard draws beside the plan, `docs("board")`) and an optional age after which its done
  units retire from the plan by themselves (§6.11). Each project has exactly one plan and its own
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
- **Message:** a row on a project thread: a question (`ask`), a note (`say`) or a `reply`, to
  a step, a session (`@name`) or the owner. Open questions to `owner` are the inbox (§8).
- **Identity:** who speaks and is addressed: a session names itself `@<name>`, the person at
  the dashboard is `owner`, a run is its step (§8.1). A unit's **owner** answers its runs.
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
                            the private tmux manifest, the build toolchain, and `schema`: the
                            database schema its sluice reads and writes (absent: 1)
```

The **launcher** execs `<install>/entry --installation-entry <install> <args…>`; the release
binary reads `selection.json` and execs the selected release's `bin/sluice` with `SLUICE_HOME`
set to the selected home and `SLUICE_INSTALL_DIR` to the control directory. A release binary
re-execs itself once with `SLUICE_PYTHON_DIR=<release>/python`,
`SLUICE_TMUX_PREFIX=<release>/tmux` and `PYTHONDONTWRITEBYTECODE=1`. Agent docs and dashboard
assets are compiled into the binary.

`sluice install <command>` prints the installation status as JSON
`{generation, selection, fence}` (`verify` prints the release it checked):

| command | effect |
|---|---|
| `install status` | read only |
| `install fence <reason>` | writes `fence.json` under the exclusive lock; while fenced, every coordinator activation and admission write is refused (`maintenance: <reason> (generation n)`), except a coordinator started with `--maintenance` |
| `install select <release_dir> <home>` | verifies the release's manifest (when it has one) and records the selection |
| `install unfence` | verifies the selected release and removes the fence |
| `install verify <release_dir>` | the check `select` runs (`release::verify`: every file's sha256, the release id, the guardian protocol, the private tmux), with no fence, no installation and no change; prints `{release, release_id, git_sha, schema}` |

A home that is not the installation's selected home refuses admission (`maintenance: stale
selected home`). Without a release and without `SLUICE_INSTALL_DIR` (a source build), the
installation is the sibling directory `<home>.sluice-install`.

**`scripts/build-release <prefix>`** runs `cargo build --workspace --bins --release --locked`
with its target dir under `<prefix>/.build/target`, stages `bin/sluice`, `python/`, the private
tmux (built once by `scripts/build-private-tmux` and cached under `<prefix>/.build`) and
`assets/`, asks the staged binary its schema (`sluice home schema`, which prints `{"schema": n}`
and needs no home), writes `manifest.json`, has the staged binary accept it (`install verify`),
moves the stage to `releases/<release_id>` (an existing release id must have identical
metadata), compiles the launcher into `<prefix>/bin/sluice` and prints the release path. The
release id is `<git sha>-<sha256>` of `files` as `release::verify` re-serialises the manifest's
map: compact JSON, keys in the byte order of their UTF-8 (so `a-b/x` before `a/x`, unlike path
order), `"`, `\` and control characters escaped, nothing else; a file name that is not UTF-8 is
refused. It refuses a prefix on `PATH`, `/`, or `~/.local/bin`.

**`scripts/deploy [REF] [--prefix DIR] [--skip-compat REASON]`** (REF defaults to `origin/main`)
installs one commit:

1. `git archive` the commit into a temporary source tree; a commit whose `SCHEMA_VERSION`
   differs from the home's `schema_version` is refused there, before anything is built (it goes
   out only with `--schema-cutover`, below), and so is a built release whose manifest `schema`
   differs; then run `build-release` in the tree;
2. `install verify <release>` with the release's own binary: a release it refuses stops the
   deploy here, before the fence, with nothing live changed (a binary from before `install
   verify` runs its `install select` on a scratch installation instead);
3. `scripts/compat-check` the new release: on a scratch copy of the home's database, with its
   coordinator cut off from systemd, every release an unfinished run is pinned to (plus the
   selected one and the candidate) runs `log_read`, `status`, `say`, a stale `step_submit` and
   `step_context`; a storage or schema error, a crash, a timeout or a submit not refused as
   stale stops the deploy unless `--skip-compat` gives a reason. Each deploy's outcome and any
   skip reason are appended to `<install>/deploy.log`;
4. `install fence "deploy <sha>"`;
5. stop the services recorded in `services.json` and the home's auto-started coordinator unit;
6. `install select <release> <home>`;
7. start three transient user units, `sluice-<sha16(install dir)>-coordinator`
   (`coordinator --maintenance`), `-serve` (`serve --no-runner --port 3065`, or
   `SLUICE_DEPLOY_PORT`) and `-loop` (`loop`), each with `SLUICE_HOME`, `SLUICE_INSTALL_DIR`,
   `PATH` and `HOME` set, recording them in `services.json`; after the coordinator it waits
   until the coordinator is ready to serve its socket;
8. check that every unit is active and run `sluice doctor --json`;
9. `install unfence`;
10. prune releases: keep the newest three plus every release a live process runs from or an
   unfinished run in the home records.

**`scripts/ship [REF] [--dry-run]`** takes a branch through its gate to a verified live deploy:
it refuses a dirty tree and a ref whose `SCHEMA_VERSION` differs from the selected home's
(checked before anything moves and again on the rebased commit before the push: ship ships
compatible changes only), rebases onto `origin/main`, then gates the rebased tree with
formatting, Clippy over every workspace target, fast tests, doctests and the slow groups
selected by the base..head diff. `scripts/gate` defines both this gate and the local
`scripts/check` command. No separate pre-ship gate is required. A push retry reuses passed
common checks only if the Git tree is identical; any newly selected slow group still runs.
A changed tree gates again.

The slow nextest profile runs only `slow_*` tests. The shared gate maps every slow test to
exactly one source-based group and refuses unmapped tests. Dashboard sources select browser
tests; plan compiler and plan-store sources select scale and cost tests; conversion, backup,
migration, release, install and deploy sources select durability; engine and execution sources
select execution tests. Shared fixtures, dependency graphs and gate machinery select all groups.
The gate prints selected groups and their triggering paths. A schema change confined to new
`ADDED_COLUMNS` or `ADDED_VIEWS` entries, with every other token including the schema version
unchanged, does not select durability. `scripts/check --slow` runs all groups.

Ship pushes with a bounded retry, deploys `origin/main` to the selected home, checks that every
run live before the deploy is still running or finished with a result, that the three units
are active and that the dashboard answers, and prints one line. A deploy that fails or rolls
back fails ship, which says that main is ahead of live: the push has already happened.

Shipping another ref uses a temporary checkout. On success or failure, ship reads its Cargo
`target_directory` with `cargo metadata --format-version 1 --no-deps` and removes that target
before removing the checkout. It refuses targets outside the shim's configured
`/workspace/.cargo-targets` root, the root itself, or any target overlapping the script or
invoking checkout's own Cargo target, and reports removals or refusals. Shipping the current
checkout removes no target. `--dry-run` prints the cleanup it would perform and removes nothing.

A failure stopping the old services (step 5) leaves the installation fenced and says so. A
failure after the stop, in steps 6 to 9, rolls back when the previous selection serves the same
home: deploy stops whatever it started, selects the previous release again unless the selection
still names it, re-creates the three services from it under the same unit names and arguments,
waits for its coordinator, unfences with its binary, checks that the dashboard answers 200 and
exits 3 with `deploy rolled back to <old sha>: <error>` (and a `rolled back` line in
`deploy.log`). Without a previous selection, or when the rollback fails itself, the installation
stays fenced and deploy prints the commands that finish the recovery by hand. Running steps survive a
deploy: each run's guardian stays pinned to its own release, and the new coordinator adopts it
(§7.9).

**Incompatible cutover**. A release whose schema
differs from the home's (its manifest's `schema`) is never deployed as above: no run pinned to
the old schema may outlive it. `scripts/deploy --schema-cutover --deadline <time> [--cancel-grace
<s>] [--settle-timeout <s>] [--dry-run] [--candidate DIR] [--rehearsal FILE] [REF]` (`<time>` is
RFC 3339 or `+<n>m`; grace and timeout default to 300 s; `--candidate` takes an already built
release instead of building REF, and `--skip-compat` is refused) runs from the release the
installation selects. `<N>` is the candidate manifest's schema, currently 4:

1. **Verify** the candidate with its own binary (`install verify`, as step 2 above); a refusal
   stops the cutover before anything is drained or fenced. **Rehearse** on a private copy of
   the home (`scripts/cutover-rehearse`, below), or take
   `--rehearsal FILE`, a passed rehearsal report of this candidate over the selected release.
2. Print the **notice** the orchestrator sends every session: `sluice cutover to schema <N> at
   <deadline>: new work is refused from now; runs still live at <deadline> get a cancel request
   ("<reason>") — check how each ended in the cutover report and retry it after the cutover.`
   The reason is `schema-<N> cutover at <deadline>: stopped at the deadline; retry it after the
   cutover`. `--dry-run` stops here, after listing each live run (project, step or `call:<id>`,
   run, start, pinned release, settle intent and what the deadline would do to it); it changes
   nothing.
3. **Drain** with the old release (`sluice drain --no-wait`, author `cutover`, §2.6) and wait
   until nothing is live or the deadline comes.
4. **Fence** (`install fence "schema-<N> cutover"`), starting the old coordinator with
   `--maintenance` if none answers, then request **one `step_cancel` per live (project, step)**
   with the old release (author `cutover`, the reason above) and **verify the intent**: every
   live attempt of each step carries `cancel_requested` (a step whose attempts all ended
   meanwhile, or whose live attempts all carry it already, as a settle leaves them, is no
   refusal). A refused cancel (the old release compiles the plan before it cancels, so a
   missing fn refuses it), or an attempt left without intent, stops the cutover still fenced
   and drained, stopping nothing; the report's `refused` names each project, step, run and
   attempt with the refusal.
5. **Settle**: after `--cancel-grace`, stop the transient unit of each cancelled run and each
   running call still live (by its recorded unit name, never a pattern); the old coordinator
   records each end through its own completion and adoption paths. Then wait, at most
   `--settle-timeout`, until no attempt, run, waiting or held lease or running call is left,
   else stop fenced. The report is written now, so a later failure still says how each run
   ended.
6. **Stop** the services and prove nothing of the home is left: no active unit of the home's
   coordinator or of any of its runs, and no process holding its database or writer lock.
7. **Back up** the database (backup API) to `<install>/backups/pre-schema<N>-<time>.db`, then
   **migrate** with the candidate's `sluice home migrate`. A failed migration leaves the home
   unchanged and stops fenced.
8. **Select and start** the candidate's coordinator and serve, deferring loop until step 9.
   Check integrity, every project's `plan_get` equal to the backup's plan at its revision
   (compact serialization, `docs/design/plan-rows.md` §10.5; normalized backups are exported
   from their ordered input, step and output rows), the first plan edit in `plan_history`
   at each project's baseline revision from the conversion report, or its first logged revision
   in the backup for an unanchored project, and the dashboard answering (`/` with 200, a project
   page without a server error). A failure stays fenced: recover forward, or
   restore the backup together with the old release. To recover forward after repairing a
   post-migration failure, run `scripts/deploy --schema-cutover --resume-checks`. It requires
   the `schema-<N> cutover` fence, the home and recorded candidate at schema <N>, and the recorded
   successful migration checkpoint at `<install>/schema-cutover-checkpoint.json`. It re-creates
   coordinator and serve and repeats these checks against the recorded backup. It then
   performs step 9, preserving the original deadline and run outcomes. No deadline or rehearsal
   is needed for this retry; a different candidate is refused.
9. **Unfence**, then **release** the drain (author `cutover`). Check coordinator, serve and loop,
   re-creating any inactive service with deploy's original launch arguments. Start loop here,
   after the fenced checks. Require all three services active and the loop's scheduler lease
   held (`maintenance.scheduler_owner` set) within 60 s, or fail fenced and restore the drain.
   This also applies to
   `--resume-checks`. Then **report**: one line `cutover
   <sha> · schema <N> · <n> projects · <m> revisions converted · cancel requested for <k> runs ·
   <c> calls stopped`, then a line per run, `stopped <project> <step or call:<id>> <run>
   requested=<cancel|stop> outcome=<succeeded|failed:<kind>> advice=<none|retry|read-then-retry|call-again>`,
   printed and appended to `<install>/deploy.log` (each as a JSON line's `cutover` field), and
   the whole report (`<install>/cutover-<deadline>.json`).

A cancel request does not decide how a run ends: a settled run still succeeds, a run that
finishes first keeps its own result, a scatter step keeps its first failure, a stopped call
ends `process_lost`. So each report entry gives the run's own outcome, its step's status and
error after it, and advice that follows the outcome: `none` for a success, `retry` for a step
failed `cancelled`, `read-then-retry` for any other failure, `call-again` for a call.

**`scripts/cutover-rehearse --candidate DIR [--old DIR] [--deadline <time>] [--home DIR]
[--harness BIN] [--keep] [--json]`** copies the home as compat-check does into `/tmp/cr.*` (no
route to the user service manager), builds the `tools/cutover-rehearsal` harness against a
checkout of the old release's commit (or takes `--harness`) and lets it play the deadline on the
copy with the old release's own code and a test adoption host: the drain, one `step_cancel` per
live step, the intent check and one adoption pass. Only once its own re-read finds zero
blockers does the candidate's `sluice home migrate` convert the copy and `scripts/compat-check
--incompatible --copy` check it. It reports what the play stopped (the cutover report's shape),
the conversion and the compat table, exits non-zero on any refusal, blocker or failure, and
removes the copy and the checkout unless `--keep`.

**`scripts/compat-check`** checks a candidate whose manifest `schema` differs from the home's
(or with `--incompatible`) in incompatible mode: no run in the copy may be live (a pin is an
error), the candidate's `sluice home migrate` converts the copy (unless it is already at the
candidate's schema), the candidate's coordinator serves it (`log_read`, `status`, `plan_get` of
every project, `plan_history`, `plan_read`, `step_context`), and the old release (`--old`,
default the selected one) must refuse it: its coordinator exits on the unsupported schema and its
`log_read` refuses, with the database bytes unchanged. `--copy DIR` checks a prepared private
copy instead of copying the home, and never removes it.

### 2.3 Home layout

```
config.json                 {"fn_dirs": [], "http": {"host": "127.0.0.1", "port": 3065},
                            "log_max": 10000}; written when missing, read on each use; a key
                            of the wrong shape is ignored (with a warning) for its default,
                            except execution policy, which fails closed (§2.5):
                            fn_dirs    directories (relative to the home) whose fn dirs join
                                       the global scope
                            http       serve's host and port when the command line gives none
                            log_max    records each log keeps; past it the coordinator trims
                                       the oldest down to 90% of it (every 30 s)
                            park_timeout_seconds  positive integer seconds, default 3600;
                                       bound for park when --timeout is absent (§7.6)
                            long_run_quiet_h  hours, default 24: how long a long run
                                       (the plan's Long runs group, §13) may publish nothing and show
                                       no activity before Needs you asks the owner to look
                            unread_alert_min  minutes; when set, the "nobody reading" owner
                                       question (§8)
                            notify     {command: [argv…], timeout_s: 30}: owner notification
                                       (§8)
                            execution  run defaults, ceilings and home-wide capacity, any of
                                       them unlimited (§2.5); malformed or unreadable policy
                                       blocks admission
.env                        secrets for every run in the home (§5.4)
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
engine-probes/<engine>.json the last capability probe of each engine CLI (§15), kept per
                            binary (canonical path, mtime, size, inode) and version
projects/<project id>/
  fns/                      the project's fns
  generations/<n>/          immutable published copies of the project's fns
  recipes/<name>.json       the project's recipes
  .env                      secrets for this project's runs, over the home's (§5.4)
  icons/<generation>        the project's image icon
```

### 2.4 Coordinator

The coordinator (`sluice coordinator`) owns the home: it holds `coordinator.lock`, opens the
single SQLite writer, serves `coordinator.sock`, publishes the fn registry, runs reconciliation
and, while some client holds the **scheduler lease**, admits and launches work.

- Any CLI command that needs it, `serve` and `loop` connect to the socket and, when nothing
  answers, start the coordinator as the transient user unit `sluice-coordinator-<sha16(home)>`
  (`systemd-run --user --collect --service-type=exec -p Restart=no`) and wait up to 10 s for
  its socket. Activation is refused while the installation is fenced.
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
`Delegate=yes`, `OOMPolicy=continue`, `KillMode=control-group` and `Restart=no`. Inside it the **guardian**
(`sluice guardian --run --attempt --socket`) proves its identity to the coordinator, splits the
unit's cgroup into `control` (itself) and `payload/<invocation>` leaves, starts the payload
through the `payload-exec` launcher, serves the run's control socket (callbacks, engine hooks,
delivery acknowledgements), holds one watch on the coordinator for cancellation and messages,
and reports the start and the completion until the coordinator acknowledges them. A payload is
a Python fn (§5.4), an agent session in the private tmux, or a built-in fn.

A guardian outlives a coordinator that is busy or restarting, retrying it less and less often
(up to every 5 s), but not one that is gone for good: once nothing has listened on the
coordinator's socket for ten minutes, or its home no longer exists, the guardian stops, and its
unit's stop ends the payload. A coordinator that comes back later finds the run `lost` (§7.9).
A guardian reads its run's resource accounting once a second; an OOM kill ends the payload
whenever it happens.

An agent's supervisor acknowledges each live message its engine accepted to the guardian, which
answers once the acknowledgement is durable in the run's `delivery.json` (a redelivery after a
restart skips it). The guardian tells the coordinator afterwards, beside its loop and again
until the coordinator takes it, and the completion carries every acknowledgement, so a slow or
stalled coordinator never holds the answer.

The guardian hands each engine hook to the agent supervisor through the run's
`engine-hooks/` journal and waits up to three seconds for its decision. The journal keeps only
the hooks in flight, however many a run has: the supervisor removes a hook's request once its
reply is durable, the guardian the reply once it has read it, and each side clears what a crash
left on its next pass. Past 1024 hooks in flight a hook is refused.

Stopping a run sends TERM to its processes, waits five seconds, then kills the payload cgroup
recursively and proves it empty before any resource it held is released. The guardian, not the
coordinator, owns the payload: a coordinator restart or a deploy leaves running payloads alone.

Host prerequisites (cgroup v2, a systemd user manager with delegation, `pidfd_open`, a boot id)
are checked by `sluice doctor`; see `docs/rust/host-prerequisites.md`.

#### Execution budgets

Every run that starts a local executable has frozen `RunLimits` caps:
`{memory_max_bytes, cpu_millis, tasks}`. One CPU is 1000 milli-CPUs; tasks include threads.
Any dimension may be unlimited, stored and passed as `null`; memory only by an explicit opt-in
(below). This covers steps, each scattered item, direct and queued calls, and capacity-function
calls. Pure `core.*` transformations and an empty scatter settle in the coordinator without a
payload executable. `core.external` starts no run.

Caps are kill limits only, never a reservation: nothing counts a run's cap at admission.
`memory_max_bytes` is the payload's `memory.max` (and the service's `MemoryMax` with the
guardian's share); a run that reaches it is reclaimed and, past it, OOM-killed. The shipped
per-run defaults set no CPU quota and no task cap. Admission is by measured memory and CPU
(below), and the home total caps the home's workload slice as a whole. The caps are stored with a
`memory_bytes` field equal to `memory_max_bytes`, which releases pinned before caps alone read;
a budget they froze is read with its own `memory_max_bytes` (or, from before unlimited values,
its `memory_bytes`).

The human-facing override is `limits: {memory?, cpus?, tasks?, memory_above_ceiling?}`. Memory
accepts positive integer bytes or a positive integer with `KiB`, `MiB`, `GiB` or `TiB`; CPUs are
a positive number with at most three decimal places; tasks are a positive integer. `null` makes
a field unlimited; the string `"unlimited"` is accepted as the same value and stored as `null`.
A field left out inherits. Unknown keys, zero, overflow and other spellings are refused.
Resolve each field from home defaults, then project defaults, then the step or call override,
within the installation's per-run ceilings: an unlimited value fits only an unlimited ceiling.
The run's memory cap is the memory that comes out. A step or call may ask for a memory cap above
the ceiling only explicitly: it sets `memory_above_ceiling: true` with its `memory`, and its
project's limits must set `memory_above_ceiling: true` too (which allows the request and never
raises the project's own memory above the ceiling). Even then the cap plus the guardian's share
must fit the home memory total. Any other request above the ceiling is refused when the step is
added or edited (or the call is made) with an error naming the ceiling and both flags; there is
no home-wide bypass. `project_create(limits=...)` sets project defaults;
`project_update(limits=...)` replaces them and null clears them. Steps and recipes declare
`limits`; `step_update` or `plan_edit` can replace that field or remove it with null.
`fn_call(limits=...)` applies an override to that call. A running step cannot change its work,
including `limits`. Releases pinned before unlimited values read a `null` field of an override
as inherited.

`config.json` owns the policy under `execution`:

```json
{"execution": {
  "total": {"memory": "96GiB", "cpus": 24, "tasks": 65536},
  "ceilings": {"memory": "10GiB", "cpus": null, "tasks": null},
  "defaults": {"memory": "6GiB", "cpus": null, "tasks": null},
  "host_reserve": "16GiB",
  "memory_pressure_max": 10,
  "memory_notice_working_percent": 90,
  "memory_notice_pressure_percent": 10,
  "admission_charge": "1GiB",
  "admission_warmup_s": 30,
  "cpu_pressure_max": 30,
  "load_per_cpu_max": 1.5,
  "warmup_starts": 2,
  "cpu_weight": 20
}}
```

`defaults` are each run's caps, `ceilings` the largest caps a step may ask for without
`memory_above_ceiling`, and `total` the workload slice's caps. Each accepts the same three
optional fields, each a value or unlimited; `guardian_memory`, `host_reserve` and
`admission_charge` accept the memory syntax; `memory_pressure_max`, `cpu_pressure_max`,
`memory_notice_working_percent` and `memory_notice_pressure_percent` are
percentages above 0 and at most 100; `admission_warmup_s` is a number of seconds, 0 or more;
`load_per_cpu_max` is a number above 0; `warmup_starts` is an integer, at least 1; `cpu_weight`
is an integer from 1 to 10000. Memory is unlimited at a
level only with `"allow_unlimited_memory": true`: without it, a `null` (or `"unlimited"`)
memory in `defaults`, `ceilings` or `total` is refused at load with an error naming the key,
the size to set instead and the opt-in, so no run is ever uncapped by accident. Omitted fields
use the defaults below. Explicit values must fit the host and each other; a smaller finite
total clamps inherited ceilings and the shipped admission charge, a finite ceiling clamps
inherited defaults (an inherited unlimited default becomes that ceiling), and an explicit value
that does not fit is refused. The host's effective capacity is MemTotal and the CPU count,
lowered by the nearest `memory.max` and `cpu.max` among the process's cgroup and its ancestors
(for a user service, typically `user.slice`); a total above it is refused with an error naming
that cgroup and its limit, e.g. `execution.total.memory (104 GiB) exceeds the host's effective
memory: cgroup /user.slice has memory.max 96 GiB; …`. A malformed policy, invalid JSON or an
unreadable configuration is an error at load: the coordinator does not start on it, and when
the file goes bad while it serves, no step starts and `status` shows each ready step `held:
invalid execution config (…)` and `execution.error`. A missing file or missing `execution`
section selects the defaults below.

| Setting | Default |
|---|---|
| Home total (the workload slice's caps) | min(70 GiB, 55% of effective host memory), the rest left to agents, editors and services; 75% of the effective CPUs; 65536 tasks |
| Payload cap per run | 6 GiB memory, clamped to the ceiling; CPU and tasks unlimited |
| Payload ceiling per run | 10 GiB memory, clamped to the home total minus guardian memory; CPU and tasks unlimited |
| Guardian share per run (added to the service's caps) | 256 MiB or one eighth of total home memory, whichever is smaller; 0.1 CPU; 64 tasks |
| Host reserve | max(16 GiB, 15% of effective host memory), at most a quarter of it |
| Memory pressure limit | PSI `some avg10` 10% |
| Run memory notice | Working memory at least 90% of its cap, or payload PSI `some avg10` at least 10%; OOM always warns |
| Admission charge | 1 GiB, at most the home memory total |
| Admission warm-up | 30 s |
| CPU pressure limit | PSI `some avg10` 30% |
| Load limit | 1-minute load 1.5 per online CPU |
| Starts per warm-up while CPU is over half a limit | 2 |
| Workload slice CPU weight | 20 (systemd's default is 100) |

**Admission by measured memory and CPU.** Before each admission the coordinator reads
`/proc/meminfo` MemAvailable, `/proc/pressure/memory` and `/proc/pressure/cpu` `some avg10`
(absent on a kernel without PSI), the 1-minute load from `/proc/loadavg`, the online CPUs from
`/proc/stat` and, for each live run with a payload, its working memory (`anon + shmem +
slab_unreclaimable` from `memory.stat`), charged current memory and kernel peak. A run starts
only while all six hold:

1. **host memory:** MemAvailable less `host_reserve`, less the charges of runs still in their
   warm-up, covers the admission charge;
2. **memory pressure:** memory `some avg10` is below `memory_pressure_max`;
3. **pool memory:** the live runs' measured memory plus the admission charge stays within
   `total.memory`;
4. **cpu pressure:** CPU `some avg10` is below `cpu_pressure_max`;
5. **cpu load:** the 1-minute load divided by the online CPUs is below `load_per_cpu_max`;
6. **cpu warm-up:** while CPU pressure or load per CPU is at or over half its limit, fewer than
   `warmup_starts` runs are still in their warm-up, so a burst of ready steps starts a few at a
   time and its load shows in the readings before more start.

The admission charge is an internal estimate for a run not yet measured. A run counts at the
charge (or its reading, if larger) until it has run for the warm-up, measured on the readings'
clock from its admission, and from then on at its measured working memory (falling back to
`memory.current` only for readings without composition). MemAvailable already treats
reclaimable cache as available, so both checks exclude it consistently. A run with no
payload yet counts at the charge, and one whose cgroup cannot be read counts at its cap. Runs
admitted in the same pass count too, so a burst of launches is admitted in waves the charge
bounds, never all at once on one reading. A run that fails a check is held, not refused:
`status` shows the step with the check that held it and its numbers, from the coordinator's last
reading: `held: host memory (available 9.1 GiB, reserve 16 GiB, starting 2 GiB, need 1 GiB)`
(`starting` only while runs are in their warm-up), `held: memory pressure (some avg10 13.8%,
limit 10%)`, `held: pool memory (measured 66.2 GiB + 1 GiB > 70 GiB)`, `held: cpu pressure
(some avg10 74.0%, limit 30%)`, `held: cpu load (1-min load 590.0 on 32 CPUs, 18.44 per CPU,
limit 1.5)` or `held: cpu warm-up (2 runs started within 30 s, limit 2 while cpu pressure
20.0%, over half of 30%)`. A step's `waiting` resource is `host memory` or `host cpu`. Its unit's
owner is told once per hold (a limit notice with `limit: "host_memory"` or `"host_cpu"`), and
the scheduler checks again
every 5 s and starts it when the host recovers. A queued call stays pending and a direct call
is refused with a retryable `busy` naming the hold. An unreadable `/proc/meminfo` holds
admission. Scatter admits the items that pass and continues as memory frees; each item is
admitted separately while the step holds its logical `needs` once. Physical admission is
independent of logical resources: `lane` is an ordinary resource name, and omitting `needs`
never bypasses the checks. `status` reports `execution.total` and `execution.memory`
(`total_bytes`, `measured_bytes`, `starting_bytes`, `available_bytes`, `pressure_some_avg10`,
`host_reserve_bytes`, `pressure_max`, `admission_charge_bytes`, `admission_warmup_s`),
`execution.cpu` (`pressure_some_avg10`, `load_avg1`, `cpus`, `load_per_cpu`, `starting_runs`,
`pressure_max`, `load_per_cpu_max`, `warmup_starts`, `cpu_weight`), and each
of a step's runs in `run_resources` with `memory: {cap_bytes, current_bytes, peak_bytes,
working_peak_bytes, working_bytes, file_cache_bytes, pressure_some_avg10}`: its cap, total charged
memory, kernel peak including file cache, sampled peak working memory, current working
memory, reclaimable file cache and payload PSI while live. Working memory is `anon + shmem +
slab_unreclaimable` from cgroup v2 `memory.stat`; file cache is `file - shmem`. Thus tmpfs/shared
memory counts as working memory rather than reclaimable file cache. Unknown readings are null;
older published readings may lack composition. `current_bytes` and `peak_bytes` include cache;
`working_peak_bytes` is the maximum working-memory sample retained by admission or the guardian,
not an exact kernel high-water mark. Sampling can miss brief spikes. The Runs tab leads with
“peak working N GiB” and shows “incl. cache M GiB” faintly; older runs say working unknown.

The effective caps and policy are frozen with the attempt before any payload is launched (the
policy as `request.execution_policy`). A retry inside that run shares its existing caps. New
runs, including later scatter items, resolve the policy at their own admission. An attempt is
live work until its entire service, including its guardian, is proven empty; logical leases can
settle after payload cleanup. The coordinator reaps a terminal guardian left waiting for an
acknowledgement before it ends that live work; a durable result never allows unaccounted
guardian processes.

A policy change needs no drain. The scheduler notices a changed `config.json` within seconds
and re-admits every project at once. The home's workload slice carries the total as
`MemoryMax`, `CPUQuota`, `TasksMax` and `MemorySwapMax=0`, and `cpu_weight` as `CPUWeight`; the
coordinator applies them when it starts, and each launch applies them with `systemctl --user
set-property --runtime` when they differ and reads them back, and a slice that does not keep them
fails the launch. CPU weight only orders siblings, so at start the coordinator also sets
`CPUWeight` on the slices the home's slice is nested in (`sluice-workloads.slice` and
`sluice.slice`): under contention the user's other work (agents, editors, Sluice's own services)
is scheduled ahead of the workloads, and `total.cpus` keeps the rest of the CPUs free of them. Existing runs keep their frozen caps; changes to run
defaults and ceilings affect future admission only.

Launch, adoption and terminal cleanup share a per-run process fence. A launcher rechecks the
attempt under that fence; an attempt already ended cannot start a late service. The start
helper retains the fence after caller cancellation or coordinator death. An interrupted start
with no confirmed manager outcome stays live work and reports that capacity is retained.
An absent unit alone does not clear it: observing the service or a changed host boot ID resolves
the pending start. No timeout turns uncertain launch evidence into free capacity.

The run's cgroup has `control` for the guardian and `payload` for its combined workload.
`memory.max`, `memory.swap.max=0`, `cpu.max` and `pids.max` are set before the launch barrier
opens; an unlimited dimension is written `max` (and the service's systemd property
`infinity`, or no `CPUQuota`). The service is started with `MemoryMax`, `MemorySwapMax=0`,
`CPUQuota` and `TasksMax` from the frozen budget plus the guardian's share, and the properties
are logged. After its claim and before its first payload, the guardian reads them back, from
systemd (`MemoryMax`, `MemorySwapMax`, `TasksMax`) and from the kernel's files in the service
cgroup (`memory.max`, `memory.swap.max`, `pids.max`, `cpu.max`) and the payload's
`memory.max`, and compares them with the frozen budget. On a mismatch it applies them again
with `systemctl --user set-property --runtime` and reads them back once more; if they still
differ, the run fails with `fn_failure` naming each mismatch and no payload ever starts. Invocation and composed-agent sidecar leaves share the payload parent's allowance.
The guardian has its own control share. A per-home workload slice bounds the
aggregate; coordinator, serve and scheduler services stay outside that slice. `doctor` checks
that the required memory, CPU and task controllers can enforce the settings. Missing
controllers or failed setup refuse execution.

A payload memory OOM stops the workload as a group (`memory.oom.group=1`). The guardian
survives in `control`, stops and reaps the remaining payload, and reports `resource_exhausted`
with `resource: "memory"`, the numeric `limit` (the memory cap, or 0 when the run had none and
the home total ran out), optional `peak` (sampled peak working memory), and a message naming
the cap, peak working memory (unknown for older evidence) and peak including file cache, saying
to ask for more explicitly with a larger `limits.memory` (and `memory_above_ceiling` above the
ceiling). A task-limit hit is
not fatal: the fork or thread start fails with `EAGAIN` and the run continues (releases pinned
before this stop the run with `resource: "tasks"`). CPU quota throttles execution; it is not a
fatal condition. Exhaustion is established by kernel counters, never by `SIGKILL` or an exit
code alone, and is not automatically retried. Completion keeps `RunUsage` with
`memory_working_peak_bytes` (the largest guardian or admission sample, absent in older usage),
`memory_peak_bytes` (kernel `memory.peak`, including file cache), `oom_kills`,
`pids_limit_hits` (failed forks summed over the payload's
leaves, the most any sample saw), `memory_max_hits` (`memory.events` `max`) and
`cpu_throttled_usec`; ordinary cancellation and settlement rules still decide an already
accepted cancel or settlement. Resource accounting is an observation: read failures retry
three times with exponential backoff, then record usage as unknown (`null`) and leave the
payload running. Subsequent samples back off up to 64 s and recover when reads work again.
A missing final sample never changes a successful payload result. The guardian warns in the
run log and says once to its unit's owner per observation category. Limit enforcement and
cleanup still require their own kernel evidence; unknown counters do not imply exhaustion.

Before dispatch is granted, a failed fn start stays queued with exponential backoff (100 ms
to 30 s), while cancellation remains responsive. Retrying requires both an empty payload
subtree and no recorded continuation grant. After the launcher's Hello, waiting for the
grant or its acknowledgement has no scheduling deadline: EOF or cancellation ends that
wait, so delayed host setup never invalidates an otherwise healthy launch. A limit-readback timeout also queues dispatch until verification can complete; a definite
limit mismatch still refuses dispatch. An uncertain
accepted start is reconciled, never replayed. A deferred start is logged and told to its owner once.

**Limit notices.** Hitting a limit is told to whoever must act on it. Every 5 s the scheduler
reads each executing run's payload counters from its cgroup's event files (`memory.events`
`oom_kill` and `max`, `pids.events` `max`, `cpu.stat` `throttled_usec`); completion carries the
final counters. Live samples also read `memory.stat`, `memory.current` and `memory.pressure`.
`memory.events max` counts reclaim attempts, including healthy file-cache reclaim; it remains
in `usage.memory_max_hits` and alone never triggers a notice. A live memory notice requires
working memory at least `execution.memory_notice_working_percent` of the frozen cap (default
90%), a sustained stall measured by payload PSI `some avg10` at least
`execution.memory_notice_pressure_percent` (default 10%, averaged over ten seconds), or an
actual `memory.events oom`/`oom_kill` event. These thresholds are read from the host's current
policy; they apply without a restart and are independent of admission thresholds. If policy
is invalid, admission holds and live notices use the default thresholds, so configuration
errors cannot hide OOM kills or failed forks. An
unlimited run has no working-memory percentage trigger; PSI and OOM still apply. Missing
memory composition or PSI is unknown, never inferred from `max` or an old peak. An OOM kill
still warns even if the live memory reading is unavailable. The notice states working memory,
cap, file cache, sampled peak working memory, peak including file cache, PSI and OOM events,
without predicting a kill from cache reclaim.
When a run has failed forks, has real memory pressure as above, was OOM-killed, or
has lost at least 60 s to its CPU quota, and when a ready step is held for memory or CPU, Sluice posts a `say` from `sluice` (§8) to whoever answers the
run (its unit's owner, a call's session, else the project's fallback, else the person) and,
while the run is live, the same `say` to the step itself, so its agent reads it in its inbox.
Its `data.execution_limit` is `{run, step, limit, value, observed, outcome, since?}`: `run` is
null for a held step; `limit` is `memory`, `tasks`, `cpus`, `host_memory` or `host_cpu`;
`value` is the limit (`"unlimited"` when none), for `host_memory` `{host_reserve_bytes,
memory_pressure_max, admission_charge_bytes, total_memory_bytes}` and for `host_cpu`
`{cpu_pressure_max, load_per_cpu_max, warmup_starts, admission_warmup_s}`; `observed` is the
counters (`peak_bytes` (including cache), `working_peak_bytes`, `current_bytes`,
`working_bytes`, `file_cache_bytes`,
`pressure_some_avg10`, `oom_events`, `oom_kills`, `max_events`, `failed_forks`,
`throttled_usec`; a live memory notice also includes `working_percent` and `pressure_percent`) or, for a
held step, the hold as status says it; `outcome` is `killed`, `running`, `throttled`,
`finished` or `held`; `since` is when a held step began to wait. Each run and limit is told
once, and each hold once.
It is a message, not a new record kind, so releases pinned before it read the log unchanged;
the notice wakes its addressee's `next` (§10).

Containment covers the payload and its local descendants. A request sent to an existing
build daemon, a remote executor or a separate user service runs in that executor's resource
domain. These limits are not a security boundary against code with the owner's permissions
moving work to another service or cgroup. Nested local wrappers must preserve the run's
payload ancestry; independent executors need their own limits.

### 2.6 Maintenance modes

The home has one maintenance mode: `normal` or `drain`.

- **drain** (`drain` tool, `sluice drain`): pauses the selected projects (default every
  project not archived) that are not paused already, records them and the drain's author as
  owner, and rejects new plan work and user calls home-wide (`busy`, "drain rejects new plan
  work and user calls"): plan edits, retries, input sets and `fn_call`. Running steps and calls
  finish. Draining again with another author is a `conflict`. `release` unpauses exactly the
  recorded projects and returns to `normal`.

A schema cutover (§2.2) drains with a deadline:
work still live at the deadline has its cancel requested and its unit stopped, not waited for.

## 3. Storage

`sluice.db` is SQLite in WAL mode. Only the coordinator writes, through one writer task; reads
use a pool of read-only connections and one snapshot per answer. Every logical change (an edit
and its records, a status change and its records, a message and its record) commits in one
transaction.

Everything below describes schema 4. Execution budgets require a drained schema cutover
from schema 3; an old guardian cannot acquire the new contract by changing its metadata.
A schema-1 home (the releases before the plan-rows cutover) has 23 tables, the plan stored whole as a document in `plans` and edits as RFC 6902
`ops` in `plan_edits`; `sluice home migrate` converts it, and no schema-4 release opens it
before then.

**Schema 4.** A fresh home is created from `migrations/0004.sql`, which extends the
normalized plan layout with execution-budget columns. Projects and queued calls keep
optional limit overrides; attempts keep frozen payload caps and a `reservation` column that
marks a service not yet proven gone (written in the shape releases that admitted by reservation
read; its numbers are read by nothing else).
A binary refuses a home at any other schema: a schema-4 binary requires migration of an
older home (`sluice home migrate`), and an older binary refuses a schema-4 home. Incompatible storage changes take a new schema
version and a drained, fenced migration with no live consumer of the old schema (§2.2); schema 2 is reserved for the
historical interim board layout. Additive changes use version-scoped migrations. `sluice home
migrate` upgrades a drained schema-3 database without changing its normalized plans, history
or completed work. It adds the budget columns and advances the schema in one transaction.
It also converts a schema-1 or schema-2 database directly to schema 4 in one transaction
(nothing changes on any failure): it refuses while an attempt, run, lease or call is live, replays every project's whole
edit history into row changes and checks each revision's rows against the replayed document,
checks the final rows against the stored document, rewrites retained `plan.edit` records and
removes the plan snapshots stored in attempts. A project whose history is incomplete (a
revision missing, or a rev 1 its creation did not make) blocks the conversion by name and
revision; it is never collapsed into a new baseline. The exception is a project whose logged
history starts above rev 1 (the Python importer's): it is anchored at the earliest revision
whose completion snapshot replays through the later logged revisions to the stored plan, or,
with one logged revision and no such snapshot, at the stored plan. That revision becomes an
imported baseline (author `sluice`), the revisions at or below it are folded into it, and the
report lists each anchor with its folded edits. A table whose columns a home gained in
another order is rebuilt in the normalized layout's order with the same rows. It holds the home's
`coordinator.lock` while it runs (§14), and its report (`--json`) is `{from_schema, projects:
[{project_id, name, revisions, steps, inputs, outputs, records_rewritten,
attempt_snapshots_removed, anchor}], anchored: [{project_id, name, rev, folded_edits, source}],
warnings}`. A restore of a schema-1, schema-2 or schema-3 backup
converts its private destination the same way, with the same refusal: a backup holding live
work is refused (`invalid`, `this backup holds live work (…); a schema-changing restore needs a
backup taken with nothing live`), because the restored home's adoption would reach the original
home's units by their recorded names.

The plan is stored as rows, never as a document: `plans` (`rev`, `root_order`: the present root
sections in order, `state_epoch`), `inputs` (each plan input's declaration exactly as written,
its position and its runtime `value`), `plan_outputs` (each plan output's binding as written and
its position) and `steps` (each step's declaration as written, its position and its runtime
state). Positions order each collection; a new row is appended after the last, a removed one
leaves a gap. `steps.unit`, `step_tags` (one row per step tag), `plan_refs` (every binding, gate
and plan-output reference, by consumer and slot) and `plan_edges` (the dependency graph, unit
gates expanded to the unit's exit steps) are indexes the writer keeps in step with the
declarations in the same transaction (a removed step's or output's references go with it);
`verify` rebuilds and compares them. `steps.paused`, `steps.run`, `steps.priority` and
`steps.needs` are copies of the declaration's fields that a `CHECK` holds equal to it, so the
compact reads use covering indexes and never read a declaration. `plans.state_epoch` grows
whenever a step's status or results, a plan input's value, the project's pause or its resources
change. `plan_get` assembles the document from the rows.

Tables: `home_meta`, `projects`, `plans`, `plan_edits`, `plan_outputs`, `plan_refs`,
`plan_edges`, `step_tags`, `inputs`, `steps`, `attempts`, `runs`, `submissions`, `calls`,
`step_results`, `resources`, `leases`, `messages`, `question_attachments`,
`message_deliveries`, `readers`, `records`, `change_versions`, `maintenance`, `artifact_jobs`,
`sessions`, `notification_attempts`.

A board's document is the columns `projects.board_doc` (its markdown), `board_doc_rev` (0 before
the first write), `board_doc_at` and `board_doc_author`; automatic retiring's setting (§6.11) is
the columns `projects.prune_done_after` (seconds, null when off) and `projects.prune_keep` (a
JSON array of patterns, null when none).

A step's row also carries its progress (§6.4): `progress` (the JSON object of latest values),
`progress_at` (when it was last set) and `progress_run` (the run that set it), all null until a
run publishes some and cleared when the step's next run starts. The additive `progress_runs`
column remains in the schema for compatibility, but is no longer read or written. `steps` is
readable by `query`
as it is, so `SELECT json_extract(progress, '$.red'), progress_at FROM steps WHERE project_id =
? AND step_id = 'tests-main'` reads a rolling step's latest count.

A step's row carries its unit's owner in `owner`, and a project's row the `fallback` that answers
its unowned units (both added columns, null until set; §8.1). Neither is part of a declaration.

A step's row carries its bypass preset (§7.7) in `bypass` (an added column, null when none):
`{outputs, author, reason, at}`, and once the runner applies it `applied: {at, result}`. It is
runtime state, never part of the step's declaration: the plan's export, its history and the
plan a pinned release compiles never see it, and a release from before the column ignores it.

A run's row keeps who stopped or retried it, and why, in `stopped` (an added column, null until
one does): `{"cancel": {author, reason, at}, "retry": {author, reason, at}}`. `step_cancel`
writes `cancel` on the runs it stops (on a failed step, on its latest run of each item), `step_retry` writes `retry` on the step's latest run of
each item, each beside its `step.cancel` or `step.retry` record, so the step's page still says
who and why once the log has trimmed the record (a run from before the column reads the
record).

Views for agents' queries (§12.4 `query`): `outcomes` (removed steps' results), `log` (each
record as the log tools return it), `step_changes` (`step.status` records as rows), `edits`
(`plan_edits`: `project_id, rev, seq, at, author, reason, changes`), `questions` (the questions,
`ask`s, plus derived `state` open|answered|closed and `waiting`). A step's tags are rows of
`step_tags`, so `EXISTS (SELECT 1 FROM step_tags t WHERE t.project_id = s.project_id AND
t.step_id = s.step_id AND t.tag = 'x')` selects the steps tagged `x`. Public tables are keyed by
the immutable `project_id`, never by name.

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
changed; each publication is an immutable generation that runs are pinned to. Cached scans
use a fingerprint taken before reading the fn files, so a change during a scan causes the
next registry access to rescan. If a publication's pinned manifest differs from the scan,
the attempt returns `conflict` and clears the cached scans so the next attempt reads fresh
files.

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
  Secrets come from `.env` files, loaded at each run's launch: `<home>/.env`, then
  `<home>/projects/<id>/.env`, each over what came before, then the `SLUICE_*` variables above
  over both. A line is `KEY=value` (`export ` prefix, `#` comments and one pair of quotes
  allowed); other lines are skipped and reported by `verify`. Built-in fns and agent sessions
  get the same values (an agent session over its engine's allowlisted environment). Values are
  never logged or recorded.
- stdin: one JSON envelope `{"protocol": 1, "inputs": {...}, "context": {...}}`. `context`
  holds `home`, `run_dir`, `project_dir`, `project`, `project_id`, `step`, `run_id`,
  `attempt_id`, `invocation_id`, `fn_dir`, `prev_run`, `extra_inputs` (an open fn's
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
  (raises `ShError` on a non-zero exit when `check`; output that is not UTF-8, such as a
  binary file, is decoded with replacement characters, as `stream` does); `stream(argv, on_line, …)` runs one and
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
  `call_status`; `step_wait`; `step_progress` (its own step's); `ask`, `say` and `reply`, `message.ask`, `message.say`,
  `message.reply` and `message.wait` (as the step, with its run); and the project's
  mutations (`project_update`, the edit tools, retry, cancel, manual values). `args` are the
  tool's flat MCP arguments (§12.2) and the result is the tool's MCP result (`{"ok": true}` for
  an acknowledgement); an unspecified author is `step:<step>`. Another project, or any other
  tool, is refused (`conflict`).
- `ctx.builtin(name, inputs)` runs a builtin fn inside this run (same run and attempt, a new
  invocation) and returns its outputs.
- `ctx.submission()` returns the run's submission, if any; `ctx.submit(outputs)` submits the
  step's declared outputs, once (§6.4).
- `ctx.progress(**fields)` (or `ctx.progress({...})`) publishes the step's latest values
  while it runs, through `ctx.tool("step_progress", …)`, and returns its reply (§6.4).
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

A new project's plan is `{"inputs": {}, "outputs": {}, "steps": {}}` at rev 1 (a
schema-1 home's was `{"steps": {}}`). Project names, step ids, plan
input and output names match `^[a-z0-9][a-z0-9_-]*$`; a project name may not look like a UUID;
`owner` and `orchestrator` are not step ids. Plan inputs and steps share one namespace.

Step keys: `run` (the fn), `in` (bindings), `scatter`, `doc`, `outputs` (declared, open fns
only), `paused` (`true` or a reason string), `after` (gate entries), `tags`, `needs`,
`priority`, `limits` (§2.5) and `restart` (§6.1.1). Plan outputs are `{"source": "<ref>"}`.

### 6.1.1 Restart policies

A step may declare `restart: "on_engine_error"` or `restart: "always"`; omission disables
restarts. `step_update` can replace or remove it while the step is running. `on_engine_error`
retries engine failures (quota exhaustion, operation/wall/stall/turn-start/readiness deadlines,
unknown acceptance, engine exit or transient/cleanup failure) and lost guardians/processes.
It never retries output validation failures, an exit without submitting, authentication/setup
failures, or cancellation. `always` also retries success and ordinary work/output failures.
A cancellation stops either policy, including while waiting after success; a manual retry
re-arms the step. Manually set outputs and skipped steps never restart.

Restarts use `step_retry` with the predecessor run, resuming the recorded engine session when
available. They deliver feedback naming the policy, count and previous result; uncertain input
is never replayed from the failed run. Admission, pauses, gates and resource limits still apply.
A restart waits at least 10 seconds after completion, doubles the delay per restart in the
rolling hour up to five minutes, and permits at most six automatic restarts per rolling hour.
A usage limit's parsed reset plus its two-second margin is an additional minimum.
An automatic restart resets only its step; it does not re-arm failed dependents. An unknown
reset uses the backoff and hourly budget. The scheduler rechecks waiting restarts on its normal
full sweep (at most 30 seconds later). Counts and the hourly budget survive coordinator restarts.
The steps status view includes `restart`, `restart_count` and, when waiting, `restart_at` (Unix
seconds). Each units status row includes a `restart` map by step with policy, count and next_at.

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
and item count preserves successful items and retries failed or not-yet-admitted items.
A scattered step holds its `needs` once, but every item is admitted by measured memory on its
own (§2.5). Items start only as the host's memory permits; a large scatter does not start every
local process at once.

### 6.4 Open fns: extra inputs, declared outputs, submission

A step whose fn is open may bind extra inputs (any id-shaped name) and declare `outputs`
(`{name: type | {"type", "doc"}}`, none named like a fn output); `submits` from fn.json join
them. Refs to declared outputs validate like any output. While the step runs, whoever does the
work calls `step_submit(project, step, run, outputs)`: checked against the declared outputs
(every required one, fitting types, no others; `invalid` lists each mismatch and nothing is
stored), refused unless the run is current. A valid submission is a `step.submit` record and
the agent's done signal: its supervisor stops the session at once (§15), so the agent
invocation returns its result (`session`, `final`, `git`) straight away. A step that runs an
agent fn itself then completes with it; a fn that composes one (`ctx.builtin("agent.run")`)
gets that result back, finishes, and completes the step with what it returns. Either way the
run's completion is the step's result: the fn's outputs, with the submission joined in (a
returned value wins), validated against the fn's outputs and the declared ones. A run submits
once: a second submission is refused (`conflict`). A run that ends without submitting a
required declared output (and without returning it) fails the step with
`exited_without_submit` (`session` when the agent's is known), unless it failed otherwise
first.

A running step whose current runs have all stored a valid submission is **finishing** until
its run ends: its agent's work is done, messages to it are refused (§8) and so are a second
submission and `step_set_output`. `status` (both views), `step_context` and `call_status`
(for a call's run that has submitted) show it as `finishing: {since, submission_seq, release}`:
the time the (latest) submission was stored, its `step.submit` record's seq (null once the log
has trimmed it), and the run's pinned release in short form (a `<git-sha>-<sha256>` release id
as its git sha's first 12 characters; any other id whole). With the done signal finishing
lasts as long as stopping the session takes. A run whose guardian is pinned to a release from
before the done signal does not end its agent on submit: that supervisor waits until the agent
has been idle through its grace, so the step can stay finishing for a long time; `step_settle`
(§7.5) settles it on its submission. No `grace_until` is shown: when the grace started (the
agent's last turn going idle) lives only in that supervisor's memory, and the grace itself in
its environment, so sluice cannot compute it honestly.

**Progress.** A running step that never finishes (a rolling test run of main, say) still has
results worth showing. Its current run publishes them with `step_progress(project, step, run,
outputs)` (`ctx.progress(**fields)` in a fn; in a run `project`, `step` and `run` default to
its own, §14): each field is one of the step's outputs (its fn's or declared) and must fit its
type; an unknown field or a value that does not fit is `invalid` with each problem, and
nothing is stored. Fields merge over the run's earlier progress. It is refused (`conflict`)
unless the step is running and `run` is its current run, and (`invalid`) for a scattered step.
Progress is never final: it feeds no input, handoff or gate, never finishes or settles the
step, is not a submission (a run that publishes progress still submits, or returns its
outputs, as before) and joins no result. It writes no log record, so it wakes no `next`,
`log_wait` or `step_wait` (§10); its commit touches a change view of its own (`progress`) that
nothing waits on and the scheduler's project versions leave out. It is kept, with its time
and run, until the step's next run is reserved, which clears it: after the run ends it stays
visible, as progress, not as outputs. Only the latest merged values are kept; publication
history is no longer collected. The reply is `{project,
step, run, progress, at}`, the merged values and when they were set. The dashboard (§13) shows a step's progress while it is
fresher than its outputs: unless the step has succeeded (or gone stale since) with a result
recorded at or after the progress was set.

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
`unit:` and `exit` are reserved tags. A unit is **settled** when every one of its steps is
settled (§7.1): a unit whose next step waits on a step that is running, or that can still
start, in it or in another unit, is not.

### 6.8 Recipes

A recipe is `recipes/<name>.json` in the home (global) or in `projects/<id>/` (the project's
wins on a name clash): `{"name", "doc"?, "params"?: {name: type | {"type", "doc"}}, "steps",
"title"?, "view"?}`, `name` matching the file. `unit` (a step id) is always a param. In every step id and string,
`{param}` is replaced by the value (a non-string as JSON); a string that is exactly `{param}`
becomes the value with its type; `{{`/`}}` are literal braces; an unknown `{x}` is an error. A
recipe step with `when` is broken.

A recipe's **stages** are its step ids without the leading `{unit}-` (`fork`, `work`, `land`),
in its order. `title` is a template for its units' title (`"{ticket}: {spec}"`): `{param}` and
`{unit}` as in steps, any other `{x}` making the recipe broken. A param bound to `{"file":
"{p}"}` in a step stands in a title for that file's title (below). `view` is an OpenUI Lang
program (§13, the unit view) drawing a unit's summary; one that does not check leaves the recipe
usable (`recipe_list` reports `view_error`, the dashboard a note). Nothing about a recipe is
stored with the plan: a unit's recipe is found again from the plan, the first recipe (the
project's, then the global ones, each by name) whose `{unit}-` step ids, filled with the unit,
are exactly the unit's step ids and whose steps run the same fns; its params are read back by
matching the recipe's step strings against the stored ones (the first binding wins). A unit no
recipe matches has no recipe, title template or view.

**Titles.** Every step and unit has a title, derived when read, never stored. A step's own title
is its doc's first line, else the first heading (or first line) of its `spec`, `prompt` or `task`
input, a literal string or a `{"file"}` read from its first 4 KiB. A prompt or spec body uses
its first sentence or non-empty line, capped at 80 characters with an ellipsis; an explicit
doc or markdown heading and a recipe title keep their words, capped at 160 characters on
compact surfaces (their own pages and the drawer can head them with the whole title).
All compact surfaces use the derived title. The full prompt stays on the step page. A
unit's title is its recipe's `title` filled with its params, else the first own title among its
steps in plan order. A step's title is, in order: its unit's title from its recipe's `title`,
its own title, its unit's title when the unit has a recipe, else its id. A step of a unit of
more than one step has a **stage**, its id without `<unit>-`, shown before its title (`land · Ship the cron
fix`). Every surface names a step by its title, its id after it in mono.

`unit_add(project, recipe, unit, params, after?, inputs?, tags?, start?)` checks the params,
expands the recipe, tags every new step `unit:<unit>` (then the recipe's tags, then `tags`;
`unit:` tags in `tags` are refused), and in the same edit appends `after` entries per suffix
(`"*"` means the unit's entry steps) and binds `inputs` per suffix to `{"default": value}`. A
suffix is a recipe step's id without the leading `<unit>-`. An unknown suffix, an input the
step's fn does not declare and the recipe does not bind, or a step id the plan already has is
refused before anything is written. `bypass` (`{suffix: outputs}`, a new step's id also taken
for its suffix; `plan_edit`'s `unit.add` takes it too) sets a bypass preset (§7.7) on each
named new step in the same transaction as the add, with the edit's author and reason, each
outputs checked against its step as `step_bypass` checks them; a key naming no step of the unit
or outputs that do not fit refuse the whole edit (`invalid`, `bypass.<suffix>…`).

### 6.9 Validation

Every edit is validated whole: ids valid; every `run` visible to the project; required inputs
bound, no unknown inputs (extra inputs and declared outputs only on open fns); refs naming a
plan input or a step output (paths through record types, anything under `Any`); every source
fitting its input (list sources element-wise, the scatter input item-wise); literals passing
their types; gate entries well formed; `needs` naming declared resources within any fixed
capacity; no cycles. Errors are a list with paths
(`steps.notes.in.cwd: repo is int, which does not fit string: int is not string`).
Runtime input-value errors are ordered by field path, then message, including when an edit
revalidates only the inputs it changes.

### 6.10 Edits

Every edit tool (§12.4) becomes operations on one
candidate plan (`docs/design/plan-rows.md` §7.6): the operations apply in order, the candidate
is validated for what they change (§6.9), and the edit commits its row changes, the runtime
transitions they cause, one `plan_edits` row and one `plan.edit` record `{rev, author, reason,
changes}` in one transaction. `changes` are the resolved rows, never the operations: `header.put
{root_order}`, `input.put {name, position, declaration}`, `input.delete {name}`, `output.put
{name, position, binding}`, `output.delete {name}`, `step.put {step, position, declaration}`
(the whole declaration) and `step.delete {step}`, deletes before puts. Untouched rows never
appear, so an edit costs what it changes, not the plan's size.

The reply is the **edit result** `{project: {project_id, name}, rev, preview, steps?,
board_warnings?}`, `preview` being `{scope, changes, would_start, would_queue, would_skip,
would_stale, errors}`, `steps` the steps the edit was about (`plan_edit`'s and `unit_add`'s new
steps, `unit_tag`'s unit, `step_pause`'s selection, `plan_prune`'s and `unit_remove`'s removed
steps, `unit_update`'s changed members) and `board_warnings` each step the project's board names
(§13) that the edit took out of the plan, in `board_set`'s warning form; the edit is made all
the same. It is worked out while the edit is prepared, from the steps the edit removes or
retags and the board's program, without running the board's queries. With `dry_run: true` the reply is the preview alone and nothing is written. The
preview's `scope` is `impact` by default: the steps the edit adds or changes, the steps whose
state or readiness it changes, and the ready steps competing for the resources of a step whose
`needs` or `priority` it changes. `preview_scope: "all"` previews the whole plan, and only with
`dry_run: true` (else `bad_request`). The simulation uses cached capacities and never runs fns;
`core.external` steps never appear in `would_start`.

- Every edit tool takes an optional `rev`; a stale one is `conflict` with `current_rev`. An edit
  prepared against a plan or state that changed before it commits is prepared again, never
  committed stale; when the third preparation is stale too, the edit is refused `busy` with
  `retryable: true` and nothing is written. Preparation never runs in the writer. `order.set`
  requires `rev`.
- A running step may change only `paused`, `tags` and `restart`, and may not be removed.
- `start: false` (`plan_edit`, `step_add`, `unit_add`) adds steps with `"paused": true` unless a
  step sets `paused` itself.
- An edit that changes nothing (an edge already there, tags or pauses as they are, a prune that
  removes nothing) commits nothing: no rev, record or history row. Its reply is the edit result
  with the current `rev` and empty `preview.changes`. `step_set_input` refuses one instead
  (`bad_request`). What a caller supplies may not be empty: `plan_edit` with no `ops`, an
  operation's empty `changes`, `steps` or `after`, and `unit_update` with no member or a member
  with no field are `bad_request`.
- Removing a finished step keeps its result as an `outcomes` row; a pending step leaves none.
- A new row goes after its collection's last; removing one renumbers nothing; only `order.set`
  reorders a collection.

### 6.11 Retiring done units

A project's `prune_done_after` (seconds, 1 to ten years; set with `project_update`, null turns
it off, off by default) retires its done units by itself, so finished lanes leave the live plan
without an orchestrator remembering `plan_prune`. `prune_keep` (unit-name patterns, `*` any run
of characters and `?` one, at most 64 of at most 128 characters, no whitespace) names units it
never removes, whatever their state. Each change of either is a `project.update` record naming
the field. `query` reads both from `projects`.

- **Owner and timing.** The coordinator's scheduler does it, beside its other upkeep, on its
  30 s tick, whether or not anything holds the scheduler lease: each live project that is
  neither paused nor archived and has the setting is looked at once every 5 minutes at most (a
  coordinator restart looks again at once). A pass runs in its own task, one at a time, so a
  large plan's edit never holds admission up.
- **What goes.** A look reads, in one snapshot, what `plan_prune(older_than=prune_done_after,
  keep=prune_keep)` would remove: done units whose every step's current result was recorded at
  least that long ago, less those matching a keep pattern, less the reference closure (a unit a
  surviving step or plan output references stays). When that is nothing, there is no edit.
  Otherwise it runs that `plan_prune` at the revision it read, as one edit by author `sluice`
  with reason `retire done units older than <N>h` (`<N>m` or `<N>s` when not whole hours). A
  unit that is live, failed, stale or anything but done is never removed, and every removed
  step's result stays in `outcomes` (§7.10).
- **Races.** A plan edited between the look and the edit, or a member's result changed, is a
  `conflict`: that round is skipped and the project waits for its next turn; it never retries
  in a loop. A drain (§2.6), a blocked registry or any other refusal skips the round the same
  way.
- **Waits.** The edit is an ordinary `plan.edit` record (no new kind), so it wakes what any plan
  edit wakes: `log_wait` and `watch` without a `kinds` filter, `next` with `all`, and a
  `step_wait`'s re-read. `next` without `all` ignores it, so an orchestrator is not woken by
  retiring.

## 7. Running

### 7.1 Step status

`pending`, `running`, `succeeded`, `failed`, `stale`, `skipped`. A succeeded step may be
`manual` (set by hand). A scattered step also has `done`, `total` and `instances`. A step's
`error` is a structured error object (§12.2).

A step is **ready** when it is pending, not paused, its project is not paused, every plan input
it reads has a value, every handoff source succeeded and every gate is satisfied. A ready step
starts at once unless it has `needs` (§7.6); a ready `core.external` step only waits.

A step is **settled** when nothing more happens to it until someone edits the plan, retries,
cancels, sets an input or sets its outputs by hand: its status is `succeeded`, `failed`,
`stale` or `skipped`, or it is pending and **held**. A pending step is held when it or its
project is paused, when it is a ready `core.external` step, or when it is not ready and every
step it depends on (handoff sources and gate steps, a unit gate's exit steps) that has not
succeeded or been skipped is itself settled; what it waits for is then a failed, stale or held
step, or a plan input with no value. A running step is not settled, nor is a pending step that
is ready (queued on resources included), about to be skipped or failed by the scheduler, or
waiting on a step that is running or can still start. A unit is settled when all its steps are
(§6.7).

### 7.2 Admission and launch

While the scheduler lease is held the coordinator, on every relevant change, settles skips,
marks staleness, and admits ready steps in priority order (higher first, ties in plan order).
Admission reserves an attempt and its runs in one transaction (status `running`, fresh run ids,
the messages assigned to each run, `prev_run`), then launches each run's unit. A reservation
whose launch fails before the payload starts fails that run with `process_lost`; it is never
started twice. Built-in fns that need no process run inside the coordinator. A step with a
bypass preset waiting (§7.7) is never admitted: it settles instead.

If an edit moves the plan revision between the scheduler's plan read and an admission
check, reconciliation restarts immediately with a fresh plan, with three attempts in all per
call. Reservations and completions already committed remain in place and count toward that
call's admissions. If the third attempt is stale too, the scheduler defers the project as
usual. Other refusals still defer immediately; every attempt keeps the same scheduler lease,
installation fence, drain and resource checks.

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

**The previous attempt.** Each run of a step is an attempt at it; a retry, a re-arm, a
send-back (a retry with a message, or a `Rejected` completion action) or any other relaunch
starts a new one, and its `prev_run` (§7.8) names the one before. sluice derives a note about
that previous attempt from what it keeps anyway (the run, its result, its submission, the
`step.cancel` or `step.settle` record) and stores nothing new. The note says:

- which attempt this is (1 for the first: "None: this is the first attempt at this step.");
- how the previous one ended: succeeded, failed (its error kind, agent kind and message),
  cancelled (by whom and why, while the log still has the `step.cancel` record), lost, or
  settled on its submission (by whom and why);
- its submission's fields, or when it submitted nothing its outputs, every string cut to 200
  characters and the line to 1,200;
- when its outputs include a `git` value (the agent fns' result), the head it ended at, the
  commits since its baseline and whether it left uncommitted changes;
- where the agent was ("Where you were (…): …"): the later of the last `say` or `ask` a run of
  the step sent to a session or the owner (its body) and the step's last `step_progress` (as
  JSON), cut to 1,200 characters, with its run and time. Reserving a run clears the step's
  progress (§6.4), so the attempt freezes it (`previous_progress` in its request) for this;
- when the step's inputs bind a `cwd` that is a git work tree, `git status --porcelain` of it
  now: how many entries and the first 20 (`M path`, `?? path`); of `workspace` instead when the
  step binds one and it is a directory (relative to `cwd`). git runs with a 3-second limit;
  git absent, failing or too slow is said in the note (`unavailable: <why>`) and never fails
  the launch or the call.

`step_context` carries it as `attempt: {number, previous?, worktree?, note}` (`previous`:
`{run, started, finished, ended, error?, by?, reason?, submitted?, outputs?, git?, memo?}`,
`memo`: `{kind: "message" | "progress", run, at, to?, text}`; `worktree`: `{cwd, git: "clean" |
"dirty" | "unavailable", count?, paths?, reason?}`, `cwd` the directory it checked; `note`:
the text), for a running step about its current run, otherwise about the attempt its next run
would follow; a scattered step has none. Every agent's task starts with `note` (§15), computed
at launch.

### 7.5 Cancel

`step_cancel(project, steps?, tags?, reason?, expected_rev?)` asks running steps to stop (their
guardians stop the payload) and fails a pending `core.external` step at once. A cancelled step
fails with the error `{"error": "cancelled", "message": <reason>}` and a `step.cancel` record;
who asked and why are kept on the run too (`runs.stopped`, §3). On a failed step it sets the
failure aside: a `step.cancel` record (author, reason) and the cancel kept on its latest run as
usual, and its error becomes `{"error": "cancelled", "message": <words>}`, the words keeping the
failure (`shown::set_aside`: `<reason> (it had failed: <kind>: <first line>)`, or `it had failed:
<kind>: <first line>` with no reason; the record and the run keep the same words), recorded as a
new result beside the failure's. The step stays `failed`, so what waits on it stays held exactly
as before, no `step.status` record is written, `step_retry` takes it as any failure, and the
owner can dismiss it (§12.5); its runs keep their own results. A failed step whose error is a
cancel already is left as it is, nothing written. Any other selected step (succeeded, stale,
skipped, pending and not `core.external`) refuses the whole call. Reply `{"ok": true}`.

`step_settle(project, step, reason?)` settles a finishing step (§6.4) on its submission, for
the owner or a session (a run's callbacks may not). It is refused (`invalid`, saying
why) unless the step is running, not scattered, and its one current run has stored a valid
submission, and unless the step runs an agent fn itself (`agent.run`, `agent.codex`,
`agent.devin`, `agent.claude`, `agent.review`, `decide.llm`). Its outputs are the ones the done
signal would have given: the submission plus the agent fn's own, derived from what the run's
supervisor checkpointed (`runs/<run>/native.json`: the session, the agent's last message, the
git baseline, read by field so an older release's checkpoint reads too), the git facts of its
`cwd` (or `workspace`, §15) now, the model its `model` input composes (a retired string `model` leaves `model` out,
as the run's frozen outputs then lack it) and, for `agent.codex` and `agent.devin`, the engine
log as their `log` output. They are checked against the run's frozen outputs before anything
is written; when they cannot be derived or do not fit, the call is refused (`invalid`) with
the way by hand. A fn that composes an agent (`ctx.builtin("agent.run")`) completes with what
it returns, which sluice cannot derive from the submission: it is refused with the same way by
hand, `step_cancel` and then `step_set_output` with the outputs it should have. Otherwise one
transaction records the outputs on the attempt, sets its cancel intent (the guardian stops the
payload as for a cancel) and writes a `step.settle` record; a second `step_settle` is a
`conflict`. The run's completion then succeeds with those outputs, whatever it reports
(cancelled, lost or failed), unless it succeeded on its own first, which keeps its own result.
Reply `{project, step, run, outputs}`.

### 7.6 Resources and leases

These are project-local logical scheduling resources. Every executable run also needs its
admission by measured memory (§2.5), whether it declares `needs` or not.

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

A **section lease** (`ctx.acquire`, §5.4, or `lease_acquire` from inside a run) waits for and
holds an amount of a resource inside a running step's run; grants follow the request's priority
(`lease_acquire`'s `priority`, else the step's current `priority`), then arrival, and a request
that fits is granted even while a larger one ahead of it waits. Held leases count in the same
totals as `needs`. A run that ends, finished or dead, releases its leases and drops its waiting
requests, so a dead holder never blocks a queue. Grants and releases are `step.lease` records.

The tools `lease_acquire(project, run, resource, amount=1, wait=false, timeout?, priority?)`,
`lease_release(project, run, lease)` and `lease_status(project, resource?)` give an agent the
same leases (`sluice tool lease acquire <resource> [--amount N] [--wait] [--timeout S]
[--priority P]`, `lease release <id>`, `lease status [<resource>]`). The run named must be one
of the project's, and the current, executing run of its step; in a run, `project` and `run`
default to its own (§14). `lease_acquire` answers `{lease, resource, amount, priority, state,
position}`, `state` `held` or `waiting` and `position` the request's place in its resource's
queue (from 1) while it waits; with `wait` it answers once the lease is held or `timeout`
seconds (default and cap 600) pass, then with `timed_out: true` and the request still queued.
`lease_release` gives back a held lease or drops a waiting request (`bad_request` for any other
id) and grants the next in line in the same transaction. `lease_status` answers
`{resources: {<name>: {capacity, held, holders, queue, waking}}, parked}`: `holders` are the
resource's section leases and running steps' needs (`{kind: "lease"|"needs", lease?, run, step,
amount, since}`), `queue` its waiting requests in grant order (`{position, lease, run, step,
amount, priority, since}`), `waking` the parked runs taking it back, and `parked` the project's
parked runs (`{run, step, until, phase, lease, since}`).

**Parking.** `park(project, run, until, timeout?)` (`sluice tool park --until <until>
[--timeout S]`) ends an agent's turn and waits for an event without its lane. `until` is
`lease:<resource>[:<amount>]` (the run's section lease is granted), `pid:<n>` (the process
exits; the CLI pins it by its start time, so a reused pid does not hold the wait) or
`file:<absolute path>[:<marker>]` (the file exists and, with a marker, contains it; the CLI
resolves a relative path against its working directory). A `lease:` condition adopts the run's
oldest waiting request of that resource and amount, keeping its place, else queues one. A
condition that holds already answers `{parked: false, until, lease?, message}` and parks
nothing (a lease granted at once is the run's). Otherwise the park is kept on the run's attempt
and the answer is `{parked: true, until, lease, position, timeout_seconds, message}`: the agent
ends its turn. When the turn has ended, the run's supervisor (§15) marks the run parked and the
coordinator releases the step's needs in the same transaction, so a queued step may start; the
engine's session stays up, idle. An addressed say, ask, reply, or step_retry with a message
wakes it; the first message is delivered with `woke: woken by a message`, under its normal
message delivery id, and any other messages follow in order. A retry with feedback to a parked
step wakes its existing run without resetting its work generation; ordinary retry semantics
apply to other selected steps. Without `timeout`, config.json's `park_timeout_seconds` applies
(default 3600); zero is refused, and explicit longer bounds are allowed. When a message arrives,
the condition fires or `timeout` seconds pass, the coordinator queues the step's
needs again (`waking`), granted with the section leases in priority order before any new step
is admitted; once they are held the park ends and the same session gets one input:
`woke: <until>, lease <id>` (a granted lease), `woke: <until>` (pid, file), or
`woke: your park condition did not happen within <n>s: <until>` (with `; lease <id> is still queued` when its lease was not
granted). Agent runs have no total time limit; idle detection restarts after waking. A cancelled parked run stops
as any other. The bound is recorded on the park; supervisor recovery counts time already waited,
and legacy unbounded parks use the one-hour fallback. Status exposes `shown: "parked"` and
`parked: {run, until, phase, since, waited_seconds, timeout_seconds}` while the stored status
remains running. The units view has state `parked` when all its running steps are parked,
and lists their parks with step ids. The public `parked` SQL view exposes live parks as
`{project_id, step_id, run_id, condition, phase, since, waited_seconds, timeout_seconds}`;
`condition` is JSON. Only agent runs on the `claude` or `codex` engine park: `devin` (whose session
cannot be held idle and resumed) and a run of anything but an agent are `bad_request`, and so is
a scattered step's item; a run that is not its step's current, executing one is `conflict`.

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
  while a gate is unsatisfied or something it reads is not ready. A pause, the step's or its
  project's, does not block it: pausing holds back a launch, so a paused step can be given its
  result without ever starting. A `step.output` record.
- `step_bypass(project, step, outputs?, off?, reason)`: drops a pending step from the run while
  keeping its place, so the steps after it still go on. It stores a **bypass preset** on the
  step (`steps.bypass`, §3: the outputs, author, reason and when) and notes each set and clear
  in the log as a `message` record: a say from `sluice` whose `data.step_bypass` is `{step,
  action: "set"|"clear", by, reason, rev, outputs?}` ("@a bypassed step s (reason). Once ready
  it settles succeeded with {…} instead of running.", as a unit handoff's note, §8), to the
  author when it is an @session (which registers it in the project) and else to `owner`. A say wakes nobody, and the note is
  never addressed to the step, whose next run would read it. It is no new record kind, so a
  release pinned before `step_bypass` reads the whole log unchanged. When the step's gates and inputs are ready, the
  reconciliation that sees it so (the transaction that made it ready, or the runner's next
  pass) does not launch it: it settles it as `step_set_output` without `force` would, status
  `succeeded`, `manual: true`, the preset's outputs, with that `step.output` record (the
  preset's author and reason) and its `step.status` record, and its dependents then run in
  order. A step already ready settles in the call itself. A pause, the step's or its
  project's, does not hold it back (no run starts). `outputs` (default `{}`) are checked when
  the preset is set, against the step's declared outputs as `step_set_output` checks them:
  each required one present, the types fitting (`invalid` names each field), nothing else. If
  the step's declaration changes so that they no longer fit, the step fails instead
  (`invalid`, "bypass: its outputs no longer match the step's outputs"), never launching.
  Only a pending step takes it: any other is refused (`invalid`) naming the verbs that apply
  (`step_cancel`, `step_settle` and `step_set_output` for a running step; `step_set_output` and
  `step_retry` otherwise); `reason` is required. `off: true` clears a preset still waiting
  (`invalid` when there is none). Setting it again replaces it. A preset is applied once and
  then kept only as the record of what settled the step: a later `step_retry` of the step runs
  it for real, while a preset not yet applied stays through a retry upstream that makes its
  step wait again. The reply is `{project, step, status, bypass, applied}` (`bypass` the preset
  still waiting, null once applied or cleared). `status` shows a waiting preset as the step's
  `bypass` (`{outputs, author, reason, at}`). No new step status: a bypassed step is
  `succeeded`, manual; the dashboard words its manual result "bypassed" (§13).

### 7.8 Message delivery and `prev_run`

Each step keeps a delivery cursor: the last message id addressed to it that a run of it was
given. Reserving a run assigns it every message to the step after the cursor and records the
range on the run; the live feed continues from the end of that range, and the cursor advances
once the run has started. It also advances over messages the live feed hands a run once the
run acknowledges them (its session has them): over each message after the cursor that a run
of the step has acknowledged, up to the first that none has, which the next run is assigned
again. So a resumed or retried run is never given a message its step's session already has.
Every run of a scattered step gets the step's messages: its items' acknowledgements leave the
cursor alone.
`runs/<run>/messages.json` shows the assigned range. Each run records `prev_run`, the step's (or
item's) previous run, seen as `SLUICE_PREV_RUN` and `ctx.prev_run`. Agent fns resume the
previous session when a bound `session` says so, or when `session` is unbound, the run was
assigned messages or the previous session was given any (they are in it, and no later run is
given them again), `prev_run` recorded a session, and the engine and cwd are unchanged;
`session: ""` always starts fresh.

### 7.9 Adoption and completion

A coordinator that starts finds runs whose guardians are still alive and watches them
(`run.adopt` outcome `watching`), finishes runs that completed while it was away (`finished`),
and fails runs whose guardian and payload are gone without a completion (`lost`, error
`process_lost`). A live run that no step or call references is stopped (`run.orphan`). A
completion is journalled by the guardian and acknowledged durably by the coordinator.
A run's result is decided from what its attempt
froze when it was admitted (its outputs contract, its inputs and its identity), never from a
plan: the attempt keeps no copy of the plan, only the revision it was admitted at
(`admitted_rev`), and the project's current plan only reconciles after the result, so a plan
that no longer compiles (a broken fn catalog) never fails a completion.

A starting coordinator answers at once. Its first adoption pass adopts runs concurrently, at
most 16 at a time. During that pass it serves reads, the scheduler lease and the runs' own
guardians; `install` commands never wait for the coordinator. Every other command, and a run's
own plan edits and section-lease requests, wait until the pass is done. A run's message that
answers a question is served at once, even when it sets a plan input. A waiting request is
refused with `busy` (retryable, "… this request was not executed, so it is safe to retry") when
32 requests already wait, when its client hangs up, or when the coordinator stops or its pass
fails first. Nothing is admitted or launched before the pass is done. A run whose adoption fails
stays as it was and is tried again by the next pass, every 30 s.

### 7.10 Outcomes

Removing a finished step from the plan (any edit, including `plan_prune`) stamps its result row
`removed_at`; the `outcomes` view shows these rows (`result_id, project_id, step_id,
generation, work_generation, attempt_id, unit, declaration, inputs, inputs_hash, status,
outputs, error, manual, run_ids, recorded_at, removed_at`). They are never trimmed and go with
the project.

## 8. Messages

Three verbs post a message; the thread and the sender are derived, never given, and the
recipient is resolved when it is posted (§8.1):

- `ask(project, to, body, title?, ui?, input?, data?)`: a question that needs a reply.
- `say(project, to, body, data?)`: a note; no reply is expected.
- `reply(project, to_message?, to?, body="", answer?)`: a reply selected by id or sender.
  Give exactly one of `to_message` (a message id) or `to` (a step id or sender name).
  With `to`, it answers that sender's single open question addressed to the caller. Selection
  and posting happen in one transaction. With no match it returns `conflict` saying so; with
  several it returns `conflict` listing their ids and first body lines, at most 200 characters
  each, so the caller can choose `to_message`. No refusal posts or answers anything.
  With `to_message`, a non-message log seq in the project returns `invalid`, naming the
  record kind and listing up to five open questions addressed to the caller, oldest first,
  with their message ids, senders and first body lines, at most 200 characters each. With
  none, it says "Open questions to you: none." A message in another project returns
  `invalid`, "message N is in project <name>". An id that names neither returns `not_found`,
  "no message N". A log seq is never resolved to a different message.

Each takes `run?` too: the run that speaks (agent and fn callers; `sluice tool` fills it in a
run, §14), and `me?`: the session that speaks (§8.1). The dashboard speaks as the owner
(`owner: true`, which MCP and `sluice tool` do not take).

- `from` is derived: a run's step for a step's run (`call:<run>` for a call's run), the
  session's `@name`, `owner` for the dashboard, `sluice` for the coordinator's own alerts. A
  session with no name may not speak (§8.1).
- `to`, for `ask` and `say`, is a step id in the project's current plan, an `@session`
  registered in the project or `owner`; a run may leave it out (or give `orchestrator`) to
  reach its unit's owner (§8.1). Anything else (an unknown or removed step, an unregistered
  session, another name, the sender itself; from a session, nothing or `orchestrator`) is
  `invalid` and nothing is stored. A reply's `to` is the original's `from`.
- A step whose status is `succeeded`, `failed`, `stale` or `skipped`, or running with every
  run it has already submitted (its agents' sessions are over; the runs are only finishing),
  takes no messages: an `ask` or `say` to it, and a reply whose `to` it is (to a question it
  asked that is already answered, say), are refused (`conflict`, saying why) and nothing is
  stored or queued. A reply that answers or closes such a question while it is open is still
  taken: the question is answered (setting its `input`) or closed, and the step's retry takes
  it up. A retry's message reaches the step after the retry has reopened it.
- The thread: a reply keeps the original's; a message from a step's run lives on its own
  step's thread `step-<step>`; one to a step on `step-<step>`; one between a session and the
  owner on the session's thread, named as the session (`@lead`), and one between two sessions
  on the recipient's; the coordinator's alerts to the owner on `owner`, where the orchestrator
  and the owner talked before sessions named themselves.
- A reply to an open question answers it (an **answering reply**) atomically: with or without
  `answer` `{action, params?, values?}`; `answer.action == "close"` closes it without setting
  anything. A reply to a question already answered or closed is just a message, but one with
  an `answer` is refused (`conflict`, "question is no longer open"); `answer` on a reply to a
  message that is not a question is `invalid`. `body` may be empty only with an `answer`; an
  `ask` or `say` body may not be blank (an `ask` with `input` may have an empty body: it shows
  the input's doc).
- `input` names a declared plan input. The answering reply sets it: the value is
  `answer.values.value`, else `answer.params.value`, else the body; it goes through
  `plan_set_input`'s path with reason `message <id>: <title>`. A value that does not fit
  refuses the reply (`invalid`) and the question stays open.
- An open question to `owner` reserves a notification attempt and writes a `project.notify`
  record (`outcome: reserved`). Notes, replies and questions to anyone else never notify.

Each verb returns its receipt `{id, to, thread, delivery, run?, note?}`: `to` is the resolved
recipient. `delivery` is `delivered`
when a live run of the step that listens has started and is handed the message on its live
feed (`run` names it), and for a session and `owner` (their inbox); `note` says when a session
recipient has no live reader (§8.1). A run listens when
its fn takes a `listen` input (every agent fn, and a pack fn that runs one and passes
`listen` on) and its inputs do not set it to `false`; its reservation freezes this, so the
receipt reads stored rows, never which guardian protocol (a held watch or a poll) the run's
guardian speaks. A run reserved before reservations froze it listens if its step runs an
agent fn without binding `listen: false`, or once it has acknowledged a message. `delivery`
is `queued` when the step will run (pending and not paused, or its run reserved but not
started, which `run` names) and its next run is assigned the message; `no_live_run` when the
step is pending but paused, or its live run does not listen: the message is kept and given to
the step's next run if one is ever started.

A message is a row `{id, verb, from, to, thread, body, title?, ui?, input?, data?, run?, at,
to_message?, answer?}` plus a `message` record written in the same transaction; a field with
no value is left out. `verb` is `ask`, `say` or `reply`; `to_message` and `answer` are a
reply's. Read as rows, a question also carries `state` (`open`, `answered` with
`answered_by`, the answering message's id, or `closed`). The record carries the same fields
flattened into `{seq, at, project, kind: "message", ...}` with the message's own time as
`posted_at`, and never `state`. Rows and records stored before the verbs keep their stored
fields and read in this shape: a reply if they replied (`reply_to` becomes `to_message`), else
a question if they needed a reply, else a note. Rows are never trimmed with the log and are
deleted with their project.

The retired `message_post` is accepted only from a caller presenting a run identity (a run's
callback, or `run`), as runs started on older releases still post: translated to `reply` when
it names `reply_to`, else to `ask` unless `needs_reply` is false, else `say`, addressed to
the run's unit's owner when it names nobody (§8.1); its `thread`, `from` and `author` are
ignored. It answers
`{id}` and logs a deprecation line. It is not an MCP tool, not listed by `sluice tool` and not
documented for agents.

**Notify.** With `notify: {command, timeout_s}` in config.json, the coordinator runs `command`
(argv, no shell; cwd the home; its environment plus the home's and the project's `.env`) once
for each reserved notification, whether or not anything holds the scheduler lease, outside any
transaction and in a task of its own. stdin is the message's JSON plus `project` (its name) and
`project_id`; stdout is ignored; the last 2 KiB of stderr are kept. The attempt is claimed
durably before the command starts, so it never runs twice: exit 0 is `dispatched`; a non-zero
exit or a failure to start is retried twice more (after 1 s and 2 s) and then `failed`; a
timeout (`timeout_s`, default 30) is `uncertain` and not retried; a claim a stopped coordinator
left is `uncertain` (`process_lost`). Each result is a `project.notify` record and stays on the
attempt (`notification_attempts`). A reservation whose question is answered or closed before it
goes out is `failed` (`cancelled`) without running anything. Without `notify` reservations wait.

**Nobody reading.** With `unread_alert_min` in config.json, every 30 s the coordinator asks the
owner (a question from `sluice`) about each `unit.settled` record at least that many minutes
old in a project whose orchestrator readers (`next`) have all been silent that long and none has
read past it: once per record, whatever the threshold later becomes.

Question state is derived: `open`, `answered` or `closed`. A question posted by a step's run
is `waiting` while that run is live; otherwise it reports why nobody waits: `asking run is
being cancelled`, `<step> is <status>`, `<step> is cancelled`, `running another run`, `not in
the plan`, `call <run> is <status>`.

`message.ask` with `wait: true` (§16) blocks until the first answering reply and returns it as
`reply`; a closed question fails it (`question closed`). A retried step asking with the same
title takes up its own latest earlier question: an open one nobody waits on gets the new run as
asker; an answered one whose answer nobody claimed returns that answer at once. A claimed
answer (`claimed_by`) is never reused.

`messages(project, view, thread?, since?, owner=false, me?)` → `{project, messages, last_id}`,
read as the caller: the owner for the dashboard and with `owner`; else the session `me` names
(its own inbox and history; its inbox also holds the open questions sent to `orchestrator`
before sessions named themselves, which are any session's to answer); else, unnamed,
`orchestrator`, the inbox of what was addressed to the orchestrator before sessions named
themselves:

| view | shows |
|---|---|
| `inbox` | open questions to the reader, then the other messages to it after its read position in their thread |
| `questions` | every open question in the project |
| `history` | every thread with a message to or from the reader |
| `thread` | one thread in full (`thread` required) |

Read positions are kept per project, reader and thread and advance through `mark_read`; each
message it passes to that reader also gets its `read_at` (an added column; older releases
ignore it). The dashboard marks read, as `owner`, only when the owner asks (§13).

### 8.1 Identities, owners and routing

Every message's `from` and `to` is an identity:

| identity | who | named by |
|---|---|---|
| `@<name>`, `^@[a-z][a-z0-9-]{0,39}$` | a session: an orchestrator's MCP connection or shell | itself (`me`) |
| `owner` | the person, at the dashboard | the dashboard alone |
| a step id | a run of that step (a call's run is `call:<run>`) | its run |
| `sluice` | the coordinator's own alerts | nobody; it takes no messages |

`orchestrator` is no longer an identity. Messages stored before sessions named themselves keep
it, and the dashboard shows it as they stored it, "orchestrator", muted; it never rewrites them.
The dashboard names every other identity as itself: a session by its `@name`, a step as pages
name steps, the owner "you".

**The caller's name** resolves in order: the call's `me` (every tool takes it); `SLUICE_ME` for
`sluice tool`, `sluice next` and `sluice drain`, except in a run (`SLUICE_RUN_ID` set), which
speaks as its step; the MCP session's binding. The first `me` an MCP session gives binds that
session, its later calls default to it, and a different `me` on it is `invalid` ("this MCP
session is @lead; a session keeps one name"). HTTP tool calls have no session binding:
`me` applies only to that request; without `me` or a run, the caller is unnamed. The name is
the identity, not the connection: a successor that reuses `@name` inherits its units and what
is addressed to it. With no name a session reads freely, but `ask`, `say`, `reply` and a
`step_retry` with a `message` are refused: `invalid`, "name this session: pass me=@name or set SLUICE_ME". A `me` that is not a
session name is `invalid` and says how to write one; `owner` (only the dashboard speaks as the
owner), `orchestrator` and `sluice` are never names. A run that names `me` is refused: it
speaks as its step. A write's `author` defaults to the session's name (§12.3).

**Registration.** A session's first message, plan edit with `me`, claim, or `next` or
`log_wait` naming it in a project adds its `readers` row there (stream `orchestrator`, thread
`''`); that row is the registry. Its `heartbeat_at`, which `next` and `log_wait` write every
30 s while they wait, says whether the session is reading: it is when the heartbeat is under
10 minutes old. Its `cursor` is where the session's last `next` batch ended in that project,
which `log_wait` never moves.

**Owner.** A unit's owner is who answers its runs: an `@session`, `owner` (the person) or
nobody. It is `steps.owner` (an added column), equal across the unit's rows. When an edit adds
a step to a unit that keeps rows the edit leaves there, the step takes that unit's owner. A new
unit takes the owner the call names (`owner` on `step_add`, `unit_add` and `plan_edit`'s
`unit.add`: `owner` or an `@session` registered in the project), else the editing session
(`me`; a run's edit, its own unit's owner), else the project's `fallback`, else nobody. A step
its tags move into another unit takes that unit's owner. Row updates never change it, and naming
an owner other than an existing unit's is `invalid` ("unit u already exists, owned by @lead").
The owner is runtime metadata, never part of a declaration: `plan_get`, the plan's history and
the log never show it, and releases pinned before it never read it. Units added before owners
are unowned. The project's `fallback` (an added column, set by `project_update(fallback)`; null
clears it) is `owner` or an `@session` registered in the project, and answers its unowned
units.

**Routing.** `to` is resolved when the message is posted, and stored resolved:

- A step id of the current plan, an `@session` registered in the project, or `owner`, as
  given. Any other `@name` is `invalid` ("@laed is not a session of project p: …"), which
  catches a typo.
- A run that leaves `to` out, or gives `orchestrator` (runs pinned to older releases still do
  until they drain), reaches its unit's owner, else the fallback, else `owner`. A call's run
  reaches its caller when the caller is a session.
- A session names its recipient: `to` left out, or `orchestrator`, is `invalid`.
- The dashboard leaving `to` out (a board Button) reaches the fallback session; without one it
  is refused.
- A reply goes to the message's `from`; `reply(to=…)` chooses among the open questions to the
  caller (for a session, also those addressed to `orchestrator` before it named itself).

A message from a session or the dashboard to a session that is not reading is stored and is in
that session's inbox all the same; its receipt carries `note: "queued; @peer has no live
reader"`. A run's receipt never carries `note`, so a run pinned to an older release decodes it.

**What a session sees.** `next` and `log_wait` take `scope`: `mine`, a named session's default,
or `all`, everyone's and an unnamed caller's only scope (`mine` without a name is `invalid`). In
scope `mine` a record is read when it is:

- a unit's: a `unit.settled`, or any record naming a step (`step.*`, `run.*` of a step), whose
  unit is the session's: its owner is the session, or it is unowned and the project's
  fallback is the session or there is none (a removed step's unit reads as unowned);
- a message to or from the session, to `orchestrator` or to nobody (stored before sessions
  named themselves), or to or from a step of such a unit;
- a call's record when the session made the call;
- the project's own (pause, archive, settings, plan edits and every other record).

Ownership is read when the record is read, so a claim moves a unit's past records too. `units`
(unit names or patterns, `*` any run of characters, `?` one) and `tags` (a unit any of whose
steps carries one) narrow either scope to those units, keeping messages to or from the session.
In scope `all` a named session's `next` wakes on a question to any session, as an unnamed one's
does; in `mine`, only on one to it.

**Claim and release.** `unit_claim(project, unit? | units? | tags?, owned_by?, expected_owner?,
force=false, reason?, me)` makes the session the owner of every unit selected (`unit` and
`units` by name or pattern, `tags` by any step carrying one; together, every unit any of them
names; `owned_by` (`@session`, `owner` or `unowned`) keeps those its owner holds now, alone every
unit it holds, and is the owner expected unless `expected_owner` is given; selecting nothing is
`invalid`, a name, pattern or tag matching no unit, or an owner holding none of them,
`not_found`), in one write transaction, all or none:

- a unit already the session's is left as it is (`unchanged`);
- with `expected_owner` (`@session`, `owner` or `unowned`) each unit's owner must be that now,
  a compare-and-set; another is `conflict` ("unit u is owned by @x, not @y");
- without it a unit is taken when it is unowned or its owner session is not reading; one whose
  owner is reading is `conflict`, naming the owner and its last heartbeat and how to take it
  over (`expected_owner="@x"`), unless `force`;
- a unit the person holds (`owner`) needs `force`, with or without `expected_owner`.

Every refusal is listed in one `conflict` ("nothing changed: …"). `unit_release(project, unit? |
units? | tags?, owned_by?, to?, force=false, reason?, me)` hands units the session owns to `to` (`owner` or
an `@session` registered in the project), or, without it, leaves them unowned, so the fallback
(else the person) answers them; a unit another owns, or nobody, needs `force`; a unit already
`to`'s is unchanged. Neither is plan work: no revision, and both are taken during a drain.

For each unit that changes owner, in the same transaction: its open questions (asked by its
steps) addressed to whoever answered it before (its owner, else the fallback, else `owner`) or
to `orchestrator` are re-addressed to whoever answers it now (a question re-addressed to `owner`
reserves its notification), and the log keeps each question's record as it was posted; and
`sluice` says so, in one `say` per party told, on a claim whoever answered the units before, on
a release whoever answers them now (the caller itself when that is the caller): "@b claimed unit
u from @a (reason). Open questions re-addressed to @b: #12.", its `data` `{"unit_owner": {by,
reason, units: [{unit, from, to}], questions: [ids]}}`. The handoff is a message, not a new
record kind, so releases pinned before it read the log unchanged. Routing reads the owner when a
message is posted, so the units' later questions go to the new owner. The reply is `{project,
units: [{unit, from, to}], unchanged, questions: [{id, from, to, title, body}], notices}`: the
units moved, those left, the questions re-addressed (their first line) and the notes' ids.

## 9. The log

Every record is `{seq, at, project, kind, …}`; `seq` is home-wide and increasing, so one log's
seqs have gaps. Kinds:

| kind | fields |
|---|---|
| `plan.edit` | `rev, author, reason, changes` (§6.10; `ops`, RFC 6902, before schema 3) |
| `plan.input` | `rev, author, reason, name, value` |
| `step.output` | `rev, author, reason, step, outputs, force` |
| `step.retry` | `rev, author, reason, step, work` |
| `step.cancel` | `step, author, reason` |
| `step.submit` | `step, run, outputs, author` |
| `step.settle` | `step, run, author, reason` |
| `step.status` | `step, from, to, error, run_ids, needs` |
| `step.lease` | `step, run, lease, resource, amount, state, reason` |
| `step.queued` | `step, needs, resources, reason` |
| `call` | `call, fn, status, inputs, outputs, error, direct, author` |
| `message` | the message's fields, with `posted_at` for its time |
| `project.pause` | `paused, reason, author` |
| `project.archive` | `archived, reason, author` |
| `project.update` | `fields, reason, author`; a change to the board's document names `board_doc` in `fields` (older homes also hold `board_slot:<key>` from the retired slots) |
| `project.board` | `rev, cleared, reason, author` (never the program) |
| `project.rename` | `old_name, new_name, author` |
| `project.delete` | `project_id, name, author` |
| `project.capacity` | `resource, fn, capacity, error` |
| `project.notify` | `message, outcome, error` |
| `run.adopt` | `run, step, call, outcome` (`watching`, `finished`, `lost`) |
| `run.orphan` | `run` |
| `run.completion_action.register` | `run, target, message, author` |
| `run.completion_action` | `run, outcome, author` |
| `unit.settled` | `unit, work, steps: [{id, status, held, outputs, omitted}]` (`held`: a pending step held, §7.1) |

`step_progress` (§6.4) writes no record: progress is state on the step, not history.

`kinds` filters take exact kinds or the groups `plan`, `step`, `project`, `run`, `unit`.
`threads` keeps only messages on those threads (alone it means messages only). `statuses`
(step statuses) keeps only the `step.status` records whose `to` is one of them; `recipients`
(step ids, `@session` names, `owner`) keeps only the `message` records whose `to` is one of them.
Each filter speaks of its own records and lets records of other kinds through: a record is
returned when its kind passes `kinds` (and, when `threads` is given without `kinds`, is
`message`) and it passes every other filter that applies to its kind. `threads` and
`recipients` both apply to a message. An empty list is no filter. So `kinds: ["step.status",
"message"], statuses: ["failed", "stale"], recipients: ["@lead"]` returns failures,
stale steps and messages to the session `@lead`, and nothing else. `next` and `sluice watch`
take none of these filters; `next` and `log_wait` take a scope (§8.1). Calls made without a project go to the home log (`project` null).

Each log keeps at most 10,000 records; past that it is trimmed to 9,000 in the same transaction.
A `since_seq` older than a log's trim floor, or newer than any seq the home has issued, is
`cursor_expired`. `plan_edits` is never trimmed, so `plan_history` reaches the baseline revision,
rev 1 for a new project or the imported anchor for a converted project. Records carry
payload version 2 from schema 3 on; a converted
home's records are rewritten to it.

## 10. Waiting: `log_wait`, `step_wait`, `next`, `watch`

Waits block on the log's commits rather than polling: a wait reads once, then again only after
a record is committed to the log it watches, so a caller that would sleep and re-check holds one
of these instead.

`log_wait(project?, since_seq?, kinds?, threads?, statuses?, recipients?, limit=200,
timeout=300, wake="any", me?, scope?, units?, tags?)` returns `{records, last_seq}` as soon as a
matching record (§9) in its scope (§8.1, "What a session sees") exists after `since_seq`, or
with no records after `timeout` seconds (capped at 3600). Records that do not match never wake
it. `wake: "questions"` holds notes until a record that is not a note arrives or the timeout
passes. A named session's wait writes its heartbeat (at the start and every 30 s), never its
`next` position.

`step_wait(project, steps? | tags?, until, timeout=300)` waits until every selected step meets
`until`: `"succeeded"` (each succeeded), `"settled"` (each settled, §7.1) or `{"any_of":
[statuses]}` (each in one of them). Exactly one of `steps` and `tags` selects (a tag selects
every step carrying it, `unit:<name>` a unit). It reads the selected steps' statuses once, at
once, and again after each commit to the project's log (every status change, pause and plan
edit writes a record), until the condition holds or `timeout` seconds pass (capped at 3600; 0
reads once). The reply is `{met, steps: {id: status}, seq}`: whether the condition held, each
selected step's status in plan order, and the project log's last seq at that reading, so a
`log_read` or `log_wait` from `seq` sees what came after. An unknown step, a tag no step
carries, both or neither of `steps` and `tags`, and an `until` that is not one of the three
shapes (an unknown status, an empty `any_of`, another field) are `invalid`, `errors` naming
each (`steps: no step x`, `tags: no step is tagged t`, `until.any_of[1]: …`); a selected step
removed from the plan during the wait ends it with the same error.

`next(projects=[], since_seq?, me?, scope?, units?, tags?, timeout=300, all=false, settle=20,
settle_max=120, settles="short")` waits across projects (all live projects when empty) for what
an orchestrator acts on, in its scope (§8.1, "What a session sees": a named session's own units
and messages by default), and returns `{records, notes, last_seq, timed_out}`. Without
`since_seq` a named session resumes after its last batch: the earliest of its `readers`
positions in the selected projects (a position the log no longer reaches, or none yet, starts at
the top of the logs); an unnamed caller starts at the top. Each session keeps its own position,
so sessions never share or move one cursor. It wakes on:

- a `unit.settled` record, written each time a unit comes to rest: it is settled (§6.7), and
  its steps, their generations or work generations, or their statuses differ from its last
  settlement's. A unit that settles, is retried or acted on and comes to rest again settles
  again; one waiting on running work settles only once that work has finished and nothing
  more can move;
- a `step.status` to `failed`, `stale` or `skipped`;
- a question to `me` (or, stored before sessions named themselves, to `orchestrator`, or
  before the verbs, to nobody), not from `me`; without `me` or in scope `all`, a question to any
  session (or to `orchestrator` or nobody);
- a reply to a question, not from `me`;
- `project.pause` or `project.archive` not authored by `me`;
- for a named session, a `project.update` of the project's `fallback` not authored by it;
- a limit notice (§2.5, a `say` from `sluice` with `data.execution_limit`) to `me`; without
  `me` or in scope `all`, one to any session;
- with `all`, any record in scope.

A step's progress (§6.4) writes no record, so it wakes none of these waits, `all` included.
Notes are held and returned in `notes`. After the first waking record it keeps collecting until
`settle` seconds pass with nothing new, or `settle_max` seconds after the first. Messages come
first in `records`. `settles` sets how much of a settled unit's step outputs are carried:
`short` (booleans, numbers, strings up to 80 characters and a `summary`'s first line; the rest
named in `omitted`), `full` or `none`. Every call records the reader's position and heartbeat in
`readers` (for `me`, its registration, §8.1; without it, under `orchestrator`).

`sluice next` is the same wait (§14), and `sluice watch` follows the log as JSON lines.

### Stored subscriptions

`subscription_set(name, projects, wake, me, say_from_steps="asks-only", digest?)` stores a
home-wide named subscription. `projects` is a nonempty list of project selectors, resolved
once to immutable IDs; `me` is a session name and its existing `mine` ownership scope (§8.1)
applies. `wake` is a nonempty list of `ask`, `reply`, `failed`, `stale`, `settled`, `limit`,
`quota` and `say-to-me`. Names are nonblank and at most 128 bytes. Creation starts at the
selected logs' current end; setting an existing name replaces the filter and reader while
preserving its cursor. `subscription_get(name)` returns its stored filter, reader, projects,
cursor and revision token; `subscription_list()` lists them by name; `subscription_remove(name)`
removes one (a missing name succeeds). These are coordinator tools on CLI and MCP. The CLI
accepts repeated `--project` and comma-separated `--wake` for `subscription_set`.

`next(subscription=name, timeout=300)` (shell: `sluice next --subscription name --timeout S`)
uses that stored filter and cursor. Do not supply projects, since_seq, scope, units, tags or
all; its stored reader applies even when the client session changes. It subscribes before
reading and checks every page of every selected project in one snapshot, returning every
matching record after the cursor, ordered by seq, as soon as one exists. It has no settle
delay: an ask wakes it immediately, regardless of `digest`. It returns the usual
`{records, notes: [], last_seq, timed_out}` plus `suppressed`, a map of suppressed kind to
count (including an empty map). Message kinds are `ask`, `reply`, `say`, step failures and
staleness are `failed` and `stale`, unit settlements are `settled`; other counts use log
kinds. Out-of-scope records are neither returned nor counted.

`ask` and `reply` select messages addressed to the stored reader (including historical
unaddressed and `orchestrator` messages), excluding its own messages. `failed` and `stale`
select those step status transitions; `settled` selects unit settlements. `limit` selects
execution-limit notices to the reader and resource-exhausted step failures. `quota` selects
step failures with `AgentFailure.kind="QuotaExhausted"`. `say-to-me` selects notes addressed
to the reader. For step-authored says, `say_from_steps="none"` suppresses them all;
`"asks-only"` permits them only while the reader has an open question addressed to that
step; `"all"` permits unsolicited updates too. Ask and reply messages remain governed by
their wake kinds. Thus reserving, queued and still-waiting step notes do not wake a default
subscription, even with `say-to-me` enabled, unless the reader requested an update.

Suppressed events are counted until a matching event or timeout returns them. Optional
`digest` (positive integer seconds, or `s`, `m`, `h`, `d`, up to 365d) also returns a digest-only
batch that long after the first suppressed event in the wait. A digest expiry has
`timed_out=false`; a timeout without matching records has `timed_out=true`. Shell output
has one line per matching event and a final `DIGEST suppressed kind=N, ... · seq N` line;
`--json` prints record JSON lines and a final `{digest, seq, timed_out}` line.

The filter and cursor live in `maintenance.settings.subscriptions`, independently of
session read positions and client files. Only a completed batch advances the cursor;
interrupting a pending wait leaves its unreturned events for the next client. Simultaneous
waits or changes to the same name use a revision-token compare-and-set: a losing wait
returns conflict and retries from the stored cursor. Removing or changing a subscription
wakes its pending wait to refuse that obsolete filter. These cursors have the bounded log's
retention: `cursor_expired` is returned if history has been trimmed; remove and recreate the
subscription to start from now. A subscription should have one active reader.

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
  each call has a fresh identity scope. Pass `me` on every request that needs a name: it
  never binds another request, even on the same connection. Without `me` (or a run), the
  caller is unnamed: `ask`, `say` and `reply` are refused, and edits use the ordinary
  author default (§12.3). The reply is the tool's JSON, errors mapped to 400 (`bad_request`,
  `invalid`), 404, 409 (`conflict`, `cursor_expired`), 503 (`busy`), 408 (`cancelled`) and 500.
- **MCP over stdio**: `sluice mcp` serves the same tools, instructions and resources on its
  stdin and stdout, through the home's coordinator (started when nothing answers).
- **CLI**: `sluice tool <name> '<json>'`, or `--field value` flags (§14).
- **Wire**: every tool is a command `{"command": name, "args": {...}}` with a reply `{"reply":
  kind, "data": …}` on the coordinator socket (`sluice tool rpc '<request>'` sends a raw
  request). `docs/rust/schemas.json` (`CommandRequest`, `CommandReply`) is its schema.

The HTTP server binds loopback only. Every route refuses a `Host` that is not `127.0.0.1`,
`localhost` or `[::1]`, and refuses a present `Origin` that is not the same loopback origin.
Bodies are at most 1 MiB and at most 64 requests run at once. A tool call has a deadline of its
wait plus 30 s (`busy`, not retryable, "command deadline exceeded").

### 12.2 Arguments, results and errors

MCP and HTTP arguments are flat; an argument a tool does not take is refused (`bad_request`,
below). `project` is a current name or `id:<uuid>`; `steps`, `tags`, `projects` and `state`
also accept a single string. Waits (`wait`, `timeout`, `settle_max`) are capped at 3600 s.

Arguments are forgiving in the same way on every path that takes them (MCP, HTTP, a fn's
`ctx.tool` and `sluice tool`), and both rules read the command's generated schema:

- An unknown tool, or an argument a tool does not take, is `bad_request` naming it and the
  nearest valid names, at most three: those within edit distance 3 (at most half the given
  name's length, at least 1), and those containing it or contained in it (case, and `-` for
  `_`, ignored): `unknown tool step_contxt; did you mean step_context?`, `step_submit takes no
  argument 'output'; did you mean outputs?`. With no near name, the message lists the
  arguments the tool takes instead.
- Where an argument's schema takes an integer and no string (message ids such as
  `to_message` and `since`, seqs such as `since_seq`, revisions such as `rev` and
  `expected_rev`, limits and waits), at any depth, a string of decimal digits (surrounding
  whitespace ignored) is that integer: `"to_message": "24771"` is `24771`. Any other string
  there is `bad_request` naming the field (`to_message: expected an integer, not "abc"`). A
  field that takes strings keeps its digits as a string. A reply object is returned as is; a non-object reply is wrapped as
`{"result": …}` in MCP structured content; an acknowledgement is `{"ok": true}`.

Errors are `{"error": kind, "message", …}`:

| kind | extra fields | meaning |
|---|---|---|
| `bad_request` | | malformed or unknown argument |
| `not_found` | | unknown project, step, fn, call, unit |
| `conflict` | `current_rev?` | stale revision, state changed, question no longer open |
| `invalid` | `errors` | validation failed; each error has a path |
| `busy` | `retryable` | drain, maintenance, deadline, adopting runs |
| `storage` | | database or I/O failure |
| `cursor_expired` | | `since_seq` outside the log |
| `process_lost` | | a run's process vanished without a result |
| `resource_exhausted` | `resource, limit, peak?` | the run ran out of memory (or, in a release pinned before task-limit hits stopped being fatal, tasks); the guardian stops its payload and records kernel evidence (§2.5) |
| `cancelled` | | cancelled work |
| `fn_failure` | | a fn failed |
| `transient` | | a retryable failure that exhausted its retries |
| `rejected` | | a fn refused the work on purpose |
| `agent_failure` | `kind, session?` | an agent session failed |
| `exited_without_submit` | `session?` | a step's work ended without a valid `step_submit` |

### 12.3 Authors

Every write records an author: the tool's `author` argument when given; else the session's
name (`me`, `SLUICE_ME` or the MCP session's binding, §8.1); else `SLUICE_AUTHOR`;
else `step:<SLUICE_STEP>` when set; else the MCP client's name; else `mcp` (MCP) or `cli`
(`sluice tool`). The dashboard writes as `owner`, capacity calls as `capacity`.

### 12.4 Tool reference

Edit tools share `rev?`, `dry_run=false`, `preview_scope="impact"`, `reason=""` and `author?`
(`plan_edit`, `unit_update` and `unit_remove` require `reason`) and return the edit result
(§6.10) or, with `dry_run`, the preview. Every tool takes `me?`, the caller's session name
(§8.1).
`plan_edit` batches operations. Selection tools take `steps?`
and/or `tags?` (a `unit:` tag selects the unit); naming neither is `bad_request`, an unknown
step `not_found`.

**Projects and fns**

| tool | arguments | result |
|---|---|---|
| `docs` | `topic?` | the topic's markdown, or the index |
| `projects_list` | | `[{project_id, name, description, rev, settings_rev, counts, paused, archived, board_rev, resources?, limits?, icon?}]` (live projects, by name); `counts` maps step status to the plan's steps in it, `resources` each declared resource to `{capacity}` or `{capacity_fn}`, `icon` is `{kind: "image", type}` or `{kind: "text", text}` |
| `project_create` | `name`, `description=""`, `icon?`, `resources={}`, `limits?` (§2.5), `author?` | `{project_id, name}` |
| `project_update` | `project`, `new_name?`, `description?`, `icon?` (`""` removes), `resources?` (each key set, null removes), `limits?` (replaces the project defaults; null clears), `paused?`, `archived?`, `prune_done_after?` (seconds, null turns it off, §6.11), `prune_keep?` (patterns, `[]` or null clears), `fallback?` (`owner` or a registered `@session` that answers unowned units, null clears, §8.1), `expected_settings_rev?`, `reason?`, `author?` | `{project_id, name}`; changes write `project.rename`, `project.pause`, `project.archive`, `project.update` |
| `board_set` | `project`, `program` (null clears), `expected_rev?`, `reason?`, `author?` | `{rev, warnings}`; a stale `expected_rev` is `conflict`, a program that does not check `invalid` with each problem as `line N: …` (a `Slot` is told it was replaced by `Doc`); validation runs before the store write transaction; the same program again changes nothing; writes `project.board`. `warnings` lists each step the program names that the plan cannot give it (`docs("board")`), as `line N: <Component> names step `x`, which is not in the plan` or `… selects `tag:x`, which no plan step carries`, and each Metric, Query or Chart whose SQL compares `status` with `'failed'` and never reads `error` (it counts the owner's cancels as failures: `Count` counts as the dashboard does); it never refuses |
| `board_get` | `project` | `{project, rev, program}` (`rev` 0 and `program` null before any board) |
| `board_doc_read` | `project` | `{rev, updated_at, author, markdown, numbered}`: the board's document (`rev` 0 and `markdown` "" before the first write), `numbered` its lines with right-aligned numbers and a tab, as `cat -n` |
| `board_doc_write` | `project`, `markdown`, `expected_rev?`, `reason?`, `author?` | `{rev, changed}`; replaces the whole document (at most 64 KiB, else `invalid`); a stale `expected_rev` is `conflict` with `current_rev`; the same text again changes nothing |
| `board_doc_edit` | `project`, `expected_rev`, `edits` (`[{start, end, text}]`), `reason?`, `author?` | `{rev, changed}`; each edit replaces lines `start..end` (1-based, inclusive, of `expected_rev`) with `text`'s lines; `end = start - 1` inserts, `start = lines + 1` appends; edits must not overlap and apply all or none; another current rev is `conflict` with `current_rev`, a bad range or an overlap `invalid` naming the edit |
| `project_delete` | `project`, `confirm_name`, `expected_settings_rev` (from `projects_list`), `author?` | `{project_id, name, deleted}`; the project must be archived, and nothing of it live |
| `fn_list` | `project?` | `[{name, doc, inputs, outputs, scope, submits?, icon?, open?}]` |
| `fn_get` | `name`, `project?` | the fn.json plus `scope` and `path` (null for a builtin) |
| `fn_save` | `fn`, `main_py`, `project?` | `{name, scope, path, generation}` |
| `fn_call` | `name`, `inputs={}`, `project?`, `limits?` (§2.5), `wait?` (seconds, default 0), `direct=false`, `author?` | `{call, project_id, status, inputs, outputs, error, direct, limits?, usage?}`; `direct` runs it now through a guardian and ignores `wait` |
| `call_status` | `call`, `project?` | as `fn_call`, plus `finishing?` (§6.4) |
| `recipe_list` | `project` | `[{name, doc, params, scope, stages, title?, view?, title_error?, view_error?}]`, a broken file as `{name, scope, error}`; a view that does not check is `view_error`, the recipe still usable |

An `icon` is a text icon (at most 16 characters), an absolute or `~/` path to an SVG, PNG,
WebP, JPEG or GIF file of at most 256 KiB, which the coordinator reads once and stores, or
`{media_type, bytes_base64}`. A missing or unreadable path is `invalid`.

Each call's status changes are `call` records. `core.external` cannot be called.

A step's run may call `board_set`, `board_doc_write`, `board_doc_edit` (and `board_get`,
`board_doc_read`) for its own project, as it may the plan edits; another project's board is
outside its authority. The document tools refuse (`invalid`) while the board's program has no
`Doc()`. Each document change writes one `project.update` record with `fields`
`["board_doc"]`, its `reason` and `author`, and never moves the board's `rev`.

**Plan edits**

| tool | specific arguments |
|---|---|
| `plan_edit` | `project`, `ops` (operations, below), `start=true`, `me?`; `rev` required with `order.set` |
| `step_add` | `project`, `step`, `spec`, `start=true`, `owner?` (its unit's, when new; §8.1), `me?` |
| `step_update` | `project`, `step`, `changes` (each key replaces that field, null removes it) |
| `step_remove` | `project`, `steps?`, `tags?` |
| `step_pause` | `project`, `steps?`, `tags?`, `subtree=false`, `paused=true` (sets `"paused"` to the `reason`, or `true` without one, a step already paused keeping its own; `false` removes it) |
| `unit_add` | `project`, `recipe`, `unit`, `params={}`, `after={}`, `inputs={}`, `tags=[]`, `bypass={}`, `start=true`, `owner?` (§8.1), `me?` |
| `unit_tag` | `project`, `unit`, `add=[]`, `remove=[]` |
| `unit_update` | `project`, `unit`, `changes` (`{step id: changes}`, each a member; as `step_update`'s) |
| `unit_remove` | `project`, `unit` (every member; running members or surviving references refuse it) |
| `edge_add`, `edge_remove` | `project`, `step` (a step or `unit:<name>`: its entry steps), `after` (entries) |
| `step_set_input` | `project`, `steps?`, `tags?`, `inputs` |
| `plan_prune` | `project`, `units?`, `tags?`, `older_than=0` (seconds), `keep?` (unit-name patterns) |

`plan_edit`'s `ops` are a closed list applied in
order to one candidate, validated once and committed whole: `input.put {name, declaration}`,
`input.remove {name}`, `output.put {name, source}`, `output.remove {name}`, `step.add {step,
spec}`, `step.update {step, changes}`, `step.remove {steps}`, `edge.add` and `edge.remove {step,
after}`, `unit.add {recipe, unit, params, after?, inputs?, tags?}`, `unit.update {unit,
changes}`, `unit.remove {unit}` and `order.set {collection: steps|inputs|outputs, ids}` (every
member once, in the new order). A refused operation is `invalid` with its path (`ops[1].step: no
step x`), all of them at once; `steps` in the reply are the steps its adds added. `changes`
(`step_update`, `step.update`, `unit_update`) take the step keys only; an unknown key is
`bad_request`. `in` replaces the whole input map, with null entries omitted:
`{"in": {"x": null}}` removes `x`; include bindings for any other inputs to keep.
`{"default": null}` remains a binding to a literal null. `in: null` removes the whole map.
`run` may change in the same update; the step keeps its id, position and history.

`step_add` and `step.add` refuse an existing id with `step <id> already exists (status:
<status>)`, using its current stored state.

`step_pause` with `subtree` also selects every step downstream of the selection (reading from
or gated on one, transitively). `unit_add` reports the steps it added in `steps`, `unit_tag`
the unit's steps and `step_pause` the steps selected.

`plan_prune` selects done units (all, or those named or tagged) whose last step finished at
least `older_than` seconds ago, keeps any unit that a surviving step or plan output references
(computed as a closure), and removes the rest in one edit; naming a unit that does not exist or
is not done is `invalid`. A selected unit whose name matches a `keep` pattern (`*` any run of
characters, `?` one; as §6.11's `prune_keep`) is kept too, and its steps hold what they
reference like any surviving step's; without `keep` nothing changes. The reply is the edit
result plus `units` (removed) and `kept`: `[{unit, step}]`, `[{unit, output}]` or `[{unit,
keep}]`, each kept unit with the step, plan output or pattern that holds it; `steps` lists the
removed steps.

**State and values**

| tool | arguments | result |
|---|---|---|
| `plan_get` | `project` | `{project, rev, plan}`; assembled from the plan's rows, without compiling it |
| `plan_read` | `project`, `units?`, `steps?`, `status?`, `recipe?`, `compact=true`, `limit=200`, `cursor?` | `{project, rev, state_epoch, recipe_generation, steps, next_cursor}`: the matching steps in plan order, compact `{id, unit, recipe, position, run, status, paused, priority}` or full (plus `spec`, the declaration, and `references`) |
| `step_get` | `project`, `step`, `compact=false` | `{project, rev, state_epoch, step}` |
| `unit_get` | `project`, `unit`, `compact=false` | `{project, rev, state_epoch, recipe_generation, unit: {id, recipe, entry_steps, exit_steps, done, settled, steps}}` |
| `plan_history` | `project`, `since_rev?`, `after_seq?`, `limit=200` | `{project, entries, next_after_seq}`: every edit (`plan.edit`, from the baseline revision) and the log's `plan.input`, `step.output`, `step.retry`, oldest first |
| `plan_set_input` | `project`, `name`, `value`, `rev?`, `dry_run`, `reason`, `author?` | `{ok: true}`, or the preview |
| `step_set_output` | `project`, `step`, `outputs`, `force=false`, `reason`, `author?` | `{ok: true}` |
| `step_bypass` | `project`, `step`, `outputs={}`, `off=false`, `reason`, `author?` | `{project, step, status, bypass, applied}` (§7.7) |
| `step_retry` | `project`, `steps?`, `tags?`, `message?`, `reason`, `expected_rev?`, `author?` | `{project, steps, rearmed, stopped_at}` |
| `step_cancel` | `project`, `steps?`, `tags?`, `reason`, `expected_rev?`, `author?` | `{ok: true}`; running, pending `core.external` or failed steps (§7.5) |
| `step_submit` | `project`, `step`, `run`, `outputs`, `author?` | `{ok: true}` |
| `step_progress` | `project`, `step`, `run`, `outputs` | `{project, step, run, progress, at}` (§6.4) |
| `lease_acquire` | `project`, `run`, `resource`, `amount=1`, `wait=false`, `timeout?`, `priority?` | `{lease, resource, amount, priority, state, position, timed_out?}` (§7.6) |
| `lease_release` | `project`, `run`, `lease` | `{lease, state: "released"}` (§7.6) |
| `lease_status` | `project`, `resource?` | `{resources, parked}` (§7.6) |
| `park` | `project`, `run`, `until`, `timeout?` | `{parked, until, lease?, position?, timeout_seconds?, message}` (§7.6) |
| `step_settle` | `project`, `step`, `reason=""`, `author?` | `{project, step, run, outputs}` (§7.5) |
| `status` | `project`, `steps?`, `tags?`, `brief=false`, `all=false`, `view="steps"`, `state?` | below |
| `step_context` | `project`, `step` | below |
| `plan_view` | `project`, `format="mermaid"`, `all=false`, `units?`, `steps?`, `status?`, `recipe?` | text |
| `verify` | `project?` | `[{where, message}]` |

`status`, steps view: `{project, rev, board_rev, paused, inputs, outputs, resources, execution, steps: {id: {status,
outputs, error, run_ids, done, total, instances, manual, limits?, run_resources?, budget_error?, paused?, queued?, waiting?,
finishing?}}, done_units?}`; a finishing step (§6.4) stays `running` and carries `finishing`.
Every plan step has a status, including a step that never started or whose file input is
missing. Without a selection, done units are left out and counted in `done_units: {units, steps}` unless
`all`. `brief` cuts strings over 200 characters (`… [n more characters]`). `resources` maps
each to `{capacity, held, queued, error}`. `execution` reports the home-wide physical
`total`, `held`, `unbounded_attempts` and capacity `reason`, with memory bytes, milli-CPUs
and tasks. When policy configuration is invalid, `execution.error` reports the diagnostic;
status remains readable. A pending step's `limits` describes the currently resolved admission
budget, or `budget_error` explains why it cannot resolve. `run_resources` is an array of
`{run, limits, usage, memory}` from the step's stored runs: limits are frozen, usage is completion
evidence, and `memory` includes live working memory and file cache (§2.5). Unavailable
historical evidence is null. For a step that is no longer pending,
`limits` comes from its latest run. Frozen limits and recorded usage do not change when home
or project defaults change; live memory readings do change. Call replies expose the same frozen `limits` and recorded `usage`
when present; usage fields are optional `memory_working_peak_bytes`, cache-inclusive
`memory_peak_bytes`, `oom_kills`, `pids_limit_hits`,
`memory_max_hits` and `cpu_throttled_usec`. These are recorded observations, not a live usage
feed. In `status`, `limits` and `execution.total` show an unlimited dimension as `"unlimited"`.
`paused` is `true` or the pause's reason. Every
pending step that is not about to start has `waiting`, the reasons in order: `paused` or
`paused: <reason>`, `project paused`, each handoff not ready (`step <id> is <status>`, `plan
input <name> has no value`), each gate not satisfied (`after <entry> (<why>)`), the resource
shortfall (`queued: needs lane 1 (4/4 held)`, with `queued` listing the resources), and for a
`core.external` step with nothing else, `external: set its outputs with step_set_output`. Units view (`view: "units"`, `brief` refused): `{project, rev, board_rev, paused, resources?, execution,
units: [{unit, title, recipe, state, shown, age, engine, steps, blocked, last, line, finishing?, parked?}], done_units?}`
(`recipe` the matched recipe's name or empty; the steps view gives each step `title` and, in a
unit of more than one step, `stage`);
`finishing` lists the unit's finishing steps `[{step, since, submission_seq, release}]`, whose
mark in `steps` is `▷` and which `line` names (`finishing <step>`) when nothing blocks it; `state` one of
`failed` (a step failed or stale, checked first), `parked` (all its running steps parked),
`running`, `settled`, `blocked` (held: every
pending step paused, its project paused, outside work, a plan input with no value, or behind a
failed, stale or held step), `queued`, `pending`, filterable with `state` (`state` is refused
in the steps view). `steps` marks each step with the status table's lane mark (§13) as the
store knows it: ✗ failed (a cancel too: tools keep `failed`), ~ stale, □ stopping (a cancel
asked for), ▷ finishing, Ⅱ parked, ▶ running, ↗ outside (ready `core.external`), ‖ paused (its own or its
project's pause), ⊖ blocked (behind a failed or stale step), ∅ held (a plan input with no
value), ≡ queued, · pending, ✓ succeeded, – skipped; `line` leads with the mark of the unit's
first step in the table's order.

`step_context` (also `sluice me`): `{project, project_id, step, fn, doc, status, started,
finished, elapsed, run, inputs, upstream: [{step, fn, status, outputs, error}], messages (open
questions on its thread), submit: {outputs, command, note}, thread, ask, needs?, queued?,
leases?, finishing?, attempt?}` (`finishing` §6.4, `attempt` §7.4); `submit.command` and `ask` are ready-to-run `sluice tool` lines; `submit.note` is
"Submit only when you are finished: submitting ends your session."

`plan_view`: Mermaid `flowchart TD` with one subgraph per unit, nodes labelled `id / fn /
status [done/total] / doc`, a class per status, done units left out with a `%% n done units (m
steps) left out` comment unless `all`; or `format: "html"`, a standalone page with an SVG of the
same graph.

`plan_read` filters by `units`, `steps`, `status`
(stored statuses) and `recipe` (the units it matches now): a list matches any of its values,
filters combine with AND, an empty list matches nothing, and a name that matches nothing is no
error. `limit` is 1 to 1000 (larger reads as 1000). `next_cursor`, passed back as `cursor` with
the same project and filters, reads the next page; it is `cursor_expired` once the plan's `rev`
changed (or, for a `status` filter, its `state_epoch`; for a `recipe` filter, its recipes).
`step_get` and `unit_get` of a missing step or unit are `not_found`. `plan_view`'s filters
select what it draws; a step outside the selection that an edge joins to it is drawn as an
`outside` boundary node. `plan_history` pages: `next_after_seq`, passed as `after_seq`, reads
on.

**Messages and the log**

| tool | arguments | result |
|---|---|---|
| `ask` | `project`, `to?` (a session must give it), `body`, `title?`, `ui?`, `input?`, `data?`, `run?`, `me?` | `{id, to, thread, delivery, run?, note?}` (§8, §8.1) |
| `say` | `project`, `to?`, `body`, `data?`, `run?`, `me?` | `{id, to, thread, delivery, run?, note?}` (§8, §8.1) |
| `reply` | `project`, `to_message?` or `to?`, `body=""`, `answer?`, `run?`, `me?` | `{id, to, thread, delivery, run?, note?}` (§8) |
| `messages` | `project`, `view="inbox"`, `thread?`, `since?`, `owner=false`, `me?` | `{project, messages, last_id}`; the inbox and history are `me`'s (§8) |
| `log_read` | `project?`, `since_seq?`, `kinds?`, `threads?`, `statuses?`, `recipients?`, `limit=200` | `{records, last_seq}`; without `since_seq` the latest records; filters §9 |
| `log_wait` | as `log_read`, plus `timeout=300`, `wake="any"`, `me?`, `scope?`, `units?`, `tags?` | `{records, last_seq}` (§10) |
| `step_wait` | `project`, `steps?` or `tags?`, `until`, `timeout=300` | `{met, steps, seq}` (§10) |
| `next` | §10; `subscription?` resumes a stored filter | `{records, notes, last_seq, timed_out, suppressed?}` |
| `subscription_set` | `name`, `projects`, `wake`, `me`, `say_from_steps="asks-only"`, `digest?` (§10) | stored filter, cursor and revision token |
| `subscription_get` | `name` | stored subscription |
| `subscription_list` | | subscriptions ordered by name |
| `subscription_remove` | `name` | acknowledgement |
| `query` | `sql`, `params=[]`, `limit=200` | `{columns, rows, truncated}` |

`query` runs one read-only statement on a fresh connection: at most 100,000 bytes of SQL, `limit`
1 to 1000 rows, a 2 s deadline, a 1 MiB response; writes, `ATTACH`, extensions and unsafe
functions are refused. JSON columns come back as JSON text.

**Owners**

| tool | arguments | result |
|---|---|---|
| `unit_claim` | `project`, `unit?`, `units?`, `tags?`, `owned_by?`, `expected_owner?`, `force=false`, `reason?`, `me` | `{project, units, unchanged, questions, notices}` (§8.1) |
| `unit_release` | `project`, `unit?`, `units?`, `tags?`, `owned_by?`, `to?`, `force=false`, `reason?`, `me` | as `unit_claim` |

**Maintenance**

| tool | arguments | result |
|---|---|---|
| `drain` | `projects?`, `author?` | `{paused, status: {mode, owner, paused, blockers, pending_calls, open_questions, drained}}` |
| `release` | `author?` | `{released}` |

### 12.5 Commands outside MCP

The wire also carries `mark_read` (advance a reader's position on a thread), `step_dismiss`
(`project`, `step`, `dismissed`: the owner sets a cancelled step aside, or back; kept as the
owner's `readers` row on the stream `dismissed`, its `cursor` the step's `work_generation`,
its `unread_alert_min` the step's `generation`, and its `heartbeat_at` when it was set.
A failure becomes a cancel to dismiss by `step_cancel` (§7.5).
It holds while that key is unchanged and the step is still failed as a cancel, regardless of
record retention or later project activity. Retry increments the work generation; removing
and recreating the step changes its generation. Writer startup rewrites legacy dismissal
rows, identified by a null `unread_alert_min`, to the current cancel's key without changing
`heartbeat_at`, and removes legacy rows for steps no longer cancelled. No record is written;
only a cancel can be dismissed), `needs_dismiss` (`project`, `items`, `dismissed`: the owner
sets items of a plan's Needs you aside, or back; an item is `{key, stamp}`, its key `stop:<step>`,
`quiet:<step>` (its `stamp` the run's last activity, seconds since the epoch) or
`ask:<message>`; one to 500 items. Kept as the owner's `readers` rows on the stream `needs`, a
row an item (its `thread` the key, its `heartbeat_at` when it was set aside): a stop's `cursor`
is the step's `work_generation` and its `unread_alert_min` the step's `generation`, so a retry or
a recreated step brings it back; a silence's `cursor` is its stamp, so a run that writes and
goes quiet again comes back; a question's holds while it is open. It changes nothing but what
the plan lists: no record is written, the inbox still holds the question, and the tab title still
counts it), `backup`, `builtin`
(guardian-authenticated only), `submission`, `acquire_lease`, `release_lease` and
`register_completion_action` (run callbacks only).


Read the unit or steps you need with `unit_get`, `step_get` or `plan_read`. Use the typed tools for single changes and `plan_edit` for an atomic batch. Pass `rev` when a change depends on an earlier read. `plan_get` exports the whole plan. A preview describes the edit's affected work; ask for a full dry run explicitly. An edit refused `busy` is retried as is.

## 13. Dashboard

`sluice serve` serves the dashboard on the same port. Pages (look: `DESIGN.md`):

| route | page |
|---|---|
| `/` | every project with steps as a card. Its head (the bar names the page; "All projects" its heading for a screen reader) is one sentence of counts alone across them: the open questions to the owner first, linked to For you, then each state that needs a look by its word, then how many run in how many projects ("3 questions for you, 2 failed, 1 quiet; 63 running in 4 projects."). "Runner stopped" (nothing new starts) sits under it while nothing holds the scheduler lease (no `loop`, no `serve` without `--no-runner`). For you lists every open question to the owner someone waits on, oldest first, as a line of the Needs-you list: its teal dot, its project and who asked (the asking step by title, else the session by its `@name`, or "orchestrator", muted, for one from before sessions named themselves), its title linking where it is answered, Answer and × (which sets it aside in its plan's Needs you). One answered or closed in the last ten minutes stays as the line that says so. Beside it, Today has a line a project of its last 24 hours of runs (how many, how many ended needing a look, what runs now) and the way to `/day`. Then each project with steps as a card, ordered by what needs the owner (a question to the owner, then something stopped, running, waiting, done, a paused one after the waiting). A card is its plan in brief: its name, its state's glyph, its units and steps counted, its description, its plan's summary sentence, At work's groups a line each (as the plan counts them, its remarkable units named), "Finished lately:" and the last three finished by name (each its unit's link), and the way to its plan and its day. Under the cards, a line names the projects with no steps yet and another the archived ones, their steps counted; no table repeats the cards' counts |
| `/day`, `/projects/id/<p>/day` | the day: every step's run of the last 24 hours (across the projects, or one project's), from the runs alone, its hours on the reader's clock (the `sluice_zone` cookie `nav.js` keeps, minutes east of UTC; UTC until it is set). Its head: the date (the bar names the page); over the day line one sentence (how many runs and in which project, how many ended failed or cancelled, what runs now and the longest, how many succeeded in under a minute). The day line: a row a project, each run a bar from its start to its end (a running one to now), stacked into tracks where runs overlapped, with the hours along the top and a rule at now; a run that succeeded in under a minute is counted, not drawn. The timetable: a row an hour, the current one last with the rule at now, a column a project (the busiest first; stacked under each hour on a narrow screen): each run by its start minute (a run that started before the window, in the first row, by its day and time), its unit's title and its stage (its step and run number in the link's title), how it ended (its glyph and, but for a success, its word; a run still going as its step reads, running, quiet, stopping or finishing) and how long it took (a running one ticking), linked to its step's Runs (a step its plan has since retired, to its records on the project's log, `log?step=`); a busy hour (more than six runs in a project's cell, quick ones aside) lists its notable runs (any that did not succeed: failed, cancelled, running or ended with no result; any that took an hour or more) and folds the rest, every one a success, behind "+23 more succeeded, 1m to 12m", a `details` that opens in place and stays open through a patch (its longest is listed when none is notable); past eight listed, the runs still running fold the same way behind "+14 more running", every one needing a look kept in sight; quick runs are counted; a cell's runs flow into as many columns as the frame's width allows; two or more hours in a row in which no run started are one line, "03–14 · nothing ran". Read-only; its stream patches only when the runs (or the nav) change, and on the hour |
| `/projects/id/<p>/stats` | a project's stats over a window (`window=24h\|7d\|30d\|all`, 7 days by default and for anything else; 24 hours to the end of the reader's hour, 7 and 30 days whole days on the reader's clock, all from the day of its first run), from its runs, steps, units and recipes alone: the one place a stage's usual time is said. Its head (the bar names the page): how many runs ended and units finished in the window, the window switch beside it, links. **Durations by stage**: a table a recipe, in the project's recipe order, of the recipes whose units ended a run in the window (its name, how many units and runs), a row a stage in the recipe's order: how many of its runs succeeded, their median (the mean of the two middles for an even count), p90 (nearest rank) and longest, the median drawn inside the p90 on one scale for every table (minutes on a log, to the longest p90 of any stage); a unit the plan holds goes with the recipe its naming matched, one it has retired with the recipe whose stages hold every stage its runs ran with the fewest left over, and the units of no recipe and the loose steps form "Without a recipe", a row a step (a unit's step by its stage, a loose step by its id; the busiest first). **Outcomes**: the same tables, a stage's runs that ended in the window as succeeded, failed and cancelled (the status table's reading of each run's result), retried (a run that is a second or later try of its step and item), the failure rate (failed of those that ended) and its failures by kind in words (`views::failure`). **Throughput**: units finished and started a bucket (an hour for 24 hours, a day, or a week for an all of more than 92 days): a unit (a loose step one) starts with its first run and finishes with its last run's end once every step is done (the plan's steps all succeeded or skipped; a retired unit's every step's last run succeeded); one sentence, a column a bucket and every count in a table for a screen reader. **The slowest runs**: the ten longest that ended in the window, by its unit's title (a loose step's) and its stage, how it ended when not a success, when, linked to its step's Runs (a retired step's records on the log), its step, run, try, unit and item in its Details. Read-only; its version is what it draws, so its stream (`stats/stream?window=`) patches only when that changes |
| `/projects/<name>` | redirects (307) to `/projects/id/<uuid>` |
| `/projects/id/<p>` | the plan (**The plan**, below); on a project with a board, `view=plan\|both\|board` draws that view and keeps it in the `sluice_view_<p>` cookie, which a later load without `view=` draws (the page menu's view links, the server the view's one source); with none kept, or a kept Board while the address asks the plan something (`q`, `show`, `root`, `recipe`), the page is drawn with no view and the window's width picks one: both from 1280 px, the plan between, a phone the board unless the plan comes first (`data-plan-first`: something needs attention or a pause, or the address asks the plan something); query `show=all\|active\|attention\|done` (which units: every one, not done, with a step in a state that needs attention (failed, cancelled, stale, quiet), done; a board it leaves empty says how many units it hid, "12 units hidden by Show: Attention.", with a "Show all" link to the same view under `show=all`), `q=` (a find: the units whose id or title, or one of whose steps' id, title or doc, contain every word of it, any case and order, at most 200 characters, each kept whole; the others hide; the page says how many units matched), `tag=`, `recipe=` (the units that recipe made, done ones too, the Done index drawn open with every one; a line over the board counts them by state, "4 units · 1 running · 3 done", with "Show everything"), `root=<step>` with `up=1`, `down=1` and `depth=N` (a step's chain: the step, what it comes after (`up`) and what comes after it (`down`), both when neither is given, at most N steps away (0 or none: any); the units holding a step of it are drawn, whole, the others hide; a line over the board names it, "Showing the chain of <title>", or says the step is no longer in the plan, with "Show everything"; the tools' form carries these as hidden fields), `format=mermaid` (the `plan_view` Mermaid; `all=true` keeps the done units), `tab=` (with `#step:<s>`, the tab the drawer opens that step on: the drawer writes it while a step is open on a tab past Overview and takes it out when it closes; the board's own query leaves it alone). They combine; its `…/stream` takes the same query and draws the plan under it. Every done unit (every step succeeded or skipped) is counted under At work ("25 done, the last
2h ago", linking `show=done`), and is a line of the Done index under At work when `show=done`, a
find, `recipe=` or `root=` asks for it: "n units · m steps · the last finished 2h ago", every
one newest first, each a line of its finish time, glyph, title and how long it took (a dismissed
cancel "b cancelled and dismissed"). No line is drawn between units or steps: a waiting unit's
row says what it waits for in words (each source by its title, linked, and what it is doing). On a phone the page opens on the plan while a step needs attention (failed, cancelled, stale or quiet) or is paused, else on the board |
| `/projects/id/<p>/units/<u>` | one unit, its head under the frame's row: its way back, its title with its Details (its id, recipe and params), its steps summed by state ("6 steps · 1 running · 4 pending · 1 succeeded"), a short recipe view on that line, its owner (`@lash-f4`, "you" for the owner; nothing when it has none), and a link to its log (`log?unit=`); then on the sheet its recipe's view drawn whole when it says more than a line (below; a view of params alone draws nothing), its step view (below), and a module for each step that needs the owner, in the view's order (the one asking on the teal rule, answered in place; a stop on the ochre with why it stopped), each headed by its stage with its Details |
| `/projects/id/<p>/steps/<s>` | one step, its head under the frame's row (in the drawer the same head tops the step): a way back to its plan and unit, its stage and title the heading (two lines at most, the whole in its tooltip) with its one "⋯" beside it (its Details: its id, unit, recipe, fn, run and earlier runs, tags; then its Actions: Pause or Unpause, Cancel, a failure's Set it aside, its log (`log?step=`) and, with no thread yet, Message it; the tab its stage and title, led by "Question ·" while its own question to the owner waits, kept by each patch), a pending or stale step retried since its last run started saying so under its header ("You retried it 2m ago, with feedback: <its first line>."; a pending step nothing holds says on Overview that it is ready and when it starts: "Ready: nothing holds it, so the runner starts it next.", or "Ready, but the runner is stopped: it starts when `sluice loop` runs."; a failed step whose run before failed the same way says so, and its retry feedback box stands
open under that advice in why it failed rather than folded in its head), its status line (one muted sentence after its state's glyph: its state and since when, "Running 6h 34m · heard 1h 18m ago · run 2, after 1 failed"; while it runs when it was last heard from, an ochre "quiet 3h" pill and "not heard from since …" once quiet, "waiting on your answer" leading to its question while one to the owner waits (the teal dot in place of the glyph), else "N awaiting reply" leading to Messages; once ended its word and when ("Failed 2h ago · after 18m"; from its result when no run is kept; a pending step that ran "its last run ended 12h ago · after 3m"); its run when it has had more than one; its items done; a waiting bypass), never against a usual time, the rest of its doc, its unit's stage strip when the unit has more than one step (its own stage ringed, `aria-current="step"`), and only its next moves (Retry with feedback, its box folded under it, then Retry: the first primary on a
failed step, not on a cancelled one, which has a Dismiss instead; on a failure that repeated
(below) its head's Retry with feedback leads to the box standing open in why it failed, focusing
it, and the bare Retry is outlined; asking first on a succeeded one, which says how many steps come after it; and a failure's "To resume …" hint under the status line), then tabs, each only when it has something to show: **Overview** (always; why it failed first on a failed step (one sentence from its failure kind, or a fn's from its traceback's last exception, a wall-clock cap's as the agent's; a link to the failure's own log record, `log?step=<s>&before=<seq+1>#r<seq>`, which the log marks; what it said and a link opening Activity on its last failed call, then
what to do next (`failure::Advice`, one decision from the step's own failure and its last two
runs' results, read with its card's timer, so its plan card, the drawer, its band and Overview
say the same: the run before failed the same way, "Run 1 failed the same way, so a bare Retry
would most likely fail again. Retry with feedback, or change its inputs."; else its kind's
remedy, an engine that exited partway told apart from one that would not start; nothing for a
cancel), and, when the failure repeated, the retry's feedback box open under it; a cancel that
set a failure aside says "It had failed: <kind>: <first line>" with a link to its run, whose Runs
entry keeps its failure and says "Cancelled after it failed, by you: <reason>."; its latest own message; the traceback and the pane at failure folded; its run's files), while it runs what it is doing now, live first (its latest turn: "Turn 5 · 49s", or how long it has run under the live dot, then what it said last at reading size, then its calls in a closed fold, "5 tool calls, 1 failed", opening to its latest six as one-line rows (the tool, its key argument clipped, how long it took or "failed"), each opening the Activity tab on that call; under Now its brief, the first paragraph of its prompt (`spec`, `prompt` or `task`, cut at about 280 characters) with Whole brief opening Inputs, and So far, the outputs it has set (from an earlier run said so) or how many it has not set yet, with All outputs opening Outputs; a step only waiting on its question to the owner has no Now: the question is all it does; then, on a retry after a failure or one somebody asked for, in plain words how the run before ended and who started this one, "Run 3 started 1h ago, after run 2 failed: <its sentence>. Sluice retried it on its own." (a cancel's "cancelled by cli: <reason>", as the run keeps it (`runs.stopped`) or else its
record; "You retried it: <reason>, with feedback", the feedback linked; that run linked in Runs; a completion action's own wording never shown); its live progress; its own latest message to anyone and the latest to it since, as message cards: who to whom, when, its first words and "Read it whole" when cut; an answered question to the owner whole, its answer the card after it), what holds it (each step whose outputs it takes and that has not succeeded, by title and id, linked, with how it reads; a pause in words: "Paused by <who>: <reason>." from the plan edit that paused it (the edit that made its latest pause transition, read from the plan's history; an edit that only carried the pause along, or a pause of a step of the same id removed since, is never credited), "Paused." when none names who, "Its project is paused."; when every step it waits on is pending too, the first step up its chain that holds it, neither pending nor done, "Held up by <title> <id> (running), before <its wait>"; and while no runner holds the scheduler lease, "The runner is stopped: nothing starts until `sluice loop` runs.") and what it comes after, queued, skipped, finishing, outside, progress kept from an ended run (§6.4), its key output (the first set of `summary`, `result`, `report`, `final`, `answer`, `verdict`, else up to three short outputs, else the first; under a line saying which run set them, a running step's "before this run"), a finished step's last message, a link to its chain on the plan (`?root=<s>&up=1&down=1`) when it comes after a step or one comes after it), **Activity** (an agent run's, below, its turns counted), **Messages** (`?tab=messages`, `?tab=thread` still opening it; its conversation, every message counted: its own thread `step-<s>` and every message it sent or was sent on another thread, its latest 60, a question's replies under it, with a message box to it; its messages counted), **Inputs** (each value and where it came from: a default, a step's output linked, a plan input, a file), **Outputs** (set of all: those not set yet named on one line; which run set them, "From run 2 · 1h ago", and an output another run set says which; a long value equal to an earlier one's says "Same as <name>"), **Runs** (its unit's step view first, its own row ringed; each run its ordinal, outcome (a cancelled run's one line from who stopped it, as the run keeps it or else its record:
"Cancelled after 1h 18m by cli, which retried it."; a run after the first says what started it, from the retry kept on the run before it (or
recorded) or its going stale: "You retried it: <reason>, with feedback.", "`fig-5571-land` sent it back, with feedback." (a completion action), "Sluice retried it on its own.", "An input it reads changed, so it ran again."), an agent run's calls by tool, kind in words, what it said (not on a failed step's last run, whose words lead the page), outputs, engine, session, run id and files; "No run of it is kept" when none is). `?tab=overview\|activity\|messages\|inputs\|outputs\|runs` opens a tab (Overview when the step has no such tab); an anchor in a tab (`#run-3`, a call's, `#message-<id>`, `#activity`) opens its tab on it; the chosen tab is the page's `tab` signal, so every stream patch draws it chosen. `?activity=all` draws every turn |
| `/projects/id/<p>/runs/<run>/files/<name>` | one of a run's files as a page (a way back to its step's Runs, the run, its text with terminal escape sequences taken out; a long one "Jump to end" to `#file-end` under its last line, where a live run's file opens from its step's links), or with `?raw=1` as plain text, read-only: `pane-at-failure.txt` (its newest invocation's), `stderr.log`, `stderr-tail.log`, `summary.txt`, and an engine's record there, masked (`account::redact`): `codex.log`, `codex-wire.jsonl`, `devin-hooks.jsonl`, `devin.log`, `devin.json` (the run's own, else its newest invocation's); only a run of that project, only from its own directory, never through a link; a large file's last 2 MiB; a run name that is no run id is a 404 page |
| `POST /projects/id/<p>/steps/<s>/actions` | `action=pause\|unpause\|retry\|cancel\|cancel_dismiss\|dismiss\|undismiss` (`cancel` on a failure sets it aside, §7.5; `cancel_dismiss`, only on a failure, is that cancel and then its dismissal, one click for both: a dismissal refused after the cancel says so and leaves it cancelled), `revision` (none for dismiss and undismiss, which edit no plan), `seen` (the step as the page drew it: `<state>:<paused>:<runs>`), `message` (retry feedback, a cancel's reason), A stale `revision` with `seen` still matching applies at the current revision; otherwise nothing is done. A page's form (a Datastar `@post` naming itself in the `sluice-form` header) is answered in its own notice: a write taken empties it, a refusal says why there as an alert, what was typed kept (a 200, since Datastar applies only those); any other client gets 204, or the JSON error at the refusal's status (409 for a step that changed or an action that no longer applies, 413, 503 while the coordinator does not answer) |
| `/inbox`, `/questions`, `/history` | the message views across projects (the nav's Inbox, whose count is the questions put to the owner that someone still waits on: "Questions for you" on both views, the inbox saying that Questions also lists those between agents and those nobody waits on); Questions lists those, then "Between agents" (one agent's question to another, answerable as the owner), then the ones nobody waits on; History lists every thread with a message to or from the owner, the latest first. All three head the page with its name and, under it, the switch For you · Questions · History, the page's one row of tabs (the frame's row is the site's or the project's) |
| `/projects/id/<p>/{inbox,questions,threads,thread}` | the same for one project (its nav section Messages, its inbox titled "Messages"); the views' switch reads For you · Questions · History; each question a one-message conversation as a step's Messages tab draws it; `thread?thread=<name>`, under a way back to its plan and its step (or History, an empty thread too), with its message box even before anyone wrote in it (a step's "Message it"), its Details its thread and step ids, a step's thread headed and titled by its step's name as the step's own page has it ("work · Fix the cron driver"; `StepRef`, the one name a step has in words: its page, its thread, every message box's "Message to …", the log), with its message count (and, on a step's thread whose step's conversation holds more, "2 in this thread · 11 with this step", a link to its Messages tab), "Jump to latest" and, while notes in it are unread, a button to mark them read, then its conversation (below) and a message box to its step (while the plan has it; to a step not running, a line saying when it reads it) or, on a session's thread (`@lead`, headed "You and @lead"), to the session; the `owner` thread from before sessions named themselves has none; a page arriving without an anchor scrolls to the newest message |
| `POST /projects/id/<p>/needs` | Needs you's own forms: `action=dismiss\|undo\|retry`, `items` (space apart: `stop:<step>`, `quiet:<step>@<stamp>`, `ask:<message>`, `cancel:<step>`; for `retry`, `stop:<step>@<seen>`, `seen` as a step's action form carries it), `revision` (for `retry`). `dismiss` and `undo` set the items aside or back: a cancel as its step's Dismiss does (`step_dismiss`), the rest by `needs_dismiss` (§12.5). `retry` retries each step as its own Retry would, the first refusal answered as that step's action's is; answered as a step's actions are (its form's notice, or 204 and the JSON error) |
| `/projects/id/<p>/history` | Finished units, newest first, grouped by the reader's local day (“Today”, “Yesterday”, “Oct 20”, adding the year for older years). Each linked row opens its unit or loose step page and shows outcome glyph, engine chip, one-line title with ellipsis, faint finish time and duration on the right. Includes succeeded, failed and cancelled units once all their work is terminal; a retry removes a unit until it finishes again. Renders at most 50 rows per page; `?page=N` (zero-based) and Newer/Older links browse the rest. Empty: “Nothing finished yet”. `/projects/id/<p>/history/stream` patches the list and navigation. |
| `POST /projects/id/<p>/history/seen` | Mark as seen keeps `through` (the newest finish time on the displayed page, epoch seconds, never moving an existing mark backward) in this browser's `sluice_seen_<p>` cookie for 400 days. The form response sets the page's `seen` signal and reopens its stream; other clients receive 204. Rows newer than a kept mark have a quiet dot; all rows stay visible when marked. “Seen through 04:21 · Undo” posts `through=0` to clear the mark. No store write; each browser keeps its own look. |
| `/projects/id/<p>/ref?r=<kind>:<key>` | a hover card's body, HTML for `#hover-card`: `step:<id>` (its state, fn and unit), `sha:<hex>` (the steps whose outputs name the commit) or `t:<ticket>` (the units whose titles name it); 404 for any other kind |
| `POST /projects/id/<p>/messages` | post an answer or reply as `owner`. A question's Close question acts immediately with the action `close`. A muted Undo line remains for ten minutes; action `reopen`, as owner, restores its open, answerable state only inside that window after an owner's close. Both actions are recorded replies: the asker is told it was closed, then “Question reopened.” The close stays in the thread; reopening does not restart a stopped run |
| `POST /projects/id/<p>/messages/read` | mark messages read (`thread`, `through`); the pages send it only when the owner asks: a note's Mark read, Mark all read, or the thread page's button. Never on render, on sight or on opening a thread. The inbox lists the notes read in the last 24 hours under "Read today", from each message's `read_at` |
| `POST /messages/close`, `/messages/reopen` | close or reopen several questions immediately as `owner` (each `m=<project>/<message>`); each closed question has Undo for ten minutes, with Undo all for recent closes |
| `/log`, `/projects/id/<p>/log` | the log, 50 records a page, each in a sentence (a failure as its step page says it, a cancel as a cancel, never JSON; the record under it), filtered by kinds, threads (`thread=`), one step (`step=`, its records and its thread's messages), one unit (`unit=`, its steps' records and threads and its own `unit.settled`) or failures (`errors=1`: steps that failed, failed calls, orphaned runs); a fn call that succeeded is left out unless `kind=call` asks for calls, a status record that changes nothing unless `kind=step.status` does, a lease record unless `kind=step.lease` does, a notification's delivery (`project.notify`) unless `kind=project.notify` does; a settled unit's row holds its steps' status changes in the six hours before it ("Unit u settled, 5 steps · 9 status changes", those linked to `log?unit=`, which lists each), and a question to the owner reads by its title ("fig-5576-work asked you: Land l1 first?"); any other message names its sender and recipient as pages name steps ("work · Ship the cron fix `fix-cron` to @lash-f4: …", the owner "You", a session by its `@name`, "orchestrator" muted on older messages), then its first words as plain text and one small link to its thread after them, and one note sent to several within a minute is one row ("… to 6 steps: Heads up…"). `/log` holds every project's records, each under its project's name. A sentence links its step, unit and thread; records in a row that say the same are one row counted ("×10"); `errors=1` leaves out the owner's cancels; a sentence is cut at 240 characters; sluice's own plan edits that retire done units within half an hour of each other are one row, whatever came between ("sluice retired done units 4 times: plan revs 1209 to 1212, 219 changes", each change a step retired); a step's status changes within a minute are one row ("x pending → running → succeeded"), a failure always its own; a page with no records names the filters that left them out, each with a link that drops it |
| `/fns` | the functions a project (`?project=`) or the home sees |
| `/projects/id/<p>/settings` (GET, POST), `/preview`, `/icon`, `/delete` | project settings, under an index of its sections (Details, Resources, Board, Retiring, Activity, Delete). Each section is one form with one Save (a Datastar `@post`, its fields as a form, `section` naming it; Details as multipart, for its image): Details (name, description, icon as letters or an image, fallback), Resources (each resource's capacity, a new one, Remove immediately with Undo for ten minutes), Retiring, and Activity (Pause or Resume). Each form carries `seen`, the highest record seq among its fields when it was drawn; a Save is refused when one of its fields changed after that ("Not saved: the description changed since this page was drawn (by cli). Reload to see it, then save again."), never writing over the change, and otherwise applies at the current settings revision, so a change to another section never refuses it. The answer is the form's notice: "Saved." as a status, a refusal as an alert, the page's stream drawing what changed; the forms themselves are not patched, so what is typed stays until it is saved; dashboard deletion alone is guarded, in inline details with a final button naming the project, archives first if needed, and calls `project_delete` with the known name and current revision; live work blocks deletion; a resource's capacity is a number or the fn that reports it (the form's `capacity=number\|fn\|remove`, `number`, `capacity_fn`); Details hold the project's fallback (`field=fallback`: a select of its registered sessions and "none", which clears it; the owner kept as "you" while it is the fallback) and, while any are registered, its sessions on one line, each its `@name` and when it was last seen (its reader's last heartbeat or its last message, whichever is later: "@lash-f4 · seen 2m ago") |
| `POST /projects/id/<p>/settings/board` | the Board section: `op=save\|clear`, `program`, `expected_rev`; clear acts immediately and leaves Undo for ten minutes, restoring the previous program with its document revision preserved; a newer board edit invalidates Undo; its form's notice says the outcome (a save's warnings, a refusal) |
| `POST /projects/id/<p>/settings/undo` | `key=board` or `resource:<name>` restores a saved clear or removal within ten minutes, in one writer transaction with its revision check and authored record. Resource Undo re-adds the previous fixed capacity or capacity fn; re-adding or editing that resource invalidates its Undo. Saved values survive reloads and coordinator restarts; a later edit is never overwritten |
| `POST /projects/id/<p>/settings/board/preview` | the body (a draft program) drawn as the board would draw it, with the project's data, as a Datastar patch into the preview |
| `POST /projects/id/<p>/board/action` | a board Button: `board_rev`, `button` (its number), `field-<n>`; its form's notice says "Sent to <session>.", or why it was not sent with each refused field's own note under it; any other client gets 204 or the JSON error |
| `/projects/id/<p>/icon` | the project's image icon |
| `POST /settings` | display preferences: `theme` (`paper` or `carbon`; anything else is refused) and value types (`types`), each kept a year in its own cookie (`sluice_theme`, `sluice_types`); the page posts its signals (`theme`, `types`) as JSON and is answered 204 with the cookies. With no theme cookie (or one naming a theme sluice no longer has) a page names no theme and draws Paper or Carbon by the system's scheme; the page posts that choice once (`<html>`'s `data-init`), so from then on only a pick changes it |
| `/title`, `/title?project=<p>` | a page's tab title alone, as text, and its favicon's address in `x-sluice-favicon`: the index's ("2 questions · 2 failed · 1 quiet · Projects · sluice") or a project's, the plan's glance: its open questions to the owner in brackets, a warning sign and how many of its steps need a look the owner has not set aside, its name ("(1) ⚠4 · almanac · sluice", "⚠2 · almanac · sluice", "(1) almanac · sluice", its name alone when nothing needs the owner), its favicon the mark with a teal dot while a question waits, an ochre square while a step needs a look; what a hidden tab asks for every 30 s while its stream is closed; the open questions to the owner someone waits on lead it (the inbox's title too: "2 questions · Inbox") |
| `/_ui` | "States and parts": what each state and shared part of the dashboard means as its pages show it, in every state, in Paper and Carbon (DESIGN.md, Components), how each is built under it, and every component with its props and events; nothing on it is read from the store |
| `/docs`, `/docs/<topic>` | the agent docs (`docs/agent`, what `sluice docs` and the MCP `docs` tool serve) as pages: an index of the topics (the overview first, then by task), then each topic's markdown drawn, every topic listed beside it (under it on a narrow screen); display preferences link it beside the guide |
| `/static/<name>` | assets |

A browser that asks for a page (a GET whose Accept names HTML) and meets an error gets a page in
the layout: what is missing ("No such step"; an address whose project id is no id at all is
"No such project", 404), why it may be gone (a plan edit, or the
project's retiring of done units) and the ways back (its plan, the log searched for it); a
JSON client, a stream and a script's fetch keep the JSON error.

A running step is quiet once it has written nothing (its run's files, its progress, its own
messages in its thread) for its cadence: its
plan's `cadence:<n>m|h|d` tag, else 2 hours. One threshold for the index's tags and order, the
drawer's badge, the tab title's count, the project's "n quiet" tag, the card (a "quiet" caption
with how long, in place of its run's time) and Show: Attention.

**The status table.** Every page draws a step's state from one table
(`sluice_model::shown`, DESIGN.md's status ramp): a stored status named from the facts read
beside it, in this order, which is the one priority: failed, cancelled, stale, quiet, blocked,
stopping, finishing, parked, running, outside (`external`), paused, held, queued, pending, set by hand
(`manual`), succeeded, skipped. Each state has one glyph, word, lane mark, tone and band, and
says whether it needs attention (failed, cancelled, stale, quiet). Whatever stands for several
steps reads as the first of their states in that order: a unit (a failed step and a running
one read failed), a project's glyph and its place on the index, a unit's place on the plan (a project's glyph reads set by hand as succeeded; a cancel the owner
dismissed (`step_dismiss`) stands as done for its unit and project: its unit's band (Done),
the project's glyph, place, tab title and attention tags, the index's rows, though its Done line, its
page and its stage cell still say cancelled); and every count counts each step once, under its state (a unit's sum, a recipe block's head, `Count`, the tab title's counts of the states that need
attention). The facts are read once per page, with the store's snapshot, before any step is
counted or drawn: a pause (the step's or its project's) makes a pending step paused; ready
`core.external` work is outside; a step behind a failed or stale one is blocked; one reading a
plan input with no value is held; a ready step short of a resource is queued; a running step
whose cancel was asked for (`step_cancel` sets its attempt's `cancel_requested`; a settle is
not one) is stopping, and Cancel is not offered on it again; one whose run has submitted is
finishing; one whose run has written nothing past its cadence is quiet; a succeeded step set
by hand is set by hand, worded "bypassed" on its page, its drawer and its unit's stages when
its bypass preset (§7.7) settled it; a pending step with a preset waiting carries a "bypass ·
<author>" tag in its stage's cell and its band. A status the store holds that is none of the six is an error the page
says, never read as pending. The index has no plan: it counts a blocked, held, queued or
outside step as pending.

A failed step the owner cancelled is shown as cancelled: its stored error is `cancelled`, an
agent failure of kind `Cancelled`, or a fn's message `cancelled: <reason>` (`shown::is_cancel`;
the log's Errors filter runs the same rule as SQL, `shown::cancel_sql`). It keeps `failed` in
the store and every tool; the dashboard gives it its own glyph and word, counts it apart ("n
cancelled"), names it so wherever a step waiting on it says why ("after up (cancelled)", "step
up is cancelled", a gate's glyph, the gates' count), in the board's Units table and the log, and
makes Retry a plain button on it.

Times read one way on every page. The server writes a time in a `<time>` as it read the clock
at render, so a page reads "running for 1h 25m" and "12m ago" from its first paint (never raw RFC
3339; the UTC day and minute, "2026-10-07 20:47 UTC", in its title); the page's script ticks it
on in the same words ("just now", "12m ago", "3d 12h ago"; a running time "45s", "12m", "2h
14m" or "3d 12h") and reads a time again at once when a stream patch writes the server's text
back. That text is the clock's: a stream's version leaves it out, so ticking never patches.

A step's cell on the plan carries a timer: while it runs, how long its current run has gone
(its latest run in the step's current generation, so a retry starts it again; a scatter's latest
round, from its first item's start), ticked by the page's script from the run's start; once it
has succeeded or failed, how long that run took (start to end), still. A step set by hand, and
one pending, stale or skipped, shows none; the title counts the step's runs. A running or
failed step past its first run (a scatter's: past its first round) says "run 3" after the
timer, after a mark for how each of its last three earlier runs ended ("+n" for more), read
from each run's result in the step's current generation; a quiet run keeps it. Its unit's row and the
step's badges say it. A stopped unit's module names its failure's kind in a word or two ("cap", "stalled", "quota", "auth", "engine exited", "engine fault",
"lost", "no submit", "invalid", "rejected", "transient", else "failed"); the module
says the failure's sentence under it (a cancel's too). A step that asked the owner a question
its run waits on puts its unit in the plan's Needs you, the question whole and answered in
place; its step's status line says "waiting on your answer" (in place of "n awaiting reply"),
leading to it on the step's Overview; the plan's summary sentence leads with "1 question for
you", linked to Needs you. The page draws the
time as of its render, so it reads right before the clock ticks, and the version (below) leaves a
ticking time's text out, so the clock alone never patches the page.

A finishing step (§6.4) keeps its running glyph; its cell on the plan reads "finishing", and its
drawer and page add a "finishing" badge and a Finishing section: when it submitted, the
`step.submit` record's seq, its release, and that `step_settle` settles a run that lingers.

A step with progress fresher than its outputs (§6.4) shows it in its Overview (under Now while
it runs): its fields as the outputs show theirs, under a "live" badge (the running
glyph) while it runs, else "progress", and when it was set: by its current run, or by its
last, which ended without making it outputs. The board's `Output` shows the same value.

**Activity.** A step whose latest run is an agent's has an Activity tab in its drawer and
page: what the agent did, read when the page draws from
the transcript its engine already keeps (`sluice_agents::activity`; nothing is stored). Claude's
is its session JSONL under its config home (`CLAUDE_CONFIG_DIR`, else `~/.claude`, as Claude
finds it), Codex's its session rollout in the run's Codex home (`codex-native-homes/`), Devin's
the hook journal in each invocation (`devin-hooks.jsonl`, each line timed by sluice's hook as it
appends it); the run's `native.json` names the engine and session. Only the run's part of a
resumed session is read: its records from the run's start to the next run's. One row per turn:
a turn is one thing sluice sent, headed by what it is, never by how it was delivered: "Task" (read
from its `## Task` section); a message by who sent it, "From @lash-f4" ("From orchestrator" on older ones), "Reply from <step>",
"Question 12 from you" (read from its handed-over file, or, once the file is gone, from the
messages table by its id, the id a link); sluice's own nudge "From sluice"; a compacted context
handed back "Compacted" and a session carried on from an earlier run "Carried on", both muted)
and what the agent did until the next, its last
words, its tool calls counted by tool, how long it took and how many failed ("1 failed", in the ochre's ink). A turn opens
(`<details>`, no script) to its calls as a tree: the tool, its key argument (the
command, the path, the pattern, the query), how long it took and its outcome; a call opens to its
arguments as name and value rows and its result (a long one folded behind "Show all"; a failed
one's error in a box scrolled to its end). A call fails by its own result: Claude's `is_error`,
Codex's `failed` status or non-zero exit, Devin's `success: false` or a non-zero "Exit code". Two
or more reads, searches or listings in a row fold into one row ("3 reads, 2 searches"). The latest
five turns are drawn, and every earlier turn with a failed call; `?activity=all` (its link "Show
earlier turns") draws all of them, the latest 12 calls of a turn likewise (and every earlier failed one). A failed call and its
turn never fold away. On a failed step (not a cancelled one) the last call that failed is open,
its turn too, and "Why it failed" (in Overview, where the page and the drawer open) links to
it, opening the Activity tab on it. While the run runs its open turn shows the breathing live dot and a ticking
"running for" (`<time data-since>`); new turns and calls arrive with the step's stream, which
reads only what the transcript appended (a cache follows each by size and modification time). Every
text kept is masked as the engines' notes are (`account::redact`) and capped: what was sent and
said at 1,200 characters, an argument at 600, a result at its first 400 and last 1,200. Each agent
run's Runs row says its calls by tool on one meta line ("Bash 42 · Edit 9 · Read 17 · 2 failed").
The section's line counts the run's turns and calls ("8 turns · 34 tool calls, 2 failed"). The section links the run's own record when its directory holds one (Codex's `codex.log`, else
`codex-wire.jsonl`; Devin's `devin-hooks.jsonl`), served masked.

**Conversations.** Every page that draws messages (a step's Messages tab and Overview, a thread's
page, the inbox's notes) draws them one way, in one column: a run of messages from one sender is
a group under who sent it and to whom (a step by its title and id, the page's own step "This
step", a session by its name (`@lead`), "orchestrator", muted, on older messages, the owner "You"); a line at each day's start (UTC) and at the first note
to the owner not read yet ("New", by the owner's read mark on its thread; nothing is marked read
by being drawn); a question with its state ("Waiting on your answer", after the teal dot, when it waits on the owner,
"Awaiting reply", "Answered", "Closed") and the replies to it directly under it; each message's
id and its time (relative, the absolute time in its title); a body past 700 characters or 14
lines folded behind "Show all", but never an open question the owner can answer, which is
drawn whole wherever it is answered. A group whose messages go to different recipients names each
message's recipient on it; one note sent to several recipients within a minute (same sender,
title and body, not a question, not replied to) is one entry, "to 9 steps", that opens to each
recipient, every message id it stands for still an anchor. The message box at its end posts a note (or, checked, a
question) as the owner and keeps what is typed through a patch. An open question to the owner is
drawn whole wherever it is drawn, a step's Overview too (never cut to its first words there),
with Answer and Close (`sluice-answer`); two drawings of one message on a page keep their ids
apart (the Overview's are `ov-…`). A question to the owner is drawn whole once answered or
closed too, its answer under it, never cut to its first words. An answer or a close is acknowledged
where it was given, in one sentence the server draws (`MessageItem::answered_html`; each answer
box carries it unshown for the page's script): its buttons and box give way to "Answered just
now: <title> sent to <asker> · Read the thread" (or "Closed just now: <title> · Undo"), a polite
`role=status` that takes the keyboard's focus, and a patch that replaces it hands the focus to
what stands for the answer then (the inbox's line, else the owner's reply under the question,
else the question). The inbox keeps a question the owner answered or closed in the last ten
minutes in its place as that line ("Answered 2m ago: …"), through every patch; an answer sent
from the page gives way to that line at once, whether or not the page's stream is connected.

Keys move around (`sluice-keys`): `/` focuses the page's search field, `g` then a
letter opens a page the nav links (`g h` projects, `g p` plan, `g m` messages, `g l` log, `g f`
functions, `g i` inbox), `?` lists them in the display preferences (its Keyboard shortcuts); none acts while a field has
the focus, a dialog is open or a modifier is held.

Every page has a `…/stream` twin that patches the page live over Datastar SSE. Every page is
drawn whole by the server (Datastar's own way: the backend is the source of truth, the browser
morphs what it sends; https://data-star.dev/guide/the_tao_of_datastar), and every write is a
Datastar `@post` whose effect comes back on the stream; every value is HTML-escaped and markdown bodies are rendered on the
server with unsafe link schemes refused. A page loads nothing from any other host: every
script is served from `/static/` (Datastar 1.0.4, OpenUI lang-core 0.3.0, zod 4.6.5 and
sluice's own), and every face from `/static/fonts/` (`font/woff2`, vendored fontsource 5.3.0
files of its two faces, Inter and JetBrains Mono, whose `@font-face` rules are in the one
stylesheet). A page and each stream batch are drawn from one store
snapshot, with each running run's activity (its run files' modification times, and when its
agent was sent each turn and made each tool call, from its transcript, cached by the bytes
already read so a page reads only what was appended) read once;
nothing that changes meanwhile fails the page. The version a page carries (`ver`) is a
fingerprint of the HTML its stream patches with what the page's clock keeps left out: a ticking
`<time data-since>`'s or `<time data-ago>`'s text (every time that ticks is one, the board's
Units ages too), the place of each mark and span on At work's pulse strips (`<i data-a>`'s
style, kept by `nav.js` from its times) and a row's live dot (`data-heard`'s class). So a
stream opened at it sends nothing until something shown changes, however long the page stays
open. A page marks parts of what
it patches as regions of their own (`<!--r:id-->…<!--/r:id-->` around the element of that id):
the sidebar's head and its project list and the top bar's tabs on every page, the plan's head,
each section of the sheet (Needs you, At work, Done; one
with nothing in it a hidden placeholder, so it keeps its place), each item of Needs you, each
row of At work, a step's page's head (`#step-band`)
and detail apart, a unit's page's head and detail, the message views' head, the plan's facts
and the board's pane.
A batch sends, for each region that changed, the smallest region around the change whose
skeleton (its HTML with its own regions left out) is unchanged: one unit's state patches that
unit's row, and a unit moving between sections the sections it left and joined. A stream renders its
page again only when a cheap token moves (the store's `change_versions`, which every commit
bumps, and the function catalog's version), and at least every 30 s for what the clock alone
changes (a run going quiet), which patches only when that changes what is shown; the drawer and a step's page read that step and its unit, not the board,
and apply the board's activity observer before drawing the unit's stage strip or step view.
While the step runs, its detail renders every second (its transcript grows with no commit). A
step or unit no longer in the plan, or a deleted project, is said in place ("This step left the
plan; it may have retired.", with its log) and its stream stays open and quiet. The drawer's
stream has its own `sver` and `sstale` (a step's own page streams under the page's `ver`); the page's line under the nav reads only the page's
stream: nothing while live, an ochre pill in the top bar, "Updates paused · Reconnect", while
Datastar retries (its Reconnect opens the stream at once), and "Updates stopped at 14:02." with
Reconnect under the bar once it gives up (its ten retries, about three minutes, or a
stream the server ended), opening the stream again on Reconnect, on going online, on the tab
showing and on the window's focus. Every stream's first signals carry the build that drew them
(`rel`, the release and its assets' fingerprint); a page drawn by another build says "sluice was
updated · Reload" in muted ink. A hidden tab's stream is closed (Datastar reopens it when the tab
shows); the index and a project page then ask `/title` for their tab title every 30 s. A page that fails answers the shared JSON error with its status, its
message never empty. Assets linked with their fingerprint (`?v=`, the favicon and logo
included) are served immutable; a
module another imports by name is mapped to its fingerprinted URL by the page's import map;
any other asset URL carries its fingerprint as an ETag and revalidates to a 304. Pages, styles,
scripts, JSON and streams are compressed (brotli or gzip at quality 6, as the client accepts); a
stream's encoder flushes whenever the stream waits, so each batch arrives whole. Each page carries its icons once, as a sprite of `<symbol>`s, and draws each with a
`<use>`.

**Questions with a ui.** A question's `ui` is an OpenUI Lang program drawn by the inbox page:
one statement per line, the first drawn, components `Stack`, `Heading`, `Text`, `Callout`,
`Table`, `Separator`, `Form`, `Input`, `Textarea`, `Select`, `Radio`, `Checkbox`, `Button`
(signatures in `docs("inbox")`). Unparseable lines are dropped and counted; the text box always
remains. A Button answers `{action, params, values}`; the text box answers with action
`answer` and its text as the body; "Close" answers with action `close`.

**The project board.** A project with a board (`board_set`, `docs("board")`) draws it on its
plan page: from 1280 px wide a right-hand column beside the plan (the page's column widens so
nav, plan and board keep shared edges; the column scrolls on its own; with the step drawer open
the drawer takes the side instead); below that a section after the plan, shown in turn with the
plan by a Plan · Board switch in the bar whose links the server answers and keeps in the project's cookie
(until one is picked a phone shows the board, a wider window the plan). Without a board the page is as before. The program is
checked again and drawn on the server. Board and recipe view programs are at most 64 KiB
and 400 statements, with at most 48 levels of syntactic nesting. Resolution visits at most
4,000 values, counting components, scalars, arrays, objects and references, including every
repeated reference expansion; exceeding this budget refuses the program with a size problem
and its line. The question components draw as the inbox draws them, and the data components
are filled when the page renders and with every live patch:
`Units(state?)` (the units view's rows), `StepStatus(step)`, `Output(step, field)` (the
freshest value: the step's progress for that field while it is fresher than the outputs, §6.4,
marked "live" with the running glyph while the step runs and "progress" after, with when it
was set; otherwise the output), `Count(label, of)` (the number the dashboard's own summary
counts for `of`: the steps in one state of the status table, by its key (`failed` never counts
a cancel, `running` never a quiet, stopping, finishing or parked run, `pending` never a paused, blocked,
held or queued step), or `steps`, every step), `Metric(label, query)`, `Query(query, caption?)`,
`Chart(kind, query, caption?)` (bar or
line, an inline SVG), `Doc(fallback?)` (the board's document, `board_doc_write`, as markdown
with "Edited <time> by <author>" under it; before it says anything, its fallback muted or "Not
written yet."; at most one per program) and `LatestMessage(from, chars?)` (the project's newest message whose `from` is
`from`, with its time linking to it in its thread, its body cut to `chars`, default 280, with
an ellipsis and a link to the whole; none, "No message from <from> yet."); `Markdown(text)`
draws its text as markdown. A step argument (`StepStatus`, `Output`, `LatestMessage`'s `from`)
may be `tag:<tag>`, the one plan step carrying it; none or several is the component's error
box, naming the tag and the count. A Metric, Query, Chart or LatestMessage whose step (a
`step_id` literal in its SQL, or its sender, when that was once a step) is not in the plan draws
a line in the attention colour above it: "Step `x` is not in the plan; showing its last
data." Doc, Markdown and LatestMessage use the dashboard's markdown
renderer (escaped, unsafe link schemes refused); `Text` stays plain. A query runs through the
`query` tool's path, views and limits, every `?` bound to the project's id, at most 16 per
board; a stream reruns them when the project's log, plan, board or step progress changes (a
document edit is both of the first) and at least every 30 s. A component that cannot be drawn (a bad
query, an unknown step, a non-numeric chart) is an inline error box naming it, its line and the
reason; the page still renders. A Button sends a `say` as `owner` to the project's fallback
session (§8.1; refused without one) with body
`Board: <label>` and data `{board_rev, action, params, values}` (a primary button checks its
form's rules first, `invalid` otherwise); if the board's rev changed since the page was drawn
it is refused (`conflict`, shown under the board) and nothing is sent. The board never edits
the plan. Project settings has a Board section: the program, a live preview, Save (fenced by
`expected_rev`) and Clear (a save shows `board_set`'s warnings under it), then the board's
document, read-only and drawn as markdown, with its rev, when it was last edited and by whom,
and a note when the program has no `Doc()`. Its Retiring section sets §6.11's
`prune_done_after` in hours (empty is off) and `prune_keep` (patterns separated by commas or
spaces), and says whether retiring is on and when it last retired how many steps.

**Steps by title.** Every page names a step by its title (§6.8), its stage before it ("land ·",
muted): the index, the plan, the unit and step pages and their tab titles, threads and the
inbox. Its id is in the Details "⋯" of whatever names it, never beside its title, except on the
log, whose records name exactly what they are about (its title, then its id in mono, muted).
A step whose title is its id shows the id alone.

**Details.** What identifies an item rather than explains it (its ids, run number and earlier
runs, fn, engine and model, tags, recipe, params, a message's id, hashes and paths) is behind
one "⋯" at the item's end on every row, module, head and message: a `<details>`, Escape or a
click elsewhere closing it, each id whole with a copy button. A progress
field or output is shown or kept there by its JSON shape alone: numbers, booleans and short
words show; a hash, id, path, URL or long token is kept there; prose shows on two lines and
whole there.

**The plan.** The project page is a briefing over a roster in the centred main column
(at most 1240px). Its head (the top bar names the project, and the name is its heading for a
screen reader) is one short line: "3 at work · 4 waiting · 70 of 81 done". When something
needs the owner it leads: "1 question for you · 2 failed · 3 at work · 4 waiting · 70 of 81 done".
Questions link to where they are answered. Waiting units that have not begun are counted
immediately after At work, omitted when none wait. The done count links to `?show=done#done`
when any unit is done. Items set aside are omitted from the attention
counts; settling a cancel as done also completes its unit. The line names no unit and has no
step count, recent-activity assurance or separate long-run clause. The project's sections
(Plan, Day, Stats, Messages, Log, Functions, Settings) are the top bar's tabs, with Inbox at its
right; under 620px the last three fold into "More". The sidebar's find (`/` focuses it) finds
on the plan. The head's one ⋯ holds project details, Settings, Export (Mermaid text,
`?format=mermaid&all=true`), Order (Created, Plan order, Recent activity), and, with a board,
text links for Plan, Plan and board, Board. The view cookie and responsive defaults are retained.
Needs you, when needed, then At work occupy the same main column at every width.

- **Needs you**: every item that needs the owner, a hairline row each, a sentence with its
  actions and a ×: the open questions to the owner someone waits on (its unit's title, "asks
  you", who asked and when; its words; the answer in place under them: a one-line field that
  grows to about four lines on focus, ⌘↵ or Ctrl+↵ sends it as the owner's reply, Escape empties
  it, and Answer is the page's one filled button; a question with an OpenUI form opens its form
  from Answer instead), then the stops (each unit whose current step failed, was cancelled or is
  stale: "<title> failed after 1h 4m", who and when, the failure's headline, what it last said
  and what to try next, `failure::Advice`, for a historical wall-cap failure ("Agent runs now have no time limit. Retry to use
  the current release."); Retry and Retry
  with feedback, whose field opens in place under the item and sends the retry with its words to
  the step's thread, `POST …/steps/<s>/actions` `action=retry` with `message`; when the failure
  repeated the field stands open at once and there is no bare Retry; a cancel says its reason
  and, when it set a failure aside, "It had failed: …", with Retry and "Settle as done"),
  failures that share a cause of their own (a wall-clock cap, a stall, an engine's usage cap,
  sign-in, exit or readiness, a lost process, a passing failure outlasting its retries, exiting
  without submitting) said once in the head's word ("Two units failed: they hit their
  wall-clock cap.", the advice in the plural), the units in a fold, with Retry all; then the
  silences, the longest first ("<title> has not been heard from since 14:02, 3h ago.", Message
  it), a long run's (Long runs, below) only past `long_run_quiet_h`. Each is marked: a teal dot a
  question, a red square a stop, an ochre ring a silence, said in words to a screen reader. A
  cancel stays in Needs you: a cancelled step leaves its unit unfinished and holds every step
  after it, and the status table counts it as needing a look on every page (home,
  `show=attention`, the tab title), so the owner still has one decision to make: Retry, or
  "Settle as done" (its step's Dismiss), which settles the cancel so its unit is done; its step's
  page undoes it. Settling is never the ×. Every × sets its item aside for attention alone
  (`POST …/needs`, `needs_dismiss`, nothing settled), kept until it changes (a stop's step is
  retried, recancelled or recreated: its generations move on; a silent run writes; a question is
  answered or closed), when it comes back by itself. For ten minutes after, "Set aside: <what> ·
  Undo" stands under the list ("Set aside: the silence of <title>"), the focus going to it, Undo
  putting it back. "Set all aside" in the section's head sets every stop and silence aside the
  same way, a cancel included, and settles nothing; a question keeps its own ×, so the head
  never counts a question the list hides. With nothing needing the owner, the head and empty
  state are absent; recent Undo lines remain available without a section head.
- **Long runs**: a running step of no recipe alone in its unit that publishes progress
  (`step_progress`) or has run six hours or more is an ordinary At work row, in the final group
  named "Long runs". Its latest short progress fields are its message line ("red 3"). It keeps
  the engine chip, title and pulse strip. Its normal silence is not a quiet warning; only a
  failure, question or silence past `long_run_quiet_h` (config.json, 24 hours by default) needs
  the owner. There is no separate Watching section, verdict classification or result history.
- **At work**: the head shows "At work" and its counts ("8 units · 1 waiting"). Order lives in
  the page's ⋯: Created sorts by creation revision then plan position, Plan order by plan
  position, Recent activity by latest heard then plan position. The per-viewer, per-project
  `sluice_order_<p>` cookie is read by pages and streams alike; activity never reorders Created
  or Plan order. No legend or axis labels stand beside the head. Rows group by recipe (an open
  `<details>` whose name links `?recipe=<name>&show=all`, with counts), then plain rows for
  units with no recipe, then "Long runs". The plain rows have no group heading or fold, even
  in a big roster; when all units lack recipes they sit directly under At work. In a roster
  of more than 12 rows a
  group folds by row: its remarkable rows first (a silence, a wait of an hour or more since its
  last run, a unit with an open item in Needs you), the rest, when more than 3, folded under
  them to one line ("+7 running · each heard from in the last 11m"), a `<details>` opening in
  place; a group with nothing remarkable folds whole to its line ("article · 23 running · each
  heard from in the last 27m"). Every Plan fold preserves the viewer's open and closed choices
  through patches and re-creation, including unit moves between groups, with
  `data-preserve-attr="open"` and project-scoped session memory keyed by fold or unit id.
  Counts have their own stream regions; count and activity changes alone never replace the
  roster skeleton. A stop is in Needs you, not a row. A row: what its steps run on as a chip
  (an engine's name; the column kept, empty, for a fn, and drawn only when some row of the
  roster runs on an agent); its title (two lines at most, whole when the row is open, its
  `title` the whole). An owner tag is omitted when it matches the project's fallback or every
  roster row has the same owner. The stage line shows just the current stage and elapsed time
  with heard time ("work 14m · heard just now"), or "paused at fork" or "land next"; other stages
  remain available on the step or unit page. A dot is green while heard within five minutes
  (`data-heard`, kept by the page's clock). The row's message is its recipe's view or
  its last message with its sender (one line; a waiting row says what it waits on instead); and
  its pulse strip: the last hour, linear, now at the right, a mark each minute its agent acted
  (each turn it was sent and each tool call it made, as its transcript says; the latest 90 such
  minutes, so the strip changes only when the agent acts), a taller mark each message, a start
  notch where its run began when that is inside the hour, and a dotted tail from its last
  action, message or write to now, faint while recent and ochre once the unit is quiet (a strip
  whose last mark is over an hour old is all ochre dots); under 760px wide the strip gives way to
  the dot. Each strip says what it shows ("Its pulse: acted in 14 minutes of its run, 3 messages
  lately, then quiet"; it counts since the run began, so the clock alone never changes it).
  Pointed at, tapped or reached with the arrows (the strip is one tab stop; Escape closes), a
  mark's minute opens a popover of what happened then, asked for only then
  (`GET /projects/id/<p>/pulse?unit=<u>[&step=<s>]&at=<minute>`, a Datastar patch of the
  popover's body; never in the page or its stream): the time on the reader's clock, then in
  order each tool call (its tool and first argument, one line), turn (what opened it and its
  first sentence) and message (who and an excerpt), read from the run's transcript and the
  store; eight at most, then "+N more" and a link to the first in its step's Activity tab. A
  strip whose run began before the hour fades at its left edge, which says "Started 1h 42m ago,
  at 14:05" (`&from=<start>`). A row whose state the stream changes stays where it is and
  flashes (its wash fading over 1.4 s, `data-flash`), never moving under the reader; reduced
  motion skips it. A unit that has not begun and waits on earlier work is counted in the
  summary's "N waiting", with no line under At work, unless a find, `root=` or `recipe=` asks
  for its row. A row is a
  `<details>`: opening it asks for its body (`GET /projects/id/<p>/units/<u>/drill`, which
  answers a Datastar patch that replaces the row's empty body; while it is open the row's
  stream carries a stamp of that body, and a new stamp asks for it again), so the page and its
  stream carry rows, never the conversations of rows nobody opened. It opens to its
  conversation, oldest first (each message its sender, when, its markdown), then when it was last
  heard from ("Last heard 22:51 UTC (6m ago)"; for a quiet unit "Not heard from since 19:14
  (3h ago)"), Message and Unit page as quiet links and Open step (the drawer, on its current
  step); it stays open through a patch. It draws no step table: the unit's page keeps its step
  view.
- **Finished today**: under At work a quiet “N finished today · History →”, only when at
  least one unit finished on the reader's current day. Completed rows, the read marker and
  Mark as seen belong to the project's History tab, beside Plan. Home keeps its short
  “Finished lately:” names, using the same derived titles.

A reference in prose (a step id, a commit, a ticket) carries a hover card: pointed at for
320 ms, or focused, it opens a card of what the plan knows of it, asked for then
(`GET /projects/id/<p>/ref?r=step:<id>|sha:<hex>|t:<ticket>`: a step's state, its fn and its
unit; the steps whose outputs name a commit; the units a ticket names), closed 160 ms after the
pointer leaves, on Escape or on a scroll.

A view whose program does not check says so once over its group ("This recipe's view does not
check, so its summary is left out: …"). The find, `show`, `tag`, `recipe` and `root` keep or
hide whole units. Each item of Needs you and each row is a patch region of its own, so a
change patches the item or row it moved and nothing else. A step whose state the live plan moves on is
said once to a screen reader ("alpha-review failed"), from the `data-said` each item, row, card
and Done line carries, also when its unit moves between sections. A time of day is drawn on the
server in UTC and says so ("19:14 UTC"); the page's script sets it on the reader's own clock.

**The step view.** A unit drawn step by step: a row a step in graph order (its recipe's order,
else the plan's, each step after every step of its unit it comes after), named by its stage
(else its id less the unit's prefix) and linking to it; then what it runs on (an engine's name,
a fn), a column only when its steps run on more than one thing. A rail at its left draws the gates
between the unit's steps (an `after` entry or a value passed): a node a step, filled when done,
ringed when not reached, an arc turning while it runs, ochre when it needs a look; a chain runs
down one lane, a branch and a join take lanes beside it with an arrow into each step they reach,
and a condition is named at the row of the step it gates ("if ok", "if not ok": the gate's value, negated or not). Then its state (its
glyph and word from the status table, "run 3" with its earlier runs' marks past its first), its
progress (a scatter's items done of all, else the first short field of its live `step_progress`,
with a bar for "a/b"; "—" for none; a column only when one of its steps reports any), its runs as bars on one time axis every row shares (the
current run whole, the earlier ones faint) and how long its current run took or has run. A head
names the columns and the axis's labels, and after the first its unit's owner, small and muted
on the Runs tab (the plan row and unit page already say it, so their step views leave it out);
its foot is the unit's last message, when and from whom, linking its thread. The axis runs from the unit's first start to now while the unit is not
done, else to its last end; "now" is the read's time rounded up to a step of a tenth of the time
drawn or more (5 minutes at least), so the drawing changes once a step and never with the clock
alone, and the page moves the now line and a running bar's end between. It is linear, but a run
is never drawn under 3 px, and a stretch where nothing ran that is a tenth or more of the time
still drawn to scale (again until none is, the longest four at most, a minute at least) is a
break of 4% of the width, marked and labelled with its length ("+1d 2h"). Labels sit at a nice
spacing of the time drawn to scale ("0s", "15m", "1h 30m"), at least 17% apart. Under 560 px of
its width a row is its rail and name, then its state, progress and time, then its runs as a thin
bar, and the head (but its owner) and the axis give way. The plan's rows open it in place (the current step's row tinted) on
the row's own window instead: the last hour to now, linear, across exactly the row's strip,
the run going now as the row's pulse strip, how long each took before it, earlier runs' marks left out. The unit page is it, and
a step's Runs tab draws its unit's with its own row ringed; a row in it opens its step (in the
drawer, where there is one), and the arrows move between its rows.

**Usual durations.** How long a stage usually takes is said on the project's Stats page alone
(`/projects/id/<p>/stats`, below the table): no plan row, cell, step head, unit page, home
module, Day line or drawer compares a run with it. A running step says how long it has run; an
ended one how long it took.

**The unit view.** A recipe's `view` is an OpenUI Lang program with its own vocabulary, drawn on
the server inside the frame sluice owns (the unit's row on the plan, the unit page), every value escaped:
`Stack(children, direction?)` (`col` or `row`), `Text(text, tone?)` (`muted`), `Markdown(text)`,
`Link(label, href)` (an `http(s)://` or `/` href; another is drawn as text), `Param(name)` (the
unit's param: how the unit was made, not how it stands, so its Details "⋯" say it, on its row
and its page, and the view draws nothing for it), `Output(step, field)` (a
stage's output as the board's Output reads it, its progress first while fresher, nothing until
set), `StepStatus(step)` (a stage's glyph, word and caption) and `LastMessage(chars?)` (the
unit's last message and who sent it, in the plan's row only: the step view's foot says it on the
unit page; the
last one its own steps sent or said in their threads, else one sent to them, "Note from x";
a unit whose step waits on the owner says "Question for you: <its title>" instead). A
`step` is a stage name; `{param}` in any string is the unit's param. Every check (an unknown
component, a stage or param the recipe lacks, an unknown `{x}`) names its line.

## 14. CLI

`sluice <mode>`; errors print as JSON on stderr with a non-zero exit.

`coordinator`, `serve`, `loop` and `guardian` also log to stderr (their unit's user journal),
one line per event at info unless `SLUICE_LOG` (an EnvFilter, e.g. `warn` or
`info,sluice_store=debug`) says otherwise; `RUST_LOG` is not read, and no other mode logs. At
info: the start (mode, release, home) and stop, the startup adoption pass (runs live, adopted,
deferred), each committed or refused plan edit (project, rev, author, op count, prepare and
writer ms, how many times it was prepared again) and each retiring round. At warn: a write
transaction that held the writer over 1 s (naming the function that sent it), a busy answer, a
request a peer left unanswered (at most one line every 10 s per process), a socket read or
write that timed out, and connections dropped for want of a permit.

| mode | |
|---|---|
| `serve [--no-runner] [--port P] [--host H]` | dashboard, MCP and HTTP tools; takes the scheduler lease unless `--no-runner`; loopback only. Host and port default to config.json's `http`, else 127.0.0.1:3065 |
| `loop` | takes the scheduler lease and holds it until SIGINT/SIGTERM |
| `coordinator [--maintenance]` | runs the home's coordinator in the foreground |
| `install fence <reason> \| unfence \| select <release_dir> <home> \| verify <release_dir> \| status` | §2.2 |
| `home schema` | `{"schema": n}`: the database schema this binary reads and writes (needs no home; `build-release` records it) |
| `home migrate [--dry-run] [--json]` | converts `SLUICE_HOME`'s schema-1 or schema-2 database to this binary's schema (§3), holding `coordinator.lock` (refused while a coordinator holds it); `--dry-run` converts a backup-API copy in a scratch directory and leaves the home untouched; `--json` prints the conversion report |
| `tool [name [json\|-] [--field value]…]` | without a name, lists the tools; with one, runs it (its arguments as below) and prints its result as MCP returns it (`{"ok": true}` for an acknowledgement); `tool <name> --help` lists its fields |
| `tool rpc '<request>'` | sends a raw wire request |
| `tool lease acquire\|release\|status [<resource or id>] [--field value]…` | `lease_acquire`, `lease_release` or `lease_status` (§7.6), a leading bare word their `resource` (`lease` for release) |
| `say`, `ask`, `reply` | aliases of `tool say`, `tool ask`, `tool reply`, with the same JSON, flags, files and run defaults |
| `next --subscription NAME [--timeout 300] [--cut 600] [--json]` | stored wait (§10); one line per matching event and a digest with suppressed counts; resumes its cursor across clients |
| `next [-p P]… [--since-seq N \| --cursor FILE] [--me @NAME] [--scope mine\|all] [--units U,…] [--tags T,…] [--timeout 300] [--settle 20] [--settle-max 120] [--all] [--settles short\|full\|none] [--cut 600] [--json]` | the `next` wait; without a since a named session resumes after its last batch and an unnamed one starts at the top of the selected logs; `--cursor` reads and writes the seq in a file |
| `watch [-p P] [--kinds K,…] [--threads T,…] [--since-seq N] [--wake any\|questions]` | follows the log, one JSON record per line, until killed |
| `drain [-p P]… [--no-wait] [--release]` | drains (waits until drained unless `--no-wait`) or releases |
| `me [--project P] [--step S] [--json]` | `step_context` for the current step (from `SLUICE_PROJECT_ID`/`SLUICE_PROJECT` and `SLUICE_STEP`) |
| `doctor [--json]` | host prerequisites, each engine (`codex`, `claude`, `devin`): its executable on PATH, its `--version`, its tested versions, floor and newer-major rule, its status (`tested`, `untested, accepted`, `refused` with why, `not found`), what its capability probes verified, found missing or must assume (§15), and the selected release's manifest check. The engine probes run with HOME and the engines' config dirs in a private scratch directory, so no session starts and no credential is read; a missing, untested or refused engine is a warning, not a failure |
| `query [SQL [PARAM…]] [--limit N] [--table [--width 60]]` | the `query` tool, read directly from the database; without SQL, every public table and view with its columns |
| `backup PATH [--force]` | an online copy of `sluice.db` |
| `docs [topic]` | the agent docs: the pages the `docs` tool serves |
| `mcp` | the MCP server over stdio (§12.1) |
| `agent hook --engine codex\|claude\|devin --event E [--run R]` | engine hook entry (internal) |
| `guardian`, `payload-exec` | internal |

`sluice next` prints one entry per event: `ASK|SAY|REPLY #<message-id> <thread> <from> -> <to>:
<title or body>` (a question's title leads, its body on the lines after, each indented two
spaces), `STEP <id> <from> -> <to>: <error tail>`, `UNIT <u> settled · <outcome>: <step>
<status>[ (held)] · …` then its outputs (long ones named with a hint to read them with `sluice
query` or `--settles full`), `PROJECT <id> paused by <author>`, then a blank line and last `seq
N` or `timeout seq N`; `--json` prints the reply. A message's body is whole up to 4,000
characters; a longer one shows its first paragraph and every later paragraph that is a marked
question (it starts with "Question" or "Q:", or ends with "?"), each whole, never cut
mid-sentence, then `[N more characters of #<id> not shown: messages(view="thread",
thread="<thread>") reads it whole]`. A settled unit's `<outcome>` says how it ended: `succeeded`
when none of its steps failed, was cancelled or turned stale (`skipped` or `held` when none
even succeeded); else the one way it went wrong, counted past one (`failed`, `2 failed`,
`cancelled`, `stale`), or each count when mixed (`1 failed, 1 cancelled`). A failed step whose
failure was a cancel (its last `step.status` before the settlement) shows as `cancelled`, so
`UNIT x settled · failed: work failed · rm pending (held)` never reads as a success.
Reply with the `#N` on the ASK line; the last line's `seq N` is the cursor to pass to
`--since-seq`, not an id to reply to.
The message id is a decimal integer after `#`, always the second token, for every verb
and `--settles` mode. `log_wait` and JSON `next` records retain their `id` field.
`status(view="units")` prefixes the last-message summary with `#<id>`; the rendered line
includes it when it shows that message.

`sluice tool` takes the wire argument names with these conveniences: `steps`/`tags` (and
`after`, `projects`, `state`) as plain values or lists, `expected`/`dry_run`/`reason`/`author`
flat on edit tools, MCP's public names (`rev` for `expected`, `wait` for `fn_call`, `timeout`
and `wake` for `log_wait`, `timeout` for `step_wait`, `timeout`, `settle` and `settle_max` for
`next`, `older_than` for `plan_prune`, `fn` for `fn_save`), `older_than_hours` for
`plan_prune`, `prune_done_after_hours` for `project_update`, `name` for `project_update` and `project_delete` (which also fills
`confirm_name` and the current `expected_settings_rev`), `params.unit` for `unit_add`, and
`step`/`input`/`value` for `step_set_input`. Any other name is refused with the nearest ones (§12.2).

A tool's arguments are one JSON object, flags, or both:

- **JSON**: the object as the argument after the name; `-` there reads it from stdin. With
  neither an argument nor a flag, stdin is read only when it is a redirected regular file
  (`< args.json`), never a pipe or a terminal, so such a call never waits on a stdin nobody
  writes to.
- **Flags**: `--<field> VALUE` or `--<field>=VALUE`, one per field, the flag being the field
  name with `-` or `_` (`--to-message`, `--to_message`). The value is the text as given for a
  field whose schema takes only strings (`--to owner`, `--body 42` and `--project demo` stay
  strings). For any other field it is parsed as JSON when it parses (`--to-message 24771`,
  `--outputs '{"ok": true}'`, `--steps '["a","b"]'`, `--until '{"any_of": ["failed"]}'`);
  else, for a list of strings that takes no single string, it is a one-item list of the text
  (`--kinds message`, `--statuses failed`, `--recipients owner`); else it is the text
  (`--steps a`, `--until settled`, `--value hello`). A field that takes neither a string nor
  a number nor a boolean (an object such as `outputs`) refuses text that is not JSON. A boolean field's flag with no value is
  `true` (`--owner`, `--dry-run`). A value that starts with `--` needs the `=` form; a
  repeated flag keeps its last value.
- **Files**: `--<field>-file PATH` reads the value from a file by the same rule, taking the
  file's text exactly (no newline trimmed); `-` reads stdin, which one argument at most may do.
- **Both**: the flags apply over the JSON object, so a flag wins over the same field in it.
- **Run defaults**: in a run (`SLUICE_RUN_ID` set), a call that leaves out `project` gets
  `id:$SLUICE_PROJECT_ID` (when set), and one that leaves out `run` gets `$SLUICE_RUN_ID`, on
  every tool that takes them; `step_submit`, `step_progress` and `step_context` also get
  `step` from `SLUICE_STEP`. No other step is defaulted: a step that names a target is always given. A
  field the call gives is kept, even as null (`"run": null` speaks as the session `me`,
  `"project": null` leaves an optional project out), and `project_update` or `project_delete`
  given `name` names its project that way. Only `sluice tool` defaults; MCP, HTTP and
  `ctx.tool` do not, except that `ctx.tool("step_progress", …)` defaults `step` and `run` to
  the run's own.
- **Help**: `sluice tool <name> --help` (or `-h`) prints the tool's description and each field
  with its type, whether it is required or its default, and what it is, from the tool's schema;
  `sluice tool --help` prints the usage.

```sh
sluice tool reply --project demo --to-message 24771 --body-file - <<'END'
It's on the "parser" branch.
END
sluice tool say --body 'blocked on the schema'     # in a run: to its unit's owner
SLUICE_ME=@lead sluice tool ask --project demo --to work --body 'Which schema?'
sluice tool step_submit --outputs-file outputs.json                # in a run
sluice tool status '{"project": "demo"}' --brief
```

## 15. Agent engines

`agent.claude`, `agent.codex`, `agent.devin`, `agent.review` and `agent.run` run a supervised
interactive session of the engine CLI in the run's private tmux, in `cwd`. The supervisor
writes the task (for a step's run, first the note on the step's previous attempt under `##
Previous attempt` (§7.4) and then `## Task`; the prompt or spec, the step's inputs under `## Inputs`, the outputs to submit
with the exact `step_submit` command under `## Outputs you must submit` and "Submit only when
you are finished: submitting ends your session." with the `--outputs-file` form that needs no
shell quoting, and, unless `listen: false`, how to `ask` and `say` (naming nobody: its unit's
owner answers, §8.1; `to` a step, an `@session` or `owner` reaches someone else) and `reply`,
with the run's id, and their flag form, §14), watches the session through the
engine's hooks, nudges a stalled session, and ends it when the agent is done. The result carries
`session` (pass it back to resume) and `git` facts `{head_before, head_after, commits, dirty}`
of `cwd`, or of `workspace` when it is bound: the git work tree the work lands in when it is
not `cwd` (a lane that runs in the repository and works in its own worktree), relative to
`cwd`. Its baseline is the workspace's HEAD when the run starts, or `cwd`'s while the
workspace does not exist yet (a worktree added from the same repository shares it); a
workspace that is gone or lacks the baseline when the run ends leaves `git` out and says so
in the run's notes, never failing the run. sluice does not guess the workspace from where
commits landed: an agent may commit in several trees, so only the input says which is the
lane's. `git` is omitted when the baseline or final facts could not be observed. Git baseline,
quiet-worktree and final-fact probes retry three times with exponential backoff (10 s per
attempt, waits of 100 and 200 ms). A persistent observation failure leaves the fact unknown,
warns in the run log and says once to the unit's owner for that category; it never fails or
stops the session. Quiet CPU observations behave the same, and unknown activity cannot
justify a quiet warning. Quiet notes are suppressed during observed background work.
Engine version/help and read-only pane probes also retry. An unavailable startup probe
queues preflight; an unavailable live pane observation leaves the same session running and
backs off to 30 s. Unknown observation time does not count as evidence of a stall, missed
turn start or idle session. A definite missing capability still refuses startup.
Checkpoint mirror notifications also retry and degrade to a warning; the local durable
checkpoint remains required for safe delivery. A failed session is an `agent_failure` error
with its `kind` and `session`. Agent fns
retry a transient failure up to 3 times, 600 s apart, resuming the session; a rate limit whose
reset the engine reported waits until just after that reset instead.
After resuming, the supervisor sends `Your session was interrupted (<cause>). Continue your
task where you left off.` The cause is a short fixed phrase chosen from the triggering
failure's kind and message, never the engine's full message or pane text. It names a network
loss, an undelivered message, a rate limit or capacity when recognized, otherwise a temporary
failure. The cause stays in memory because pinned releases reject unknown checkpoint fields.
After a supervisor restart or outer-helper re-entry, the message is `Your session was
interrupted. Continue your task where you left off.`

A Codex network loss costs a run only time. Codex retries a dropped connection itself, sending an
`error` notification with `willRetry: true` (`Reconnecting... 2/5`) for each try: the turn goes
on, and the run's log (`stderr.log`) gets one line per try, `codex: Codex is retrying
(responseStreamDisconnected, HTTP 403): Reconnecting... 2/5` (Codex's text masked as below and
cut to 300 characters). When Codex gives up on a network or server failure (a final `error`, or
a turn whose `turn.error` says so) the failure is `Transient`, `codex: network error
(responseStreamDisconnected, HTTP 403); Codex gave up after its own retries. Codex said: …`. A
network or server failure is a `codexErrorInfo` of `responseStreamConnectionFailed`,
`responseStreamDisconnected`, `httpConnectionFailed`, `internalServerError`, `serverOverloaded`
or `responseTooManyFailedAttempts` (one with `httpStatusCode` 401 is an auth failure, below),
or, with no `codexErrorInfo` or `other`, a message that starts with Codex's `stream disconnected
before completion` (not its content filter), `Connection failed`, `exceeded retry limit`,
`We're currently experiencing high demand` or `Selected model is at capacity`; every other
`codexErrorInfo` (`badRequest`, `contextWindowExceeded`, `sandboxError`, …) is classified as
before. Once that turn has ended, the supervisor continues the same thread in the same
app-server: it logs `agent transient, continuing the same session in 30s: …`, waits 30 s (2 min
and 8 min for the run's second and third loss), then sends `Your turn was cut off by a network
error. Continue your task where you left off.` as the next attempt's `Continue` input, never
replayed if its acceptance is unknown. These continuations share the transient budget (3 per
invocation) with the resumes above, and work the same whether a step runs the agent or a fn
composes it. A loss whose turn stays open 10 s past it, or one past the budget, takes the
transient path above (a composing fn's own retry, e.g. `run(main, retries=3)`, resumes the
session), so a network that stays down fails the run `Transient` with that message. A resume
that meets Codex's `thread … already has an active writer` (its previous app-server died within
about a minute) is retried every 2 s for up to 90 s.

Codex runs with a private `CODEX_HOME` under `<home>/codex-native-homes/`, whose `auth.json` is a
symlink to the owner's (`$CODEX_HOME/auth.json`, by default `~/.codex/auth.json`), never a copy,
with `cli_auth_credentials_store = "file"`. Codex rotates its refresh token at each refresh,
writes that file in place and reloads it before refreshing, so one refresh, in any run or in the
owner's own Codex, serves them all. Each launch or resume, under `codex-native-homes/.auth.lock`,
replaces any copy an earlier release left in a private home with the link; a copy for the owner's
account refreshed later than the owner's file (`last_refresh`) is written to the owner's file
first, and an older one never is.

A stalled coordinator or guardian never fails an agent run. The supervisor's reads, its
acknowledgements of live messages and its notes to its unit's owner retry a peer that answers
`busy` (retryable) or does not answer within its 5 s, with waits growing from 50 ms to 1 s,
until it answers, the run is cancelled, or (acknowledgements and notes) ten minutes have passed;
it keeps answering the engine's hooks meanwhile. Each retry of a note is the same request, so
the coordinator posts it once. A guardian refuses an acknowledgement of a message it has not
offered yet, and its watch may lag the supervisor's read, so a refusal is retried for a minute
before it counts. An acknowledgement still unanswered after ten minutes fails the run as
`Transient` (the helper's retry resumes the session); a definite refusal fails it as `Invalid`.
A note that never got through is logged and dropped. A `busy` from the socket client says what
happened and to whom: `could not connect to the coordinator (…); the request was not sent`,
`the run's guardian did not answer within 5s; acceptance may be unknown`, or `… ended the
exchange without an answer (…)`.

The agent is done when it submits: the supervisor stops the session as soon as the run's
valid submission is stored, busy or not, and returns the result (§6.4); `final` is the agent's
last final-answer message (Codex excludes commentary), which a busy agent may not have finished.
An idle agent that has not submitted is nudged after the settle (`SLUICE_AGENT_SETTLE_S`), or,
before the first nudge of an engine that does not report background work (Codex, Devin), after
`SLUICE_AGENT_GRACE_MIN`. Reminders are at most hourly and say "if you are waiting on a job,
keep waiting". A helper re-entry after a reminder waits a full hour again. No reminder or quiet
note is sent while a live job remains in the run's payload cgroup (including detached jobs),
while the engine reports background work, while the run holds or waits for a section lease, or
while it is parked. The agent CLI, private tmux, composing fn helpers and engine control/MCP processes are not jobs.
An unavailable process observation conservatively suppresses reminders; it cannot fail the run.
Observed work also suspends the transcript stall check. Once all work ends, the forgotten-submit
reminder still applies. Once the final nudge's turn ends and the settle passes, or when the
engine exits first, the run fails without another reminder interval with
kind `ExitedWithoutSubmit` and the step with `exited_without_submit`. A step with nothing to
submit ends a settle after its agent goes idle and its background work ends. A terminal engine
error is never treated as successful completion merely because no outputs were required; it
retains its failure kind, message and session. A stored valid submission still ends the session
at once.

`agent.codex` and `agent.devin` return `log: string?`, a path to the run's readable
record of turns, completed assistant messages, tool calls and errors. Codex writes
`codex.log` alongside its redacted diagnostic `codex-wire.jsonl`; Devin writes `devin.log`.
A nonempty `log` input copies that record to the given path. If the run has no log file,
`log` is null and nothing is copied. Claude returns its last message as `result` and has
no `log` output; its transcript remains engine-owned.

Claude's transcript (its session JSONL and its subagents') is read for progress, the last
message so far and API errors. A record over 1 MiB (Claude Code writes one for a large tool
result or file read) is skipped, never fatal, and the run's log says how many were skipped; the
turn still ends at its `Stop` hook, which carries the final message, or on a stable idle pane.
A turn that ended with background work running waits until that work ends, even while
Claude's own status says busy: after such a turn 2.1.294 says so under a compaction spinner
frozen at `(0s)` and writes nothing until the work notifies it. Once the `Stop` hook and the
transcript (`stop_hook_summary`, `turn_duration`) have both ended the turn, a busy status with
background work the `Stop` hook listed is that wait, not a turn for `SLUICE_AGENT_STALL_MIN`; a
prompt, tool result or reply in the transcript makes it a turn again.

Input reaches Claude as one bracketed paste into its composer, sent with Enter once the draft
shows, and the `UserPromptSubmit` that carries it accepts it. The adapter pastes only into a
composer the pane shows and only while Claude has bracketed paste on, so a paste cannot submit;
it empties the composer with `C-e C-u BSpace`, never with Escape. Claude opens Rewind on two
Escapes it handles within 800 ms, and while it loads a long resumed session it handles keys late
and together, so Escape goes only to a surface over a composer the launch has shown: one that
closes on it (a dialog whose guide offers `Esc to cancel`, such as Rewind, or the prompt-history
search), or a screen the adapter does not recognize that has stood unchanged for 2 s while
Claude is not working. Each such surface gets one Escape, Escapes are at least 3 s apart, and a
paste waits 1.5 s after one. A draft that does not show within 5 s was not taken, since Enter
was not sent: the run's log gets the pane and the input is pasted again. After 3 pastes the
adapter gives back every input it holds as not accepted and the session fails as `Transient`, so
the retry delivers them again. Only an input sent with Enter whose draft stays in the composer
for 20 s fails the run as `UnknownAcceptance`.

A Devin turn ends at a `Stop` hook that no other hook follows for 10 s, once the pane no longer
shows Devin working (the `(esc twice to interrupt)` spinner or the `Guide Devin while it works`
placeholder). A tool or compaction hook after a `Stop` keeps the turn open, or reopens it: in
Fusion the sidekick's turn end fires `Stop` under the lead's prompt and the lead works on. Input
pasted into Devin is accepted by the `UserPromptSubmit` that carries it. Input pasted while
Devin works waits in its queue until the turn ends. It fails the run as `UnknownAcceptance` only
after 20 s with no hook, no working pane and no sight of it queued.

An engine's account problems end the run at once, typed: a hard usage cap with `agent_failure`
kind `QuotaExhausted`, an auth failure (logged out, a token expired or revoked, the account
barred) with kind `AuthFailed`. Neither is transient: the supervisor neither retries nor replays
input. A step with a restart policy (§6.1.1) retries quota failures after the reset and backoff;
authentication still needs the owner. The failure keeps the session, so the step can also be retried with its `session` bound
once the owner has acted (after the reset or with quota added; after signing in again on this
host), or run on another engine. A hard cap is a usage, plan, spend or credit limit whose reset
is unknown, or any limit whose reset is more than `SLUICE_AGENT_QUOTA_RESET_MIN` (15) away. A
short rate limit (its reset within the threshold, or a rate limit with no known reset) stays
transient; with a known reset the retry starts just after it (2 s) instead of 600 s later.

The message says what happened and what to do, then gives the engine's own text: `<engine>:
<what happened>[ (<window>)][; resets <YYYY-MM-DDTHH:MMZ> (in <relative>)] — <what to do>, then
step_retry. <Engine> said: <text>`, for example ``codex: not logged in (token revoked) — run
`codex login` on this host, then step_retry. Codex said: Your access token could not be
refreshed because your refresh token was revoked. …``, `claude: weekly limit reached
(seven_day); resets 2026-10-08T23:00Z (in 3d) — wait for the reset or buy usage credits at
https://claude.ai/settings/usage, then step_retry (or run the step on another engine). Claude
said: …` or `devin: weekly usage quota exhausted — buy usage or turn on auto-reload at
https://app.devin.ai/settings/usage, or wait for it to reset, then step_retry (…). Devin said:
…`. A short rate limit's message ends its head with `— retrying just after the reset` (or `after
the standard backoff`). The engine's text is cut to 1000 characters and anything token-like in it
is masked as `[redacted]`: a run with a credential prefix (`sk-`, `eyJ`, `ghp_`, `ya29.`, …), one
of 40 or more characters with letters and digits, or one with a digit after a credential word
(`Bearer`, `token=`, `api_key`, …); a 32-hex trace id, a `req_…` request id or a UUID stays. The
run's log (`stderr.log`) records the same text under the kind (`agent failed: AuthFailed: …`),
and each transient backoff with its wait. Each engine's signals, structured first:

- Codex: a turn's error (`turn/completed` or `turn/failed`, or an `error` notification Codex
  will not retry) with `codexErrorInfo` `unauthorized`, or an HTTP failure with
  `httpStatusCode` 401, is an auth failure; `usageLimitExceeded` is a usage limit and
  `rateLimitExceeded` a rate limit, their reset taken from the used-up window (`usedPercent` 100,
  `windowDurationMins`, `resetsAt`) of the latest `account/rateLimits/updated`, unless that
  limit's `credits` still carry on (`hasCredits` or `unlimited`). That update with
  `rateLimitReachedType` set and no credits fails the run at once, even with the turn still
  open: a `workspace_*_credits_depleted` or `workspace_*_usage_limit_reached` type is a usage
  limit, `rate_limit_reached` a rate limit. After either failure no further turn starts. At
  launch `account/read` with no account while the provider requires OpenAI auth fails as
  `AuthFailed` before any turn. Failing the structured fields, an error object's message in
  Codex's own words: `You've hit your usage limit. … try again at …`, `Quota exceeded. Check your
  plan and billing details.`, `Your workspace is out of credits.` and the like are a usage limit
  with no known reset; `Your access token could not be refreshed …`, `Your authentication session
  could not be refreshed …`, `unexpected status 401 Unauthorized: …` and the like are an auth
  failure. Any other failed turn (including safety/cybersecurity flags, API errors and refused
  requests) reports `agent_failure` with its classified kind and Codex's message; a completed
  commentary item cannot replace it with success. The failure comes from `turn.error` on
  `turn/completed` or `turn/failed`, or the terminal `error` notification (`willRetry: false`).
- Claude: the API error's category, from the transcript's error entry (`isApiErrorMessage`,
  `error`) or `StopFailure`'s `error`: `authentication_failed` is an auth failure (except
  Claude's own `Authentication error · This may be a temporary network issue`, which stays
  transient), as are `oauth_org_not_allowed`, `account_on_hold`, `verification_required` and
  `cloud_credential_error` with their own fix; `billing_error` is a usage limit with no reset;
  `quotaLimits` `{status: "rejected", rateLimitType, resetsAt}` (`five_hour`, `seven_day`,
  `seven_day_opus`, …) is a usage limit with that reset; a `rate_limit` error whose text starts
  with Claude's limit wording (`You've hit your …`, `You've reached your …`, `You're out of usage
  credits`, `Claude AI usage limit reached|<epoch>`, …) is a usage limit. With no category (or
  `invalid_request`, `unknown`), text starting with Claude's login wording (`Not logged in`,
  `Login expired`, `OAuth token revoked`, `Invalid API key`, `Please run /login`, …) is an auth
  failure. The entry, when read, decides over the hook. Any other 429 stays transient. Logged
  out, Claude opens on its login screen (`Select login method:`, with no composer): the run
  fails as `AuthFailed` before any input is taken.
- Devin, out of quota or no longer authenticated, prints a notice after the prompt it took
  (`⚠︎ Quota exhausted` with `Your weekly usage quota has been exhausted. Visit
  https://app.devin.ai/settings/usage ... (trace ID: ...)`, `Usage limit reached`, `Usage
  paused`; `Authentication required` with `Your session is no longer authenticated. Run /login
  …`), sends no further hook and leaves its composer idle. While a turn is open or input is
  pasted, each observation reads the pane, and when such a notice is the last entry right above
  the idle composer (not tool output, an earlier entry or a quote) it is a usage limit with no
  reset or an auth failure, so the run fails at once instead of at the stall cap.

Only an engine's error channels are read (Codex's error objects, rate-limit updates and account,
Claude's API error entry, `StopFailure` and login screen, Devin's last pane entry and login
picker), and text only at its start, so the words in tool output or the agent's own prose never
match. Logged out, Devin opens on its login picker (`How would you like to log in?`, `Log in with
browser`): the run fails as `AuthFailed` before any input is taken, as Claude's login screen does.
`sluice doctor` probes each engine's version and capabilities only, in a scratch home that holds
no credential, so it does not report auth.

Engine versions and capabilities. An engine CLI that updates itself must not take runs down
unless it stopped doing something sluice needs, so a launch judges the CLI's `--version` by the
engine's policy and then probes what it needs, rather than pinning one version:

| engine | tested | floor | a newer major |
|---|---|---|---|
| Codex | 0.160.0, 0.160.1 | 0.160.0 | runs untested |
| Claude Code | 2.1.283, 2.1.284, 2.1.294 | 2.1.283 | runs untested |
| Devin | 3000.11.3 | 3000.11.3 | refused |

- A tested version runs as it is. A version at or above the floor that was not tested runs too:
  the run's log (`stderr.log`) and the service log (a `warn`) say so, and the run's result
  carries the note in `notes`, e.g. `codex 0.161.0 is newer than the tested 0.160.0, 0.160.1;
  accepted untested (floor 0.160.0)`. A version below the floor, one sluice cannot read, or
  (Devin only) a newer major fails the launch with `agent_failure` kind `CapabilityMismatch`
  naming the version, the floor and the tested versions. The floor is the oldest version the
  adapter was built and checked against. Codex is 0.x and ships a minor version per release, and
  Claude Code a patch version, so their numbers do not mark protocol breaks; Devin's major is
  not a release counter (every release is 3000.x), so a new one marks a different CLI line, and
  Devin is driven through its pane, hook journal and session SQLite schema, which no probe shows
  before a session.
- The probe, before anything starts: Codex's `codex --help` (`app-server`, `resume`, `debug`,
  `--remote`, `--dangerously-bypass-approvals-and-sandbox`, `--config`), `codex app-server
  --help` (the `unix://` listener) and the protocol schema `codex app-server generate-json-schema
  --experimental` writes (every request and notification sluice uses, and `initialize`'s
  `experimentalApi` capability; a CLI without the command leaves them assumed); Claude's
  `claude --help` (each flag sluice launches with); Devin's `devin --help` (`--config`,
  `--export`, `--model`, `--resume`, `--respect-workspace-trust`). What no cheap probe can show
  is assumed and shown by the session itself: Claude's and Devin's hooks, Claude's status file,
  transcript and inline compaction context, Devin's session schema (checked when a session is
  looked up), Codex's remote TUI and its in-place `auth.json` writes. Codex's `initialize` reply
  is checked at every launch. A capability the probe finds missing fails the launch with kind
  `CapabilityMismatch`: ``codex 0.161.0 lacks `turn/steer` that sluice needs (tested 0.160.0,
  0.160.1; this version is untested)``. A probe with nothing missing is kept in
  `<home>/engine-probes/<engine>.json` for that binary (canonical path, mtime, size, inode) and
  version, so later launches only read the version.
- On an untested version, a run that fails in a way a protocol change explains names the
  version: `CapabilityMismatch` and `EngineExited` add `; likely cause: untested codex 0.161.0
  (tested 0.160.0, 0.160.1)`, and `UnknownAcceptance`, `ReadyTimeout`, `TurnStartTimeout` and
  `StallCap` `; possible cause: …`. An app-server request Codex does not implement fails as
  `CapabilityMismatch` naming the method (``Codex app-server does not implement `turn/steer`:
  …``), and a model listing an untested CLI prints differently names the version too.

A pane-driven engine (Claude, Devin) that stops before its first turn on an interactive screen
sluice does not answer can take no input, so the run fails at once with `agent_failure` kind
`BlockedScreen`; like the account kinds it is neither retried nor replayed and keeps the session.
The screens are read only while the engine's composer is not showing, before any turn of the
invocation:

- Claude 2.1.284: its first-run setup (the theme picker, `Let's get started.` / `Choose the text
  style that looks best with your terminal`; `Security notes:`; `Use Claude Code's terminal
  setup?`), setup's failed connectivity check (`Unable to connect to Anthropic services`), the
  API-key prompt (`Do you want to use this API key?`), Anthropic's updated terms (`Updates to
  Consumer Terms and Policies`), `Managed settings require approval`, a project's MCP servers
  (`New MCP server found in this project`), `Allow external CLAUDE.md file imports?`, and a
  required update (`… needs an update.`, `… older than the minimum version required by your
  organization`), which Claude prints as it exits;
- Devin 3000.11.3: its organization picker (`Select your Devin organization:`) and
  workspace-trust prompt (`Do you trust the authors of …`).

Sluice still answers Claude's folder-trust and bypass-permissions dialogs and closes Devin's menus
(`Select a menu item`, `Select model`, `Search sessions`). Any other non-blank screen that stands
unchanged before the first turn for `SLUICE_AGENT_SCREEN_S` (20 s) fails the same way: a normal
start draws its composer within seconds, and a slower one (a connectivity check, MCP servers
connecting, a long session loading) animates a spinner, which restarts the clock. The message says
which screen and what the owner must do, then quotes the screen's non-empty rows joined by ` | `
(cut to 1000 characters, anything token-like masked): `<engine>: blocked on <screen> — <what to
do>, then step_retry. <Engine> showed: <rows>`, for example ``claude: blocked on its first-run
setup (theme picker) — Claude Code's first-run setup is unfinished for the account it runs as:
run `claude` once on this host as that user and finish it (…), then step_retry. Claude showed:
Let's get started. | Choose the text style that looks best with your terminal | …``, or for an
unknown one ``<engine>: blocked on a screen sluice does not recognize, unchanged for 20s before
any turn — run `<engine>` once in the step's cwd on this host and answer it (or attach to the
run's private pane while it waits), then step_retry. …``.

When a pane-driven run fails before its engine completed a turn, or with a timeout or stall kind
(`TurnStartTimeout`, `ReadyTimeout`, `StallCap`, `OperationTimeout`), the supervisor keeps the pane before
tearing it down: its screen and up to 50 rows above it, anything token-like masked, in
`pane-at-failure.txt` (mode 0600) in the invocation's directory (`runs/<run>/`), and the message
ends with `pane at failure (last rows; whole screen: <path>):` and the last 6 non-empty rows, each
trimmed, on lines of their own. A `BlockedScreen` message, which already quotes its screen, ends
with `pane at failure: <path>` instead. A cancellation keeps nothing, and a capture or write that
fails leaves the failure as it was. The private tmux keeps an exited engine's dead pane
(`remain-on-exit`) until the teardown, so what it printed as it exited is kept too. The run's log
records every failure but a cancellation as `agent failed: <kind>: <message>`.

Agent runs have no overall wall-clock limit. Each engine or host operation has a fresh
660-second timeout (`OperationTimeout`), long enough for a host's 600-second retry patience
and its last transport attempt. Starting or resuming the engine uses `SLUICE_AGENT_READY_S`
(`ReadyTimeout`); delivering input uses `SLUICE_AGENT_TURN_START_S` (unknown acceptance on a
timed-out send). Each transient backoff is bounded independently at one hour
(`OperationTimeout`), and the retry count still applies. None of these budgets decreases with
run age. Parked time stays out of idle detection's active-time clock.

Timing (minutes unless noted), overridable through environment variables:
`SLUICE_AGENT_STALL_MIN` (30), `SLUICE_AGENT_SETTLE_S` (10),
`SLUICE_AGENT_GRACE_MIN` (10), `SLUICE_AGENT_POLL_S`, `SLUICE_AGENT_READY_S` (180),
`SLUICE_AGENT_TURN_START_S` (60), `SLUICE_AGENT_DIALOG_S` (60),
`SLUICE_AGENT_QUIET_MIN` (60, never more often than hourly), `SLUICE_AGENT_NUDGES`,
`SLUICE_AGENT_QUOTA_RESET_MIN` (15; 0 makes every limit with a known reset a hard cap),
`SLUICE_AGENT_SCREEN_S` (20, seconds: how long an unrecognized screen may stand before the first
turn). `SLUICE_AGENT_WAIT_MIN` and `SLUICE_AGENT_WORK_MIN` remain accepted for compatibility;
waiting jobs no longer cause reminders or forced background cleanup.

The supervisor ticks every `SLUICE_AGENT_POLL_S` (0.5 s) while anything it observes changes.
After a tick that changed nothing (the same engine observation, no action, an unchanged
checkpoint) it waits twice as long as before, up to ten polls, parked or not; a hook request,
or a message the guardian hands the run, ends the wait at once. It writes the checkpoint only
when it changed.

### 15.1 Choosing a model

The `model` input of `agent.claude`, `agent.codex`, `agent.devin` and `agent.run` is one of two
JSON objects; left out, the engine's default runs, which is the same object filled in: Devin
`{"type":"normal","model":"swe-2","effort":"high"}`, Codex
`{"type":"normal","model":"sol","effort":"high"}`, Claude
`{"type":"normal","model":"opus","effort":"high"}`.

| shape | composes |
|---|---|
| normal `{"type":"normal","model":M,"effort":E?,"fast":bool?}`, any engine | Devin `M[-E][-fast]`: `swe-2-high`, `claude-opus-5-5-high-fast`, `adaptive`. Codex: model `M` (`sol` and `astra` name `gpt-6.1-sol` and `gpt-6-astra`; any other name is a `codex debug models` slug) at reasoning effort `E`, recorded `gpt-6.1-sol@high` (no `E`: the model's own default, recorded `gpt-6.1-sol`). Claude: `--model M --effort E` (`M` is `opus` or `claude-opus-5-5`, `E` `low`…`max`; no `E`: Claude's settings), recorded `opus@high`. Codex and Claude cannot run fast: `"fast": true` is refused there |
| fusion `{"type":"fusion","main":{"model":M,"effort":E?,"fast":bool?},"sidekick":{"model":S,"effort":F?,"priority":bool?}}`, Devin only | `fusion-M[-E][-fast]-sidekick-S[-F][-priority]`: `fusion-claude-opus-5-5-high-sidekick-swe-2-high`, `fusion-claude-opus-5-5-high-fast-sidekick-gpt-5-6-luna-high-priority`, `fusion-claude-opus-5-5-high-sidekick-glm-5-2` |

The objects are strict: an unknown key anywhere, a string that is not a non-empty string, or a
`fast` or `priority` that is not a boolean is refused; `effort` is left out for a model that
has none. At launch, before the session starts, the composed id is checked against what the
engine lists: `devin models list --format json` (each variant's `model_uid`), `codex debug
models` (each slug, and `<slug>@<effort>` for each reasoning effort it supports), and for
Claude, which has no listing, the pinned models and efforts above. A listing is reused for 10
minutes per process. An id the engine does not list fails the run with `agent_failure` kind
`Invalid` naming the composed id and up to 5 nearest listed ids (those it begins first, then by
edit distance); nothing else runs in its place. A fusion on Codex or Claude, `fast` there, or a
malformed object is refused the same way. The plan types `model` as `Any?`, so a stored plan
still holding a retired form (a string such as `"sol"` or `"fusion"`, or a separate `effort`
input) keeps validating; such a step fails at launch with kind `Invalid` and a message giving
the object to use instead (`"sol"` with effort `xhigh` →
`{"type":"normal","model":"sol","effort":"xhigh"}`, `"fusion"` →
`{"type":"fusion","main":{"model":"claude-opus-5-5","effort":"high"},"sidekick":{"model":"swe-2","effort":"high"}}`).
An edit refuses a string model before it reaches a launch: `step_add`, `unit_add` (after its
recipe's expansion and `inputs` overrides), `step_update`, `step_set_input`, `plan_edit` and
`unit_update` refuse (`invalid`, with that same message, and
`steps.<id>.in.model: …` in `errors`) a new step,
or a step whose fn or `model` binding they change, when it is an agent step and its `model`
binds a literal that is not an object, or reads a plan input whose value is not one;
`plan_set_input` refuses setting a plan input that such a step's `model` reads to one. An agent
step is one running an agent built-in, or an open fn whose own `model` input is typed `Any` (an
agent block that hands it on to one); a fn typing `model` otherwise (an enum of names) takes
what it declares. A model read from another step's output is known only at launch. A stored
step keeping a string model is left alone, as is any edit that does not change its model.
The result's `model` is the id the run resolved and launched. `agent.review` and `decide.llm`
run Claude's default.

## 16. Built-in fns

| fn | inputs | outputs | notes |
|---|---|---|---|
| `core.echo` | `value: Any` | `value: Any` | inline |
| `core.collect` | `items: Any[]` | `items: Any[]` | the fan-in join; inline |
| `core.format` | `template: string`, `values: Any` | `text: string` | `{0}` from an array, `{name}` from a record, `{{`/`}}` literal; non-strings as JSON; inline |
| `core.external` | | | open; never runs (§6.5) |
| `inline.bash` | `code: string`, `cwd: string?`, `check: boolean?` | `stdout, stderr: string`, `code: int` | open; errexit and pipefail; extra inputs as environment variables (`-` → `_`); declared outputs from the JSON object written to `$OUT`; fails on a non-zero exit unless `check: false` |
| `inline.python` | `code: string`, `cwd: string?` | `value: Any?`, `stdout: string` | open; standard library; sees `inp` and each extra input; `out` is the result |
| `message.ask` | `to`, `body`, `title?`, `ui?`, `input?`, `data: Any?`, `wait: boolean?` | `id: int`, `receipt: Any`, `reply: Any?` | §8; `wait` blocks until answered |
| `message.say` | `to`, `body`, `data: Any?` | `id: int`, `receipt: Any` | §8 |
| `message.reply` | `to_message: int`, `body?`, `answer?` | `id: int`, `receipt: Any` | §8 |
| `message.wait` | `thread`, `since: int?`, `to?`, `timeout: int?` (300), `wake?` | `messages: Any[]`, `last_seq: int` | waits for messages on a thread after `since`; `wake: "questions"` holds the rest |
| `message.post` | `body`, `thread?`, `to?`, `needs_reply?`, `reply_to?`, `answer?`, `title?`, `ui?`, `input?`, `data?`, `from?`, `wait?` | `id: int`, `reply: Any?` | retired; plans that name it run through the `message_post` translation (§8), only in a run |
| `agent.claude` | `cwd`, `prompt`, `model: Any?` (Opus, default `{"type":"normal","model":"opus","effort":"high"}`), `session?`, `listen?`, `workspace?` | `result`, `model`, `session`, `git` | open |
| `agent.codex` | `cwd`, `spec`, `model: Any?` (default `{"type":"normal","model":"sol","effort":"high"}`), `log?`, `session?`, `report_path?`, `listen?`, `workspace?` | `log: string?`, `final`, `model`, `report?`, `session`, `git` | open |
| `agent.devin` | `cwd`, `spec`, `model: Any?` (default `{"type":"normal","model":"swe-2","effort":"high"}`; or a fusion), `log?`, `session?`, `report_path?`, `listen?`, `workspace?` | `log: string?`, `final`, `model`, `report?`, `session`, `git` | open |
| `agent.review` | `cwd`, `base`, `standards`, `notes?`, `session?`, `listen?`, `workspace?` | `summary`, `sha`, `commits: int`, `session`, `git` | open; reviews and fixes a branch diff with Claude |
| `agent.run` | `engine` (`devin`, `codex`, `claude`), `cwd`, `spec`, `model: Any?`, `session?`, `report_path?`, `listen?`, `workspace?` | `final`, `model`, `report?`, `session`, `git` | open |
| `decide.llm` | `question`, `context: Any?`, `options: string[]`, `threshold: float?` | `choice`, `p: float`, `confident: boolean` | 2 retries, 30 s apart |
| `git.head` | `path` | `branch`, `sha` | |
| `git.merge` | `repo`, `source`, `target`, `message?`, `push: boolean?` | `merged: boolean`, `sha?`, `conflicts: string[]` | in a temporary worktree; conflicts are data |
| `git.push` | `path`, `branch`, `remote?`, `force_with_lease: boolean?` | `sha` | |
| `git.rebase` | `path`, `onto` | `ok: boolean`, `sha`, `conflicts: string[]` | conflicts abort and are data |
| `git.worktree` | `repo`, `base`, `branch`, `path?` | `path`, `branch`, `sha` | |
| `git.worktree_rm` | `repo`, `path`, `force: boolean?` | `removed: boolean` | |
| `gh.pr` | `path`, `base`, `head`, `title`, `body`, `draft: boolean?` | `number: int`, `url` | creates or updates the open PR; 3 retries, 30 s apart, when GitHub failed (a server error, a GraphQL "Something went wrong", a dropped connection) |
| `gh.pr_wait` | `path`, `pr`, `until` (`checks`, `merged`), `interval: int?`, `timeout: int?` | `state` (`green`, `red`, `conflicting`, `merged`, `closed`, `timeout`), `sha`, `url`, `failed: string[]` | 3 retries, 30 s apart |
| `gh.run_cancel` | `path`, `run_id: int` | `cancelled: boolean` | retries like `gh.pr` |
| `gh.run_latest` | `path`, `branch?`, `workflow?` | `run_id: int`, `sha`, `status`, `conclusion?`, `url`, `workflow`, `failed_jobs: string[]` | retries like `gh.pr` |
| `jev.ask` | `state: Any`, `questions: Any`, `model?` | `answers: Any`, `model`, `usage: Any` | TypeSafe System One; needs `TYPESAFE_API_KEY`; 3 retries, 5 s apart (all `jev.*`) |
| `jev.choice` | `state`, `instructions`, `options`, `min_confidence: float?`, `model?` | `choice`, `probabilities`, `confidence: float`, `confident: boolean`, `model` | |
| `jev.score` | `state`, `instructions`, `levels: Any[]`, `model?` | `score: float`, `probabilities`, `confidence: float`, `legend`, `model` | |
| `jev.noul` | `state`, `instructions`, `yes?`, `no?`, `model?` | `noul: float`, `model` | |

Unmarked inputs and outputs are `string`; `?` marks optional ones. Unless noted, a builtin does
not retry. Builtin icons: a spark for the agent fns, a review mark, a fork for `decide.llm`, a
branch for `git.*`, a PR mark for `gh.*`, an arrow leaving a box for `core.external`, a bubble
for `message.ask`, `message.say`, `message.reply` and `message.post`, and an envelope for
`message.wait`.
