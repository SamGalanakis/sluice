# sluice: notes for agents

- Rust workspace under `crates/`. Ship gates: `scripts/ship` runs formatting, workspace
  Clippy with `-D warnings`, the fast suite, doctests and the slow groups selected by changed
  sources on the rebased tree. `scripts/check` is the same gate for local use, not a prerequisite
  to shipping. Both use the shared definition in `scripts/gate` and this checkout's `target/`.
- Day to day, `cargo nextest run --workspace --locked` (or `-p <crate> -E 'test(name)'`) builds
  into cargo's configured target dir; leave it to cargo's configuration and never set `CARGO_*`
  variables. To pin one invocation to this checkout's `target/`, pass
  `--config 'build.target-dir="<worktree>/target"'`, as `scripts/check` does.
- Keep a lane's private target for its lifetime and iterate with `-p <crate>`. Do not seed
  targets across worktrees: measurements on this host found no net win after copying. Never
  hardlink targets or build into another lane's target. Keep configured targets under
  `/workspace/.cargo-targets`, where ship can clean them; the gate uses a real `target/`
  directory (symlinks change canonical binary paths). Under multi-lane load use
  `NEXTEST_TEST_THREADS=4`; do not change host Cargo settings or set `CARGO_*` variables.
- Historical cutover fixtures are an exception to lane-local builds: `scripts/historical-build`
  caches runnable outputs for immutable Git revisions under
  `/workspace/.cargo-targets/sluice-historical`, with harnesses keyed by source and schema features.
  This is not cross-worktree target seeding: no lane target is copied or hardlinked. Builds take
  a per-key file lock and publish a completed staging directory by rename; ship cleanup protects
  the cache. When no durability gates are active, run `scripts/historical-build prune --dry-run`
  then `scripts/historical-build prune` to remove revisions no longer selected by `old_revision()`
  or the normalized schema-3 cutover scenario, plus interrupted staging directories. Lock files
  stay in place so concurrent waiters always share one inode.
- Timing seams are direct Rust APIs, never home settings: `Notifier::with_retry_timing`,
  `Claude::with_delivery_timing`, `Devin::with_delivery_timing` and
  `DashboardState::with_clock`. Dashboard `Clock::tokio` ties wall time to Tokio time;
  paused current-thread tests can advance stream deadlines and rendered ages together.
  Use `Clock::scope` for helpers called outside the page router. Production uses the system
  clock and the original intervals. Child-process fixtures can set
  `SLUICE_TEST_DEVIN_TURN_QUIET_S`, `SLUICE_TEST_CUTOVER_LEASE_S` or
  `SLUICE_TEST_GUARDIAN_ORPHAN_S` (how long a guardian outlives a coordinator nothing listens
  for: 30 s in tests, ten minutes in production; its service stops the payload on exit).
  Each home's workload slice uses `StopWhenUnneeded=yes`, so systemd retires it after its
  last dependent service stops, even when a killed test cannot run teardown. Its runtime
  drop-in belongs only to that home; tests never configure shared parent slices' lifetimes.
  The test timing variables are read only when `SLUICE_TEST=1` and otherwise ignored.
  Admission's host reading of memory and CPU (`ExecutionHost::host_reading`) is the same kind
  of seam: in test mode the OS host measures nothing unless `SLUICE_TEST_HOST_READING` holds a reading as JSON, and the coordinator's
  start applies no workload slice settings (a launch still does). Notification command
  timeouts stay real.
- Tests start the binaries they test through `env!("CARGO_BIN_EXE_sluice")` (and the other
  `CARGO_BIN_EXE_*`), never a path under `target/`, so each test runs what cargo just built.
- The only Python left is the fn helper (`python/`), the custom-fn and engine test fixtures,
  the shipping scripts and the dependency-inventory generator in `docs/rust/`. Run
  `uv sync --locked` once per checkout: the engine acceptance gates use the repository's
  `.venv/bin/python`. Agent and engine tests need the private tmux: run
  `scripts/build-private-tmux` once per checkout.
