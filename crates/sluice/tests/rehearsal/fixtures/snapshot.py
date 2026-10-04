"""Online backup and a checked asset snapshot. Every source connection is read-only."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import stat
import subprocess
import sys

LIVE = Path('/home/sam/.sluice')
REPO = Path(__file__).resolve().parents[5]


def readonly(path):
    return sqlite3.connect(f'file:{path}?mode=ro', uri=True)


def rows(con):
    names = ['projects', 'plans', 'states', 'inbox', 'calls', 'submissions']
    existing = {r[0] for r in con.execute("SELECT name FROM sqlite_master WHERE type='table'")}
    return {name: sorted(con.execute(f'SELECT * FROM {name}').fetchall(), key=repr)
            for name in names if name in existing}


def assets(con):
    paths = set()
    def add(path):
        if path.is_symlink():
            # Keep the link itself; never traverse a credential or an unchecked target.
            paths.add(path)
        elif path.is_dir():
            for child in sorted(path.iterdir()):
                if child.name not in {'.git', '__pycache__'} and child.suffix != '.pyc':
                    add(child)
        elif path.is_file():
            paths.add(path)
    for part in ['fns', 'recipes', 'config.json', '.env']:
        add(LIVE / part)
    projects = con.execute('SELECT name FROM projects').fetchall()
    for name, in projects:
        assert Path(name).name == name
        base = LIVE / 'projects' / name
        for part in ['fns', 'recipes', 'config.json', '.env']:
            add(base / part)
        state = con.execute('SELECT doc FROM states WHERE project=?', (name,)).fetchone()
        for step in json.loads(state[0] if state else '{}').get('steps', {}).values():
            if step.get('status') != 'running':
                continue
            for run in step.get('run_ids', []):
                assert Path(run).name == run
                run_dir = base / 'runs' / run
                for part in ['native.json', 'input.json', 'output.json', 'exit.json']:
                    add(run_dir / part)
                if not (run_dir / 'native.json').exists():
                    continue
                native = json.loads((run_dir / 'native.json').read_text())
                session = native.get('session')
                if native.get('engine') != 'codex' or not session:
                    continue
                assert Path(session).name == session
                mapping = LIVE / 'codex-native-sessions' / f'{session}.json'
                add(mapping)
                private = Path(json.loads(mapping.read_text())['home']).resolve()
                assert private.is_relative_to(LIVE / 'codex-native-homes')
                for part in private.iterdir():
                    if (part.name in {'sessions', 'archived_sessions', 'session_index.jsonl',
                                     'history.jsonl', 'auth.json'}
                        or part.name.startswith('state_') and '.sqlite' in part.name):
                        add(part)
    for _, _, raw in con.execute('SELECT project,rev,doc FROM plans'):
        for step in json.loads(raw).get('steps', {}).values():
            for binding in step.get('in', {}).values():
                if isinstance(binding, dict) and isinstance(binding.get('file'), str):
                    path = Path(binding['file'])
                    if path.is_relative_to(LIVE):
                        add(path)
    return sorted(paths)


def fingerprints(paths):
    result = {}
    for path in paths:
        meta = path.lstat()
        if stat.S_ISLNK(meta.st_mode):
            content = os.readlink(path)
        else:
            digest = hashlib.sha256()
            with path.open('rb') as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b''):
                    digest.update(block)
            content = digest.hexdigest()
        result[str(path)] = (meta.st_mode, meta.st_size, meta.st_mtime_ns, content)
        for directory in path.parents:
            if directory == LIVE:
                break
            meta = directory.stat()
            result[str(directory)] = (meta.st_mode, meta.st_mtime_ns)
    return result


def copy_assets(paths, source):
    for path in paths:
        target = source / path.relative_to(LIVE)
        target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        shutil.copy2(path, target, follow_symlinks=False)
        assert stat.S_IMODE(path.lstat().st_mode) == stat.S_IMODE(target.lstat().st_mode)
    for mapping in (source / 'codex-native-sessions').glob('*.json'):
        value = json.loads(mapping.read_text())
        value['home'] = str(source / Path(value['home']).relative_to(LIVE))
        mapping.write_text(json.dumps(value))
    directories = {parent for path in paths for parent in path.parents
                   if parent != LIVE and parent.is_relative_to(LIVE)}
    for directory in sorted(directories, key=lambda p: len(p.parts), reverse=True):
        shutil.copystat(directory, source / directory.relative_to(LIVE))


def staging(source, target):
    target.mkdir(mode=0o700)
    ref = os.environ.get('SLUICE_IMPORT_STAGING_REF', 'a882b7e')
    names = subprocess.check_output(
        ['/usr/bin/git', 'ls-tree', '-r', '--name-only', ref, 'cutover-staging'],
        cwd=REPO, text=True).splitlines()
    assert names, 'converted p4-06 staging is unavailable'
    for name in names:
        relative = Path(name).relative_to('cutover-staging')
        if 'projects' not in relative.parts:
            continue
        dest = target / relative
        dest.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        dest.write_bytes(subprocess.check_output(['/usr/bin/git', 'show', f'{ref}:{name}'], cwd=REPO))
    with readonly(source / 'sluice.db') as con:
        names = [r[0] for r in con.execute('SELECT name FROM projects')]
    for scope in [Path(), *(Path('projects') / name for name in names)]:
        for part in ['config.json', '.env']:
            src = source / scope / part
            if src.exists():
                dst = target / scope / part
                dst.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                shutil.copy2(src, dst)
    # Snapshot pause states precede every simulated pause in the measured window.
    with readonly(source / 'sluice.db') as con:
        pauses = {r[0]: bool(r[1]) for r in con.execute('SELECT name,paused FROM projects')}
    (target / 'plan-conversion.json').write_text(json.dumps({'pause_states': pauses}))


def capture(root):
    assert root.resolve().parent == Path('/tmp') and root.name.startswith('sluice-test-')
    before = LIVE.joinpath('sluice.db').stat()
    for attempt in range(1, 33):
        source = root / 'source'
        shutil.rmtree(source, ignore_errors=True)
        source.mkdir(mode=0o700)
        try:
            with readonly(LIVE / 'sluice.db') as live, sqlite3.connect(source / 'sluice.db') as backup:
                live.backup(backup)
                original = rows(backup)
                selected = assets(backup)
                first = fingerprints(selected)
                copy_assets(selected, source)
                stable = (first == fingerprints(assets(live)) and original == rows(live))
            if stable:
                break
        except (FileNotFoundError, json.JSONDecodeError):
            continue
    else:
        raise RuntimeError('source changed during all 32 snapshots; retry at a quieter point')
    after = LIVE.joinpath('sluice.db').stat()
    assert (before.st_mtime_ns, before.st_size) == (after.st_mtime_ns, after.st_size), 'live database metadata changed during snapshot'
    (source / '.snapshot-origin.json').write_text(json.dumps({'home': str(LIVE)}))
    staging(source, root / 'staging')
    summary = {'snapshot_attempts': attempt, 'files': len(selected),
               'bytes': sum(p.lstat().st_size for p in selected),
               'live_db_mtime_ns': before.st_mtime_ns, 'live_db_size': before.st_size}
    (root / 'snapshot.json').write_text(json.dumps(summary))
    print(json.dumps(summary))


if __name__ == '__main__':
    capture(Path(sys.argv[1]))
