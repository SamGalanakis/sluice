# Temporary Python-home importer fixture

Regenerate the synthetic schema-v6 database with the checked-out Python code:

```sh
uv run python crates/sluice/tests/fixtures/build_python_home.py
```

The builder sets SLUICE_HOME to its own temporary directory before importing
sluice.db. It does not use the live home. Tests copy source and staging to scratch,
expand @SOURCE@, and keep an active WAL to prove source preservation. The worker
script raises if anyone executes it. The fixture secret and Codex session are fake.

Run the Rust command with an absolute build path:

```sh
/path/to/sluice import-python-home /stopped/python-home /fresh/rust-home --staging /reviewed/cutover-staging
```

Without --staging, SLUICE_CUTOVER_STAGING or ./cutover-staging supplies the path.
The staging layout is:

```text
config.json, .env                 reviewed global configuration and secrets
fns/<name>/{fn.json,main.py,...}   converted global fns and sibling helpers
recipes/*.json                   converted global recipes
projects/<name>/fns/...           converted project fns and sibling helpers
projects/<name>/recipes/*.json    converted project recipes
projects/<name>/{config.json,.env}
builtins.json                    optional exact release descriptor array
plan-conversion.json             optional units and pre-window pause states
```

plan-conversion.json has this shape:

```json
{"units":{"project":{"unit":["step-a","step-b"]}},"pause_states":{"project":false}}
```

If pause_states is supplied, it must cover every imported project. The owner must
supply the saved flags from before the pause window for real cutover. Without it,
the importer labels pause provenance as stopped-snapshot. All imported projects
remain operationally paused regardless of their saved flags.

This firehose checkout has no completed builtin registry. The isolated fallback
uses pinned shipped descriptors. Supply builtins.json with the exact final release
catalog before installing an import. Unknown or incompatible declarations fail
conversion. Legacy message fns requiring output/cursor migration fail explicitly
rather than retaining old callbacks or log sequence ids.

The importer creates .<destination>.python-import beside the destination. Its
private ledger allocates fresh IDs once. All database rows commit in one writer
transaction; artifact recovery only publishes files. The home becomes visible by
atomic rename after required files, manifests and the generation marker are
synced. A retry rebuilds only its owned unpublished home, preserving IDs. Once
published, an identical snapshot is a no-op; changed snapshots or missing required
files are refused. The destination must have an existing parent directory.

Only referenced session data is retained. Codex private state is copied and its
rollout paths are rewritten. External credential symlinks are inspected without
reading their contents. Claude and Devin keep their external engine stores; the
importer validates the selected session and cwd read-only. Missing recorded cwd
is a reported paused resume error; no replacement directory is created. Actual
session resume belongs to P5, after fresh configuration and session locking.

The ignored stopped_copy_rehearsal test invokes rehearse_python_home.py. It makes
a read-only SQLite online backup in /tmp, copies only required source files, and
stages reviewed scripts from the immutable p4-06 commit a882b7e. Set
SLUICE_IMPORT_STAGING_REF to another reviewed commit when needed. It never runs
or resumes copied work. It prints counts and asserts live sluice.db mtime is
unchanged. The normal suite never accesses the live home.
