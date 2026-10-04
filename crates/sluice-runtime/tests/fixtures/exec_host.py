import contextlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import struct
import subprocess
import sys
import tempfile
import time
import traceback

ROOT = Path(os.environ['SLUICE_EXEC_TEST_ROOT'])
REPO = Path(os.environ['SLUICE_EXEC_TEST_REPO'])
BIN = Path(os.environ['SLUICE_EXEC_TEST_BIN'])
UV = shutil.which('uv')

def read(s, n):
    out = b''
    while len(out) < n:
        part = s.recv(n-len(out))
        if not part:
            raise RuntimeError('socket closed')
        out += part
    return out

class Gate:
    def __init__(self, label):
        # Leave room for run UUID and agent-host/agent.sock under sun_path's limit.
        self.root = ROOT / str(len(list(ROOT.iterdir())))
        self.root.mkdir()
        self.home = self.root
        (self.home/'probe-label').write_text(label)
        self.env = os.environ.copy()
        # Both engine selection and callback executables are pinned to scratch fakes.
        self.env.update(SLUICE_HOME=str(self.home), SLUICE_FIXTURE='1',
            SLUICE_BACKOFF='0', SLUICE_PYTHON_DIR=str(REPO/'python'),
            SLUICE_UV_BIN=shutil.which('uv'),
            SLUICE_FAKE_ENGINE_BIN=str(ROOT/'fake-engine'),
            SLUICE_FAKE_ENGINE_SCRIPT=str(self.home/'fake-script.json'))
        self.env.pop('SLUICE_TEST_AGENT_TRANSIENT_MARKER', None)
        fake_bin = self.root / 'bin'
        fake_bin.mkdir()
        for name, source in [('uv', UV), ('python3', '/usr/bin/python3'),
                             ('bash', '/usr/bin/bash'), ('git', '/usr/bin/git')]:
            (fake_bin / name).symlink_to(source)
        fake = fake_bin / 'fake-engine'
        fake.write_bytes((REPO / 'crates/sluice-runtime/tests/fixtures/exec_engine.py').read_bytes())
        fake.chmod(0o700)
        self.env['SLUICE_FAKE_ENGINE_BIN'] = str(fake)
        self.env['PATH'] = self.env['SLUICE_HOST_PATH'] = str(fake_bin)
        (self.home/'fake-script.json').write_text('{}')
        self.broker = None
        self.leases = []
        try:
            self.boot()
            self.project = self.rpc('project_create', dict(name='review', description='', resources={'section':1}, author='review'))['data']['project_id']
        except BaseException:
            self.close()
            raise

    def exchange(self, command, capability=None, keep=False):
        s = socket.socket(socket.AF_UNIX)
        s.settimeout(20)
        s.connect(str(self.home/'coordinator.sock'))
        body=json.dumps(dict(protocol=1, request_id='review-'+str(time.time_ns()), run_capability=capability, command=command)).encode()
        s.sendall(struct.pack('>I',len(body))+body)
        reply=json.loads(read(s,struct.unpack('>I',read(s,4))[0]))
        if keep:
            self.leases.append(s)
        else:
            s.close()
        return reply

    def rpc(self, name, args=None):
        command={'command':name}
        if args is not None:
            command['args']=args
        r=self.exchange(command)['result']
        assert r['status']=='ok', r
        return r['value']

    def boot(self):
        log=(self.home/'broker.log').open('ab')
        self.broker=subprocess.Popen([str(BIN),'coordinator'],env=self.env,stdout=log,stderr=log)
        log.close()
        self.wait(lambda: self.available(), 20)

    def available(self):
        try:
            self.rpc('projects_list')
            return True
        except (OSError, RuntimeError):
            assert self.broker.poll() is None, (self.home/'broker.log').read_text()
            return False

    def wait(self, f, seconds=20):
        until=time.monotonic()+seconds
        while time.monotonic()<until:
            if value := f():
                return value
            time.sleep(.02)
        raise AssertionError('Timed out: '+str(self.home))

    def selector(self):
        return {'kind':'id','value':self.project}

    def plan(self, steps):
        self.rpc('plan_patch',dict(project=self.selector(),rev=1,ops=[dict(op='replace',path='',value=dict(inputs={},steps=steps,outputs={}))],start=True,dry_run=False,reason='review',author='review'))

    def lease(self):
        r=self.exchange({'runtime':'acquire_scheduler','args':{'owner':'review'}},keep=True)
        assert 'Ok' in r['result'],r

    def status(self):
        return self.rpc('status',dict(project=self.selector(),selection=dict(steps=None,tags=None)))['data']['steps']['work']

    def function(self, code):
        d=self.home/'projects'/self.project/'fns/custom.worker'
        d.mkdir(parents=True)
        (d/'fn.json').write_text(json.dumps(dict(name='custom.worker',inputs={},outputs={},open=True)))
        (d/'main.py').write_text('# /// script\n# requires-python = ">=3.12"\n# dependencies = []\n# ///\n'+code)

    def script(self, data):
        (self.home/'fake-script.json').write_text(json.dumps(data))

    def events(self):
        p=self.home/'fake-events.jsonl'
        return [json.loads(l) for l in p.read_text().splitlines()] if p.exists() else []

    def ready(self):
        return any(e.get('command',{}).get('command')=='deliver_text' for e in self.events())

    def wrapper(self, suffix=''):
        self.function('from sluice_fn import run\nimport time\ndef main(inp, ctx):\n    result=ctx.builtin("agent.run", {"engine":"fake", "cwd":'+repr(str(self.root))+', "spec":"Do the work"})\n'+suffix+'    return {}\nrun(main)\n')
        self.plan({'work':dict(run='custom.worker',needs={'section':1},outputs={'summary':'string'})})

    def native(self):
        self.plan({'work':dict(run='agent.run',needs={'section':1},outputs={'summary':'string'}, **{'in':{k:{'default':v} for k,v in dict(engine='fake',cwd=str(self.root),spec='Do the work').items()}})})

    def close(self):
        for s in self.leases:
            s.close()
        if self.broker and self.broker.poll() is None:
            self.broker.kill()
            self.broker.wait()
        for runtime in (self.home/'runs').glob('*/runtime.json'):
            run=runtime.parent.name
            import uuid
            uuid.UUID(run)
            unit='sluice-test-'+run+'.service'
            subprocess.run(['/usr/bin/systemctl','--user','stop',unit],capture_output=True,timeout=20)
            subprocess.run(['/usr/bin/systemctl','--user','reset-failed',unit],capture_output=True,timeout=20)
            state = subprocess.run(['/usr/bin/systemctl','--user','show',unit,'-p','ActiveState','-p','ControlGroup'],capture_output=True,text=True,timeout=20).stdout
            assert 'ActiveState=active' not in state and all(not line.removeprefix('ControlGroup=') for line in state.splitlines() if line.startswith('ControlGroup=')), (unit, state)

