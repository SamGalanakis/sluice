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
without selecting it. Before the fence, deploy runs `scripts/compat-check`, which starts the
new release on a copy of the home and checks that every release a live run is pinned to can
still read, say and submit against it; a failure stops the deploy unless
`--skip-compat "<reason>"` is given.

To ship a branch in one go, `scripts/ship [REF]` rebases it onto `origin/main`, runs the shared
gate (fmt, Clippy, fast tests, doctests and slow groups selected by changed sources), pushes to main, deploys `origin/main` to the selected home and checks that every live run was
adopted and the dashboard answers; `--dry-run` prints the steps without doing them.

Put the launcher on your `PATH`:

```sh
export PATH="$HOME/.local/share/sluice/bin:$PATH"
sluice install status    # the selected release and home, and any fence
sluice doctor            # host prerequisites, engine versions and the release manifest
```

The launcher runs the selected release with its home. Every command uses `SLUICE_HOME` when it
is set, else the installation's selected home (`SLUICE_INSTALL_DIR`, default
`~/.local/share/sluice/install`), and stops with an error when there is neither.

### Memory

Runs share the host with your agents, editor and services, so sluice caps them. Each run is
capped at 6 GiB (`execution.defaults.memory` in the home's `config.json`) and a step may ask
for up to 10 GiB (`ceilings.memory`); more needs `"memory_above_ceiling": true` on the step and
on its project's limits. A cap only kills a run that outgrows it; nothing is reserved. Runs
start on measured memory: while MemAvailable stays at least `execution.host_reserve` (default
max(16 GiB, 15% of memory)) above a 1 GiB admission charge (`execution.admission_charge`),
memory pressure (PSI `some avg10`) is below `execution.memory_pressure_max` (10), and the runs'
measured memory plus the charge fits `execution.total.memory` (default min(70 GiB, 55% of the
host's memory)). This configured total is a ceiling: the effective pool and workload slice's
`MemoryMax` use the smaller of it and the host's current memory, including ancestor cgroup
limits such as `user.slice`'s `memory.max`. A new run counts at the charge for its first 30 s
(`execution.admission_warmup_s`). Otherwise it waits and `status` names the check, e.g.
`held: host memory (available 9.1 GiB, reserve 16 GiB, need 1 GiB)`. Runs also wait while
CPU pressure (PSI `some avg10`) reaches `execution.cpu_pressure_max` (30) or the 1-minute load
per CPU reaches `execution.load_per_cpu_max` (1.5), and while either is over half its limit at
most `execution.warmup_starts` (2) start per warm-up, e.g. `held: cpu pressure (some avg10
74.0%, limit 30%)`. The workload slice is capped at `execution.total.cpus` (default 75% of the
available CPUs), clamped to current affinity and cgroup CPU capacity, with `CPUWeight`
`execution.cpu_weight` (default 20). Totals above current capacity remain valid: plan edits
and dry runs still work, and `status.execution.total` shows configured and effective values
with the reason for each clamp. Effective totals recover as host capacity returns, without a
restart. Memory can be made unlimited (`null`) only with `"allow_unlimited_memory": true`
under `execution`; without it such a config is refused. SPEC.md §2.5 has the details.

## Use

- Dashboard: <http://127.0.0.1:3065/>.
- MCP (streamable HTTP): `http://127.0.0.1:3065/mcp`, e.g.
  `claude mcp add --transport http sluice http://127.0.0.1:3065/mcp`. The server's instructions
  and the `docs` tool explain the workflow. Over stdio: `sluice mcp`, e.g.
  `claude mcp add sluice -- sluice mcp`.
- Shell: `sluice tool` lists the tools and `sluice tool <name> '<json>'` runs one (or
  `--field value` flags; `sluice tool <name> --help` lists them); `sluice next` waits for
  what an orchestrator should act on; `sluice watch` follows a log; `sluice query` reads the
  database; `sluice docs` prints the agent docs.

## Develop

```sh
cargo --config 'build.target-dir="target"' test --workspace --locked
scripts/check            # optional local use of the same gate ship runs
scripts/check --dry-run  # show slow groups selected by changed sources
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
