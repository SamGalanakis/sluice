use sluice_agents::{engines::*, prompt::*, supervisor::*, *};
use sluice_model::{
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, JsonValue, RunCapability},
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

fn inputs(value: serde_json::Value) -> JsonMap {
    sluice_model::rpc::decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
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

#[tokio::test]
async fn settled_engine_logs_are_null_when_absent_and_copy_existing_records() {
    let scratch = Scratch::new();
    fs::write(
        scratch.0.join("native.json"),
        r#"{"session":"fixture","final_text":"done"}"#,
    )
    .unwrap();
    for engine in ["codex", "devin"] {
        let source = scratch.0.join(format!("{engine}.log"));
        let destination = scratch.0.join(format!("{engine}-copy.log"));
        let inp = inputs(serde_json::json!({"cwd":scratch.0,"spec":"fixture","log":destination}));
        let name = format!("agent.{engine}");
        let output = settled_outputs(&name, &inp, &scratch.0, &BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(output.0["log"].as_value(), &serde_json::Value::Null);
        assert!(!destination.exists());
        fs::write(&source, "assistant\ndone\n").unwrap();
        let output = settled_outputs(&name, &inp, &scratch.0, &BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(output.0["log"].as_value(), &serde_json::json!(destination));
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "assistant\ndone\n"
        );
        let inp = inputs(serde_json::json!({"cwd":scratch.0,"spec":"fixture","log":source}));
        settled_outputs(&name, &inp, &scratch.0, &BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "assistant\ndone\n");
    }
}