@contextlib.contextmanager
def gate(label):
    g=Gate(label)
    try:
        yield g
    finally:
        g.close()

def cancel_sidecar():
    with gate('cancel-sidecar') as g:
        # Agent finishes, then wrapper stays alive while its persistent sidecar is idle.
        g.script({'outputs':{'summary':'complete'}})
        g.wrapper("    (ctx.home / 'wrapper-waiting').write_text('yes')\n    time.sleep(300)\n")
        g.lease()
        g.wait(lambda:(g.home/'wrapper-waiting').exists())
        run=g.status()['run_ids'][0]
        sidecar=json.loads((g.home/'runs'/run/'agent-host/executor.json').read_text())
        g.rpc('step_cancel',dict(project=g.selector(),selection={'steps':['work'],'tags':None},reason='review',author='review'))
        time.sleep(12)
        status=g.status()
        alive=Path('/proc')/str(sidecar['pid'])
        with sqlite3.connect(g.home/'sluice.db') as db:
            holds=db.execute("select state from leases where run_id=?",(run,)).fetchall()
        evidence=dict(home=str(g.home),status=status,sidecar_alive=alive.exists(),sidecar=sidecar,leases=holds)
        print(json.dumps(evidence),flush=True)
        if status['status'] != 'failed':
            print('broker log:', (g.home/'broker.log').read_text(), flush=True)
            for file in (g.home/'runs'/run).glob('*.json'):
                if file.name in ['completion.json','collected.json']:
                    print(file.name, file.read_text(), flush=True)
            print('unit:', subprocess.run(['/usr/bin/systemctl','--user','show','sluice-test-'+run+'.service','-p','ActiveState','-p','Result'],capture_output=True,text=True).stdout,flush=True)
        assert status['status']=='failed' and not alive.exists(), 'Cancellation left the sidecar alive and run/holds unsettled'
        assert holds and all(state == 'released' for (state,) in holds), holds

