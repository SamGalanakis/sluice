# Legacy Python fixture

`sluice/` holds the only old Python modules still needed: `__init__.py`, `db.py` (the schema-v6
definition with `connect`/`write`) and `errors.py`, unchanged from the old package. The
old-schema home builders `crates/sluice/tests/fixtures/build_python_home.py` and
`crates/sluice/tests/acceptance/fixtures/python_home.py` import them to build scratch Python
homes for the `import-python-home` tests.

This directory is deleted together with `import-python-home`, as are the importer's pinned
copies under `packs/` and `src/sluice/fns/`.
