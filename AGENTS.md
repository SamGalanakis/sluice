# sluice: notes for agents

- Rust workspace under `crates/`. `scripts/check` is the gate: `cargo fmt --check`, Clippy with
  `-D warnings`, `cargo test --workspace --all-targets --locked` and doctests, all built into
  this checkout's `target/`.
- Day to day, `cargo test --workspace --locked` (or `-p <crate> --test <file>`) builds into
  cargo's configured target dir; leave it to cargo's configuration and never set `CARGO_*`
  variables. To pin one invocation to this checkout's `target/`, pass
  `--config 'build.target-dir="<worktree>/target"'`, as `scripts/check` does.
- Tests start the binaries they test through `env!("CARGO_BIN_EXE_sluice")` (and the other
  `CARGO_BIN_EXE_*`), never a path under `target/`, so each test runs what cargo just built.
- The only Python left is the fn helper (`python/`), the custom-fn and engine test fixtures,
  `tests/browser.py` and the dependency-inventory generator in `docs/rust/`. Run
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
  under the installation fence and restarts the coordinator, serve and loop units; running
  steps are adopted. `sluice install status` names the selected home. Ship with `scripts/ship`
  (below) rather than by hand.

## Shipping

- `scripts/ship [REF] [--dry-run]` takes a branch that already passed `scripts/check` to a
  verified live deploy. From any worktree, with a clean tree, it rebases REF (default `HEAD`;
  another ref is worked on in a temporary worktree) onto `origin/main`, builds, pushes to main
  (fetching and rebasing again, up to `--retries`, when the push is not a fast-forward), runs
  `scripts/deploy origin/main` with the home `sluice install status` selects, and checks
  adoption: every run live before the deploy is still live under its unit or finished with a
  recorded result, the coordinator, serve and loop units are active, and the dashboard answers
  200 on `/` and a project page. It ends with one line: `shipped <sha> <subject> · deploy ok ·
  compat N releases ok · adopted M runs`. `--dry-run` prints each step and changes nothing.
- Re-test rule: after a rebase, re-run `scripts/check` only when main's new commits changed a
  file other than prose (`*.md`) that the branch also changed; otherwise a build (`cargo build
  --workspace --all-targets --locked`, into cargo's configured target dir) is enough. A re-gate in
  ship's temporary checkout borrows this checkout's `.venv` and `target/private-tmux`. A rebase conflict stops the ship: resolve
  it, gate again and ship again.
- `scripts/compat-check [--release DIR]` proves a candidate release (default: the newest under
  the prefix, i.e. the one `scripts/build-release` just built) can serve every release a live
  run is pinned to. It copies the home's `sluice.db` (SQLite backup API, the live file opened
  read-only), `config.json` and fn trees (never a `.env`) into `/tmp/cc.*`, starts the
  candidate's coordinator on the copy as a plain child with no route to the systemd user
  manager, and runs `tool log_read`, `status`, `say`, `step_submit` (for a made-up run, which
  must be refused as stale) and `step_context` with each pinned release, the selected one and
  the candidate. A storage or schema error, a crash, a timeout or a submit not refused as stale
  fails it; any other typed refusal (e.g. `invalid` for a fixture fn the CLI's catalog lacks)
  shows as `refused` with its message but does not. It prints a release × command table, stops
  what it started by PID, removes the copy, and exits 0 when nothing failed (also when nothing
  is live). `--pinned DIR` adds a release; `--mark-schema N` marks the copy at schema
  N once the candidate has opened it, to prove the check catches a bump.
- `scripts/deploy` runs compat-check after the build and before the fence. A failure stops the
  deploy unless `--skip-compat "<reason>"` is given; the reason is printed and every deploy's
  compat outcome is appended to `<install>/deploy.log`.
- Incompatible storage changes require a new schema version and a drained, fenced migration
  with no live consumers of the old schema: a run's pinned `sluice` reads the database itself
  and refuses any other version. Schema 2 remains reserved for the historical interim board
  layout; normalized plans use schema 3 (`docs/design/plan-rows.md`). Additive changes use
  version-scoped migrations (`ADDED_COLUMNS` and `ADDED_VIEWS` for the current version). Never
  re-pin a running executable by changing database metadata.
- `scripts/ship` ships compatible changes only. A schema change goes out through
  `scripts/deploy --schema-cutover --deadline <time>` (SPEC §2.2): notice, drain, the runs still
  live at the deadline cancelled with a reason naming the cutover, then the migration.

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
- Serve a copy, never the live home: build with `scripts/check` (or `cargo build -p sluice` with
  the `--config` above), then
  `SLUICE_HOME=<scratch copy> target/debug/sluice serve --no-runner --port <free port>`.
