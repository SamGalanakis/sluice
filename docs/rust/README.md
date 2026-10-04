# Rust workspace notes

The workspace has seven crates:

| crate | owns |
|---|---|
| `sluice-model` | ids, commands and replies, events and records, the error envelope, RPC framing, plan parsing, types, gates, units, recipes, edits, status views, input hashing |
| `sluice-store` | the SQLite schema (`migrations/0001.sql`), the single writer and read pool, projects, plans, attempts, resources, messages, records, backup, the `query` reader |
| `sluice-process` | the run guardian, transient systemd units, cgroups, the payload launcher, the private tmux, file locks, the host prerequisite check |
| `sluice-agents` | the engine supervisor and the Claude, Codex and Devin engines |
| `sluice-runtime` | the coordinator, scheduler, calls, drain, verify, `next`/`watch`, the fn registry, the Python fn host, the builtins, the installation, the agent docs topics |
| `sluice-web` | the HTTP server, MCP, the dashboard views and streams |
| `sluice` | the binary: CLI modes, `me`, `doctor`, release checks, the installation entry |

`SPEC.md` is the behavioural contract; this page covers the wire contracts and build chores.

## Wire contracts

Commands are `{"command": name, "args": payload}`; replies are `{"reply": name, "data":
payload}`. Events carry a `kind` field, and a record flattens its event next to `seq`, `at` and
`project`. Errors are `{"error", "message"}` plus the variant's `errors`, `current_rev`,
`retryable`, `kind` or `session`.

Every external JSON boundary (CLI, MCP, sockets, stored JSON) decodes through
`rpc::decode_json`: documents up to 16 MiB, integers within signed i64, no duplicate keys at any
depth, no non-finite numbers, no trailing data, then the closed typed shape. Frames are a
four-byte big-endian length followed by one UTF-8 document. A request carries protocol 1, a
request id and, for run callbacks, the run capability.

`docs/rust/schemas.json` snapshots the JSON Schema of every public contract.
`crates/sluice-model/tests/contracts.rs` checks round trips, schema snapshots and unknown-field
rejection against it. Regenerate it after changing a contract:

```sh
cargo --config "build.target-dir=\"$PWD/target\"" run --locked -p sluice-model --example schemas > docs/rust/schemas.json
```

JSON Schema cannot express duplicate keys or tell `1e20` from an integer token, so those rules
are schema annotations and the decoder tests enforce them.

## Dependencies

`docs/rust/dependencies.md` lists the locked dependency tree. Regenerate it with
`uv run python docs/rust/generate_dependencies.py`.

## Checks

`scripts/check` runs formatting, Clippy, the workspace tests and the doctests against this
worktree's `target/` (it passes the target dir with `--config` and sets no `CARGO_*`
variables). Host prerequisites and the containment gates are in
[host-prerequisites.md](host-prerequisites.md).

`tests/support/` holds the helpers shared by the integration tests (scratch homes, a manual
clock, free ports, systemd unit names, the headless Chromium driver) and `tests/browser.py` the
Python DevTools driver used for dashboard screenshots. `crates/sluice/src/bin/fixture.rs` builds
the `fixture` binary that stands in for engines and fns in tests, and
`crates/sluice-web/examples/dashboard_fixture.rs` serves the dashboard over a seeded scratch
home.
