# packs: reference descriptors only

These are not installable packs; Rust builtins in `crates/sluice-runtime/src/builtins/`
replaced them. What is left is the `fn.json` of each old shipped agent, git, gh and jev fn and
five `icon.svg` files. They are the reference the builtin descriptor and icon tests
(`crates/sluice-runtime/tests/{catalog,git_gh,jev}.rs`) check the compiled catalog against.
The Apache-2.0 attribution for the adapted Omnigent code is in `crates/sluice-agents/NOTICE`.
