//! G3 through a Python fn: the agent is launched the way the owner's lanes launch it
//! (lash.worker composes `agent.run` with `ctx.builtin`), so it runs from the deep
//! `runs/<run>/invocations/<invocation>` directory whose control-socket path is longer than a
//! Unix socket address holds. The real gates are labelled and ignored; the fixture-engine
//! test runs the same sequence on every `cargo test`.
use crate::engines::{Gate, credentials, private_write, step_finished_within};
use crate::support::{Scratch, repo};
use serde_json::{Value, json};
use sluice_agents::{delivery::DeliveryState, engines::InputId, supervisor::Checkpoint};
use sluice_model::{
    commands::{CommandReply, Delivery},
    error::PublicError,
};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// A cut-down lash.worker: the same imports, header, `report_path`, `ctx.builtin` call,
/// WallCap continuation, submission read and outputs, without the lash rules.
const WORKER: &str = r#"# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""g3.worker: agent.run composed from a Python fn the way lash.worker composes it."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from sluice_fn import AgentFailure, run

HEADER = """\
Labelled G3 scratch fixture, launched through a Python fn. Work only in {cwd}; make no commits.
Before you finish, write one line saying what you did to $TASK/summary.txt.
"""

WALL_CAP_CONTINUE = "You ran past the wall-clock cap; this is the same session, resumed.\n\n"


def main(inp, ctx):
    spec = ctx.header(HEADER.format(cwd=inp["cwd"]).replace("$TASK", str(ctx.run_dir))
                      + inp["spec"])
    base = {**inp, "report_path": str(ctx.run_dir / "summary.txt")}
    try:
        out = ctx.builtin("agent.run", {**base, "spec": spec})
    except AgentFailure as failure:
        if failure.kind != "WallCap" or not failure.session:
            raise
        out = ctx.builtin("agent.run", {**base, "session": failure.session,
                                        "spec": WALL_CAP_CONTINUE + spec})
    sent = ctx.submission()
    if not sent.get("word"):
        raise RuntimeError(f"the agent submitted no word: {sent}")
    summary = (out.get("report") or out.get("final") or "").strip()[:1500]
    return {"summary": summary, "final": summary, "session": out["session"]}


if __name__ == "__main__":
    run(main, retries=1, backoff=600)
"#;

/// The live home's composed control socket was 142 bytes; `sun_path` holds 108.
const SUN_PATH: usize = 108;
const STEER_WORD: &str = "teal";

#[test]
#[ignore = "real engine: labelled G3 gate through a Python fn"]
fn g3_fn_launched_codex() {
    fn_launched("codex", true);
}
#[test]
#[ignore = "real engine: labelled G3 gate through a Python fn"]
fn g3_fn_launched_claude() {
    fn_launched("claude", true);
}
#[test]
#[ignore = "real engine: labelled G3 gate through a Python fn"]
fn g3_fn_launched_devin() {
    fn_launched("devin", true);
}
#[test]
fn fn_launched_adapter_fixtures_accept_steer_submit_and_clean_up() {
    for engine in ["codex", "claude", "devin"] {
        fn_launched(engine, false);
    }
}

