# sluice

sluice runs typed plans of functions for AI-agent work. An orchestrator, usually an agent
talking MCP, edits a plan: a small graph of steps, each a function with typed inputs and
outputs. A coordinator starts each step once what it reads is ready (and, for a step that needs
a share of its project's resources, once they have room), records what it produced, marks
results stale when their inputs change, and keeps runs alive across its own restarts and
upgrades. A local dashboard shows the plan as a live board, with an inbox for the decisions that
need a person.

> Unchecked slop: use at your own risk.

## Requirements

- Linux with cgroup v2, a systemd user manager that delegates controllers to user services,
  and `pidfd_open`. `sluice doctor` checks them (`docs/rust/host-prerequisites.md`).
- Rust 1.97.0 (pinned in `rust-toolchain.toml`), a C toolchain, `make`, `curl`, `pkg-config`
  and the ncurses headers (for the private tmux build).
- `uv` for Python fns, and the engine CLIs (`claude`, `codex`, `devin`) for the agent fns.

## Install

Build a release and install it into the default prefix, `~/.local/share/sluice`:

```sh
SLUICE_HOME=~/sluice-home scripts/deploy HEAD
```

`scripts/deploy [REF] [--prefix DIR]` (REF defaults to `origin/main`) builds the commit with
`scripts/build-release`, fences the installation, stops the running services, selects the new
release and the home, starts the coordinator, the dashboard (`serve --no-runner --port 3065`)
and the scheduler (`loop`) as systemd user units, runs `sluice doctor`, and unfences. Running
steps carry on through it. `scripts/build-release <prefix>` alone builds and stages a release
without selecting it.

Put the launcher on your `PATH`:

```sh
export PATH="$HOME/.local/share/sluice/bin:$PATH"
sluice install status    # the selected release and home, and any fence
sluice doctor            # host prerequisites and the release manifest
```

The launcher runs the selected release with its home. Every command uses `SLUICE_HOME` when it
is set, else the installation's selected home (`SLUICE_INSTALL_DIR`, default
`~/.local/share/sluice/install`), and stops with an error when there is neither.

## Use

- Dashboard: <http://127.0.0.1:3065/>.
- MCP (streamable HTTP): `http://127.0.0.1:3065/mcp`, e.g.
  `claude mcp add --transport http sluice http://127.0.0.1:3065/mcp`. The server's instructions
  and the `docs` tool explain the workflow.
- Shell: `sluice tool` lists the tools and `sluice tool <name> '<json>'` runs one;
  `sluice next` waits for what an orchestrator should act on; `sluice watch` follows a log;
  `sluice query` reads the database; `sluice docs` prints the agent docs.

## Develop

```sh
cargo --config 'build.target-dir="target"' test --workspace --locked
scripts/check            # fmt, clippy, tests, doctests
```

Run a scratch home from source, never the live one:

```sh
SLUICE_HOME=/tmp/sluice-scratch cargo run -p sluice -- serve --no-runner --port 3070
```

## Documents

- `SPEC.md`: the contract: model, tools, CLI, installation, processes.
- `DESIGN.md`: the dashboard's look.
- `PRODUCT.md`: who it is for and what it must do.
- `docs/agent/`: the topics the `docs` tool serves to agents.
- `docs/rust/`: wire schemas, dependencies, host prerequisites.
