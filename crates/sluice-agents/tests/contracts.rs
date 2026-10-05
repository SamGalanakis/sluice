use sluice_agents::{engines::*, prompt::*, supervisor::*, *};
use sluice_model::{
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, JsonValue, RunCapability},
    types::Type,
};
use sluice_process::{guardian::*, identity::ProcessIdentity, journal::*, socket::*};
use std::{
    collections::BTreeMap,
    fs, io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sluice-test-contract-{}", RunId::new()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn ports() -> PromptContext {
    PromptContext {
        header: "Step work: repair the bug".into(),
        project: "p".into(),
        step: "work".into(),
        run: "r".into(),
        inputs: BTreeMap::from([(
            "interface".into(),
            Port {
                r#type: Type::String,
                doc: String::new(),
            },
        )]),
        outputs: BTreeMap::from([
            (
                "ready".into(),
                Port {
                    r#type: Type::Boolean,
                    doc: "Gate passed".into(),
                },
            ),
            (
                "unresolved".into(),
                Port {
                    r#type: Type::Optional(Box::new(Type::String)),
                    doc: "Remaining questions".into(),
                },
            ),
        ]),
        listen: true,
        previous: None,
    }
}
fn inputs(value: serde_json::Value) -> JsonMap {
    sluice_model::rpc::decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
#[test]
fn full_prompt_contains_header_inputs_outputs_exact_submit_identity_and_live_thread_note() {
    let text = build(
        "Fix it",
        &BTreeMap::from([("interface".into(), serde_json::json!("api.md\nsecond line"))]),
        &ports(),
    );
    assert!(text.starts_with("Step work: repair the bug This step's project is `p`; pass exactly that as `project` to any sluice tool.\n\nFix it"));
    let i = text.find("## Inputs").unwrap();
    let o = text.find("## Outputs you must submit").unwrap();
    let t = text.find("Messages addressed to this step").unwrap();
    assert!(i < o && o < t);
    assert!(text.contains("`interface` (string):\napi.md\nsecond line"));
    assert!(text.contains("\"project\":\"p\",\"step\":\"work\",\"run\":\"r\""));
    assert!(text.contains("\"ready\": <boolean>"));
    assert!(
        text.contains(
            "Submit only when you are finished: submitting ends your session. Submit them"
        )
    );
    assert!(!text.contains("submission counts"));
    assert!(text.contains("step-work"));
    // The verbs, each with this run's identity; nothing is posted "to nobody".
    for verb in ["ask", "say", "reply"] {
        assert!(text.contains(&format!("`sluice tool {verb} '")), "{verb}");
    }
    assert!(text.contains("\"to\":\"orchestrator\""));
    assert!(!text.contains("needs_reply"));
    assert!(!text.contains("message_post"));
    assert!(!text.contains("nobody"));
    assert!(!text.contains("thread.post"));
    assert!(!text.contains("inbox_"));
    assert!(!text.contains("log_read"));
    assert_eq!(required_outputs(&ports()), vec!["ready"]);
}
/// The tools read a bare string as a name, so every callback the prompt gives names an id
/// project as `id:<uuid>`, and the header says to pass exactly that.
#[test]
fn every_callback_gives_an_id_project_as_an_id_selector() {
    let id = sluice_model::ids::ProjectId::new();
    let mut context = ports();
    context.project = id.to_string();
    let text = build("Fix it", &BTreeMap::new(), &context);
    let selector = format!("id:{id}");
    assert_eq!(prompt::project_selector(&id.to_string()), selector);
    assert_eq!(prompt::project_selector("p"), "p");
    assert!(text.starts_with(&format!(
        "Step work: repair the bug This step's project is `{selector}`; pass exactly that as `project` to any sluice tool.\n\n"
    )));
    assert!(text.contains(&format!(
        "sluice tool step_submit '{{\"project\":\"{selector}\",\"step\":\"work\",\"run\":\"r\","
    )));
    let post = text
        .split("sluice tool ask '")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .unwrap();
    let post: serde_json::Value = serde_json::from_str(post).unwrap();
    assert_eq!(post["project"], selector.as_str());
    assert!(text.contains("thread `step-work`"));
    assert!(text.contains(&format!("of project `{selector}`")));
    // No callback carries the bare id where a project is expected.
    assert!(!text.contains(&format!("\"project\":\"{id}\"")));
    assert!(!text.contains(&format!("`{id}`")));
    // The header alone also names it when the factory gives none.
    context.header.clear();
    context.listen = false;
    let text = build("Fix it", &BTreeMap::new(), &context);
    assert!(text.starts_with(&format!("This step's project is `{selector}`;")));
}
#[test]
fn listen_false_drops_only_the_note_and_empty_ports_have_no_sections() {
    let mut context = ports();
    context.listen = false;
    let text = build("task", &BTreeMap::new(), &context);
    assert!(text.contains("## Inputs"));
    assert!(text.contains("## Outputs"));
    assert!(!text.contains("Messages addressed to this step"));
    // Asking needs no live delivery: the exact commands still carry this run's identity.
    for verb in ["ask", "say"] {
        let post = text
            .split(&format!("sluice tool {verb} '"))
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .unwrap();
        let post: serde_json::Value = serde_json::from_str(post).unwrap();
        assert_eq!(
            post,
            serde_json::json!({"project":"p","run":"r","to":"orchestrator","body":"..."})
        );
    }
    context.inputs.clear();
    context.outputs.clear();
    let text = build("task", &BTreeMap::new(), &context);
    assert!(!text.contains("##"));
    context.step.clear();
    let text = build("task", &BTreeMap::new(), &context);
    assert!(!text.contains("sluice tool ask"));
}
#[test]
fn thread_names_and_shell_quoting_preserve_literal_data() {
    assert_eq!(thread_name("Build.Mac OS"), "step-build-mac-os");
    let data = "a'`ls` $HOME $(echo secret)";
    let command = format!("printf '%s' {}", shell_quote(data));
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), data);
}
#[test]
fn every_agent_builtin_builds_prompt_and_validates_engine_specific_inputs() {
    for name in AGENT_BUILTINS {
        let value = match name {
            "agent.run" => serde_json::json!({"engine":"codex","spec":"task","cwd":"/tmp"}),
            "agent.review" => {
                serde_json::json!({"base":"main","standards":"S.md","notes":"be strict","cwd":"/tmp"})
            }
            "decide.llm" => serde_json::json!({"question":"Pick","options":["a","b"]}),
            "agent.claude" => serde_json::json!({"prompt":"task","cwd":"/tmp"}),
            _ => serde_json::json!({"spec":"task","cwd":"/tmp"}),
        };
        let request = AgentBuiltinRequest::build(name, &inputs(value), &ports()).unwrap();
        assert!(request.task.contains("## Outputs"));
        assert!(request.required.contains(&"ready".into()));
        assert_eq!(
            request.retry.additional_tries,
            if name == "decide.llm" { 2 } else { 3 }
        );
    }
    for value in [
        serde_json::json!({"engine":"unknown","cwd":"/tmp","spec":"x"}),
        serde_json::json!({"engine":"claude","cwd":"/tmp","spec":"x","model":"sonnet"}),
        serde_json::json!({"engine":"devin","cwd":"/tmp","spec":"x","effort":"high"}),
        serde_json::json!({"engine":"claude","cwd":"/tmp","spec":"x","listen":"false"}),
    ] {
        assert!(
            AgentBuiltinRequest::build("agent.run", &inputs(value), &PromptContext::default())
                .is_err()
        );
    }
    assert!(
        sluice_agents::model::resolve(
            "fake",
            &sluice_agents::model::ModelChoice::normal("unapproved", None),
            &["fixture".into()]
        )
        .is_err()
    );
    assert_eq!(
        adapter_not_built("codex"),
        AgentDispatchError::EngineNotBuilt {
            engine: "codex".into()
        }
    );
}
fn result() -> AgentResult {
    AgentResult {
        final_text: "finished".into(),
        report: None,
        session: "s".into(),
        git: None,
        notes: vec![],
        model: None,
    }
}
#[test]
fn decision_validates_choice_probability_and_zero_threshold() {
    let request = AgentBuiltinRequest::build(
        "decide.llm",
        &inputs(serde_json::json!({"question":"Pick","options":["a","b"],"threshold":0})),
        &PromptContext::default(),
    )
    .unwrap();
    let fields = BTreeMap::from([
        ("choice".into(), serde_json::json!("a")),
        ("p".into(), serde_json::json!(0.5)),
    ]);
    let output = request.outputs(result(), &fields).unwrap();
    assert_eq!(output.0["confident"].as_value(), &serde_json::json!(true));
    assert_eq!(output.0.len(), 3);
    for (choice, p) in [("z", 0.9), ("a", 1.1), ("a", -0.1)] {
        assert!(
            request
                .outputs(
                    result(),
                    &BTreeMap::from([
                        ("choice".into(), serde_json::json!(choice)),
                        ("p".into(), serde_json::json!(p))
                    ])
                )
                .is_err()
        );
    }
    for value in [
        serde_json::json!({"question":"q","options":[]}),
        serde_json::json!({"question":"q","options":["a","a"]}),
        serde_json::json!({"question":"q","options":["a"],"threshold":2}),
    ] {
        assert!(
            AgentBuiltinRequest::build("decide.llm", &inputs(value), &PromptContext::default())
                .is_err()
        );
    }
}
#[test]
fn report_final_session_and_declared_submissions_survive_return_building() {
    let scratch = Scratch::new();
    fs::write(scratch.0.join("report.md"), "REPORT").unwrap();
    let request=AgentBuiltinRequest::build("agent.run",&inputs(serde_json::json!({"engine":"fake","spec":"x","cwd":scratch.0,"report_path":"report.md"})),&PromptContext::default()).unwrap();
    let output = request
        .outputs(
            result(),
            &BTreeMap::from([("ready".into(), serde_json::json!(true))]),
        )
        .unwrap();
    assert_eq!(output.0["report"].as_value(), &serde_json::json!("REPORT"));
    assert_eq!(output.0["final"].as_value(), &serde_json::json!("finished"));
    assert_eq!(output.0["session"].as_value(), &serde_json::json!("s"));
    assert!(output.0["ready"].as_value().as_bool().unwrap());
    assert!(!output.0.contains_key("git"));
}
#[test]
fn reprime_uses_current_snapshot_and_fallback_includes_task_path() {
    let path = std::path::Path::new("/scratch/task.md");
    assert!(sluice_agents::reprime::context(path, Ok("new snapshot")).contains("new snapshot"));
    let fallback = sluice_agents::reprime::context(path, Err("unavailable"));
    assert!(fallback.contains("/scratch/task.md"));
    assert!(fallback.contains("unavailable"));
}
#[test]
fn hook_journal_claims_before_decision_and_never_replays_claimed_requests() {
    let scratch = Scratch::new();
    let journal = scratch.0.join("engine-hooks");
    fs::create_dir(&journal).unwrap();
    let run = RunId::new();
    let request = EngineHookRequest {
        engine: "fake".into(),
        run,
        event: "Stop".into(),
        payload: JsonValue::try_from(serde_json::json!({"turn":"t"})).unwrap(),
    };
    fs::write(
        journal.join("one.request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let mut engine = ScriptedEngine::new(vec![]);
    engine.hook_reply = Some(HookReply {
        stdout: Some(serde_json::json!({"decision":"block"})),
        exit_code: 2,
    });
    process_hooks(&mut engine, run, &scratch.0).unwrap();
    process_hooks(&mut engine, run, &scratch.0).unwrap();
    assert_eq!(engine.hooks.len(), 1);
    // The claim goes once the reply is durable; the reply is the guardian's to remove.
    assert!(!journal.join("one.request.json").exists());
    assert!(!journal.join("one.claimed.json").exists());
    let reply: Result<EngineHookReply, PublicError> =
        sluice_model::rpc::decode_json(&fs::read(journal.join("one.reply.json")).unwrap()).unwrap();
    assert_eq!(reply.unwrap().exit_code, 2);
    fs::write(
        journal.join("crashed.claimed.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    process_hooks(&mut engine, run, &scratch.0).unwrap();
    assert_eq!(engine.hooks.len(), 1);
    // An uncertain claim is never decided again; the next pass discards it.
    assert!(!journal.join("crashed.reply.json").exists());
    assert!(!journal.join("crashed.claimed.json").exists());
}
#[test]
fn hook_journal_refuses_wrong_run_and_bounds_exit_code() {
    let scratch = Scratch::new();
    let journal = scratch.0.join("engine-hooks");
    fs::create_dir(&journal).unwrap();
    let run = RunId::new();
    let request = EngineHookRequest {
        engine: "fake".into(),
        run: RunId::new(),
        event: "Stop".into(),
        payload: JsonValue::try_from(serde_json::json!({})).unwrap(),
    };
    fs::write(
        journal.join("wrong.request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let mut engine = ScriptedEngine::new(vec![]);
    process_hooks(&mut engine, run, &scratch.0).unwrap();
    assert!(engine.hooks.is_empty());
    let reply: Result<EngineHookReply, PublicError> =
        sluice_model::rpc::decode_json(&fs::read(journal.join("wrong.reply.json")).unwrap())
            .unwrap();
    assert!(reply.is_err());
}

fn hook_request(run: RunId) -> EngineHookRequest {
    EngineHookRequest {
        engine: "fake".into(),
        run,
        event: "Stop".into(),
        payload: JsonValue::try_from(serde_json::json!({"turn":"t"})).unwrap(),
    }
}
fn answering_engine() -> ScriptedEngine {
    let mut engine = ScriptedEngine::new(vec![]);
    engine.hook_reply = Some(HookReply {
        stdout: None,
        exit_code: 0,
    });
    engine
}
fn journal_names(journal: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(journal)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
/// The supervisor's side as an agent payload runs it: a journal pass every millisecond.
struct HookServer {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<io::Result<ScriptedEngine>>,
}
impl HookServer {
    fn start(run: RunId, run_dir: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut engine = answering_engine();
            while !flag.load(Ordering::SeqCst) {
                process_hooks(&mut engine, run, &run_dir)?;
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(engine)
        });
        Self { stop, thread }
    }
    fn stop(self) -> ScriptedEngine {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.join().unwrap().unwrap()
    }
}
async fn exchange_once(run_dir: &std::path::Path, run: RunId, n: usize) -> EngineHookReply {
    tokio::time::timeout(
        Duration::from_secs(10),
        sluice_process::hook_journal::exchange(run_dir, &hook_request(run)),
    )
    .await
    .unwrap_or_else(|_| panic!("hook {n} got no reply"))
    .unwrap_or_else(|error| panic!("hook {n} refused: {error:?}"))
}
#[tokio::test(flavor = "multi_thread")]
async fn hook_journal_serves_more_hooks_than_any_bound_over_one_run() {
    let scratch = Scratch::new();
    let run = RunId::new();
    let server = HookServer::start(run, scratch.0.clone());
    // An older release kept two files per hook and refused the 2049th at 4096 entries.
    for n in 0..4200 {
        assert_eq!(exchange_once(&scratch.0, run, n).await.exit_code, 0);
    }
    let engine = server.stop();
    assert_eq!(engine.hooks.len(), 4200);
    // Every exchange is complete and nobody can need its files again.
    assert_eq!(
        journal_names(&scratch.0.join("engine-hooks")),
        Vec::<String>::new()
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn hook_journal_recovers_from_crashes_and_an_older_release_leftovers() {
    let scratch = Scratch::new();
    let run = RunId::new();
    let journal = scratch.0.join("engine-hooks");
    fs::create_dir(&journal).unwrap();
    let request = serde_json::to_vec(&hook_request(run)).unwrap();
    let reply = serde_json::to_vec(&Ok::<_, PublicError>(EngineHookReply {
        stdout: None,
        exit_code: 0,
    }))
    .unwrap();
    // An older release kept every finished exchange: its claim and its reply.
    for n in 0..3000 {
        fs::write(journal.join(format!("old-{n:04}.claimed.json")), &request).unwrap();
        fs::write(journal.join(format!("old-{n:04}.reply.json")), &reply).unwrap();
    }
    // A supervisor crashed while deciding, and another while writing a reply.
    fs::write(journal.join("deciding.claimed.json"), &request).unwrap();
    fs::write(journal.join("writing.claimed.json"), &request).unwrap();
    fs::write(journal.join("writing.reply.tmp"), &reply[..4]).unwrap();
    // A supervisor crashed between its durable reply and removing the claim.
    fs::write(journal.join("replied.claimed.json"), &request).unwrap();
    fs::write(journal.join("replied.reply.json"), &reply).unwrap();
    // A supervisor starting on these keeps only the replies, which are the guardian's.
    let mut engine = answering_engine();
    process_hooks(&mut engine, run, &scratch.0).unwrap();
    assert!(
        engine.hooks.is_empty(),
        "an uncertain claim is never decided again"
    );
    let names = journal_names(&journal);
    assert_eq!(names.len(), 3001);
    assert!(names.iter().all(|name| name.ends_with(".reply.json")));
    // A guardian exchange removes them: it waits on one hook at a time, so nobody reads them.
    // This one's decision times out before the supervisor's next pass, between the reply's
    // write and its read.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            sluice_process::hook_journal::exchange(&scratch.0, &hook_request(run)),
        )
        .await
        .is_err()
    );
    let names = journal_names(&journal);
    assert_eq!(names.len(), 1);
    assert!(names[0].ends_with(".request.json"), "{names:?}");
    // The abandoned request is still decided, once, and its reply is never read ...
    process_hooks(&mut engine, run, &scratch.0).unwrap();
    process_hooks(&mut engine, run, &scratch.0).unwrap();
    assert_eq!(engine.hooks.len(), 1);
    let names = journal_names(&journal);
    assert_eq!(names.len(), 1);
    assert!(names[0].ends_with(".reply.json"), "{names:?}");
    // ... so the next exchange removes it, and leaves nothing behind itself.
    let server = HookServer::start(run, scratch.0.clone());
    assert_eq!(exchange_once(&scratch.0, run, 0).await.exit_code, 0);
    assert_eq!(server.stop().hooks.len(), 1);
    assert_eq!(journal_names(&journal), Vec::<String>::new());
}
#[tokio::test]
async fn hook_journal_refuses_a_runaway_of_hooks_in_flight() {
    use sluice_process::hook_journal::MAX_IN_FLIGHT;
    let scratch = Scratch::new();
    let run = RunId::new();
    let journal = scratch.0.join("engine-hooks");
    fs::create_dir(&journal).unwrap();
    let request = serde_json::to_vec(&hook_request(run)).unwrap();
    // Nobody claims: every request stays in flight. One more is being decided.
    for n in 0..MAX_IN_FLIGHT - 1 {
        fs::write(journal.join(format!("{n:04}.request.json")), &request).unwrap();
    }
    fs::write(journal.join("deciding.claimed.json"), &request).unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        sluice_process::hook_journal::exchange(&scratch.0, &hook_request(run)),
    )
    .await
    .expect("a runaway is refused at once")
    .unwrap_err();
    let PublicError::BadRequest { message } = error else {
        panic!("{error:?}");
    };
    assert!(
        message.contains(&format!(
            "{MAX_IN_FLIGHT} hooks in flight ({} unclaimed, 1 claimed without a reply;",
            MAX_IN_FLIGHT - 1
        )),
        "{message}"
    );
    assert_eq!(journal_names(&journal).len(), MAX_IN_FLIGHT);
}

struct HookHost {
    done: Arc<AtomicBool>,
    process: ProcessIdentity,
}
struct HookPayload {
    id: InvocationId,
    done: Arc<AtomicBool>,
    process: ProcessIdentity,
}
impl FnHost for HookHost {
    async fn invoke(&self, _i: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
fn empty() -> CleanupEvidence {
    CleanupEvidence {
        cgroup: "fixture-hook".into(),
        empty: true,
        escalated: false,
    }
}
impl PayloadHost for HookHost {
    type Invocation = HookPayload;
    async fn start(
        &self,
        request: LaunchRequest,
        _cancel: CancellationToken,
    ) -> io::Result<HookPayload> {
        Ok(HookPayload {
            id: request.invocation.invocation,
            done: self.done.clone(),
            process: self.process.clone(),
        })
    }
    fn empty_without_payload(&self) -> io::Result<CleanupEvidence> {
        Ok(empty())
    }
}
impl PayloadInvocation for HookPayload {
    fn id(&self) -> InvocationId {
        self.id
    }
    fn executor(&self) -> &ProcessIdentity {
        &self.process
    }
    async fn poll(&mut self) -> io::Result<Option<(PayloadResult, ExitEvidence)>> {
        Ok(self.done.load(Ordering::SeqCst).then(|| {
            (
                PayloadResult::Succeeded(JsonMap::default()),
                ExitEvidence {
                    code: Some(0),
                    signal: None,
                    executor: Some(self.process.clone()),
                },
            )
        }))
    }
    async fn deliver(&mut self, _messages: &[DeliveryMessage]) -> io::Result<()> {
        Ok(())
    }
    async fn cleanup(&mut self) -> io::Result<CleanupEvidence> {
        Ok(empty())
    }
    async fn engine_hook(
        &mut self,
        request: EngineHookRequest,
    ) -> Result<EngineHookReply, PublicError> {
        assert_eq!(request.event, "Stop");
        self.done.store(true, Ordering::SeqCst);
        Ok(EngineHookReply {
            stdout: Some(JsonValue::try_from(serde_json::json!({"decision":"block"})).unwrap()),
            exit_code: 2,
        })
    }
}
#[tokio::test]
async fn authenticated_guardian_hook_roundtrip_returns_synchronous_cli_decision() {
    use tokio::io::AsyncWriteExt;
    let scratch = Scratch::new();
    let run = RunId::new();
    let project = ProjectId::new();
    let step: StepId = "work".parse().unwrap();
    let attempt = AttemptId::new();
    let invocation = InvocationId::new();
    let process = ProcessIdentity::read(std::process::id()).unwrap();
    let identity = AttemptKey {
        home: HomeId::new(),
        project: Some(project),
        step: Some(step.clone()),
        generation: StepGeneration(1),
        work: WorkGeneration(1),
        run,
        attempt,
    };
    let range = AssignedRange {
        after: MessageId(0),
        through: MessageId(0),
    };
    let link = MemoryCoordinator::default();
    link.reserve(identity.clone(), range, vec![]);
    let capability = RunCapability::new("test-hook-capability");
    let args = GuardianArgs {
        home_dir: scratch.0.clone(),
        run_dir: scratch.0.join("run"),
        guardian: GuardianIdentity {
            identity,
            process: process.clone(),
            unit: format!("sluice-test-{run}.service"),
            socket_challenge: "challenge".into(),
        },
        invocation: FnInvocation {
            project,
            step: Some(step),
            run,
            attempt,
            invocation,
            name: "fixture".into(),
            inputs: JsonMap::default(),
        },
        assigned: range,
        prev_run: None,
        capability: capability.clone(),
        poll_interval: Duration::from_millis(2),
    };
    let directory = args.run_dir.clone();
    let done = Arc::new(AtomicBool::new(false));
    let host = HookHost {
        done: done.clone(),
        process,
    };
    let link_check = link.clone();
    let guardian = tokio::spawn(async move { guardian_main(args, &link, &host).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !directory.join("control.sock").exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let rejected: Result<ControlReply, _> = call(
        &directory.join("control.sock"),
        &capability,
        ControlCommand::EngineHook {
            engine: "fake".into(),
            run: RunId::new(),
            event: "Stop".into(),
            payload: JsonValue::try_from(serde_json::json!({})).unwrap(),
        },
    )
    .await;
    assert!(rejected.is_err());
    assert!(!done.load(Ordering::SeqCst));
    let wrong_capability = RunCapability::new("wrong");
    let rejected: Result<ControlReply, _> = call(
        &directory.join("control.sock"),
        &wrong_capability,
        ControlCommand::EngineHook {
            engine: "fake".into(),
            run,
            event: "Stop".into(),
            payload: JsonValue::try_from(serde_json::json!({})).unwrap(),
        },
    )
    .await;
    assert!(rejected.is_err());
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("sluice");
    let mut command = tokio::process::Command::new(binary);
    command
        .args(["agent-hook", "fake", "Stop"])
        .env("SLUICE_HOME", &scratch.0)
        .env("SLUICE_RUN_DIR", &directory)
        .env("SLUICE_RUN_ID", run.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"turn\":\"t\"}")
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"decision":"block"})
    );
    guardian.await.unwrap().unwrap();
    assert!(link_check.with_state(|s| s.attempts[&run].completion.is_some()));
}

struct CompositionFactory {
    config: SupervisorConfig,
    calls: std::sync::atomic::AtomicU32,
    cleanups: Arc<std::sync::atomic::AtomicU32>,
    transient_first: bool,
    admitted: Option<Arc<tokio::sync::Notify>>,
    release: Option<Arc<tokio::sync::Notify>>,
}
/// Cleanups so far, and whether the agent has submitted: it does once its turn is over.
struct CompositionHost(Arc<std::sync::atomic::AtomicU32>, bool);
impl SupervisorHost for CompositionHost {
    async fn snapshot(&mut self, _: MessageId) -> io::Result<HostSnapshot> {
        Ok(HostSnapshot {
            submissions: if self.1 {
                BTreeMap::from([("ready".into(), serde_json::json!(true))])
            } else {
                BTreeMap::new()
            },
            ..HostSnapshot::default()
        })
    }
    async fn acknowledge(&mut self, _: &[MessageId]) -> io::Result<()> {
        Ok(())
    }
    async fn me(&mut self) -> io::Result<String> {
        Ok("composed snapshot".into())
    }
    async fn note(&mut self, _: &str) -> io::Result<()> {
        Ok(())
    }
    async fn checkpoint(&mut self, checkpoint: &Checkpoint) -> io::Result<()> {
        self.1 |= checkpoint.state == State::Idle;
        Ok(())
    }
    async fn cleanup(&mut self) -> io::Result<()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
impl AgentFactory for CompositionFactory {
    type Engine = ScriptedEngine;
    type Host = CompositionHost;
    async fn environment(
        &self,
        _: &FnInvocation,
    ) -> Result<AgentEnvironment<Self::Engine, Self::Host>, AgentFailure> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(admitted) = &self.admitted {
            admitted.notify_one();
        }
        if let Some(release) = &self.release {
            release.notified().await;
        }
        let mut config = self.config.clone();
        config.internal_attempt = call;
        let resumed = call > 1;
        let observe = |status, turns_started, turns_completed| EngineObservation {
            status,
            turns_started,
            turns_completed,
            session_id: Some("composed-session".into()),
            progress: turns_started + turns_completed,
            final_text: "composed answer".into(),
            ..EngineObservation::default()
        };
        let frame = |command, observation| ScriptFrame {
            command,
            observation,
            outcome: DeliveryOutcome::Acknowledged,
            error: None,
            delay_ms: 0,
        };
        let mut end = observe(EngineStatus::Idle, 1, 1);
        if self.transient_first && call == 1 {
            end.status = EngineStatus::Busy;
            end.turns_completed = 0;
            end.error = Some(EngineError {
                kind: EngineErrorKind::Transient,
                message: "capacity".into(),
                retry_at: None,
            });
        }
        let engine = ScriptedEngine::new(vec![
            frame(
                Some(if resumed {
                    EngineCommand::Resume {
                        session: "composed-session".into(),
                    }
                } else {
                    EngineCommand::StartFresh
                }),
                observe(EngineStatus::Idle, 0, 0),
            ),
            frame(
                Some(EngineCommand::DeliverText {
                    id: if resumed {
                        InputId::Continue { attempt: call }
                    } else {
                        InputId::Task
                    },
                    text: "*".into(),
                }),
                observe(EngineStatus::Busy, 1, 0),
            ),
            frame(None, end),
        ]);
        Ok(AgentEnvironment {
            config,
            engine,
            host: CompositionHost(self.cleanups.clone(), false),
            prompt: ports(),
            tmux: None,
            cancel: CancellationToken::new(),
        })
    }
}
fn composition(scratch: &Scratch, transient_first: bool) -> (CompositionFactory, FnInvocation) {
    let invocation = FnInvocation {
        project: ProjectId::new(),
        step: None,
        run: RunId::new(),
        attempt: AttemptId::new(),
        invocation: InvocationId::new(),
        name: "agent.run".into(),
        inputs: inputs(
            serde_json::json!({"engine":"fake", "cwd":scratch.0, "spec":"composed task"}),
        ),
    };
    let factory = CompositionFactory {
        config: SupervisorConfig {
            run: invocation.run,
            attempt: invocation.attempt,
            invocation: invocation.invocation,
            project: "p".into(),
            home: scratch.0.clone(),
            run_dir: scratch.0.join("invocation"),
            cwd: scratch.0.clone(),
            engine: "fake".into(),
            task: String::new(),
            required: vec![],
            session: None,
            previous: None,
            assigned: AssignedRange {
                after: MessageId(0),
                through: MessageId(0),
            },
            messages: vec![],
            model: None,
            limits: Limits::test_profile(),
            retry: RetryPolicy::agent(),
            internal_attempt: 1,
        },
        calls: std::sync::atomic::AtomicU32::new(0),
        cleanups: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        transient_first,
        admitted: None,
        release: None,
    };
    (factory, invocation)
}
#[tokio::test]
async fn the_result_names_the_model_the_run_resolved() {
    let scratch = Scratch::new();
    let (factory, invocation) = composition(&scratch, false);
    let host = AgentFnHost::new(factory);
    let output = host.compose(invocation).await.unwrap();
    assert_eq!(
        output.0["model"].as_value(),
        &serde_json::json!(FIXTURE_MODEL)
    );
    // An id the engine does not list fails before anything starts: no checkpoint, no session.
    let scratch = Scratch::new();
    let (factory, _) = composition(&scratch, false);
    let directory = factory.config.run_dir.clone();
    let mut config = factory.config.clone();
    config.model = Some(model::ModelChoice::normal("fixtur", Some("max")));
    let mut engine = ScriptedEngine::new(vec![]);
    let failure = supervise(
        config,
        &mut engine,
        &mut CompositionHost(Arc::default(), false),
        &mut SessionGuard::default(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Invalid);
    assert_eq!(
        failure.message,
        r#"fake has no model fixtur-max (composed from model {"type":"normal","model":"fixtur","effort":"max"}); nearest: fixture"#
    );
    assert!(engine.commands.is_empty());
    assert!(Checkpoint::read(&directory).unwrap().is_none());
    // The request refuses a retired string before the session starts.
    let (factory, mut invocation) = composition(&scratch, false);
    invocation.inputs = inputs(
        serde_json::json!({"engine":"fake", "cwd":scratch.0, "spec":"task", "model":"fixture"}),
    );
    let failure = AgentFnHost::new(factory)
        .compose(invocation)
        .await
        .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Invalid);
    assert!(
        failure
            .message
            .contains(r#"use {"type":"normal","model":"fixture"}"#),
        "{}",
        failure.message
    );
}
#[tokio::test]
async fn composed_transient_returns_to_outer_budget_and_reenters_same_checkpoint_without_task_replay()
 {
    let scratch = Scratch::new();
    let (factory, invocation) = composition(&scratch, true);
    let directory = factory.config.run_dir.clone();
    let host = AgentFnHost::new(factory);
    let failure = tokio::time::timeout(Duration::from_secs(2), host.compose(invocation.clone()))
        .await
        .expect("outer helper owns the 600-second backoff")
        .unwrap_err();
    assert_eq!(failure.kind, FailureKind::Transient);
    assert_eq!(failure.session.as_deref(), Some("composed-session"));
    let before = Checkpoint::read(&directory).unwrap().unwrap();
    assert_eq!(before.state, State::Backoff);
    let lock = SessionGuard::default()
        .acquire(&scratch.0, "fake", "composed-session", "p", RunId::new())
        .unwrap_err();
    assert_eq!(lock.kind, FailureKind::LockConflict);
    let output = host.compose(invocation).await.unwrap();
    assert_eq!(
        output.0["session"].as_value(),
        &serde_json::json!("composed-session")
    );
    let after = Checkpoint::read(&directory).unwrap().unwrap();
    assert_eq!(after.run, before.run);
    assert_eq!(after.attempt, before.attempt);
    assert_eq!(after.head_before, before.head_before);
    assert_eq!(after.started_ms, before.started_ms);
    assert_eq!(
        after
            .delivery
            .entries
            .iter()
            .filter(|e| e.id == InputId::Task)
            .count(),
        1
    );
    assert_eq!(
        after
            .delivery
            .entries
            .iter()
            .find(|e| e.id == InputId::Task)
            .unwrap()
            .tries,
        1
    );
    assert_eq!(after.state, State::Done);
    assert!(host.factory.cleanups.load(Ordering::Relaxed) >= 2);
}
#[tokio::test]
async fn fn_host_dispatches_agent_builtin_and_refuses_concurrent_composition() {
    let scratch = Scratch::new();
    let (mut factory, invocation) = composition(&scratch, false);
    let admitted = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    factory.admitted = Some(admitted.clone());
    factory.release = Some(release.clone());
    let host = Arc::new(AgentFnHost::new(factory));
    let first = {
        let host = host.clone();
        let invocation = invocation.clone();
        tokio::spawn(async move { host.invoke(invocation).await })
    };
    admitted.notified().await;
    let failure = host.compose(invocation).await.unwrap_err();
    assert_eq!(failure.kind, FailureKind::Invalid);
    assert!(failure.message.contains("concurrent"));
    release.notify_one();
    let output = first.await.unwrap().unwrap();
    assert_eq!(
        output.0["final"].as_value(),
        &serde_json::json!("composed answer")
    );
    assert_eq!(host.factory.calls.load(Ordering::Relaxed), 1);
}
/// The note on the step's previous attempt heads the task, before the task itself.
#[test]
fn the_previous_attempt_note_heads_the_task() {
    let mut context = ports();
    context.previous =
        Some("## Previous attempt\nNone: this is the first attempt at this step.".into());
    let text = build("Fix it", &BTreeMap::new(), &context);
    let note = text.find("## Previous attempt").unwrap();
    let task = text.find("## Task\n\nFix it").unwrap();
    assert!(text.starts_with("Step work: repair the bug"), "{text}");
    assert!(note < task, "{text}");
    assert!(text.contains("first attempt"), "{text}");
}
fn git_in(repo: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(repo)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
/// The work tree summary: clean, dirty (count and the first 20 paths), not a repository, and
/// git absent, which never fails: it says so.
#[tokio::test]
async fn the_work_tree_summary_counts_dirty_paths_and_survives_git_absent() {
    use sluice_model::attempt::WorkTreeState;
    let scratch = Scratch::new();
    let plain = scratch.0.join("plain");
    fs::create_dir(&plain).unwrap();
    let git = std::path::Path::new("git");
    let wait = Duration::from_secs(10);
    assert_eq!(sluice_agents::git::worktree(git, &plain, wait).await, None);
    let repo = scratch.0.join("repo");
    fs::create_dir(&repo).unwrap();
    git_in(&repo, &["init", "-q"]);
    fs::write(repo.join("kept.txt"), "kept\n").unwrap();
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "first"]);
    assert_eq!(
        sluice_agents::git::worktree(git, &repo, wait).await,
        Some(WorkTreeState::Clean)
    );
    fs::write(repo.join("kept.txt"), "changed\n").unwrap();
    for n in 0..25 {
        fs::write(repo.join(format!("new-{n:02}.txt")), "x").unwrap();
    }
    let Some(WorkTreeState::Dirty { count, paths }) =
        sluice_agents::git::worktree(git, &repo, wait).await
    else {
        panic!("dirty");
    };
    assert_eq!(count, 26);
    assert_eq!(paths.len(), 20);
    assert!(paths.contains(&"M kept.txt".to_owned()), "{paths:?}");
    assert!(paths.contains(&"?? new-00.txt".to_owned()), "{paths:?}");
    let absent = scratch.0.join("no-git-here");
    assert_eq!(
        sluice_agents::git::worktree(&absent, &repo, wait).await,
        Some(WorkTreeState::Unavailable {
            reason: "git is not installed or not on PATH".into()
        })
    );
    let note = sluice_model::attempt::AttemptNote {
        number: 1,
        previous: None,
        worktree: Some(sluice_model::attempt::WorkTree {
            cwd: repo.display().to_string(),
            state: WorkTreeState::Dirty {
                count,
                paths: paths.clone(),
            },
        }),
    };
    assert!(
        note.text().contains(&format!(
            "has 26 uncommitted paths now: {}",
            paths.join(", ")
        )),
        "{}",
        note.text()
    );
    assert!(note.text().ends_with(" and 6 more."), "{}", note.text());
}

#[test]
fn account_limits_split_at_the_threshold_and_render_their_reset() {
    use sluice_agents::engines::account::{self, Cap, Engine, Limit};
    let now = 1_791_000_000;
    let limit = |cap, resets_at| Limit {
        window: Some(account::window(10080)),
        resets_at,
        ..Limit::new(Engine::Codex, cap, "limit text")
    };
    let threshold = account::DEFAULT_THRESHOLD;
    // No known reset: a usage limit is a hard cap, a rate limit keeps the fixed backoff.
    let hard = limit(Cap::Usage, None).classify(now, threshold);
    assert_eq!(hard.kind, EngineErrorKind::QuotaExhausted);
    assert_eq!(
        hard.message,
        "codex: usage limit reached (weekly window) — buy credits at https://chatgpt.com/codex/settings/usage, or wait for it to reset, then step_retry (or run the step on another engine). Codex said: limit text"
    );
    let short = limit(Cap::Rate, None).classify(now, threshold);
    assert_eq!(short.kind, EngineErrorKind::Transient);
    assert_eq!(short.retry_at, None);
    assert_eq!(
        short.message,
        "codex: rate limited (weekly window) — retrying after the standard backoff. Codex said: limit text"
    );
    // A known reset decides for either cap: beyond the threshold it is a hard cap.
    for cap in [Cap::Usage, Cap::Rate] {
        let far = limit(cap, Some(now + 15 * 60 + 1)).classify(now, threshold);
        assert_eq!(far.kind, EngineErrorKind::QuotaExhausted);
        assert_eq!(far.retry_at, None);
        let near = limit(cap, Some(now + 15 * 60)).classify(now, threshold);
        assert_eq!(near.kind, EngineErrorKind::Transient);
        assert_eq!(near.retry_at, Some(now + 15 * 60 + account::RESET_MARGIN));
        // A reset already past retries at once.
        let past = limit(cap, Some(now - 10)).classify(now, threshold);
        assert_eq!(past.retry_at, Some(now + account::RESET_MARGIN));
    }
    let weekly = limit(Cap::Rate, Some(now + 6 * 86400 + 3 * 3600)).classify(now, threshold);
    assert_eq!(
        weekly.message,
        "codex: rate limit reached (weekly window); resets 2026-10-09T07:00Z (in 6d 3h) — wait for the reset or buy credits at https://chatgpt.com/codex/settings/usage, then step_retry (or run the step on another engine). Codex said: limit text"
    );
    assert_eq!(account::stamp(1_791_000_000), "2026-10-03T04:00Z");
    assert_eq!(account::relative(now + 40, now), "in 40s");
    assert_eq!(account::relative(now + 125, now), "in 2m");
    assert_eq!(account::relative(now + 3 * 3600, now), "in 3h");
    assert_eq!(account::relative(now + 7500, now), "in 2h 5m");
    assert_eq!(account::relative(now + 2 * 86400, now), "in 2d");
    assert_eq!(account::relative(now, now), "now");
    assert_eq!(account::window(300), "5-hour window");
    assert_eq!(account::window(43200), "30-day window");
    assert_eq!(
        account::parse_threshold("2.5").unwrap(),
        std::time::Duration::from_secs(150)
    );
    assert!(account::parse_threshold("-1").is_err());
    assert!(account::parse_threshold("soon").is_err());
}

#[test]
fn account_auth_messages_name_the_problem_and_the_fix() {
    use sluice_agents::engines::account::{Auth, Engine, login_problem};
    let error = Auth::login(
        Engine::Codex,
        "Your access token could not be refreshed. Please log out and sign in again.",
    )
    .error();
    assert_eq!(error.kind, EngineErrorKind::AuthFailed);
    assert_eq!(
        error.message,
        "codex: not logged in (token could not be refreshed) — run `codex login` on this host, then step_retry. Codex said: Your access token could not be refreshed. Please log out and sign in again."
    );
    assert_eq!(error.retry_at, None);
    for (text, problem) in [
        (
            "Failed to authenticate: OAuth session expired",
            "not logged in (token expired)",
        ),
        (
            "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.",
            "not logged in (signed out or switched account elsewhere)",
        ),
        (
            "Your session is no longer authenticated. Run `/login`",
            "not logged in (session no longer authenticated)",
        ),
        (
            "Invalid API key \u{b7} Fix external API key",
            "not logged in (invalid API key)",
        ),
        ("Something else", "not logged in (unauthorized)"),
    ] {
        assert_eq!(login_problem(text), problem, "{text}");
    }
    // An empty engine text leaves no `said` clause.
    let bare = Auth::login(Engine::Devin, "").error();
    assert_eq!(
        bare.message,
        "devin: not logged in (unauthorized) — run `devin auth login` on this host (or /login inside `devin`), then step_retry."
    );
}

#[test]
fn account_messages_mask_token_like_text_and_keep_ids() {
    use sluice_agents::engines::account::{Auth, Cap, Engine, Limit, redact};
    let jwt = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyLTEyMyJ9.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    let text = format!(
        "unexpected status 401 Unauthorized: Authorization: Bearer {jwt}, key sk-proj-AbCdEfGh1234567890IjKl, access_token=ya29.a0AfH6SMBx1234abcd, \"refresh_token\": \"rt_0123456789abcdefXYZ\", raw 9f8e7d6c5b4a39281706f5e4d3c2b1a0ffeeddccbbaa, trace ID: 202f291a871d718834e52771c111f102, request id: req_07f626009a4646dfb34bb8ac40e63cb7, cf-ray: a3f548979a7d55c4-PRG, session 01a10b56-8977-7302-9e37-9379c79804d8."
    );
    let masked = redact(&text);
    for secret in [
        "eyJhbGci",
        "sk-proj-AbCd",
        "ya29.a0AfH6",
        "rt_0123456789",
        "9f8e7d6c5b4a3928",
    ] {
        assert!(!masked.contains(secret), "{secret} in {masked}");
    }
    assert_eq!(
        masked,
        "unexpected status 401 Unauthorized: Authorization: Bearer [redacted], key [redacted], access_token=[redacted], \"refresh_token\": \"[redacted]\", raw [redacted], trace ID: 202f291a871d718834e52771c111f102, request id: req_07f626009a4646dfb34bb8ac40e63cb7, cf-ray: a3f548979a7d55c4-PRG, session 01a10b56-8977-7302-9e37-9379c79804d8."
    );
    // Both kinds of account message carry the masked text only.
    let auth = Auth::login(Engine::Claude, &text).error();
    let limit = Limit::new(Engine::Devin, Cap::Usage, &text).classify(0, std::time::Duration::ZERO);
    for message in [auth.message, limit.message] {
        assert!(message.ends_with(&masked), "{message}");
        assert!(!message.contains("eyJ"), "{message}");
    }
    // Prose, URLs and short words pass untouched.
    let prose = "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at Apr 8th, 2026 10:13 AM.";
    assert_eq!(redact(prose), prose);
}