def direct_section():
    with gate('direct-section') as g:
        g.function('from sluice_fn import run\ndef main(inp,ctx):\n    with ctx.acquire("section", timeout=2):\n        pass\n    return {}\nrun(main)\n')
        reply=g.rpc('fn_call',dict(name='custom.worker',inputs={},project=g.selector(),wait_seconds=10,direct=True,author='review'))
        print(json.dumps(dict(home=str(g.home),reply=reply)),flush=True)
        assert reply['data']['status']=='succeeded', 'A project direct call cannot acquire a section lease'
        with sqlite3.connect(g.home/'sluice.db') as db:
            holds = db.execute("select state from leases where kind='section'").fetchall()
        assert holds == [('released',)], holds

def changed_sibling_signature():
    with gate('changed-sibling') as g:
        g.function('from sluice_fn import run\nimport time\ndef main(inp,ctx):\n    (ctx.home/"waiting").write_text("yes")\n    while not (ctx.home/"finish").exists(): time.sleep(.02)\n    return {}\nrun(main)\n')
        sibling=g.home/'projects'/g.project/'fns/custom.sibling'
        sibling.mkdir()
        manifest=dict(name='custom.sibling',inputs={},outputs={'value':'int'})
        (sibling/'fn.json').write_text(json.dumps(manifest))
        (sibling/'main.py').write_text('from sluice_fn import run\nrun(lambda inp,ctx:{"value":1})\n')
        g.plan({'work':{'run':'custom.worker'},'sibling':{'run':'custom.sibling','after':['work']},'consumer':{'run':'core.echo','in':{'value':{'source':'sibling/value'}}}})
        g.lease()
        g.wait(lambda:(g.home/'waiting').exists())
        # Remove a return port on another fn, while work keeps its valid frozen bundle.
        manifest['outputs']={}
        (sibling/'fn.json').write_text(json.dumps(manifest))
        g.rpc('fn_list',dict(project=g.selector()))
        (g.home/'finish').write_text('yes')
        time.sleep(2)
        with sqlite3.connect(g.home/'sluice.db') as db:
            row=db.execute("select status,error from steps where step_id='work'").fetchone()
        journals=list((g.home/'runs').glob('*/completion.json'))
        completion=json.loads(journals[0].read_text()) if journals else None
        launch=json.loads((journals[0].parent/'runtime.json').read_text()) if journals else None
        replay=g.exchange({'method':'complete','params':completion}, launch['capability']) if journals else None
        print(json.dumps(dict(home=str(g.home),step=row,journals=[str(p) for p in journals],replay=replay)),flush=True)
        assert row[0]=='succeeded', 'Unrelated live fn schema prevents frozen completion'
        with sqlite3.connect(g.home/'sluice.db') as db:
            frozen = json.loads(db.execute('select request from attempts where step_id="work"').fetchone()[0])
        assert 'invalid stored plan' in frozen['runtime_reconciliation_error'], frozen

def stale_tool():
    with gate('stale-tool') as g:
        g.function('from sluice_fn import run\nrun(lambda inp,ctx:{})\n')
        g.plan({'work':{'run':'custom.worker'}})
        g.lease()
        g.wait(lambda:g.status()['status']=='succeeded')
        launch=json.loads((g.home/'runs'/g.status()['run_ids'][0]/'runtime.json').read_text())
        # New request identity, using the terminal run's original capability.
        helper=dict(protocol=1,request_id='stale-new-request',run_capability=launch['capability'],command={'command':'tool','args':{'name':'step_retry','args':dict(project=g.project,selection={'steps':['work'],'tags':None},message=None,reason='stale callback',author='review')}})
        wrong=g.exchange({'helper':{'identity':launch['identity'],'request':helper}},'wrong-capability')
        assert wrong['result']['status']=='error', wrong
        other=g.rpc('project_create',dict(name='other',description='',resources={},author='review'))['data']['project_id']
        foreign=json.loads(json.dumps(helper))
        foreign['command']['args']['args']['project']=other
        refused=g.exchange({'helper':{'identity':launch['identity'],'request':foreign}},launch['capability'])
        assert refused['result']['status']=='error', refused
        result=g.exchange({'helper':{'identity':launch['identity'],'request':helper}},launch['capability'])
        print(json.dumps(dict(home=str(g.home),reply=result,status=g.status())),flush=True)
        assert result['result']['status']=='error', 'Terminal capability successfully retried its step'

if __name__ == '__main__':
    globals()[sys.argv[1]]()
