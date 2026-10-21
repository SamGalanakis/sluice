//! Leases and parking from inside a run, driven as an agent drives them: `sluice tool lease`
//! and `sluice tool park` from a scripted engine, against a coordinator on a scratch home.
use super::compose::{Gate, bindings};
use crate::executable;
use serde_json::{Value, json};
use sluice_model::commands::CommandReply;

/// A fake engine that runs, for each input it is given (its task, then each later one), the
/// next turn of its step's program: `sh` runs `sluice` with the arguments (`{text}` is the
/// input's text, `{name[field]}` a saved reply's field) and saves its JSON reply, `until`
/// stays busy until the file exists, `touch` writes one, `submit` submits the outputs and
/// `die` ends the engine at once. Every input and command lands in `park-events.jsonl`.
/// The runs sequence themselves through files, each waiting parked.
const FAKE_PARK: &str = r#"#!/usr/bin/python3
import os, sys, json, socket, struct, pathlib, subprocess
home = pathlib.Path(os.environ['SLUICE_HOME'])
step, run = os.environ['SLUICE_STEP'], os.environ['SLUICE_RUN_ID']
config = json.loads(pathlib.Path(sys.argv[2]).read_text())
turns = config['steps'][step]
state = {'status':'starting','turns_started':0,'turns_completed':0,'waiting':None,'background_work':[],'compactions':0,'final_text':'','session_id':None,'acknowledged':[],'not_accepted':[],'progress':0,'error':None}
saved, actions, text, turn = {}, [], '', 0
def log(**fields):
    with (home/'park-events.jsonl').open('a') as f: f.write(json.dumps({'step':step,'run':run,'pid':os.getpid(),**fields})+'\n')
def fill(value):
    if isinstance(value, str): return value.format(text=text, **saved)
    if isinstance(value, list): return [fill(v) for v in value]
    if isinstance(value, dict): return {k: fill(v) for k, v in value.items()}
    return value
def submit(outputs):
    raw = json.dumps({'protocol':1,'request_id':'submit','run_capability':os.environ['SLUICE_RUN_CAPABILITY'],'command':{'command':'step_submit','args':{'project':os.environ['SLUICE_PROJECT_ID'],'step':step,'run':run,'outputs':outputs,'author':'fixture'}}}).encode()
    with socket.socket(socket.AF_UNIX) as s:
        s.connect(os.environ['SLUICE_CONTROL_SOCKET']); s.sendall(struct.pack('>I',len(raw))+raw)
        size = struct.unpack('>I', s.recv(4, socket.MSG_WAITALL))[0]
        answer = json.loads(s.recv(size, socket.MSG_WAITALL))
        assert answer['result']['status']=='ok', answer
def act():
    global turn
    while actions:
        action = actions[0]
        if 'until' in action and not pathlib.Path(action['until']).exists():
            state['progress'] += 1  # an agent at work prints as it goes
            return False
        if 'sh' in action:
            out = subprocess.run([config['sluice']]+fill(action['sh']), capture_output=True, text=True)
            log(sh=fill(action['sh']), code=out.returncode, out=out.stdout, err=out.stderr)
            if out.returncode != 0: os._exit(2)
            if 'save' in action: saved[action['save']] = json.loads(out.stdout)
            # A park whose condition held already parks nothing: carry on into the next turn.
            if action['sh'][1:2] == ['park'] and json.loads(out.stdout)['parked'] is False:
                turn += 1
                actions[1:1] = turns[turn - 1] if turn <= len(turns) else []
        if 'touch' in action: pathlib.Path(action['touch']).write_text(run)
        if 'save_text' in action: saved[action['save_text']] = text
        if 'submit' in action: submit(fill(action['submit']))
        if 'die' in action: log(died=True); os._exit(3)
        actions.pop(0)
    return True