- Tests run in test mode: `.cargo/config.toml` sets `SLUICE_TEST=1` for everything cargo runs,
  and every unit Sluice starts inherits it. Test mode names user units `sluice-test-*`
  (production: `sluice-run-<run>`, `sluice-coordinator-<hash>`, `sluice-doctor-*`) and refuses
  the live installation's selected home. A test binary run outside cargo must set it too.
- `SPEC.md` is the contract, `DESIGN.md` the dashboard's look; change them with the code. The
  agent docs (`sluice docs`, the MCP `docs` tool and instructions) are `docs/agent/`.
- Commits: plain sentences, the user as sole author; no AI co-author trailers or mentions.
- The live home runs a deployed release, never this working tree. After pushing to main,
  `SLUICE_HOME=<the selected home> scripts/deploy <ref>` (ref defaults to `origin/main`;
  `--prefix DIR` picks the installation) builds a pinned release of that commit, selects it
  under the installation fence and stops and re-creates the coordinator, serve and loop units.
  Running steps are adopted. `sluice install status` names the selected home. Ship with `scripts/ship`
  (below) rather than by hand.

## Tests

The suite is small, fast and end to end. The default run takes about a minute.
- A test drives a real entry point: the `sluice` binary, MCP or HTTP, the coordinator, or a
  crate's public API. It asserts what a user or a peer session would see: output, stored state,
  a page, a message. Never private state, call order or log text.
- One scenario test with several checks beats many tests that each repeat the same setup.
- Unit tests are for kernels only: plan compile, the incremental-vs-reference differential,
  codecs. Prefer a property or a differential to a list of examples there.
- Every default test finishes in 3 s on this host (nextest marks anything slower). No sleeps:
  wait on state or events, fake the clock.
- The slow tier contains only tests named `slow_*`; its nextest profile excludes fast tests.
  `scripts/gate` maps every slow test source to exactly one group and checks coverage on every
  gate. Ship selects durability, browser, scale and execution groups from the base..head diff
  and prints each selected group and its source paths. Dashboard changes select browser tests;
  plan compilation and plan-store changes select scale tests; conversion, backup, migration,
  release, install and deploy changes select durability. Additions confined to new
  `ADDED_COLUMNS` or `ADDED_VIEWS` entries with all other schema tokens unchanged do not select
  durability. Gate machinery, shared fixtures and dependency changes select all groups.
  `scripts/check` selects groups from the diff against origin/main, including local tracked edits;
  `--base REF` chooses another base and `--slow` runs every slow group. `--dry-run` prints the
  selection and commands without running them. Keep nextest at four threads under multi-lane
  load (`NEXTEST_TEST_THREADS=4`); keep each lane's target for its lifetime, without cross-worktree
  seeding. The tier also holds scenarios waiting on fixed retry or observation windows.
- A test home's teardown (`tests/support/units.rs`) stops its units and removes its workload
  slice's runtime settings, failing if a unit remains or the user manager cannot be reached.
  Abrupt test death needs no teardown to retire run units and empty slices: the guardian's
  orphan bound and systemd's slice dependencies own that cleanup.
- Before adding a test, name the real bug it catches that no existing test catches. If you can't,
  don't add it.

## Shipping

Shipping work (`deploy`, `build-release`, `compat-check` and `cutover-rehearse`) enters a
collected transient user service in `app.slice`, outside the caller's workload cgroup. The
launcher connects through `/run/user/<uid>/bus` and gives the work an explicit environment:
the account's home, UTF-8 locale, a PATH including `~/.cargo/bin`, `~/.local/bin`, `~/bin` and
system tools, plus the requested home, installation and deploy/build controls. Ship also uses
that explicit bus address for its gate and adoption checks. The gate removes inherited lane
identity and callback context; builds and tests use at most four jobs. Lane runtime, compiler
and engine settings are not inherited. Nested shipping scripts share that service.
Test mode additionally preserves fixture PATH/home and test controls; process-fixture deploys
stay in their caller because their tests own the persistent services by PID.

