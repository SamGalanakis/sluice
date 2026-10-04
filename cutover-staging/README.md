# Project cutover staging

The importer installs `projects/lash` and `projects/figments` into the new Rust home.
These files are converted copies, not the running owner's fns. The first commit preserves
the live Python sources and recipes before conversion, excluding git metadata and Python
bytecode. The conversion does not install, invoke or modify the live copies.

There are 18 fn bundles, two shared helper directories, and six recipes. Pin each project's
shared helper root with its bundles. Fn entrypoints retain their PEP 723 metadata and use
`sluice_fn.run`. The launcher supplies the standalone helper through `PYTHONPATH`.

Project identity and project directory come from the launch context, rather than a home path
built from a project name. The hardcoded kiln fork root in `_figlib` is the external kiln
repository layout, preserved to keep the existing-fork reuse policy. Archive/report paths
are also repository policies, not Sluice home paths. Renaming a Sluice project does not rename
the kiln repository or those paths.

`plan-conversion.md` records the read-only plan observations and importer rules. The six
recipes add delivery exits and cleanup accept-skip gates by intent; the importer preserves
the older plans' plain gates and existing unit tags.

## Drift from the live copies

`pristine/{lash,figments}/{fns,recipes}` is the live owner's copy each conversion was last ported
from, byte for byte (git metadata, bytecode and tool caches excluded). `check-drift` compares the
live home (`~/.sluice`, or `--home`; only read) with it and exits 1 listing every changed, added
or removed file. On drift, port each change into `projects/`, keeping the conversion, then copy
the live trees over `pristine/` in the same commit. The cutover runbook runs it in preflight and
again immediately before the pause. pytest does not collect `pristine/`.

## Verification

Run `uv sync`, then `scripts/check`. Its staging Rust tests parse all manifest signatures,
expand all six actual recipes with sample parameters for three engines, preserve the audit
fan-in, and invoke `uv run --no-sync pytest -q -n0 -rs cutover-staging`.

The Python suite exercises every fn's logic offline. It ports the existing Lash contracts,
uses local bare git repositories for landing, and scratch executable fakes for kiln,
GitHub and Linear. The fake executable fails on an unconfigured command. Tests pin both
`PATH` and `SLUICE_HOST_PATH`; a subprocess guard rejects a tool resolved outside the
scratch executable directory.

When `python/sluice_fn` is absent, the logic tests use an explicit minimal helper double.
The 18 `test_real_helper_envelope_for_each_staged_fn` cases skip with a dependency message.
They run automatically once p4-01 lands, using real helper envelopes and a fake pinned
callback binary for worker composition, submissions, owner notes and rejected landing.
For a read-only check against an unmerged helper, set `STAGING_HELPER_ROOT` to that checkout's
`python` directory. No helper source is copied into production staging.

The p4-02 runtime registry loader is absent in this starting branch. The orchestrator
accepted signature/recipe/envelope gates here and assigned real registry loading to the
scheduled full run after merge. The staging files contain no fallback registry implementation.
