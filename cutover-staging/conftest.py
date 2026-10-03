"""Offline tests use the real helper when present, otherwise an explicit test double."""
import importlib.util
import os
import subprocess
import sys
import types
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, os.environ.get('STAGING_HELPER_ROOT', str(ROOT / 'python')))
REAL_HELPER = importlib.util.find_spec('sluice_fn') is not None
if not REAL_HELPER:
    helper = types.ModuleType('sluice_fn')

    class Rejected(RuntimeError):
        pass

    class Transient(RuntimeError):
        pass

    class AgentFailure(RuntimeError):
        def __init__(self, kind, message, session=None):
            super().__init__(message)
            self.kind, self.message, self.session = kind, message, session

    class ShError(RuntimeError):
        pass

    def sh(argv, *, check=True, cwd=None, env=None):
        import os
        result = subprocess.run(argv, cwd=cwd, env={**os.environ, **(env or {})},
                                capture_output=True, text=True, check=False)
        if check and result.returncode:
            raise ShError(f'{argv[0]} exited {result.returncode}: {result.stderr}')
        return result

    def run(*args, **kwargs):
        raise AssertionError('The test double cannot validate the real protocol envelope')

    helper.Rejected, helper.Transient, helper.AgentFailure = Rejected, Transient, AgentFailure
    helper.ShError, helper.sh, helper.stream, helper.run = ShError, sh, sh, run
    sys.modules['sluice_fn'] = helper
