# sluice: notes for agents

- Python with uv: `uv sync`, then run anything through `uv run` (e.g. `uv run sluice serve`).
- Tests: `uv run pytest -q` (check pytest's own exit code, not a pipe's). Lint:
  `uv run ruff check src tests packs`.
- `SPEC.md` is the contract, `DESIGN.md` the dashboard's look; change them with the code.
- Commits: plain sentences, the user as sole author; no AI co-author trailers or mentions.

## UI changes

Any change to the dashboard (`src/sluice/views.py`, `src/sluice/static/`) is verified in a real
headless Chromium before it is called done: screenshots at 390, 1440 and 2560 px wide, in light
and dark, of every page it touches, and the agent opens and looks at each screenshot itself.
Check that the content column is centred and shares its left edge with the nav, that nothing is
clipped, and that the page never scrolls sideways.

- Chromium: `~/.cache/ms-playwright/chromium-*/chrome-linux64/chrome` (or `SLUICE_CHROME`).
- Driver: `tests/browser.py` (`Chrome`: `open`, `eval`, `wait`, `send` for any DevTools call,
  e.g. `Emulation.setDeviceMetricsOverride`, `Emulation.setEmulatedMedia` with
  `prefers-color-scheme`, `Page.captureScreenshot`).
- Serve a copy, never the live `~/.sluice`:
  `SLUICE_HOME=<scratch copy> uv run sluice serve --no-runner --port <free port>`.