fn fn_launched(engine: &str, real: bool) {
    let scratch = Scratch::new();
    let env = if real {
        match credentials(&scratch.0, engine) {
            Ok(env) => env,
            Err(reason) => {
                println!("g3_fn_launched_{engine} PENDING: {reason}");
                return;
            }
        }
    } else {
        fixture_owner(&scratch.0)
    };
    let cwd = scratch.0.join("work");
    fs::create_dir(&cwd).unwrap();
    repo(&cwd);
    let mut gate = Gate::new(&scratch.0, scratch.0.join("rust-home"), env);
    if !real {
        let binary = fixture_engine(&scratch.0, engine);
        gate.env.insert(
            format!("SLUICE_{}_BIN", engine.to_uppercase()),
            binary.to_string_lossy().into(),
        );
    }
    gate.env.insert("SLUICE_AGENT_POLL_S".into(), "0.1".into());
    gate.env
        .insert("SLUICE_AGENT_SETTLE_S".into(), "0.2".into());
    gate.boot();
    let CommandReply::Project(p) = gate.rpc(json!({"command":"project_create","args":{"name":"g3-fn","description":"Labelled scratch G3 gate: agents launched through a Python fn","icon":null,"resources":{},"author":"fixture"}})) else {
        panic!("project reply")
    };
    let selector = json!({"kind":"id","value":p.project_id});
    gate.rpc(json!({"command":"fn_save","args":{"project":selector,"main_py":WORKER,"manifest":{
        "name":"g3.worker",
        "doc":"Run an agent through ctx.builtin('agent.run') as lash.worker does (labelled G3 scratch fixture).",
        "open":true,
        "inputs":{"engine":{"type":"enum","symbols":["codex","claude","devin"]},"cwd":"string","spec":"string","model":"Any?","session":"string?","listen":"boolean?"},
        "outputs":{"summary":"string?","final":"string","session":"string"}
    }}}));
    let spec = "Do exactly this, in order. 1) Create ready.txt in the working directory containing the single word ready. 2) Run `sleep 30` in the foreground and wait for it to finish: while it runs the harness sends you one addressed live message. 3) Follow that message; it names the file to write and the word to submit. If no message has arrived when the sleep ends, finish your turn WITHOUT submitting anything; the message then arrives as your next turn. Do no other work.";
    let mut inputs = json!({"engine":{"default":engine},"cwd":{"default":cwd},"spec":{"default":spec},"listen":{"default":true}});
    if engine == "codex" && real {
        inputs["model"] = json!({"default":{"type":"normal","model":"sol","effort":"low"}});
    }
    gate.rpc(json!({"command":"plan_edit","args":{"project":selector,"rev":1,"ops":[{"op":"step.add","step":"work","spec":{"run":"g3.worker","in":inputs,"outputs":{"word":"string"}}},{"op":"step.add","step":"next","spec":{"run":"core.echo","in":{"value":{"source":"work/word"}}}}],"start":true,"dry_run":false,"reason":"scratch fn-launched engine gate","author":"fixture"}}));
    gate.scheduling();
    let mut run = String::new();
    gate.wait(
        Duration::from_secs(60),
        |g| match g.status(&selector)["steps"]["work"]["run_ids"][0].as_str() {
            Some(r) => {
                run = r.into();
                g.track(r);
                true
            }
            None => false,
        },
    );
    // The composed agent's own directory, under the fn's run.
    let invocations = gate.home.join("runs").join(&run).join("invocations");
    let mut invocation = PathBuf::new();
    gate.wait(Duration::from_secs(120), |_| {
        match fs::read_dir(&invocations)
            .ok()
            .and_then(|mut d| d.next())
            .and_then(Result::ok)
        {
            Some(entry) if entry.path().join("native.json").is_file() => {
                invocation = entry.path();
                true
            }
            _ => false,
        }
    });
    let socket = invocation.join("control.sock");
    let depth = socket.as_os_str().len();
    println!(
        "g3_fn_launched_{engine} run={run} invocation={} control_socket_bytes={depth}",
        invocation.file_name().unwrap().to_string_lossy()
    );
    assert!(depth >= SUN_PATH, "{}", socket.display());
    // Prompt acceptance: the task is acknowledged exactly once (hook engines need their
    // hooks to reach the guardian through that socket for this).
    let mut accepted = None;
    gate.wait(Duration::from_secs(180), |g| {
        live(g, &selector);
        accepted =
            checkpoint(&invocation).filter(|c| task_state(c) == Some(DeliveryState::Acknowledged));
        accepted.is_some()
    });
    let accepted = accepted.unwrap();
    assert_eq!(task_tries(&accepted), 1, "{:?}", accepted.delivery);
    println!(
        "g3_fn_launched_{engine} accepted session={:?} state={:?}",
        accepted.session, accepted.state
    );
    if real {
        gate.wait(Duration::from_secs(180), |g| {
            live(g, &selector);
            cwd.join("ready.txt").exists()
        });
    }
    let body = format!(
        "Addressed live G3 steer: write steer.txt containing steered, then submit word={STEER_WORD} for this current run with step_submit and finish your turn."
    );
    let CommandReply::Receipt(receipt) =
        gate.rpc(json!({"command":"say","args":{"project":selector,"body":body,"to":"work"}}))
    else {
        panic!("say reply")
    };
    println!("g3_fn_launched_{engine} steer receipt={receipt:?}");
    assert_eq!(receipt.delivery, Delivery::Delivered, "{receipt:?}");
    assert_eq!(receipt.run.map(|r| r.to_string()), Some(run.clone()));
    let message = receipt.id;
    gate.wait(Duration::from_secs(180), |g| {
        live(g, &selector);
        checkpoint(&invocation).is_some_and(|c| {
            c.delivery.entries.iter().any(|e| {
                e.id == InputId::Message { id: message } && e.state == DeliveryState::Acknowledged
            })
        })
    });
    println!(
        "g3_fn_launched_{engine} steer message={} acknowledged",
        message.0
    );
    let acknowledged = std::time::Instant::now();
    // An invalid submission is refused with what is wrong, and the step keeps running.
    let Err(PublicError::Invalid { errors, .. }) =
        gate.try_rpc(submit(&p.project_id, &run, json!(7)))
    else {
        panic!("an invalid submission is refused")
    };
    assert_eq!(errors.len(), 1, "{errors:?}");
    if !real {
        assert_eq!(gate.status(&selector)["steps"]["work"]["status"], "running");
        gate.rpc(submit(&p.project_id, &run, json!(STEER_WORD)));
    }
    // The valid submission ends the agent invocation at once: the fn gets agent.run's
    // result back, post-processes it and returns, and the step completes with the fn's
    // outputs, the submission joined in. Nothing waits out the idle grace (10 minutes).
    let done = step_finished_within(
        &mut gate,
        &selector,
        Duration::from_secs(if real { 240 } else { 30 }),
    );
    let settled = std::time::Instant::now();
    println!(
        "g3_fn_launched_{engine} settled {:.1} s after the steer was acknowledged",
        acknowledged.elapsed().as_secs_f64()
    );
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["run_ids"][0], run.as_str(), "{done}");
    // The fn's declared outputs and the submitted one, nothing dropped.
    let mut keys = done["outputs"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, ["final", "session", "summary", "word"], "{done}");
    assert_eq!(done["outputs"]["word"], STEER_WORD, "{done}");
    assert!(done["outputs"]["final"].is_string(), "{done}");
    let session = done["outputs"]["session"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!session.is_empty(), "{done}");
    assert_eq!(Some(session.as_str()), accepted.session.as_deref());
    // Its dependent starts on it.
    gate.wait(Duration::from_secs(30), |g| {
        g.status(&selector)["steps"]["next"]["status"] == "succeeded"
    });
    assert_eq!(
        gate.status(&selector)["steps"]["next"]["outputs"]["value"],
        STEER_WORD
    );
    // A settled step takes no more messages, and its run no second submission.
    let refused = gate.try_rpc(
        json!({"command":"say","args":{"project":selector,"body":"too late","to":"work"}}),
    );
    assert!(
        matches!(&refused, Err(PublicError::Conflict { message, .. }) if message.contains("settled")),
        "{refused:?}"
    );
    assert!(
        gate.try_rpc(submit(&p.project_id, &run, json!("again")))
            .is_err()
    );
    // The submission stopped the agent's session.
    gate.wait(Duration::from_secs(60), |_| {
        checkpoint(&invocation).is_some_and(|c| c.state == sluice_agents::supervisor::State::Done)
    });
    println!(
        "g3_fn_launched_{engine} session stopped {:.1} s after the step settled",
        settled.elapsed().as_secs_f64()
    );
    gate.wait(Duration::from_secs(30), |_| !tmux_alive(&invocation));
    if real {
        assert_eq!(
            fs::read_to_string(cwd.join("steer.txt")).unwrap().trim(),
            "steered"
        );
    }
    let last = checkpoint(&invocation).unwrap();
    assert_eq!(last.submissions.get("word"), Some(&json!(STEER_WORD)));
    assert_eq!(last.session.as_deref(), Some(session.as_str()));
    assert_eq!(task_tries(&last), 1, "{:?}", last.delivery);
    let tries: u32 = last
        .delivery
        .entries
        .iter()
        .filter(|e| e.id == InputId::Message { id: message })
        .map(|e| e.tries)
        .sum();
    assert_eq!(tries, 1, "the steer was delivered more than once");
    if engine != "codex" {
        // Claude and Devin report through hooks; they reach the guardian's journal.
        assert!(
            gate.home
                .join("runs")
                .join(&run)
                .join("engine-hooks")
                .is_dir(),
            "no engine hook reached the guardian"
        );
    }
    if engine == "devin" {
        let hooks = fs::read_to_string(invocation.join("devin-hooks.jsonl")).unwrap();
        for event in ["SessionStart", "UserPromptSubmit"] {
            assert!(hooks.contains(&format!("\"{event}\"")), "{event}: {hooks}");
        }
    }
    if !real {
        // A send-back reopens the step as a new run that resumes the same session: the
        // message reaches it, its submission ends it at once, and the fn returns that session.
        let (again, resumed) = send_back(&mut gate, &selector, &run, "Send-back: submit olive");
        gate.rpc(submit(&p.project_id, &again, json!("olive")));
        let done = step_finished_within(&mut gate, &selector, Duration::from_secs(30));
        assert_eq!(done["status"], "succeeded", "{done}");
        assert_eq!(done["run_ids"][0], again.as_str(), "{done}");
        assert_eq!(done["outputs"]["word"], "olive", "{done}");
        assert_eq!(done["outputs"]["session"], session.as_str(), "{done}");
        gate.wait(Duration::from_secs(60), |_| {
            checkpoint(&resumed).is_some_and(|c| c.state == sluice_agents::supervisor::State::Done)
        });
        assert_eq!(
            checkpoint(&resumed).unwrap().session.as_deref(),
            Some(session.as_str())
        );
        // Sent back again, the agent stops after its nudges without submitting: a typed
        // failure carrying the session, not a hang.
        let (last_run, _) = send_back(&mut gate, &selector, &again, "Send-back: think again");
        let mut failed = Value::Null;
        gate.wait(Duration::from_secs(120), |g| {
            failed = g.status(&selector)["steps"]["work"].clone();
            failed["status"] == "failed"
        });
        assert_eq!(
            failed["error"]["error"], "exited_without_submit",
            "{failed}"
        );
        assert_eq!(failed["error"]["session"], session.as_str(), "{failed}");
        assert_eq!(failed["run_ids"][0], last_run.as_str(), "{failed}");
    }
    // Cleanup: the run's units are empty and no private tmux server survives, including
    // the invocation's own.
    gate.wait(Duration::from_secs(30), |g| g.groups_empty());
    gate.assert_clean();
    assert!(
        !tmux_alive(&invocation),
        "the invocation's private tmux survived settlement"
    );
    keep_evidence(&gate, &run, &invocation);
    gate.cleanup();
    println!(
        "g3_fn_launched_{engine} PASS run={run} session={session} control_socket_bytes={depth} task_tries=1 steer=delivered+acknowledged word={STEER_WORD}"
    );
}

