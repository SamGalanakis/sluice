//! Composition acceptance uses absolute binaries and private scratch service units.
use serde_json::{Value, json};
use sluice_model::{
    commands::CommandReply,
    ids::{ProjectId, RunId},
    rpc::{RpcReply, RpcResult, decode_json, encode_frame},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::{
        fs::{PermissionsExt, symlink},
        net::UnixStream,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Gate {
    temp: tempfile::TempDir,
    home: PathBuf,
    broker: Option<Child>,
    env: BTreeMap<String, String>,
    project: ProjectId,
}
impl Gate {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        for (name, path) in [
            ("uv", uv()),
            ("python3", PathBuf::from("/usr/bin/python3")),
            ("bash", PathBuf::from("/usr/bin/bash")),
            ("git", PathBuf::from("/usr/bin/git")),
        ] {
            symlink(path, bin.join(name)).unwrap();
        }
        let fake = bin.join("fake-engine");
        std::fs::write(&fake, FAKE).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
        let script = home.join("fake-script.json");
        std::fs::write(&script, b"{}").unwrap();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let mut env = BTreeMap::new();
        for (n, p) in [
            ("SLUICE_HOME", home.clone()),
            ("PATH", bin.clone()),
            ("SLUICE_HOST_PATH", bin),
            ("SLUICE_PYTHON_DIR", repo.join("python")),
            ("SLUICE_UV_BIN", uv()),
            ("SLUICE_FAKE_ENGINE_BIN", fake),
            ("SLUICE_FAKE_ENGINE_SCRIPT", script),
            (
                "SLUICE_TEST_AGENT_TRANSIENT_MARKER",
                home.join("turn-committed"),
            ),
        ] {
            env.insert(n.into(), p.to_string_lossy().into_owned());
        }
        env.insert("SLUICE_FIXTURE".into(), "1".into());
        env.insert("SLUICE_BACKOFF".into(), "0".into());
        env.insert("COMPOSITION_ENV_FIXTURE".into(), "frozen-launch".into());
        let mut gate = Self {
            temp,
            home,
            broker: None,
            env,
            project: ProjectId::new(),
        };
        gate.boot();
        let CommandReply::Project(p)=gate.rpc(json!({"command":"project_create","args":{"name":"compose","description":"composition fixture","icon":null,"resources":{"section":1},"author":"test"}})) else {panic!("project")};
        gate.project = p.project_id;
        gate
    }
    fn boot(&mut self) {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.home.join("broker.log"))
            .unwrap();
        self.broker = Some(
            Command::new(env!("CARGO_BIN_EXE_sluice"))
                .arg("coordinator")
                .envs(&self.env)
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        self.wait(|g| UnixStream::connect(g.home.join("coordinator.sock")).is_ok());
    }
    fn wait(&self, mut condition: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !condition(self) {
            assert!(
                Instant::now() < deadline,
                "timed out, broker: {}",
                std::fs::read_to_string(self.home.join("broker.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn selector(&self) -> Value {
        json!({"kind":"id","value":self.project})
    }
    fn rpc(&self, command: Value) -> CommandReply {
        let request = json!({"protocol":1,"request_id":"compose-test","run_capability":null,"command":command});
        let mut stream = UnixStream::connect(self.home.join("coordinator.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream.write_all(&encode_frame(&request).unwrap()).unwrap();
        let reply: RpcReply = decode_json(&read(&mut stream)).unwrap();
        match reply.result {
            RpcResult::Ok(v) => *v,
            RpcResult::Error(e) => panic!("command failed: {e:?}"),
        }
    }
    fn data(&self, command: Value) -> Value {
        let CommandReply::Data(v) = self.rpc(command) else {
            panic!("data")
        };
        v.into_value()
    }
    fn status(&self) -> Value {
        self.data(json!({"command":"status","args":{"project":self.selector(),"selection":{"steps":null,"tags":null}}}))
    }
    fn plan(&self, steps: Value) {
        self.rpc(json!({"command":"plan_patch","args":{"project":self.selector(),"rev":1,"ops":[{"op":"replace","path":"","value":{"inputs":{},"steps":steps,"outputs":{}}}],"start":true,"dry_run":false,"reason":"fixture","author":"test"}}));
    }
    fn lease(&self) -> UnixStream {
        let mut stream = UnixStream::connect(self.home.join("coordinator.sock")).unwrap();
        stream.write_all(&encode_frame(&json!({"protocol":1,"request_id":"scheduler","run_capability":null,"command":{"runtime":"acquire_scheduler","args":{"owner":"compose"}}})).unwrap()).unwrap();
        let reply: Value = decode_json(&read(&mut stream)).unwrap();
        assert!(reply["result"].get("Ok").is_some(), "{reply}");
        stream
    }
    fn function(&self, name: &str, inputs: Value, outputs: Value, code: &str) {
        let dir = self
            .home
            .join("projects")
            .join(self.project.to_string())
            .join("fns")
            .join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("fn.json"),
            json!({"name":name,"inputs":inputs,"outputs":outputs,"open":true}).to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("main.py"),
            format!(
                "# /// script\n# requires-python = \">=3.12\"\n# dependencies = []\n# ///\n{code}"
            ),
        )
        .unwrap();
    }
    fn script(&self, value: Value) {
        std::fs::write(self.home.join("fake-script.json"), value.to_string()).unwrap();
    }
    fn run(&self, step: &str) -> String {
        self.status()["steps"][step]["run_ids"][0]
            .as_str()
            .unwrap()
            .into()
    }
    fn terminal(&self, step: &str) -> Value {
        self.wait(|g| {
            matches!(
                g.status()["steps"][step]["status"].as_str(),
                Some("succeeded" | "failed" | "skipped" | "stale")
            )
        });
        self.status()["steps"][step].clone()
    }
    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(self.home.join("fake-events.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn post(&self, step: &str, body: &str) -> i64 {
        let CommandReply::Posted{id}=self.rpc(json!({"command":"message_post","args":{"project":self.selector(),"body":body,"thread":format!("step-{step}"),"to":step,"needs_reply":false,"from":"test","author":"test"}})) else {panic!("post")};
        id.0
    }
}
impl Drop for Gate {
    fn drop(&mut self) {
        let _ = &self.temp;
        if let Some(mut child) = self.broker.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Ok(runs) = std::fs::read_dir(self.home.join("runs")) {
            for entry in runs.flatten() {
                if let Some(id) = entry.file_name().to_str()
                    && id.parse::<RunId>().is_ok()
                {
                    let unit = format!("sluice-test-{id}.service");
                    let _ = Command::new("/usr/bin/systemctl")
                        .args(["--user", "stop", &unit])
                        .output();
                    let _ = Command::new("/usr/bin/systemctl")
                        .args(["--user", "reset-failed", &unit])
                        .output();
                }
            }
        }
    }
}
fn uv() -> PathBuf {
    let output = Command::new("/bin/bash")
        .args(["-lc", "command -v uv"])
        .output()
        .unwrap();
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        .canonicalize()
        .unwrap()
}
fn read(stream: &mut UnixStream) -> Vec<u8> {
    let mut size = [0; 4];
    stream.read_exact(&mut size).unwrap();
    let mut body = vec![0; u32::from_be_bytes(size) as usize];
    stream.read_exact(&mut body).unwrap();
    body
}
fn bindings(values: Value) -> Value {
    Value::Object(
        values
            .as_object()
            .unwrap()
            .iter()
            .map(|(n, v)| (n.clone(), json!({"default":v})))
            .collect(),
    )
}
fn agent(g: &Gate) -> Value {
    json!({"run":"agent.run","in":bindings(json!({"engine":"fake","cwd":g.temp.path(),"spec":"Submit summary and finish"})),"outputs":{"summary":"string"}})
}
#[test]
fn builtin_python_tools_sections_and_inline_execute_through_guardian() {
    let g = Gate::new();
    g.function(
        "custom.work",
        json!({"value":"int"}),
        json!({"answer":"int"}),
        r#"from sluice_fn import run, sh

def main(inp, ctx):
    with ctx.acquire('section', timeout=5):
        reply = ctx.tool('status', {})
        assert reply['project']['project_id'] == ctx.project_id
        assert ctx.project_dir.name == ctx.project_id
        assert 'type' in ctx.extra_inputs['topic']
        assert ctx.outputs['tag']['doc'] == 'A tag'
        assert sh(['python3', '-c', 'print(7)']).stdout.strip() == '7'
    with ctx.acquire('section', timeout=5):
        answer = inp['value'] + 1
    def acquire(key):
        return ctx.callback('acquire_lease', {'run':ctx.run_id, 'resource':'section', 'amount':1, 'priority':0, 'request_id':key})['data']
    first = acquire('first')
    waiting = acquire('second')
    assert first['state'] == 'held' and waiting['state'] == 'waiting'
    ctx.callback('release_lease', {'run':ctx.run_id, 'lease':first['lease']})
    granted = acquire('second')
    assert granted['lease'] == waiting['lease'] and granted['state'] == 'held'
    ctx.callback('release_lease', {'run':ctx.run_id, 'lease':granted['lease']})
    return {'answer':answer, 'tag':'yes'}
run(main)
"#,
    );
    g.plan(json!({"seed":{"run":"core.echo","in":{"value":{"default":4}}},"python":{"run":"custom.work","in":{"value":{"source":"seed/value"},"topic":{"default":"guide"}},"outputs":{"tag":{"type":"string","doc":"A tag"}}},"inline":{"run":"inline.python","in":{"code":{"default":"out = n + 1"},"n":{"source":"python/answer"}}}}));
    let _lease = g.lease();
    let value = g.terminal("inline");
    assert_eq!(value["status"], "succeeded", "{}", g.status());
    assert_eq!(value["outputs"]["value"], 6);
    assert_eq!(g.status()["steps"]["python"]["status"], "succeeded");
    let run = g.run("python");
    let db = rusqlite::Connection::open(g.home.join("sluice.db")).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM leases WHERE run_id=?1 AND kind='section' AND state='released'",
            [run],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 4);
}
#[test]
fn fake_agent_submits_delivers_once_and_feedback_resumes_previous_session() {
    let g = Gate::new();
    g.script(json!({"outputs":{"summary":"fake output"},"wait_message":true,"marker_transient_once":true}));
    g.plan(json!({"work":agent(&g)}));
    let _lease = g.lease();
    g.wait(|g| g.home.join("fake-events.jsonl").exists());
    let id = g.post("work", "first live message");
    let value = g.terminal("work");
    assert_eq!(value["status"], "succeeded", "{value}");
    assert_eq!(value["outputs"]["summary"], "fake output");
    let first = g.run("work");
    let delivered = g
        .events()
        .into_iter()
        .filter(|e| {
            (e["command"]["command"] == "deliver_text" || e["command"]["command"] == "steer")
                && e["command"]["id"]["kind"] == "message"
                && e["command"]["id"]["id"] == id
        })
        .count();
    assert_eq!(delivered, 1, "events: {:?}", g.events());
    g.script(json!({"outputs":{"summary":"resumed output"}}));
    g.rpc(json!({"command":"step_retry","args":{"project":g.selector(),"selection":{"steps":["work"],"tags":null},"message":"Continue the existing session","reason":"feedback","author":"test"}}));
    g.wait(|g| g.status()["steps"]["work"]["run_ids"][0] != first);
    let second = g.terminal("work");
    assert_eq!(second["status"], "succeeded", "{second}");
    assert_eq!(second["outputs"]["session"], value["outputs"]["session"]);
    assert!(
        g.events()
            .iter()
            .any(|e| e["command"]["command"] == "resume")
    );
    let run = g.run("work");
    let launch: Value = serde_json::from_slice(
        &std::fs::read(g.home.join("runs").join(run).join("runtime.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(launch["prev_run"], first);
}
#[test]
fn python_agent_wrapper_retries_transient_in_same_run_and_lands_outer_outputs() {
    let g = Gate::new();
    g.script(json!({"outputs":{"summary":"from agent"},"marker_transient_once":true}));
    g.function("custom.worker",json!({"cwd":"string"}),json!({"session":"string"}),r#"from sluice_fn import run, Transient
import fcntl

def main(inp, ctx):
    try:
        result = ctx.builtin('agent.run', {'engine':'fake', 'cwd':inp['cwd'], 'spec':ctx.header('Do the work')})
    except Transient:
        locks = list((ctx.home / 'locks').glob('session-*.lock'))
        assert len(locks) == 1
        with locks[0].open('r+') as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                raise AssertionError('session lock released during helper retry')
            except BlockingIOError:
                (ctx.home / 'lock-retained').write_text('yes')
        raise
    fields = ctx.submission()
    return {'session': result['session'], 'summary':fields['summary']}
run(main, retries=1, backoff=0)
"#);
    g.plan(json!({"work":{"run":"custom.worker","in":bindings(json!({"cwd":g.temp.path()})),"outputs":{"summary":"string"}}}));
    let _lease = g.lease();
    let value = g.terminal("work");
    assert_eq!(value["status"], "succeeded", "{value}");
    assert_eq!(value["outputs"]["summary"], "from agent");
    let events = g.events();
    assert!(events.iter().any(|e| e["command"]["command"] == "resume"));
    assert_eq!(
        events
            .iter()
            .filter(|e| e["command"]["command"] == "deliver_text"
                && e["command"]["id"]["kind"] == "task")
            .count(),
        1
    );
    let run = g.run("work");
    let db = rusqlite::Connection::open(g.home.join("sluice.db")).unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM runs WHERE step_id='work'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    assert!(g.home.join("lock-retained").exists());
    assert!(g.home.join("turn-committed.injected").exists());
    assert!(!g.home.join("turn-committed").exists());
    let launch: Value = serde_json::from_slice(
        &std::fs::read(g.home.join("runs").join(&run).join("launch.json")).unwrap(),
    )
    .unwrap();
    assert!(
        events
            .iter()
            .all(|e| e["cgroup"] != launch["executor"]["cgroup"])
    );
    assert_eq!(
        events
            .iter()
            .map(|e| e["pid"].as_u64().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    let checkpoint: Value = serde_json::from_slice(
        &std::fs::read(g.home.join("runs").join(run).join("native.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(checkpoint["internal_attempt"], 2);
    let groups = events
        .iter()
        .map(|e| e["cgroup"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(groups.len(), 1);
    assert!(groups.iter().all(|p| p.contains("/payload/")));
}
#[test]
fn rejected_python_registers_action_and_rearms_target_atomically() {
    let g = Gate::new();
    g.function(
        "custom.land",
        json!({}),
        json!({}),
        r#"from sluice_fn import run, Rejected

def main(inp, ctx):
    if (ctx.home / 'rejected-once').exists():
        return {}
    (ctx.home / 'rejected-once').write_text('yes')
    ctx.retry_on_failure('work', 'Fix the review')
    raise Rejected('Review refused')
run(main)
"#,
    );
    g.plan(json!({"work":{"run":"core.echo","in":{"value":{"default":1}}},"land":{"run":"custom.land","after":["work"]}}));
    let lease = g.lease();
    g.wait(|g| g.home.join("runs").exists());
    g.wait(|g|match g.rpc(json!({"command":"log_read","args":{"project":g.selector(),"since_seq":null,"kinds":["run.completion_action"],"threads":null,"limit":100}})) { CommandReply::Records(page) => !page.records.is_empty(), _ => false });
    drop(lease);
    let db = rusqlite::Connection::open(g.home.join("sluice.db")).unwrap();
    let (action,finished):(String,Option<String>)=db.query_row("SELECT completion_action,finished_at FROM runs WHERE step_id='land' ORDER BY created_at LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert!(finished.is_some());
    assert!(action.contains("Fix the review"));
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM messages WHERE body='Fix the review'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let (result, outcome): (String, String) = db.query_row("SELECT result,action_outcome FROM runs WHERE step_id='land' ORDER BY created_at LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let result: Value = serde_json::from_str(&result).unwrap();
    let outcome: Value = serde_json::from_str(&outcome).unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(outcome["outcome"], "applied");
    let work: i64 = db
        .query_row(
            "SELECT work_generation FROM steps WHERE step_id='work'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(work >= 2);
}
#[test]
fn registry_watcher_publishes_new_fn_and_reservation_keeps_bundle() {
    let g = Gate::new();
    g.function(
        "custom.pin",
        json!({}),
        json!({"value":"int"}),
        r#"from sluice_fn import run
import time

def main(inp, ctx):
    (ctx.home / 'pinned-ready').write_text(str(ctx.fn_dir))
    while not (ctx.home / 'finish-pin').exists():
        time.sleep(.02)
    return {'value':1}
run(main)
"#,
    );
    g.plan(json!({"pin":{"run":"custom.pin"}}));
    let _lease = g.lease();
    g.wait(|g| g.home.join("pinned-ready").exists());
    let pinned = std::fs::read_to_string(g.home.join("pinned-ready")).unwrap();
    assert!(pinned.contains("generations"));
    g.function(
        "custom.pin",
        json!({}),
        json!({"value":"string"}),
        "from sluice_fn import run\nrun(lambda i,c:{'value':'changed'})\n",
    );
    g.function(
        "custom.added",
        json!({}),
        json!({"value":"int"}),
        "from sluice_fn import run\nrun(lambda i,c:{'value':9})\n",
    );
    // Observe durable publication without any command access refreshing it.
    g.wait(|g| {
        let db = rusqlite::Connection::open(g.home.join("sluice.db")).unwrap();
        db.query_row("SELECT EXISTS(SELECT 1 FROM artifact_jobs WHERE project_id=?1 AND kind='generation' AND state='done' AND json_type(manifest,'$.files.\"custom.added/fn.json\"')='text')", [g.project.to_string()], |r|r.get::<_,bool>(0)).unwrap()
    });
    assert!(
        g.data(json!({"command":"fn_list","args":{"project":g.selector()}}))
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "custom.added")
    );
    std::fs::write(g.home.join("finish-pin"), b"").unwrap();
    assert_eq!(g.terminal("pin")["outputs"]["value"], 1);
    let call=g.data(json!({"command":"fn_call","args":{"name":"custom.added","project":g.selector(),"inputs":{},"direct":true,"wait_seconds":10,"author":"test"}}));
    assert_eq!(call["status"], "succeeded", "{call}");
    assert_eq!(call["outputs"]["value"], 9);
}
#[test]
fn terminal_agent_error_preserves_kind_message_and_session_through_python() {
    let g = Gate::new();
    g.script(json!({"fatal":true}));
    g.function("custom.failure",json!({"cwd":"string"}),json!({}),r#"from sluice_fn import run

def main(inp, ctx):
    return ctx.builtin('agent.run', {'engine':'fake', 'cwd':inp['cwd'], 'spec':'Fail with a typed error'})
run(main)
"#);
    g.plan(json!({"work":{"run":"custom.failure","in":bindings(json!({"cwd":g.temp.path()}))}}));
    let _lease = g.lease();
    let result = g.terminal("work");
    assert_eq!(result["status"], "failed", "{result}");
    assert_eq!(result["error"]["error"], "agent_failure");
    assert_eq!(result["error"]["kind"], "EngineExited");
    assert_eq!(result["error"]["message"], "fixture terminal error");
    assert_eq!(result["error"]["session"], "fake-session");
}
#[test]
fn compiled_builtin_retries_transient_within_one_reserved_run() {
    let g = Gate::new();
    let fake = g.temp.path().join("bin/gh");
    std::fs::write(&fake, r#"#!/usr/bin/python3
import os, pathlib, sys, json
assert os.environ['COMPOSITION_ENV_FIXTURE'] == 'frozen-launch'
p = pathlib.Path(os.environ['SLUICE_HOME']) / 'gh-ran'
n = int(p.read_text()) + 1 if p.exists() else 1
p.write_text(str(n))
if n == 1: sys.exit(1)
print(json.dumps({'state':'OPEN','headRefOid':'fixture-sha','url':'fixture-url','statusCheckRollup':[]}))
"#).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();
    g.plan(json!({"wait":{"run":"gh.pr_wait","in":bindings(json!({"path":g.temp.path(),"pr":"1","until":"checks"}))}}));
    let _lease = g.lease();
    let result = g.terminal("wait");
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["outputs"]["sha"], "fixture-sha");
    assert_eq!(std::fs::read_to_string(g.home.join("gh-ran")).unwrap(), "2");
    let db = rusqlite::Connection::open(g.home.join("sluice.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM runs WHERE step_id='wait'", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
}
const FAKE: &str = r#"#!/usr/bin/python3
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
"#;
