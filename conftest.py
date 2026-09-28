"""For every test (tests/ and the packs'): the environment a sluice step sets is never
inherited. Run inside a step (an agent working on sluice), SLUICE_HOME is the live home: a
test that falls back to it would open, and so migrate or write, the real database."""

import pytest

STEP_VARS = ("SLUICE_PROJECT", "SLUICE_STEP", "SLUICE_RUN_ID", "SLUICE_RUN_DIR",
             "SLUICE_FN_DIR", "SLUICE_STEP_INPUTS", "SLUICE_STEP_OUTPUTS", "SLUICE_AUTHOR")


@pytest.fixture(autouse=True)
def _no_ambient_home(tmp_path_factory, monkeypatch):
    for name in STEP_VARS:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("SLUICE_HOME", str(tmp_path_factory.mktemp("ambient-home")))