fn submit(project: &impl serde::Serialize, run: &str, word: Value) -> Value {
    json!({"command":"step_submit","args":{"project":project,"step":"work","run":run,"outputs":{"word":word},"author":"fixture"}})
}
/// Retry `work` with `message`; returns its new run and that run's agent invocation once
/// the message has reached the agent.
fn send_back(gate: &mut Gate, selector: &Value, prior: &str, message: &str) -> (String, PathBuf) {
    gate.rpc(json!({"command":"step_retry","args":{"project":selector,"selection":{"steps":["work"],"tags":null},"message":message,"reason":"fixture send-back","author":"fixture"}}));
    let mut run = String::new();
    gate.wait(
        Duration::from_secs(60),
        |g| match g.status(selector)["steps"]["work"]["run_ids"][0].as_str() {
            Some(r) if r != prior => {
                run = r.into();
                g.track(r);
                true
            }
            _ => false,
        },
    );
    let invocations = gate.home.join("runs").join(&run).join("invocations");
    let mut invocation = PathBuf::new();
    gate.wait(Duration::from_secs(60), |_| {
        fs::read_dir(&invocations).ok().is_some_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                let delivered = checkpoint(&entry.path()).is_some_and(|c| {
                    c.delivery.entries.iter().any(|e| {
                        matches!(e.id, InputId::Message { .. })
                            && e.text.contains(message)
                            && e.state == DeliveryState::Acknowledged
                    })
                });
                if delivered {
                    invocation = entry.path();
                }
                delivered
            })
        })
    });
    (run, invocation)
}
/// Stop waiting as soon as the step has failed, with its status as the reason.
#[track_caller]
fn live(gate: &Gate, selector: &Value) {
    let step = &gate.status(selector)["steps"]["work"];
    assert_ne!(step["status"], "failed", "{step}");
}
fn checkpoint(directory: &Path) -> Option<Checkpoint> {
    Checkpoint::read(directory).ok().flatten()
}
fn task_state(c: &Checkpoint) -> Option<DeliveryState> {
    c.delivery
        .entries
        .iter()
        .find(|e| e.id == InputId::Task)
        .map(|e| e.state.clone())
}
fn task_tries(c: &Checkpoint) -> u32 {
    c.delivery
        .entries
        .iter()
        .filter(|e| e.id == InputId::Task)
        .map(|e| e.tries)
        .sum()
}
fn tmux_alive(directory: &Path) -> bool {
    let socket = directory.join("tmux.sock");
    socket.exists()
        && Command::new(crate::engines::workspace().join("target/private-tmux/bin/tmux"))
            .current_dir(directory)
            .args(["-S", "tmux.sock", "list-sessions"])
            .output()
            .unwrap()
            .status
            .success()
}
/// The composed invocation's files, next to what `Gate::cleanup` keeps for the outer run.
fn keep_evidence(gate: &Gate, run: &str, invocation: &Path) {
    let evidence = crate::engines::workspace()
        .join("target/g3-real-evidence")
        .join(gate.home.parent().unwrap().file_name().unwrap())
        .join(run)
        .join("invocation");
    fs::create_dir_all(&evidence).unwrap();
    fs::set_permissions(&evidence, fs::Permissions::from_mode(0o700)).unwrap();
    for name in [
        "native.json",
        "stderr-tail.log",
        "app-server.log",
        "devin-hooks.jsonl",
        "message-continuation.json",
        "fixture-errors.log",
    ] {
        if invocation.join(name).is_file() {
            fs::copy(invocation.join(name), evidence.join(name)).unwrap();
        }
    }
    println!("g3_fn_launched evidence: {}", evidence.display());
}