Deploy-started services are transient units created with `systemd-run --collect`. Stop and
re-create them with the same deploy arguments.
Never `systemctl restart` a deploy-started transient unit; re-create it with `systemd-run` as deploy does, or re-run deploy.

- `scripts/ship [REF] [--dry-run]` takes a branch through its gate to a verified live deploy.
  From any worktree, with a clean tree, it rebases REF (default `HEAD`;
  another ref is worked on in a temporary worktree) onto `origin/main`, runs the land check (below), pushes to main
  (fetching and rebasing again, up to `--retries`, when the push is not a fast-forward), runs
  `scripts/deploy origin/main` with the home `sluice install status` selects, and checks
  adoption: every run live before the deploy is still live under its unit or finished with a
  recorded result, the coordinator, serve and loop units are active, and the dashboard answers
  200 on `/` and a project page. It ends with one line: `shipped <sha> <subject> · deploy ok ·
  compat N releases ok · adopted M runs`. `--dry-run` prints each step and changes nothing.
  Cleanup removes a temporary checkout's Cargo target before its worktree, on success or failure.
  It reads the target from Cargo metadata, permits only targets inside `/workspace/.cargo-targets`
  and protects the script and invoking checkout's own targets. Shipping the current checkout
  removes no target; a dry run prints the cleanup without creating a temporary checkout.
- Ship gates once on the finished, rebased tree per push attempt: formatting, Clippy over
  every workspace target, the fast suite, doctests and the selected slow groups. Do not run a
  separate full local gate before shipping. If a push retry leaves the Git tree unchanged,
  ship reuses the common checks and covered slow groups; any newly selected slow group runs
  once. A different tree runs the gate again. A rebase conflict stops the ship: resolve it, test the resolved code and ship again. A temporary
  checkout borrows this checkout's `.venv` and `target/private-tmux`.
