# sluice: notes for agents

- Rust workspace under `crates/`: `scripts/check` runs `cargo fmt --check`, Clippy with
  `-D warnings`, `cargo test --workspace --all-targets --locked` and doctests, all into this
  checkout's `target/`. Day to day, `cargo test --workspace` is the test command. For a bare
  cargo invocation pin the same directory with
  `cargo --config 'build.target-dir="<worktree>/target"' ...`; never set `CARGO_*` variables.
- The only Python left is the fn helper (`python/`), the custom-fn and engine test fixtures,
  `tests/browser.py` and the dependency-inventory generator in `docs/rust/`. Run
  `uv sync --locked` once per checkout: the engine acceptance gates use the repository's
  `.venv/bin/python`.
- Tests run in test mode: `.cargo/config.toml` sets `SLUICE_TEST=1` for everything cargo runs,
  and every unit Sluice starts inherits it. Test mode names user units `sluice-test-*`
  (production: `sluice-run-<run>`, `sluice-coordinator-<hash>`, `sluice-doctor-*`) and refuses
  the live installation's selected home. A test binary run outside cargo must set it too.
- `SPEC.md` is the contract, `DESIGN.md` the dashboard's look; change them with the code.
- Commits: plain sentences, the user as sole author; no AI co-author trailers or mentions.
- The live home runs a deployed release, never this working tree. After pushing to main,
  `SLUICE_HOME=<the selected home> scripts/deploy [REF] [--prefix DIR]` builds a pinned release
  of that commit, selects it under the installation fence and restarts the coordinator, serve
  and loop units; running steps are adopted. `sluice install status` names the selected home.

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
- Serve a copy, never the live home:
  `SLUICE_HOME=<scratch copy> target/debug/sluice serve --no-runner --port <free port>`.
