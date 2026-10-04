"""Actual old runner and fake Codex, pinned to this checkout and a private home."""
import json
import os
from pathlib import Path
import signal
import sys
import time

REPO = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(REPO / 'src'))
from sluice import db
from sluice.runner import Runner, kill, _native_roots
from sluice.store import Store

root = Path(sys.argv[1]).resolve()
mode = sys.argv[2]
home = Path(os.environ['SLUICE_HOME']).resolve()
assert root.parent == Path('/tmp') and root.name.startswith('sluice-test-')
assert home.is_relative_to(root)


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.write_text(json.dumps(value))


if mode == 'start':
    home.mkdir(mode=0o700)
    write(home / 'config.json', {'fn_dirs':[str(REPO / 'packs/agents')]})
    store = Store(home)
    store.create_project('rollback-fixture', 'Private rollback rehearsal', 'fixture', 'fixture')
    store.create_project('already-paused', 'Keep the owner pause', 'fixture', 'fixture')
    store.update_project('already-paused', paused=True)
    fn = home / 'projects/rollback-fixture/fns/rollback.push'
    write(fn / 'fn.json', {'name':'rollback.push','inputs':{'cwd':'string'},'outputs':{'sha':'string'}})
    code = '''from sluice.fn import run, sh
def main(inp, ctx):
    sh(['git','-C',inp['cwd'],'push','-q','origin','HEAD:main'])
    return {'sha':sh(['git','-C',inp['cwd'],'rev-parse','HEAD']).stdout.strip()}
run(main)
'''
    (fn / 'main.py').write_text('# /// script\n# requires-python = ">=3.12"\n# dependencies = []\n# ///\n' + code)
    plan = {'inputs':{},'outputs':{},'steps':{
        'work':{'run':'agent.run','outputs':{'word':'string'},'in':{
            k:{'default':v} for k,v in {'engine':'codex','model':'sol','cwd':str(root / 'repo'),
                'spec':'Labelled rollback fixture. Continue this same session with the current full run header and callback instructions.',
                'listen':False}.items()}},
        'push':{'run':'rollback.push','after':['work'],'in':{'cwd':{'default':str(root / 'repo')}}}}}
    store.patch('rollback-fixture',1,[{'op':'replace','path':f'/{k}','value':v} for k,v in plan.items()],'fixture','fixture')
    staging = root / 'staging'
    write(staging / 'config.json', {'fn_dirs':[]})
    write(staging / 'plan-conversion.json', {'pause_states':{'rollback-fixture':False,'already-paused':True}})
    converted = staging / 'projects/rollback-fixture/fns/rollback.push'
    write(converted / 'fn.json', json.loads((fn / 'fn.json').read_text()))
    (converted / 'main.py').write_text((fn / 'main.py').read_text().replace('from sluice.fn import','from sluice_fn import'))
else:
    assert mode == 'restore'
    store = Store(home)
    for project in store.projects():
        store.update_project(project['name'], paused=True)

runner = Runner(store)


def stop(*_):
    kill(*(run for active in runner.active.values() for run in active.runs))
    raise SystemExit(0)


signal.signal(signal.SIGTERM, stop)
deadline = time.monotonic() + 60
try:
    while time.monotonic() < deadline:
        runner.tick()
        work = store.read_state('rollback-fixture')['steps']['work']
        if mode == 'start' and work['status'] == 'running':
            for rid in work.get('run_ids', []):
                native = store.runs_dir('rollback-fixture') / rid / 'native.json'
                if native.is_file():
                    checkpoint = json.loads(native.read_text())
                    if checkpoint.get('session') and (root / 'worker-committed').exists():
                        # Pause before the stopped backup while preserving pre-window flags.
                        store.update_project('rollback-fixture', paused=True)
                        write(root / 'python-ready.json', {'run':rid,'checkpoint':checkpoint})
                        while True:
                            time.sleep(.05)
        if mode == 'restore':
            if not (root / 'python-adopted.json').exists() and work['status'] in {'failed','stale'}:
                write(root / 'python-adopted.json', {'status':work['status']})
            if (root / 'python-adopted.json').exists() and work['status'] == 'running':
                for rid in work.get('run_ids', []):
                    native = store.runs_dir('rollback-fixture') / rid / 'native.json'
                    if native.is_file():
                        checkpoint = json.loads(native.read_text())
                        if checkpoint.get('session'):
                            task = store.runs_dir('rollback-fixture') / rid / 'task.md'
                            write(root / 'python-resumed.json', {'run':rid,'checkpoint':checkpoint,
                                'task':task.read_text() if task.exists() else '',
                                'push_status':store.read_state('rollback-fixture')['steps']['push']['status']})
                            while True:
                                time.sleep(.05)
        if work['status'] == 'failed' and mode == 'start':
            raise RuntimeError(work.get('error','old agent failed'))
        time.sleep(.02)
    raise TimeoutError(f'old {mode} did not reach its checkpoint')
finally:
    owned = [run for active in runner.active.values() for run in active.runs]
    kill(*owned)
    checks = [{'native_empty':not _native_roots(run.run_dir),
               'wrapper_reaped':run.proc is None or run.proc.poll() is not None} for run in owned]
    assert all(v['native_empty'] and v['wrapper_reaped'] for v in checks)
    db.connect(home).execute('PRAGMA wal_checkpoint(TRUNCATE)')
    write(root / f'python-{mode}-cleanup.json', checks)
