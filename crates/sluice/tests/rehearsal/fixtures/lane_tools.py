"""Installed only into the fixture bin. Git may access only its scratch root."""
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys

root = Path(os.environ['G7_ROOT']).resolve()
name = Path(sys.argv[0]).stem
args = sys.argv[1:]


def git(*args, cwd=None):
    return subprocess.check_output(['/usr/bin/git', *args], cwd=cwd, text=True).strip()


def log():
    with (root / 'tool-events.jsonl').open('a') as f:
        f.write(json.dumps({'tool': name, 'args': args, 'cwd': os.getcwd()}) + '\n')


if name == 'git':
    cwd = Path(args[args.index('-C') + 1]) if '-C' in args else Path.cwd()
    assert cwd.resolve().is_relative_to(root), f'git escaped scratch: {cwd}'
    if 'push' in args or 'fetch' in args:
        url = git('remote', 'get-url', 'origin', cwd=cwd)
        assert Path(url).resolve().is_relative_to(root), f'remote escaped scratch: {url}'
    log()
    os.execv('/usr/bin/git', ['git', *args])
elif name == 'kiln':
    log()
    if args[:2] == ['fork', 'lash']:
        dest = root / 'fork'
        subprocess.check_call(['/usr/bin/git', 'clone', '-q', str(root / 'remote.git'), str(dest)])
        git('config', 'user.name', 'Rehearsal fixture', cwd=dest)
        git('config', 'user.email', 'rehearsal@example.invalid', cwd=dest)
        (dest / 'env.sh').write_text(':\n')
        # Keep cleanup away from the real evidence archive by keeping the fork clean.
        git('config', 'core.excludesFile', str(root / 'ignore'), cwd=dest)
        print(dest)
    elif args[:2] == ['rm', 'lash']:
        assert args[2] == 'g7'
        shutil.rmtree(root / 'fork')
    else:
        raise AssertionError(f'unexpected fake kiln command: {args}')
elif name == 'linear':
    log()
    assert args[:2] == ['issue', 'comment'] or args[:2] == ['issue', 'update']
elif name == 'gh':
    raise AssertionError('GitHub must not be called by this fixture')
elif name == 'codex':
    assert Path.cwd().resolve().is_relative_to(root)
    os.environ['HOME'] = str(root / 'owner')
    os.environ['SLUICE_HOME'] = str(root / 'rust-home')
    os.environ['SLUICE_CODEX_FIXTURE'] = 'tui'
    os.environ['PATH'] = str(root / 'bin')
    if args and args[0] == '-c':
        os.execv('/bin/sh', ['sh', '-c', "printf '› '; exec /usr/bin/sleep 600"])
    if args and args[0] == 'app-server':
        private = Path(os.environ['CODEX_HOME'])
        private.mkdir(parents=True, exist_ok=True)
        rollout = private / 'sessions/g7.jsonl'
        rollout.parent.mkdir(exist_ok=True)
        rollout.write_text(json.dumps({'type':'session_meta','payload':{'id':'fixture-thread','cwd':os.getcwd()}})+'\n')
        con = sqlite3.connect(private / 'state_5.sqlite')
        con.execute('CREATE TABLE IF NOT EXISTS threads(id TEXT PRIMARY KEY,cwd TEXT,rollout_path TEXT)')
        con.execute('INSERT OR REPLACE INTO threads VALUES (?,?,?)', ('fixture-thread', os.getcwd(), str(rollout)))
        con.commit()
        con.close()
        if not (root / 'worker-committed').exists():
            Path('work.txt').write_text('scratch lane effect\n')
            git('add', 'work.txt')
            git('commit', '-qm', 'Complete the scratch lane (G7-1)')
            (root / 'worker-committed').write_text('yes')
    log()
    os.execv(os.environ['G7_FIXTURE'], [os.environ['G7_FIXTURE'], 'codex', *args])
else:
    raise AssertionError(name)
