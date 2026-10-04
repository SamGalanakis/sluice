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
    let t = text.find("Messages for you").unwrap();
    assert!(i < o && o < t);
    assert!(text.contains("`interface` (string):\napi.md\nsecond line"));
    assert!(text.contains("\"project\":\"p\",\"step\":\"work\",\"run\":\"r\""));
    assert!(text.contains("\"ready\": <boolean>"));
    assert!(text.contains("step-work"));
    assert!(text.contains("needs_reply"));
    assert!(text.contains("sluice tool message_post"));
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
        .split("sluice tool message_post '")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .unwrap();
    let post: serde_json::Value = serde_json::from_str(post).unwrap();
    assert_eq!(post["project"], selector.as_str());
    assert_eq!(post["thread"], "step-work");
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
    assert!(!text.contains("Messages for you"));
    // Posting needs no live delivery: the exact command still carries this run's identity.
    let post = text
        .split("sluice tool message_post '")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .unwrap();
    let post: serde_json::Value = serde_json::from_str(post).unwrap();
    assert_eq!(
        post,
        serde_json::json!({"project":"p","thread":"step-work","from":"work","run":"r",
                           "to":"orchestrator","body":"...","needs_reply":false})
    );
    context.inputs.clear();
    context.outputs.clear();
    let text = build("task", &BTreeMap::new(), &context);
    assert!(!text.contains("##"));
    context.step.clear();
    let text = build("task", &BTreeMap::new(), &context);
    assert!(!text.contains("message_post"));
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
    let engine = ScriptedEngine::new(vec![]);
    assert!(
        engine
            .profile()
            .validate_selection(Some("unapproved"), None)
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
    assert!(journal.join("one.claimed.json").exists());
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
    assert!(!journal.join("crashed.reply.json").exists());
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
struct CompositionHost(Arc<std::sync::atomic::AtomicU32>);
impl SupervisorHost for CompositionHost {
    async fn snapshot(&mut self, _: MessageId) -> io::Result<HostSnapshot> {
        Ok(HostSnapshot {
            submissions: BTreeMap::from([("ready".into(), serde_json::json!(true))]),
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
    async fn checkpoint(&mut self, _: &Checkpoint) -> io::Result<()> {
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
            host: CompositionHost(self.cleanups.clone()),
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
            effort: None,
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
