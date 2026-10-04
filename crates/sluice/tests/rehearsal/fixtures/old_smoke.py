"""Exercise the old-side fixture before packaging integrates; no cutover claim."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[5]
HERE = Path(__file__).resolve().parent
UV = '/home/sam/.local/bin/uv'


def run(args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs)


def wait(root, child, filename):
    deadline = time.monotonic() + 30
    while not (root / filename).exists():
        assert child.poll() is None, (root / 'old.log').read_text()
        if time.monotonic() >= deadline:
            evidence = REPO / 'target/p7-03-evidence/old-failure'
            evidence.mkdir(parents=True, exist_ok=True)
            for home in ['python-home','restored-home']:
                if (root / home / 'projects').exists():
                    shutil.copytree(root / home / 'projects',evidence / home,dirs_exist_ok=True,symlinks=True,ignore=lambda d,n:[x for x in n if (Path(d)/x).is_socket()])
                if (root / home / 'sluice.db').exists():
                    shutil.copy2(root / home / 'sluice.db',evidence / f'{home}.db')
            raise AssertionError(f'old fixture timed out; evidence: {evidence}')
        time.sleep(.05)


with tempfile.TemporaryDirectory(prefix='sluice-test-old-', dir='/tmp') as temp:
    root = Path(temp)
    bin = root / 'bin'
    bin.mkdir()
    for name in ['sh','bash','python3','git','sleep']:
        (bin / name).symlink_to(Path('/usr/bin') / name)
    (bin / 'uv').symlink_to(UV)
    (bin / 'tmux').symlink_to(REPO / 'target/private-tmux/bin/tmux')
    (bin / 'systemd-run').write_text('#!/bin/sh\nexit 1\n')
    (bin / 'systemd-run').chmod(0o700)
    (bin / 'codex.py').symlink_to(HERE / 'lane_tools.py')
    (bin / 'codex').write_text(f"#!/bin/sh\nexport G7_ROOT='{root}'\nexport G7_FIXTURE='{REPO / 'target/debug/fixture'}'\nexec /usr/bin/python3 '{bin / 'codex.py'}' \"$@\"\n")
    (bin / 'codex').chmod(0o700)
    (bin / 'sluice').write_text(f"#!/bin/sh\nexport PYTHONPATH='{REPO / 'src'}'\nexec {UV} run --project '{REPO}' --no-sync python -m sluice.cli \"$@\"\n")
    (bin / 'sluice').chmod(0o700)
    (root / 'owner/.codex').mkdir(parents=True)
    (root / 'owner/.codex/auth.json').write_text('fixture credential')
    run(['/usr/bin/git','init','-q','--bare','--initial-branch=main',str(root / 'remote.git')])
    run(['/usr/bin/git','clone','-q',str(root / 'remote.git'),str(root / 'repo')])
    for args in [['config','user.name','Old fixture'],['config','user.email','old@example.invalid'],['commit','--allow-empty','-qm','Create the fixture baseline.'],['push','-q','origin','HEAD:main']]:
        run(['/usr/bin/git',*args],cwd=root / 'repo')
    env = dict(os.environ, HOME=str(root / 'owner'),CODEX_HOME=str(root / 'owner/.codex'),PATH=str(bin),SLUICE_HOST_PATH=str(bin),
        SLUICE_HOME=str(root / 'python-home'),SLUICE_CODEX_CLI=str(bin / 'codex'),SLUICE_BACKOFF='0')
    command = [UV,'run','--project',str(REPO),'--no-sync','python',str(HERE / 'old_runner.py'),str(root)]
    log = (root / 'old.log').open('w')
    child = subprocess.Popen([*command,'start'],env=env,stdout=log,stderr=log)
    try:
        wait(root,child,'python-ready.json')
        ready = json.loads((root / 'python-ready.json').read_text())
    finally:
        child.terminate()
        child.wait(timeout=30)
    assert child.returncode == 0, (root / 'old.log').read_text()
    restored = root / 'restored-home'
    shutil.copytree(root / 'python-home',restored,symlinks=True)
    env['SLUICE_HOME'] = str(restored)
    child = subprocess.Popen([*command,'restore'],env=env,stdout=log,stderr=log)
    try:
        wait(root,child,'python-adopted.json')
        for name,args in [
            ('step_set_input',{'project':'rollback-fixture','step':'work','input':'session','value':ready['checkpoint']['session'],'reason':'resume after rollback'}),
            ('thread_post',{'project':'rollback-fixture','thread':'step-work','to':'work','body':'Continue where you left off after rollback; use this Python run header and callback tools.','needs_reply':False}),
            ('step_retry',{'project':'rollback-fixture','steps':['work'],'reason':'resume after rollback'}),
            ('step_pause',{'project':'rollback-fixture','steps':['push'],'paused':True,'reason':'old-side smoke has no Rust effect to reconcile'}),
            ('project_update',{'name':'rollback-fixture','paused':False,'reason':'restore original active project'})]:
            run([str(bin / 'sluice'),'tool',name,json.dumps(args)],env=env)
        wait(root,child,'python-resumed.json')
        resumed = json.loads((root / 'python-resumed.json').read_text())
        assert resumed['checkpoint']['session'] == ready['checkpoint']['session']
        assert resumed['run'] != ready['run']
        assert resumed['run'] in resumed['task']
        assert ready['run'] not in resumed['task']
        print(json.dumps({'old_session':ready['checkpoint']['session'],'restored_session':resumed['checkpoint']['session'],'old_tool_shapes':True}))
    finally:
        child.terminate()
        child.wait(timeout=30)
