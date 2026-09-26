# lash coding standards (for the reviewer)

You review a task branch's diff against its base with fresh context. Fix what violates these
standards by committing on the branch; do not leave comments. A question you cannot settle
inside the diff's scope goes in your summary, unfixed. Implementers get the process rules
(lane-rules.md, AGENTS.md); these are the design standards their work is held to.

## End state
- Wholehog: the change goes straight to the end state and deletes what it supersedes. No shims,
  dual paths, compat decoders, feature flags for the old way, or deprecation notes.
- Version freeze (FIG-3846): schemas and durable formats change in place. Do not bump guarded
  version constants (scripts/versioned-surfaces.toml) or add migrate expand steps. Old durable
  state a change cannot read is refused with a typed error before any effect, never decoded
  by a fallback.
- That pre-1.0 policy is never written into ADRs, CONTEXT.md or invariants.

## Boundaries
- lash exposes primitives (data, ids, events, cursors); hosts own presentation. No rendering,
  formatting for humans, or UI policy in lash crates.
- The effect interface stays engine-agnostic; Restate is its only implementor. No Restate types
  or assumptions leak above it.
- Hosts never drive: no new direct-drive or TurnBuilder execution paths (D5 / RunSpec design).
- ADR 0101 §4: the session command lane drains first at every turn boundary. No global
  command/input FIFO.
- Never add a drive-determinism site; the substrate allowlist only shrinks.
- Dialect/test262 work adds no language surface, builtins, or compiled/durable format change
  unless its spec names one.

## Tests and gates
- A regression test asserts its precondition and fails on the old code.
- Never weaken, narrow, exempt or re-pin a gate, allowlist or fixture to make it pass.
- A new or changed fallible store-trait method is driven in lash-sim's
  cross_backend_store_differential; a new durable table is in its residue digest.
- A renamed or deleted test/law has no stale entry left in scripts/restate-suites.toml,
  scripts/restate-divergences/, deferred-laws/ or any *.toml registry.
- A changed test262 census.tsv comes with regenerated differential sessions; new differential
  rows come with their corpus_laws allowlist rows.
- Programs run by `kiln run` resolve source paths through BUILD_WORKSPACE_DIRECTORY and user
  paths through BUILD_WORKING_DIRECTORY.

## Text
- No mention of Claude, Anthropic, Codex or AI in code, comments, commits or docs. No
  Co-Authored-By or "Generated with" lines.
