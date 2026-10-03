# Rust foundation

Python remains the live implementation. Phase 0 adds seven compiling Rust
crates and wire contracts alongside it. Executable modes deliberately fail
with the shared `bad_request` envelope and start no service or payload.

Run `scripts/check` for the complete G0 gate. Its commands are the named
formatting, Clippy, workspace-test and doctest recipes. They use the installed
Cargo shim and explicitly select this worktree's `target/`, because that shim
otherwise selects a shared directory. This unit's task requires its supplied
`rw/p0` worktree, rather than a new kiln fork. No CARGO variables are set or
job budgets changed. Later kiln integration can register these recipes once
Sluice has a build driver.

Use a scratch `SLUICE_HOME` even for `--help`. An absent home defaults to the
protected owner home and is refused. A home inside the owner's `.sluice`,
including a symlink alias or a nonexistent descendant, is refused before clap
parsing. The only current process creation is the test harness invoking the
inert build-path binaries. The guard resolves paths read-only; it creates no
home, lock, socket or configuration.

## Contract ownership

`sluice-model` owns commands, replies, ids, events, error envelopes and RPC.
`sluice-process::FnHost` accepts a `FnInvocation` and returns ordered outputs or
a typed error. The binary injects the host, so process never depends on agents.
`RuntimeApi` and the local coordinator-client stub require Send futures. The
web crate calls that client; only store will own SQLite connections.

Commands use `{"command": name, "args": payload}`. Replies use
`{"reply": name, "data": payload}`. Events use the `kind` field and records
flatten their event next to `seq`, `at` and immutable project id. Error JSON
uses `error`, `message` and variant-specific `errors`, `current_rev`,
`retryable` or `session`. Diagnostics remain the baseline's path-bearing
strings. No legacy inbox or thread-post aliases are provided.

Every plan editor carries `EditOptions` with dry_run, expected revision,
reason and author. PlanPatch retains its explicit rev field. Step retry
includes feedback and returns steps/rearmed/stopped_at. Set-input uses one
ordered inputs map and reports changed/running/unsupported. Rename and delete
carry immutable selectors/settings revision and deletion confirmation.
Needs-reply remains optional in message-post so the runtime can distinguish
an omitted value from an explicit false and apply the reply-specific default.

Plan documents and fn specifications are ordered raw maps until P1/P4 add
validation. `ValidatedPlan` is an opaque serialization placeholder for P1;
no validation or prepared edit can succeed in this build. `Type` and `Gate`
encode the target domain vocabulary; CWL forms and gate-string parsing belong
to P1. The six RFC 6902 patch variants have closed shared wire shapes; P1
will parse their pointer strings using jsonptr and adapt to json-patch.

## JSON and framing

Every external CLI/MCP/socket/database JSON boundary must call
`rpc::decode_json` before decoding a command. It limits documents to 16 MiB,
checks integer spellings against signed i64, rejects duplicate keys at every
nesting level, rejects non-finite numbers and trailing data, then converts to
the typed closed shape. `JsonValue` has private validated storage, and its
TryFrom also validates already parsed values. Calling serde_json directly on
maps bypasses the envelope's duplicate-key and integer-token checks.

`JsonMap` is an IndexMap wrapper; order is retained, while the generated schema
uses the ordinary object vocabulary without extra dependency features.
`rpc::encode_frame` and `decode_frame` use a four-byte big-endian byte length
followed by one UTF-8 document. The request carries protocol 1, request id and
an optional opaque capability. A run callback supplies the capability;
unauthenticated public command clients can omit it. Authentication enforcement
belongs to P3. Capability Debug output is redacted.

Generate the dependency inventory with `uv run python docs/rust/generate_dependencies.py`.

Generate `docs/rust/schemas.json` with:

```sh
cargo --config "build.target-dir=\"$PWD/target\"" run --locked --example schemas > docs/rust/schemas.json
```

The registry covers every public serializable contract. Its fixture tests
check round trips, generated-schema snapshots, positive schema instances and
unknown-field rejection for closed objects. The schema checker in tests only
implements the vocabulary emitted here. A separate web test uses rmcp's own
schemars re-export. JSON Schema cannot describe duplicate keys or distinguish
`1e20` from an integer token with the same numeric value. JsonValue therefore
records these lexical constraints as schema annotations, and decoder fixtures
prove enforcement. Schema validation cannot replace strict decoding.

## Shared test support

`tests/support/{home,clock,chrome,free_port}.rs` is shared by path modules in
Rust integration tests. ScratchHome owns a tempdir without changing global
environment. Clock exposes monotonic elapsed Duration with injectable manual
advancement. The port helper returns a bound listener, preserving the
reservation until the caller transfers it. Chrome returns a typed not
implemented error until P6 provides the driver.

`crates/sluice/src/bin/fixture.rs` builds the `fixture` binary for later fake
fn/Codex/Claude/Devin protocols. It is inert and requires a scratch home.
`crates/sluice-model/tests/fixtures/corrected.json` records same-run Transient
identity, path-only file binding, release/reacquire, all HTML patches before
the version marker, and Applied/Conflict/Discarded outcomes. These establish
wire contracts, not scheduler, process or stream behavior acceptance.
