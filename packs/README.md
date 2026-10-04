# packs: compile-time pins only

These are not installable packs; Rust builtins in `crates/sluice-runtime/src/builtins/`
replaced them. `crates/sluice/src/import_python_home/builtins.rs` embeds the old shipped fn
bundles (`fn.json`, `icon.svg`, `main.py`) and helper sources (`agents/_agents/`,
`git/_git/refs.py`) with `include_str!`/`include_bytes!` as the pinned metadata the
Python-home importer compares against. `jev/*/fn.json` and the `fn.json` files here are also
the reference the builtin descriptor tests (`crates/sluice-runtime/tests/{catalog,git_gh,jev,staging}.rs`)
check against. Deleted together with `import-python-home`.

`agents/NOTICE` and `agents/LICENSE-APACHE` stay while the adapted `_agents/native/` sources
do. `crates/sluice-agents` carries adapted Omnigent code too (see
`src/engines/claude/protocol.rs`), so this attribution moves there before `packs/` goes.