for line in sys.stdin:
    req = json.loads(line)
    if req['operation']=='command':
        log(**req)
        cmd = req['command']
        if cmd['command'] in ('start_fresh','resume'): state['status']='idle'
        if cmd['command'] in ('deliver_text','steer'):
            state['acknowledged'].append(cmd['id']); state['turns_started']+=1; state['status']='busy'; state['progress']+=1
            text, turn = cmd['text'], turn + 1
            if 'A message is in ' in text:
                before, link = text.split('A message is in ', 1)
                text = before + pathlib.Path(link.split(';',1)[0]).read_text()
            actions = list(turns[turn-1]) if turn <= len(turns) else []
        if cmd['command']=='request_exit': state['status']='exited'
    if req['operation']=='observe' and state['status']=='busy' and act():
        state['status']='idle'; state['turns_completed']=state['turns_started']; state['progress']+=1
    print(json.dumps({'observation':state,'outcome':'acknowledged','error':None,'hook':None}),flush=True)
"#;

struct Park {
    g: Gate,
}
impl Park {
    /// A coordinator whose fake engine plays `programs` (step → turns), with a project that
    /// declares `resources`.
    fn new(resources: Value, programs: Value) -> Self {
        let mut g = Gate::configured(|g| {
            executable::write(g.temp.path().join("bin").join("fake-engine"), FAKE_PARK);
        });
        g.script(json!({"sluice": env!("CARGO_BIN_EXE_sluice"), "steps": programs}));
        let CommandReply::Project(project) = g.rpc(json!({"command":"project_create","args":{
            "name":"parking","description":"leases and parks","resources":resources,
            "limits":{"memory":"512MiB","cpus":0.5,"tasks":128},"author":"test"
        }})) else {
            panic!("project")
        };
        g.project = project.project_id;
        Self { g }
    }
    /// An agent step running its program, with these `needs` and `priority`.
    fn agent(&self, needs: Value, priority: i64) -> Value {
        json!({"run":"agent.run","in":bindings(json!({"engine":"fake","cwd":self.g.temp.path(),"spec":"Follow your program"})),
            "outputs":{"summary":"string"},"needs":needs,"priority":priority})
    }
    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(self.g.home.join("park-events.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    /// The JSON a step's first `sluice tool <words>...` printed.
    fn reply(&self, step: &str, words: &[&str]) -> Value {
        let event = self
            .events()
            .into_iter()
            .find(|e| {
                e["step"] == step && words.iter().enumerate().all(|(n, w)| e["sh"][n + 1] == *w)
            })
            .unwrap_or_else(|| panic!("{step} ran no {words:?}: {:?}", self.events()));
        serde_json::from_str(event["out"].as_str().unwrap()).unwrap()
    }
    fn step(&self, step: &str) -> Value {
        self.g.status()["steps"][step].clone()
    }
    /// The texts delivered to `step`'s engine, with the engine process that took each.
    fn inputs(&self, step: &str) -> Vec<(String, u64)> {
        self.events()
            .into_iter()
            .filter(|e| e["step"] == step && e["command"]["command"] == "deliver_text")
            .map(|e| {
                (
                    e["command"]["text"].as_str().unwrap().to_owned(),
                    e["pid"].as_u64().unwrap(),
                )
            })
            .collect()
    }
}
/// A parked run gives its lane to a queued step, then wakes when its lease is granted: it
/// takes its lane back and the same engine session gets one "woke:" message naming the lease.
#[test]
fn a_parked_run_frees_its_lane_and_wakes_in_its_session_with_its_lease() {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_owned();
    let (holds, ran) = (path("holds"), path("queued-ran"));
    let p = Park::new(
        json!({"lane": 1, "gate": 1}),
        json!({
            // Holds the gate, parked, until the queued step has run on the parked one's lane.
            "holder": [
                [
                    {"sh": ["tool", "lease", "acquire", "gate"], "save": "lease"},
                    {"touch": holds},
                    {"sh": ["tool", "park", "--until", format!("file:{ran}")], "save": "park"},
                ],
                [
                    {"sh": ["tool", "lease", "status"], "save": "status"},
                    {"sh": ["tool", "lease", "release", "{lease[lease]}"]},
                    {"submit": {"summary": "{text}"}},
                ],
            ],
            "parker": [
                [{"until": holds}, {"sh": ["tool", "park", "--until", "lease:gate"], "save": "park"}],
                [{"submit": {"summary": "{text}"}}],
            ],
            "queued": [[{"touch": ran}, {"submit": {"summary": "ran"}}]],
        }),
    );
    p.g.plan(json!({
        "holder": p.agent(json!({}), 0),
        "parker": p.agent(json!({"lane": 1}), 1),
        "queued": p.agent(json!({"lane": 1}), 0),
    }));
    let _scheduler = p.g.lease();
    let parker = p.g.terminal("parker");
    assert_eq!(parker["status"], "succeeded", "{parker}");
    for step in ["holder", "queued"] {
        let peer = p.g.terminal(step);
        assert_eq!(peer["status"], "succeeded", "{peer}");
    }
    let park = p.reply("parker", &["park"]);
    assert_eq!(park["parked"], true, "{park}");
    let lease = park["lease"].as_i64().unwrap();
    // A file the queued step wrote woke the holder, which had held its lease while parked.
    let holder = p.step("holder");
    assert_eq!(
        holder["outputs"]["summary"],
        format!("woke: file:{ran}"),
        "{holder}"
    );
    // While the queued step ran, the parked run held no lane and waited first in line.
    let status = p.reply("holder", &["lease", "status"]);
    let gate = &status["resources"]["gate"];
    assert_eq!(gate["holders"][0]["step"], "holder", "{status}");
    assert_eq!(gate["queue"][0]["lease"], lease, "{status}");
    assert_eq!(gate["queue"][0]["position"], 1, "{status}");
    assert_eq!(status["parked"][0]["step"], "parker", "{status}");
    assert_eq!(status["parked"][0]["until"], "lease:gate", "{status}");
    assert_eq!(status["parked"][0]["phase"], "parked", "{status}");
    let lane = &status["resources"]["lane"]["holders"];
    assert!(
        lane.as_array()
            .unwrap()
            .iter()
            .all(|h| h["step"] != "parker"),
        "{status}"
    );
    // It woke with its lease, in the same session.
    let woke = format!("woke: lease:gate, lease {lease}");
    assert_eq!(parker["outputs"]["summary"], woke, "{parker}");
    let inputs = p.inputs("parker");
    assert_eq!(inputs.len(), 2, "{inputs:?}");
    assert_eq!(inputs[1].0, woke);
    assert_eq!(inputs[0].1, inputs[1].1, "one engine process: {inputs:?}");
}

/// Three runs contend for a capacity-1 lease: the waiting two are granted in the order they
/// asked, the first holder's lease is freed when its engine dies, and a parked request keeps
/// its place in the queue.
#[test]
fn lease_requests_are_granted_in_order_and_a_dead_holder_frees_its_lease() {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_owned();
    let (a_holds, b_queued, c_queued) = (path("a-holds"), path("b-queued"), path("c-queued"));
    // Waits parked for `after`, asks, then parks on its waiting request; woken, runs `then`.
    let waiter = |after: &str, queued: &str, then: Vec<Value>| {
        json!([
            [{"sh": ["tool", "park", "--until", format!("file:{after}")]}],
            [
                {"sh": ["tool", "lease", "acquire", "gate"], "save": "lease"},
                {"touch": queued},
                {"sh": ["tool", "park", "--until", "lease:gate"], "save": "park"},
            ],
            then,
        ])
    };
    let p = Park::new(
        json!({"gate": 1}),
        json!({
            // Holds the gate until both others queue, then its engine dies holding it.
            "a": [
                [
                    {"sh": ["tool", "lease", "acquire", "gate"], "save": "lease"},
                    {"touch": a_holds},
                    {"sh": ["tool", "park", "--until", format!("file:{c_queued}")]},
                ],
                [{"die": true}],
            ],
            "b": waiter(&a_holds, &b_queued, vec![
                json!({"sh": ["tool", "lease", "status", "gate"], "save": "status"}),
                json!({"sh": ["tool", "lease", "release", "{lease[lease]}"]}),
                json!({"submit": {"summary": "{text}"}}),
            ]),
            "c": waiter(&b_queued, &c_queued, vec![json!({"submit": {"summary": "{text}"}})]),
        }),
    );
    p.g.plan(
        json!({"a": p.agent(json!({}), 0), "b": p.agent(json!({}), 0), "c": p.agent(json!({}), 0)}),
    );
    let _scheduler = p.g.lease();
    let a = p.g.terminal("a");
    assert_eq!(a["status"], "failed", "{a}");
    let (b, c) = (p.g.terminal("b"), p.g.terminal("c"));
    let lease = |step: &str| p.reply(step, &["lease", "acquire"]);
    assert_eq!(lease("a")["state"], "held");
    assert_eq!(
        (lease("b")["state"].clone(), lease("b")["position"].clone()),
        (json!("waiting"), json!(1))
    );
    assert_eq!(
        (lease("c")["state"].clone(), lease("c")["position"].clone()),
        (json!("waiting"), json!(2))
    );
    let (b_lease, c_lease) = (lease("b")["lease"].clone(), lease("c")["lease"].clone());
    // The dead holder's lease went to b, the first in line, while c kept its place.
    let status = p.reply("b", &["lease", "status"]);
    let gate = &status["resources"]["gate"];
    assert_eq!(gate["holders"].as_array().unwrap().len(), 1, "{status}");
    assert_eq!(gate["holders"][0]["lease"], b_lease, "{status}");
    assert_eq!(gate["queue"][0]["lease"], c_lease, "{status}");
    assert_eq!(gate["queue"][0]["position"], 1, "{status}");
    for (done, lease) in [(b, b_lease), (c, c_lease)] {
        assert_eq!(done["status"], "succeeded", "{done}");
        assert_eq!(
            done["outputs"]["summary"],
            format!("woke: lease:gate, lease {lease}"),
            "{done}"
        );
    }
}

/// Say, ask, reply and retry feedback reach the idle session even if its file never appears.
#[test]
fn an_addressed_message_wakes_a_parked_session() {
    let dir = tempfile::tempdir().unwrap();
    let conditions: Vec<_> = (0..4)
        .map(|i| format!("file:{}/never-{i}", dir.path().display()))
        .collect();
    let p = Park::new(
        json!({"lane":1}),
        json!({
            "work": [
                [{"sh":["tool","ask","--to","owner","--body","Which ruling?"]},
                 {"sh":["tool","park","--until",conditions[0]],"save":"park"}],
                [{"save_text":"say"},{"sh":["tool","park","--until",conditions[1]]}],
                [{"save_text":"ask"},{"sh":["tool","park","--until",conditions[2]]}],
                [{"save_text":"reply"},{"sh":["tool","park","--until",conditions[3],"--timeout","7200"]}],
                [{"submit":{"summary":"{say}\n{ask}\n{reply}\n{text}"}}]
            ]
        }),
    );
    let mut agent = p.agent(json!({"lane":1}), 0);
    agent["tags"] = json!(["unit:manual-park"]);
    p.g.plan(json!({"work":agent}));
    let _scheduler = p.g.lease();
    let mut first_run = Value::Null;
    for (index, condition) in conditions.iter().enumerate() {
        p.g.wait(|g| {
            let view = g.data(json!({"command":"lease_status","args":{"project":g.selector()}}));
            view["parked"][0]["phase"] == "parked" && view["parked"][0]["until"] == *condition
        });
        let status = p.g.status();
        assert_eq!(status["steps"]["work"]["shown"], "parked", "{status}");
        let park = &status["steps"]["work"]["parked"];
        assert_eq!(park["until"], *condition);
        assert!(park["waited_seconds"].as_f64().unwrap() >= 0.0);
        assert_eq!(
            park["timeout_seconds"],
            if index == 3 { 7200 } else { 3600 }
        );
        if index == 0 {
            first_run = status["steps"]["work"]["run_ids"].clone();
        }
        assert_eq!(status["steps"]["work"]["run_ids"], first_run);
        let query = p.g.data(json!({"command":"query","args":{"sql":"SELECT step_id, phase, waited_seconds, timeout_seconds FROM parked WHERE project_id=?","params":[p.g.project.to_string()],"limit":10}}));
        assert_eq!(query["rows"][0][0], "work", "{query}");
        assert_eq!(query["rows"][0][1], "parked", "{query}");
        let units = p.g.data(json!({"command":"status","args":{"project":p.g.selector(),"view":"units","state":["parked"],"all":true,"selection":{"steps":null,"tags":null}}}));
        assert_eq!(units["units"][0]["state"], "parked", "{units}");
        assert_eq!(units["units"][0]["shown"], "parked", "{units}");
        assert_eq!(
            units["units"][0]["parked"][0]["until"], *condition,
            "{units}"
        );
        assert!(
            units["units"][0]["line"]
                .as_str()
                .unwrap()
                .contains("parked"),
            "{units}"
        );
        let command = match index {
            0 => {
                json!({"command":"say","args":{"project":p.g.selector(),"to":"work","body":"Say ruling"}})
            }
            1 => {
                json!({"command":"ask","args":{"project":p.g.selector(),"to":"work","body":"Ask ruling"}})
            }
            2 => {
                json!({"command":"reply","args":{"project":p.g.selector(),"to_message":p.reply("work", &["ask"])["id"],"body":"Reply ruling","owner":true}})
            }
            _ => {
                json!({"command":"step_retry","args":{"project":p.g.selector(),"selection":{"steps":["work"],"tags":null},"message":"Retry ruling","reason":"wake the parked session","author":"owner"}})
            }
        };
        p.g.rpc(command);
    }
    let done = p.g.terminal("work");
    assert_eq!(done["status"], "succeeded", "{done}");
    let summary = done["outputs"]["summary"].as_str().unwrap();
    assert_eq!(
        summary.matches("woken by a message").count(),
        4,
        "{summary}"
    );
    for body in ["Say ruling", "Ask ruling", "Reply ruling", "Retry ruling"] {
        assert!(summary.contains(body), "{summary}");
    }
    let inputs = p.inputs("work");
    assert_eq!(inputs.len(), 5, "{inputs:?}");
    assert!(
        inputs.iter().all(|(_, pid)| *pid == inputs[0].1),
        "{inputs:?}"
    );
}

/// Cancelling a park releases its lane and section lease without waking its agent.
#[test]
fn a_parked_run_is_cancelled_and_releases_its_leases() {
    let dir = tempfile::tempdir().unwrap();
    let p = Park::new(
        json!({"lane":1,"gate":1}),
        json!({
            "work": [[{"sh":["tool","lease","acquire","gate"]},
                       {"sh":["tool","park","--until",format!("file:{}/never",dir.path().display())],"save":"park"}]]
        }),
    );
    std::fs::write(
        p.g.home.join("config.json"),
        r#"{"park_timeout_seconds":7200}"#,
    )
    .unwrap();
    p.g.plan(json!({"work":p.agent(json!({"lane":1}),0)}));
    let _scheduler = p.g.lease();
    p.g.wait(|g| {
        let view = g.data(json!({"command":"lease_status","args":{"project":g.selector()}}));
        view["parked"][0]["phase"] == "parked"
    });
    assert_eq!(p.reply("work", &["park"])["timeout_seconds"], 7200);
    p.g.rpc(json!({"command":"step_cancel","args":{"project":p.g.selector(),"selection":{"steps":["work"],"tags":null},"reason":"cancel the wait","author":"owner"}}));
    let done = p.g.terminal("work");
    assert_eq!(done["status"], "failed", "{done}");
    let view =
        p.g.data(json!({"command":"lease_status","args":{"project":p.g.selector()}}));
    assert_eq!(view["parked"], json!([]), "{view}");
    for resource in ["gate", "lane"] {
        assert_eq!(view["resources"][resource]["holders"], json!([]), "{view}");
    }
    assert_eq!(p.inputs("work").len(), 1);
}