- `scripts/compat-check [--release DIR]` proves a candidate release (default: the newest under
  the prefix, i.e. the one `scripts/build-release` just built) can serve every release a live
  run is pinned to. It copies the home's `sluice.db` (SQLite backup API, the live file opened
  read-only), `config.json` and fn trees (never a `.env`) into `/tmp/cc.*`, starts the
  candidate's coordinator on the copy as a plain child of the host service, with a scratch
  runtime directory and dead D-Bus address so it has no route to the systemd user manager,
  and runs `tool log_read`, `status`, `say`, `step_submit` (for a made-up run, which
  must be refused as stale) and `step_context` with each pinned release, the selected one and
  the candidate. A storage or schema error, a crash, a timeout or a submit not refused as stale
  fails it; any other typed refusal (e.g. `invalid` for a fixture fn the CLI's catalog lacks)
  shows as `refused` with its message but does not. It prints a release × command table, stops
  what it started by PID, removes the copy, and exits 0 when nothing failed (also when nothing
  is live). Failures print the last 200 lines of each coordinator log, preserving line breaks,
  and an exact rerun command before removing the scratch directory. `--pinned DIR` adds a
  release; `--mark-schema N` marks the copy at schema N once the candidate has opened it, to prove the check catches a bump. A candidate whose
  manifest `schema` differs from the home's (or `--incompatible`) is checked in incompatible
  mode instead: no run in the copy may be live, the candidate's `sluice home migrate` converts
  it, the candidate serves it, and the old release (`--old`, default the selected one) must
  refuse it unchanged. `--copy DIR` checks a prepared copy (the rehearsal's) and never removes
  it.
- `scripts/deploy` verifies the candidate before the fence and before stopping anything: the
  release's own binary runs `sluice install verify <release>`, the check `install select` runs
  (build-release has the staged binary run it too). A release that won't verify stops the
  deploy with live untouched.
- `scripts/deploy` runs compat-check after the build and before the fence. A failure stops the
  deploy unless `--skip-compat "<reason>"` is given; the reason is printed and every deploy's
  compat outcome is appended to `<install>/deploy.log`.
- A deploy that fails after it stopped the services (select, start or unfence) rolls back: it
  stops what it started, re-selects the previous release if needed, re-creates the previous
  services under the same unit names and arguments, unfences with the old binary, checks the
  dashboard and exits 3 with `deploy rolled back to <old sha>: <error>`. `scripts/ship` then
  fails saying main is ahead of live (the push already happened). Only a rollback that fails
  itself, or a deploy with no previous selection, stays fenced; it prints the manual recovery
  commands.
- Before rolling back to a release older than `step_bypass`, clear the waiting bypass presets
  (`step_bypass(..., off=true)`; `status` shows them as a step's `bypass`): an older
  coordinator ignores `steps.bypass` and launches those steps.
- Incompatible storage changes require a new schema version and a drained, fenced migration
  with no live consumers of the old schema: a run's pinned `sluice` reads the database itself
  and refuses any other version. Schema 2 remains reserved for the historical interim board
  layout; schema 3 introduced normalized plans (`docs/design/plan-rows.md`), and schema 4
  requires bounded execution. Additive changes use
  version-scoped migrations (`ADDED_COLUMNS` and `ADDED_VIEWS` for the current version). Never
  re-pin a running executable by changing database metadata.
- `scripts/ship` ships compatible changes only: it refuses a ref whose `SCHEMA_VERSION` differs
  from the selected home's before anything moves, and `scripts/deploy` refuses such a commit
  before building it. A schema change lands on main by hand and goes out through
  `scripts/deploy --schema-cutover --deadline <time>` (SPEC §2.2): a rehearsal on a copy
  (`scripts/cutover-rehearse --candidate <release>`, which `--rehearsal FILE` can reuse), the
  notice, the drain, at the deadline the fence and then a cancel request (with a reason naming
  the cutover) for each run still live, nothing converted until nothing is live, then the
  migration; `<install>/cutover-<time>.json` says how each run actually ended. `--dry-run`
  rehearses and lists what the deadline would stop. After a post-migration failure, repair the
  cause and run `scripts/deploy --schema-cutover --resume-checks` with the same scratch or live
  installation selection. It requires the `schema-<candidate schema> cutover` fence and a home at the recorded
  candidate's schema. It checks the recorded backup and each project's baseline, then unfences,
  releases the drain, re-creates inactive services and waits up to 60 s for the loop's scheduler
  lease before printing the original cutover report. Fenced checks start only coordinator and
  serve; the loop starts after unfence and drain release. The plan-rows lanes land together
  from one integration branch (`docs/design/plan-rows.md` §12); none is shipped alone.

## UI changes

Any change to the dashboard (`crates/sluice-web/`: views, templates, `assets/`) is verified in
a real headless Chromium before it is called done: screenshots at 390, 1440 and 2560 px wide,
in light and dark, of every page it touches, and the agent opens and looks at each screenshot
itself. Check that the content column is centred and shares its left edge with the nav, that
nothing is clipped, and that the page never scrolls sideways.

- Chromium: `~/.cache/ms-playwright/chromium-*/chrome-linux64/chrome` (or `SLUICE_CHROME`).
- Driver: `tests/support/chrome.rs` (`Chrome`: `open`, `eval`, `wait`, `send` for any DevTools
  call, e.g. `Emulation.setDeviceMetricsOverride`, `Emulation.setEmulatedMedia` with
  `prefers-color-scheme`, `Page.captureScreenshot`).
- Serve a copy, never the live home: build with `cargo build -p sluice` and the `--config` above, then
  `SLUICE_HOME=<scratch copy> target/debug/sluice serve --no-runner --port <free port>`.
