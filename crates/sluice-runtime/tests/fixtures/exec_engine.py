#!/usr/bin/python3
import os, sys, json, socket, struct, pathlib
home = pathlib.Path(os.environ['SLUICE_HOME'])
config = json.loads(pathlib.Path(sys.argv[2]).read_text())
run = os.environ['SLUICE_RUN_ID']
state = {'status':'starting','turns_started':0,'turns_completed':0,'waiting':None,'background_work':[],'compactions':0,'final_text':'fake done','session_id':'fake-session','acknowledged':[],'not_accepted':[],'progress':0,'error':None}
messages = False
submitted = False
transient = False
sessions = home / 'fake-sessions.json'
data = json.loads(sessions.read_text()) if sessions.exists() else {}
data['fake-session'] = os.getcwd()
sessions.write_text(json.dumps(data))
def rpc(command):
    raw = json.dumps({'protocol':1,'request_id':str(state['progress']),'run_capability':os.environ['SLUICE_RUN_CAPABILITY'],'command':command}).encode()
    with socket.socket(socket.AF_UNIX) as s:
        s.connect(os.environ['SLUICE_CONTROL_SOCKET']); s.sendall(struct.pack('>I',len(raw))+raw)
        def read(n):
            out=b''
            while len(out)<n:
                part=s.recv(n-len(out))
                if not part: raise RuntimeError('callback closed')
                out+=part
            return out
        size=struct.unpack('>I',read(4))[0]; answer=json.loads(read(size))
        assert answer['result']['status']=='ok', answer
for line in sys.stdin:
    req=json.loads(line)
    cgroup=pathlib.Path('/proc/self/cgroup').read_text().strip().split('::')[-1]
    with (home/'fake-events.jsonl').open('a') as f: f.write(json.dumps({'run':run,'pid':os.getpid(),'cgroup':cgroup,**req})+'\n')
    error=None
    if req['operation']=='command':
        cmd=req['command']
        if cmd['command'] in ('start_fresh','resume'): state['status']='idle'
        if cmd['command'] in ('deliver_text','steer'):
            if cmd['id']['kind']=='message': messages=True
            state['acknowledged'].append(cmd['id']);state['turns_started']+=1;state['status']='busy';state['progress']+=1
        if cmd['command']=='request_exit': state['status']='exited'
    if req['operation']=='observe' and state['turns_started']:
        if config.get('fatal'):
            state['error']={'kind':'fatal','message':'fixture terminal error'}
        elif config.get('marker_transient_once') and not (home/'turn-committed.injected').exists():
            (home/'turn-committed').write_text(run)
            state['status']='idle';state['turns_completed']=state['turns_started'];state['progress']+=1
        elif config.get('transient_once') and not (home/'transient-ran').exists():
            (home/'transient-ran').write_text(run)
            state['error']={'kind':'transient','message':'fixture rate limit'}
        elif not config.get('wait_message') or messages:
            if not submitted:
                rpc({'command':'step_submit','args':{'project':os.environ['SLUICE_PROJECT_ID'],'step':os.environ['SLUICE_STEP'],'run':run,'outputs':config.get('outputs',{}),'author':'fixture'}})
                submitted=True
            state['error']=None;state['status']='idle';state['turns_completed']=state['turns_started'];state['progress']+=1
    print(json.dumps({'observation':state,'outcome':'acknowledged','error':error,'hook':None}),flush=True)
