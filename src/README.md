# src: compile-time pins only

The Python package is gone. `sluice/fns/*/fn.json` are the pinned shipped signatures that
`crates/sluice/src/import_python_home/builtins.rs` embeds with `include_str!`; three
`icon.svg` files beside them are the source assets `crates/sluice-runtime/tests/catalog.rs`
compares the compiled builtin icons against. Deleted together with `import-python-home`.