fn fixture_owner(root: &Path) -> BTreeMap<String, String> {
    let owner = root.join("owner");
    private_write(&owner.join(".codex/config.toml"), b"");
    private_write(&owner.join(".codex/auth.json"), b"fixture credential");
    private_write(&owner.join(".config/devin/config.json"), b"{}");
    fs::create_dir_all(owner.join(".claude")).unwrap();
    fs::create_dir_all(owner.join(".local/share")).unwrap();
    BTreeMap::from([
        ("HOME".into(), owner.to_string_lossy().into()),
        (
            "CODEX_HOME".into(),
            owner.join(".codex").to_string_lossy().into(),
        ),
        (
            "CLAUDE_CONFIG_DIR".into(),
            owner.join(".claude").to_string_lossy().into(),
        ),
        (
            "XDG_CONFIG_HOME".into(),
            owner.join(".config").to_string_lossy().into(),
        ),
        (
            "XDG_DATA_HOME".into(),
            owner.join(".local/share").to_string_lossy().into(),
        ),
    ])
}
fn fixture_engine(root: &Path, engine: &str) -> PathBuf {
    let config = root.join("adapter-fixture.json");
    let turns: Vec<Value> = (0..8)
        .map(|_| json!({"busy_s":0.5,"busy_ms":500,"reply":"fixture done"}))
        .collect();
    private_write(
        &config,
        &serde_json::to_vec(&json!({"turns":turns,"prompts":root.join("prompts.jsonl")})).unwrap(),
    );
    let binary = root.join("adapter-fixture");
    let variable = if engine == "claude" {
        "SLUICE_FAKE_CLAUDE"
    } else {
        "FAKE_DEVIN"
    };
    let codex_tui = if engine == "codex" {
        "export SLUICE_CODEX_FIXTURE=tui\ncase \"$1\" in -c) exec /usr/bin/sleep 600 ;; esac\n"
    } else {
        ""
    };
    crate::executable::write(
        &binary,
        format!(
            "#!/bin/sh\nset -e\n{codex_tui}export {variable}='{}'\nexec '{}' {engine} \"$@\"\n",
            config.display(),
            Path::new(env!("CARGO_BIN_EXE_fixture")).display()
        )
        .as_bytes(),
    );
    binary
}
