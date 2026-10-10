//! Real uv, isolated scratch PEP 723 bundles, and minimal framed callback servers.
#[allow(dead_code)]
#[path = "../../sluice-store/tests/support/plan_rows.rs"]
mod plan_rows;
use serde_json::{Value, json};
use sluice_model::{
    commands::CommandRequest,
    ids::*,
    rpc::{FnInvocation, JsonMap, MAX_FRAME_BYTES, RunCapability, decode_json, encode_frame},
};
use sluice_runtime::python::*;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};
use tokio_util::sync::CancellationToken;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("sluice-test-python-{}", RunId::new()));
        let path = sluice_process::host::guard_scratch_home(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn host(&self, code: &str, returns: Value) -> (PythonHost, FnInvocation) {
        let bundle = self.0.join("bundle");
        let run_dir = self.0.join("run");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(&run_dir).unwrap();
        let script = if code.starts_with("# /// script") {
            code.to_owned()
        } else {
            format!(
                r#"# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
{code}"#
            )
        };
        std::fs::write(bundle.join("main.py"), script).unwrap();
        let config = PythonConfig::new(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../python")
                .canonicalize()
                .unwrap(),
            self.0.join("pinned-sluice"),
        );
        let invocation = FnInvocation {
            project: ProjectId::new(),
            step: Some("work".parse().unwrap()),
            run: RunId::new(),
            attempt: AttemptId::new(),
            invocation: InvocationId::new(),
            name: "test".into(),
            inputs: JsonMap::default(),
        };
        let host = PythonHost {
            config,
            bundle: PinnedPythonFn {
                bundle_dir: bundle,
                sibling_helper_root: None,
            },
            context: PythonContext {
                home: self.0.clone(),
                run_dir,
                project_dir: self.0.join("projects").join(invocation.project.to_string()),
                project: "p".into(),
                prev_run: None,
                extra_inputs: JsonMap::default(),
                outputs: JsonMap::default(),
                returns: map(returns),
                control_socket: None,
                run_capability: Some(RunCapability::new("scratch-capability")),
            },
            cancellation: CancellationToken::new(),
        };
        (host, invocation)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn map(value: Value) -> JsonMap {
    decode_json(&serde_json::to_vec(&value).unwrap()).unwrap()
}
fn data(value: Value) -> Value {
    json!({"reply":"data", "data":value})
}
fn value(map: &JsonMap) -> Value {
    serde_json::to_value(map).unwrap()
}
async fn execute(host: &PythonHost, invocation: &FnInvocation) -> Result<JsonMap, PythonError> {
    tokio::time::timeout(Duration::from_secs(30), host.execute(invocation))
        .await
        .expect("uv deadline")
}
struct Server {
    stop: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    path: PathBuf,
}
impl Server {
    fn new(path: PathBuf, mut handle: impl FnMut(HelperRequest) -> Value + Send + 'static) -> Self {
        Self::new_async(path, move |request| std::future::ready(handle(request)))
    }
    fn new_async<F: std::future::Future<Output = Value> + Send + 'static>(
        path: PathBuf,
        mut handle: impl FnMut(HelperRequest) -> F + Send + 'static,
    ) -> Self {
        let listener = UnixListener::bind(&path).unwrap();
        let stop = CancellationToken::new();
        let signal = stop.clone();
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! { _=signal.cancelled()=>break, result=listener.accept()=>result.unwrap() };
                let mut socket = accepted.0;
                let size = socket.read_u32().await.unwrap() as usize;
                assert!(size <= MAX_FRAME_BYTES);
                let mut bytes = vec![0; size + 4];
                bytes[..4].copy_from_slice(&(size as u32).to_be_bytes());
                socket.read_exact(&mut bytes[4..]).await.unwrap();
                let request = HelperRequest::decode_frame(&bytes).unwrap();
                assert_eq!(
                    serde_json::to_value(&request.run_capability).unwrap(),
                    json!("scratch-capability")
                );
                let id = request.request_id.clone();
                let payload = handle(request).await;
                let result = if payload.get("status").is_some() {
                    payload
                } else {
                    json!({"status":"ok","value":payload})
                };
                let reply = json!({"protocol":1,"request_id":id,"result":result});
                socket
                    .write_all(&encode_frame(&reply).unwrap())
                    .await
                    .unwrap();
            }
        });
        Self {
            stop,
            task: Some(task),
            path,
        }
    }
    async fn close(mut self) {
        self.stop.cancel();
        self.task.take().unwrap().await.unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[tokio::test]
async fn g2_real_uv_imports_only_the_standalone_helper_and_separates_stdout() {
    let scratch = Scratch::new();
    let (host,mut invocation)=scratch.host("from sluice_fn import run\nimport sluice_fn, importlib.util, os\ndef main(inp, ctx):\n    print('user progress')\n    return {'sum':inp['a']+1, 'helper':sluice_fn.__file__, 'old_absent':importlib.util.find_spec('sluice') is None, 'project':os.environ['SLUICE_PROJECT_ID']}\nrun(main)\n",json!({"sum":"int","helper":"string","old_absent":"boolean","project":"string"}));
    invocation.inputs = map(json!({"a":2}));
    let out = execute(&host, &invocation).await.unwrap();
    assert_eq!(value(&out)["sum"], 3);
    assert_eq!(value(&out)["old_absent"], true);
    assert_eq!(value(&out)["project"], invocation.project.to_string());
    assert_eq!(
        value(&out)["helper"],
        host.config
            .helper_dir
            .join("sluice_fn/__init__.py")
            .to_str()
            .unwrap()
    );
    let stderr = std::fs::read_to_string(host.context.run_dir.join("stderr.log")).unwrap();
    assert!(stderr.contains("user progress"));
    let version = tokio::process::Command::new(&host.config.uv)
        .arg("--version")
        .output()
        .await
        .unwrap();
    println!(
        "G2: {}helper={} old_package_absent=true",
        String::from_utf8_lossy(&version.stdout),
        value(&out)["helper"]
    );
}

#[tokio::test]
async fn same_run_transient_preserves_pid_context_and_retry_budget() {
    let scratch = Scratch::new();
    let (mut host,invocation)=scratch.host("from sluice_fn import run, Transient\nimport os\npids=[]\ndef main(inp,ctx):\n    pids.append(os.getpid())\n    if ctx.attempt < 3: raise Transient('busy')\n    return {'attempt':ctx.attempt, 'pids':pids, 'run':ctx.run_id}\nrun(main,retries=2,backoff=99)\n",json!({"attempt":"int","pids":"int[]","run":"string"}));
    host.config
        .environment
        .insert("SLUICE_BACKOFF".into(), "0".into());
    let out = value(&execute(&host, &invocation).await.unwrap());
    assert_eq!(out["attempt"], 3);
    let pids = out["pids"].as_array().unwrap();
    assert_eq!(pids.len(), 3);
    assert!(pids.iter().all(|p| p == &pids[0]));
    assert_eq!(out["run"], invocation.run.to_string());
}

#[tokio::test]
async fn exhaustion_and_ordinary_failure_are_terminal_typed_errors() {
    for (error, expected) in [
        ("Transient('busy')", PythonError::Transient("busy".into())),
        ("ValueError('bad')", PythonError::Failure("bad".into())),
        (
            "Rejected('refused')",
            PythonError::Rejected("refused".into()),
        ),
    ] {
        let scratch = Scratch::new();
        let (host,invocation)=scratch.host(&format!("from sluice_fn import run,Transient,Rejected\ndef main(inp,ctx):\n    raise {error}\nrun(main,retries=1,backoff=0)\n"),json!({}));
        assert_eq!(execute(&host, &invocation).await.unwrap_err(), expected);
    }
}

#[tokio::test]
async fn invalid_backoff_never_calls_main() {
    for backoff in ["-1", "nan", "inf", "nope"] {
        let scratch = Scratch::new();
        let (mut host,invocation)=scratch.host("from sluice_fn import run\ndef main(inp,ctx):\n    raise AssertionError('main must not execute')\nrun(main)\n",json!({}));
        host.config
            .environment
            .insert("SLUICE_BACKOFF".into(), backoff.into());
        let err = execute(&host, &invocation).await.unwrap_err();
        assert!(matches!(err, PythonError::Failure(_)));
        assert!(!err.to_string().contains("main must not execute"));
    }
}
async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn signal_interrupts_helper_backoff_before_a_second_call() {
    let scratch = Scratch::new();
    let (host,invocation)=scratch.host("from sluice_fn import run,Transient\nimport os\ndef main(inp,ctx):\n    (ctx.run_dir/'pid').write_text(str(os.getpid()))\n    if ctx.attempt>1: (ctx.run_dir/'second').touch()\n    raise Transient('busy')\nrun(main,retries=3,backoff=600)\n",json!({}));
    let signal = async {
        wait_file(&host.context.run_dir.join("pid")).await;
        let pid = std::fs::read_to_string(host.context.run_dir.join("pid")).unwrap();
        // Only signal the scratch Python child whose identity this fixture just obtained.
        assert!(
            tokio::process::Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .await
                .unwrap()
                .success()
        );
    };
    let (result, ()) = tokio::join!(execute(&host, &invocation), signal);
    assert!(matches!(result, Err(PythonError::Cancelled(_))));
    assert!(!host.context.run_dir.join("second").exists());
}
#[tokio::test]
async fn host_cancellation_reaps_uv_while_backoff_is_active() {
    let scratch = Scratch::new();
    let (host,invocation)=scratch.host("from sluice_fn import run,Transient\ndef main(inp,ctx):\n    (ctx.run_dir/'started').write_text(str(__import__('os').getpid()))\n    raise Transient('busy')\nrun(main,retries=3,backoff=600)\n",json!({}));
    let cancel = async {
        wait_file(&host.context.run_dir.join("started")).await;
        let pid: u32 = std::fs::read_to_string(host.context.run_dir.join("started"))
            .unwrap()
            .parse()
            .unwrap();
        let guard = OwnedChildGuard(sluice_process::identity::OwnedProcess::capture(pid).unwrap());
        host.cancellation.cancel();
        guard
    };
    let (result, guard) = tokio::join!(execute(&host, &invocation), cancel);
    assert!(matches!(result, Err(PythonError::Cancelled(_))));
    // This isolated test has no guardian to clean uv's interpreter child.
    drop(guard);
}

#[tokio::test]
async fn child_environment_restores_host_tools_and_keeps_callbacks() {
    let scratch = Scratch::new();
    let (mut host,invocation)=scratch.host("from sluice_fn import run,child_env,sh\nimport os\ndef main(inp,ctx):\n    os.environ['VIRTUAL_ENV']='/uv/environment'\n    os.environ['PATH']='/uv/environment/bin:/bad'\n    os.environ['PYTHONHOME']='/bad'\n    os.environ['UV_PROJECT_ENVIRONMENT']='/bad'\n    os.environ['CLAUDECODE']='1'\n    e=child_env({'X':'yes'})\n    return {'path':e['PATH'], 'x':e['X'], 'clean':all(k not in e for k in ['VIRTUAL_ENV','PYTHONPATH','PYTHONHOME','UV_PROJECT_ENVIRONMENT','CLAUDECODE']), 'run':e['SLUICE_RUN_ID'], 'host':sh(['python3','-c','import sys;print(sys.prefix)']).stdout.strip(), 'binary':sh(['printf','\\\\037\\\\213gz']).stdout}\nrun(main)\n",json!({"path":"string","x":"string","clean":"boolean","run":"string","host":"string","binary":"string"}));
    host.config
        .environment
        .remove(std::ffi::OsStr::new("PYTHONPATH"));
    host.config
        .environment
        .remove(std::ffi::OsStr::new("VIRTUAL_ENV"));
    let host_path = host.config.environment[std::ffi::OsStr::new("PATH")]
        .to_string_lossy()
        .into_owned();
    let out = value(&execute(&host, &invocation).await.unwrap());
    assert_eq!(out["path"], host_path);
    assert_eq!(out["x"], "yes");
    assert_eq!(out["clean"], true);
    assert_eq!(out["run"], invocation.run.to_string());
    assert!(!out["host"].as_str().unwrap().contains("environments-v2"));
    // Output that is not UTF-8 (a gzip header) is decoded with replacement characters.
    assert_eq!(out["binary"], "\u{1f}\u{fffd}gz");
}

#[tokio::test]
async fn envelope_carries_project_dir_and_the_fn_sees_ctx_project_dir() {
    let scratch = Scratch::new();
    let (host, invocation) = scratch.host(
        "from sluice_fn import run\nimport os\ndef main(inp,ctx):\n    return {'ctx':str(ctx.project_dir),'env':os.environ['SLUICE_PROJECT_DIR']}\nrun(main)\n",
        json!({"ctx":"string","env":"string"}),
    );
    let expected = scratch
        .0
        .join("projects")
        .join(invocation.project.to_string());
    let envelope: Value = decode_json(&host.envelope(&invocation).unwrap()).unwrap();
    assert_eq!(
        envelope["context"]["project_dir"].as_str().unwrap(),
        expected.to_str().unwrap()
    );
    let out = value(&execute(&host, &invocation).await.unwrap());
    assert_eq!(out["ctx"], expected.to_str().unwrap());
    assert_eq!(out["env"], expected.to_str().unwrap());
}

#[tokio::test]
async fn strict_result_envelopes_and_rust_output_validation_cannot_be_bypassed() {
    for raw in [
        "{}",
        "{\"ok\":true,\"outputs\":{\"n\":1}}{}",
        "{\"ok\":true,\"ok\":true,\"outputs\":{\"n\":1}}",
        "{\"ok\":true,\"outputs\":{\"n\":\"bad\"}}",
        "{\"ok\":true,\"outputs\":{}}",
        "{\"ok\":false,\"error\":{\"kind\":\"rejected\",\"message\":\"x\"}}",
        "{\"ok\":true,\"outputs\":{\"n\":1,\"other\":0}}",
    ] {
        let scratch = Scratch::new();
        let (host, invocation) = scratch.host(
            &format!("import sys\nsys.stdout.write({raw:?})\n"),
            json!({"n":"int"}),
        );
        assert!(execute(&host, &invocation).await.is_err(), "accepted {raw}");
    }
    let scratch = Scratch::new();
    let (host, invocation) = scratch.host(
        "print('{\"ok\":true,\"outputs\":{\"n\":1}}')\nraise SystemExit(75)\n",
        json!({"n":"int"}),
    );
    assert!(matches!(
        execute(&host, &invocation).await,
        Err(PythonError::Exit(_))
    ));
}
#[tokio::test]
async fn size_limits_bound_input_result_stderr_artifact_and_public_tail() {
    let scratch = Scratch::new();
    let (host, mut invocation) = scratch.host(
        "import sys\nsys.stdout.write('x'*(16*1024*1024+1))\n",
        json!({}),
    );
    assert!(
        execute(&host, &invocation)
            .await
            .unwrap_err()
            .to_string()
            .contains("16 MiB")
    );
    invocation.inputs = map(json!({"x":"x".repeat(MAX_FRAME_BYTES-20)}));
    assert!(host.envelope(&invocation).is_err());
    let (host,invocation)=scratch.host("from sluice_fn import run\nimport sys\ndef main(inp,ctx):\n    sys.stderr.write('x'*(2*1024*1024))\n    raise ValueError('é'*3000)\nrun(main)\n",json!({}));
    let err = execute(&host, &invocation).await.unwrap_err();
    assert!(err.to_string().len() <= ERROR_TAIL_BYTES);
    assert_eq!(
        std::fs::metadata(host.context.run_dir.join("stderr.log"))
            .unwrap()
            .len(),
        STDERR_ARTIFACT_BYTES as u64
    );
}

#[tokio::test]
async fn builtin_composition_and_tool_callbacks_keep_the_outer_run_identity() {
    let scratch = Scratch::new();
    let (mut host,invocation)=scratch.host("from sluice_fn import run\ndef main(inp,ctx):\n    a=ctx.builtin('agent.fake',{'task':ctx.header('do it')})\n    b=ctx.tool('status',{})\n    return {'session':a['session'],'project':b['project']}\nrun(main)\n",json!({"session":"string","project":"string"}));
    let outer = invocation.clone();
    let server = Server::new(
        scratch.0.join("control.sock"),
        move |request| match request.command {
            HelperCommand::Runtime(CommandRequest::Builtin { invocation }) => {
                assert_eq!(invocation.run, outer.run);
                assert_eq!(invocation.attempt, outer.attempt);
                assert_eq!(invocation.step, outer.step);
                assert_eq!(invocation.project, outer.project);
                assert_ne!(invocation.invocation, outer.invocation);
                assert_eq!(invocation.name, "agent.fake");
                data(json!({"session":"session-1"}))
            }
            HelperCommand::Extension(HelperExtension::Tool(request)) => {
                assert_eq!(request.name, "status");
                assert_eq!(value(&request.args)["project"], outer.project.to_string());
                data(json!({"project":outer.project.to_string()}))
            }
            HelperCommand::Runtime(CommandRequest::Submission { run }) => {
                assert_eq!(run, outer.run);
                data(json!({}))
            }
            _ => panic!("unexpected callback"),
        },
    );
    host.context.control_socket = Some(server.path.clone());
    let out = value(&execute(&host, &invocation).await.unwrap());
    assert_eq!(out["session"], "session-1");
    server.close().await;
}

#[tokio::test]
async fn section_release_allows_a_second_holder_and_same_run_reacquisition() {
    let scratch = Scratch::new();
    let first = Scratch::new();
    let second = Scratch::new();
    let path = scratch.0.clone();
    let first_code = format!(
        "from sluice_fn import run\nfrom pathlib import Path\nimport time\nroot=Path({:?})\ndef main(inp,ctx):\n    with ctx.acquire('land'):\n        (root/'first').touch()\n        while not (root/'waiting').exists(): time.sleep(.01)\n    while not (root/'second').exists(): time.sleep(.01)\n    with ctx.acquire('land'):\n        (root/'reacquired').touch()\n    return {{}}\nrun(main)\n",
        path.to_str().unwrap()
    );
    let second_code = format!(
        "from sluice_fn import run\nfrom pathlib import Path\nimport time\nroot=Path({:?})\ndef main(inp,ctx):\n    while not (root/'first').exists(): time.sleep(.01)\n    (root/'waiting').touch()\n    with ctx.acquire('land'):\n        (root/'second').touch()\n        time.sleep(.1)\n    return {{}}\nrun(main)\n",
        path.to_str().unwrap()
    );
    let (mut a, ai) = first.host(&first_code, json!({}));
    let (mut b, bi) = second.host(&second_code, json!({}));
    let mut owner = None;
    let mut leases = std::collections::BTreeMap::new();
    let mut serial = 0i64;
    let history = Arc::new(Mutex::new(Vec::new()));
    let trace = history.clone();
    let server = Server::new(
        scratch.0.join("control.sock"),
        move |request| match request.command {
            HelperCommand::Runtime(CommandRequest::AcquireLease(request)) => {
                assert_eq!(request.resource, "land");
                assert_eq!(request.amount, 1);
                let lease = *leases.entry(request.request_id).or_insert_with(|| {
                    serial += 1;
                    serial
                });
                if owner.is_none() {
                    owner = Some((lease, request.run));
                    trace.lock().unwrap().push(("acquire", request.run));
                }
                let state = if owner == Some((lease, request.run)) {
                    "held"
                } else {
                    "waiting"
                };
                json!({"reply":"lease","data":{"lease":lease,"state":state}})
            }
            HelperCommand::Runtime(CommandRequest::ReleaseLease(request)) => {
                if owner == Some((request.lease.0, request.run)) {
                    owner = None;
                    trace.lock().unwrap().push(("release", request.run));
                }
                json!({"reply":"ack"})
            }
            HelperCommand::Runtime(CommandRequest::Submission { .. }) => data(json!({})),
            _ => panic!("unexpected callback"),
        },
    );
    a.context.control_socket = Some(server.path.clone());
    b.context.control_socket = Some(server.path.clone());
    let (ar, br) = tokio::join!(execute(&a, &ai), execute(&b, &bi));
    ar.unwrap();
    br.unwrap();
    assert!(path.join("reacquired").exists());
    assert_eq!(
        *history.lock().unwrap(),
        vec![
            ("acquire", ai.run),
            ("release", ai.run),
            ("acquire", bi.run),
            ("release", bi.run),
            ("acquire", ai.run),
            ("release", ai.run)
        ]
    );
    server.close().await;
}

#[tokio::test]
async fn open_submissions_are_merged_and_validated_in_rust_even_without_the_helper() {
    for (submitted, valid) in [
        (json!({"summary":"submitted"}), true),
        (json!({"summary":5}), false),
        (json!({"other":1}), false),
        (json!({}), false),
    ] {
        let scratch = Scratch::new();
        let (mut host, invocation) = scratch.host(
            "print('{\"ok\":true,\"outputs\":{\"n\":1}}')\n",
            json!({"n":"int"}),
        );
        host.context.outputs = map(json!({"summary":{"type":"string"},"optional":"int?"}));
        let server = Server::new(scratch.0.join("control.sock"), move |request| {
            assert!(matches!(
                request.command,
                HelperCommand::Runtime(CommandRequest::Submission { .. })
            ));
            data(submitted.clone())
        });
        host.context.control_socket = Some(server.path.clone());
        let result = execute(&host, &invocation).await;
        assert_eq!(result.is_ok(), valid);
        if valid {
            assert_eq!(
                value(&result.unwrap()),
                json!({"summary":"submitted","n":1,"optional":null})
            );
        }
        server.close().await;
    }
}

#[tokio::test]
async fn converted_open_fixture_submits_and_resubmits_with_run_capability() {
    let scratch = Scratch::new();
    let source = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/open_fn.py"),
    )
    .unwrap();
    let (mut host, mut invocation) = scratch.host(
        &source,
        json!({"ports":"Any","extra":"Any","results":"Any"}),
    );
    host.context.outputs = map(json!({"summary":"string"}));
    host.context.extra_inputs = map(json!({"topic":{"type":"string"}}));
    invocation.inputs =
        map(json!({"attempts":[{"summary":"first"},{"summary":"last"}],"topic":"hello"}));
    let outer = invocation.clone();
    let mut submission = json!({});
    let server = Server::new(
        scratch.0.join("control.sock"),
        move |request| match request.command {
            HelperCommand::Runtime(CommandRequest::StepSubmit(request)) => {
                assert_eq!(request.run, outer.run);
                assert_eq!(request.project, outer.project);
                submission = value(&request.outputs);
                json!({"reply":"ack"})
            }
            HelperCommand::Runtime(CommandRequest::Submission { .. }) => data(submission.clone()),
            _ => panic!("unexpected"),
        },
    );
    host.context.control_socket = Some(server.path.clone());
    let out = value(&execute(&host, &invocation).await.unwrap());
    assert_eq!(out["summary"], "last");
    assert_eq!(out["extra"], json!({"topic":"hello"}));
    assert_eq!(out["results"].as_array().unwrap().len(), 2);
    server.close().await;
}

#[tokio::test]
async fn file_binding_bytes_are_frozen_by_caller_and_read_fresh_for_each_run() {
    let scratch = Scratch::new();
    let (host,mut invocation)=scratch.host("from sluice_fn import run\ndef main(inp,ctx):\n    return {'brief':inp['brief']}\nrun(main)\n",json!({"brief":"string"}));
    let file = scratch.0.join("brief.txt");
    std::fs::write(&file, "first").unwrap();
    invocation.inputs = map(json!({"brief":std::fs::read_to_string(&file).unwrap()}));
    std::fs::write(&file, "next").unwrap();
    assert_eq!(
        value(&execute(&host, &invocation).await.unwrap())["brief"],
        "first"
    );
    invocation.run = RunId::new();
    invocation.inputs = map(json!({"brief":std::fs::read_to_string(&file).unwrap()}));
    assert_eq!(
        value(&execute(&host, &invocation).await.unwrap())["brief"],
        "next"
    );
}

#[tokio::test]
async fn agent_failure_transient_and_tool_errors_have_the_requested_exception_shapes() {
    let scratch = Scratch::new();
    let (mut host,invocation)=scratch.host("from sluice_fn import run,AgentFailure,Transient,CallbackError\ndef main(inp,ctx):\n    if ctx.attempt==1: ctx.builtin('transient',{})\n    try: ctx.builtin('failure',{})\n    except AgentFailure as e: agent={'kind':e.kind,'message':e.message,'session':e.session}\n    try: ctx.tool('invalid',{})\n    except CallbackError as e: tool={'error':e.error,'message':e.message,'errors':e.errors,'current_rev':e.current_rev,'retryable':e.retryable}\n    return {'agent':agent,'tool':tool,'attempt':ctx.attempt}\nrun(main,retries=1,backoff=0)\n",json!({"agent":"Any","tool":"Any","attempt":"int"}));
    let server = Server::new(scratch.0.join("control.sock"), |request| {
        match request.command {
            HelperCommand::Runtime(CommandRequest::Builtin { invocation })
                if invocation.name == "transient" =>
            {
                json!({"status":"error","value":{"error":"transient","message":"busy"}})
            }
            HelperCommand::Runtime(CommandRequest::Builtin { .. }) => {
                json!({"status":"error","value":{"error":"agent_failure","kind":"WallCap","message":"time budget exhausted","session":"session-1"}})
            }
            HelperCommand::Extension(HelperExtension::Tool(_)) => {
                json!({"status":"error","value":{"error":"invalid","message":"bad request","errors":["inputs.n"],"current_rev":3,"retryable":false}})
            }
            HelperCommand::Runtime(CommandRequest::Submission { .. }) => data(json!({})),
            _ => panic!("unexpected"),
        }
    });
    host.context.control_socket = Some(server.path.clone());
    let out = value(&execute(&host, &invocation).await.unwrap());
    assert_eq!(out["attempt"], 2);
    assert_eq!(
        out["agent"],
        json!({"kind":"WallCap","message":"time budget exhausted","session":"session-1"})
    );
    assert_eq!(
        out["tool"],
        json!({"error":"invalid","message":"bad request","errors":["inputs.n"],"current_rev":3,"retryable":false})
    );
    server.close().await;
}

// These adapters use real store commands. The fixture has no needs or assigned messages.
struct Hooks;
impl sluice_store::plans::RetryMessages for Hooks {
    fn validate_retry(
        &self,
        _tx: &sluice_store::WriteTransaction<'_>,
        _project: ProjectId,
        _steps: &[StepId],
        body: &str,
        _author: &str,
    ) -> sluice_store::Result<()> {
        assert!(!body.is_empty());
        Ok(())
    }
    fn post_retry(
        &mut self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        project: ProjectId,
        step: &StepId,
        body: &str,
        author: &str,
    ) -> sluice_store::Result<()> {
        let _ = author;
        sluice_store::messages::post(
            tx,
            sluice_store::messages::Post {
                project: ProjectSelector::Id(project),
                speaker: sluice_store::messages::Speaker::Orchestrator,
                body: body.into(),
                verb: sluice_store::messages::Verb::Say {
                    to: step.to_string(),
                    data: None,
                },
            },
            &sluice_store::messages::NoPlanInputs,
        )?;
        Ok(())
    }
}
impl sluice_store::attempts::ExecutionHooks for Hooks {
    fn assign(
        &mut self,
        _tx: &mut sluice_store::WriteTransaction<'_>,
        _id: &sluice_store::attempts::AttemptIdentity,
        cursor: i64,
        exact: Option<&sluice_store::attempts::AssignedRange>,
    ) -> sluice_store::Result<sluice_store::attempts::AssignedRange> {
        Ok(exact
            .cloned()
            .unwrap_or(sluice_store::attempts::AssignedRange {
                after: cursor,
                through: cursor,
            }))
    }
    fn started(
        &mut self,
        _tx: &mut sluice_store::WriteTransaction<'_>,
        _id: &sluice_store::attempts::AttemptIdentity,
        _range: &sluice_store::attempts::AssignedRange,
    ) -> sluice_store::Result<()> {
        Ok(())
    }
    fn hold(
        &mut self,
        _tx: &mut sluice_store::WriteTransaction<'_>,
        _id: &sluice_store::attempts::AttemptIdentity,
        needs: &[(String, u64)],
        _scatter: bool,
    ) -> sluice_store::Result<()> {
        assert!(needs.is_empty());
        Ok(())
    }
    fn release(
        &mut self,
        tx: &mut sluice_store::WriteTransaction<'_>,
        id: &sluice_store::attempts::AttemptIdentity,
    ) -> sluice_store::Result<()> {
        sluice_store::resources::release_needs(tx, id.run)?;
        Ok(())
    }
}
#[tokio::test]
async fn helper_rejection_applies_or_conflicts_in_real_store_and_duplicate_completion_is_stable() {
    use sluice_model::{commands::*, plan::FnSignature};
    use sluice_store::{
        RetrySafety, Writer,
        attempts::*,
        plans::{self, PlanContext},
    };
    for conflict in [false, true] {
        let scratch = Scratch::new();
        let (mut host,mut invocation)=scratch.host("from sluice_fn import run,Rejected\nimport time\ndef main(inp,ctx):\n    ctx.retry_on_failure('work','please fix')\n    ctx.retry_on_failure('work','please fix')\n    (ctx.run_dir/'registered').touch()\n    while not (ctx.run_dir/'finish').exists(): time.sleep(.01)\n    raise Rejected('refused landing')\nrun(main)\n",json!({}));
        let writer = Writer::open(&scratch.0).unwrap();
        let project = invocation.project;
        let mut signatures = indexmap::IndexMap::new();
        signatures.insert("empty".to_string(), FnSignature::default());
        signatures.insert(
            "core.external".to_string(),
            FnSignature {
                open: true,
                ..FnSignature::default()
            },
        );
        let doc = map(
            json!({"steps":{"work":{"run":"core.external","outputs":{"ready":"boolean"}},"land":{"run":"empty","after":["work"]}}}),
        );
        let rows = sluice_model::plan_rows::PlanRows::from_document(&doc, None).unwrap();
        let plan = sluice_model::plan::compile_rows(&rows, &signatures).unwrap();
        let context = PlanContext {
            project,
            revision: Revision(2),
            plan,
        };
        let copy = context.clone();
        writer
            .write(RetrySafety::NonIdempotent, move |tx| {
                tx.sql().execute(
                    "INSERT INTO projects(project_id,name,created_at) VALUES (?1,'p','now')",
                    [project.to_string()],
                )?;
                tx.sql().execute(
                    "INSERT INTO plans(project_id,rev,root_order) VALUES (?1,1,'[\"steps\"]')",
                    [project.to_string()],
                )?;
                plan_rows::commit_document(tx, project, &doc, None, None)?;
                plans::step_set_output(
                    tx,
                    &copy,
                    StepSetOutput {
                        project: ProjectSelector::Id(project),
                        step: "work".parse().unwrap(),
                        outputs: map(json!({"ready":true})),
                        force: false,
                        reason: "fixture".into(),
                        author: Some("sam".into()),
                    },
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let copy = context.clone();
        let reservation = writer
            .write(RetrySafety::Idempotent, move |tx| {
                let step: StepId = "land".parse().unwrap();
                let state = plans::read_state(tx.sql(), project)?;
                let hash =
                    sluice_model::plan::inputs_hash(&copy.plan, &state, &copy.plan.steps()[&step])
                        .unwrap();
                let reservation = reserve(
                    tx,
                    &copy,
                    Reserve {
                        step,
                        attempt: AttemptId::new(),
                        run: RunId::new(),
                        item_index: -1,
                        item_count: None,
                        inputs: JsonMap::default(),
                        inputs_hash: hash,
                        provenance: JsonMap::default(),
                        release_id: "scratch".into(),
                        protocol_major: 1,
                    },
                    &mut Hooks,
                )?;
                assert!(claim(
                    tx,
                    &reservation.identity,
                    GuardianIdentity {
                        unit_name: format!("sluice-test-{}", reservation.identity.run),
                        boot_id: "fixture".into(),
                        pid: std::process::id(),
                        start: "fixture".into(),
                        cgroup: "fixture".into(),
                        socket_challenge: "fixture".into()
                    }
                )?);
                assert!(started(tx, &reservation.identity, &mut Hooks)?);
                Ok(reservation)
            })
            .await
            .unwrap();
        invocation.run = reservation.identity.run;
        invocation.attempt = reservation.identity.attempt;
        invocation.step = Some(reservation.identity.step.clone());
        let service_writer = writer.clone();
        let server = Server::new_async(scratch.0.join("control.sock"), move |request| {
            let writer = service_writer.clone();
            async move {
                let HelperCommand::Extension(HelperExtension::RetryOnFailure(request)) =
                    request.command
                else {
                    panic!("unexpected")
                };
                let ack = writer
                    .write(RetrySafety::Idempotent, move |tx| {
                        // Registration must reuse a saved target rather than recapturing newer work.
                        let saved: Option<String> = tx.sql().query_row(
                            "SELECT completion_action FROM runs WHERE run_id=?1",
                            [request.run.to_string()],
                            |r| r.get(0),
                        )?;
                        let target = if let Some(saved) = saved {
                            let saved: Value = serde_json::from_str(&saved)?;
                            serde_json::from_value(saved["target"].clone())?
                        } else {
                            completion_target(tx, request.project, &request.step)?.unwrap()
                        };
                        register_completion_action(
                            tx,
                            RegisterCompletionAction {
                                project: request.project,
                                run: request.run,
                                target,
                                message: request.message,
                                author: request.author,
                            },
                            &Hooks,
                        )
                    })
                    .await
                    .unwrap();
                assert!(ack);
                json!({"reply":"ack"})
            }
        });
        host.context.control_socket = Some(server.path.clone());
        let change = async {
            wait_file(&host.context.run_dir.join("registered")).await;
            if conflict {
                let copy = context.clone();
                writer
                    .write(RetrySafety::NonIdempotent, move |tx| {
                        plans::step_retry(
                            tx,
                            &copy,
                            StepRetry {
                                expected_rev: None,
                                project: ProjectSelector::Id(project),
                                selection: StepSelection {
                                    steps: Some(vec!["work".parse().unwrap()]),
                                    tags: None,
                                },
                                message: None,
                                reason: Some("concurrent explicit retry".into()),
                                author: Some("sam".into()),
                            },
                            &mut Hooks,
                        )
                    })
                    .await
                    .unwrap();
            }
            std::fs::write(host.context.run_dir.join("finish"), "").unwrap();
        };
        let (result, ()) = tokio::join!(execute(&host, &invocation), change);
        let PythonError::Rejected(message) = result.unwrap_err() else {
            panic!("expected typed rejection")
        };
        let copy = context.clone();
        let identity = reservation.identity.clone();
        let completion = Complete {
            completion_id: identity.run.to_string(),
            identity,
            kind: CompletionKind::Rejected { message },
            outputs: JsonMap::default(),
            processes_gone: true,
            submission_version: None,
        };
        let replay = completion.clone();
        let completed = writer
            .write(RetrySafety::Idempotent, move |tx| {
                complete(tx, &copy, completion, &mut Hooks)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, StepStatus::Failed);
        if conflict {
            assert!(matches!(
                completed.action,
                Some(CompletionActionOutcome::Conflict(_))
            ));
        } else {
            assert!(matches!(
                completed.action,
                Some(CompletionActionOutcome::Applied(_))
            ));
        }
        let copy = context.clone();
        let repeated = writer
            .write(RetrySafety::Idempotent, move |tx| {
                complete(tx, &copy, replay, &mut Hooks)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.action, repeated.action);
        assert_eq!(completed.result, repeated.result);
        server.close().await;
    }
}

struct OwnedChildGuard(sluice_process::identity::OwnedProcess);
impl Drop for OwnedChildGuard {
    fn drop(&mut self) {
        if self.0.exited().unwrap_or(true) {
            return;
        }
        let identity = self.0.identity();
        let status = std::process::Command::new("/usr/bin/python3")
            .args([
                "-c",
                r#"
import os,signal,select,sys
pid,start=map(int,sys.argv[1:])
try:
    fd=os.pidfd_open(pid)
    actual=int(open(f'/proc/{pid}/stat').read().rsplit(')',1)[1].split()[19])
    if actual != start: raise RuntimeError('scratch child identity changed')
    signal.pidfd_send_signal(fd,signal.SIGKILL)
    if not select.select([fd],[],[],5)[0]: raise RuntimeError('scratch child did not stop')
except ProcessLookupError:
    pass
"#,
                &identity.pid.to_string(),
                &identity.start_time.to_string(),
            ])
            .status();
        assert!(
            status.is_ok_and(|s| s.success()),
            "scratch child cleanup failed"
        );
    }
}

#[tokio::test]
async fn helper_rejects_malformed_inputs_before_running_user_code() {
    let scratch = Scratch::new();
    let (host,_)=scratch.host("from sluice_fn import run\ndef main(inp,ctx):\n    raise AssertionError('user code ran')\nrun(main)\n",json!({}));
    for raw in [
        b"{}".to_vec(),
        b"{\"protocol\":1,\"inputs\":{},\"context\":{}}{}".to_vec(),
        b"{\"protocol\":1,\"protocol\":1,\"inputs\":{},\"context\":{}}".to_vec(),
        vec![b'x'; MAX_FRAME_BYTES + 1],
    ] {
        let mut command = tokio::process::Command::new(&host.config.uv);
        command
            .args(["run", "--no-project", "--quiet"])
            .arg(host.bundle.bundle_dir.join("main.py"))
            .env("PYTHONPATH", &host.config.helper_dir)
            .current_dir(&host.bundle.bundle_dir);
        use std::process::Stdio;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let write = async {
            let _ = stdin.write_all(&raw).await;
            drop(stdin);
        };
        let ((), output) = tokio::join!(write, child.wait_with_output());
        let output = output.unwrap();
        assert!(!output.status.success());
        let envelope: Value = decode_json(&output.stdout).unwrap();
        assert_eq!(envelope["ok"], false);
        assert!(
            !envelope["error"]["message"]
                .as_str()
                .unwrap()
                .contains("user code ran")
        );
    }
}

#[tokio::test]
async fn approved_sibling_helper_and_previous_run_context_survive_retry() {
    let scratch = Scratch::new();
    let (mut host,invocation)=scratch.host("from sluice_fn import run,Transient\nimport sibling,os\ndef main(inp,ctx):\n    if ctx.attempt==1: raise Transient('busy')\n    return {'previous':ctx.prev_run,'env':os.environ['SLUICE_PREV_RUN'],'sibling':sibling.VALUE}\nrun(main,retries=1,backoff=0)\n",json!({"previous":"string","env":"string","sibling":"int"}));
    let previous = RunId::new();
    host.context.prev_run = Some(previous);
    let sibling = scratch.0.join("approved");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(sibling.join("sibling.py"), "VALUE=7\n").unwrap();
    host.bundle.sibling_helper_root = Some(sibling);
    let out = value(&execute(&host, &invocation).await.unwrap());
    assert_eq!(
        out,
        json!({"previous":previous.to_string(),"env":previous.to_string(),"sibling":7})
    );
}

#[tokio::test]
async fn section_timeout_and_exception_send_idempotent_release() {
    for waiting in [false, true] {
        let scratch = Scratch::new();
        let code = if waiting {
            "from sluice_fn import run\ndef main(inp,ctx):\n    with ctx.acquire('land',timeout=0): pass\nrun(main)\n"
        } else {
            "from sluice_fn import run,Rejected\ndef main(inp,ctx):\n    with ctx.acquire('land'): raise Rejected('no')\nrun(main)\n"
        };
        let (mut host, invocation) = scratch.host(code, json!({}));
        let released = Arc::new(Mutex::new(false));
        let seen = released.clone();
        let server = Server::new(scratch.0.join("control.sock"), move |request| match request
            .command
        {
            HelperCommand::Runtime(CommandRequest::AcquireLease(_)) => {
                json!({"reply":"lease","data":{"lease":1,"state":if waiting{"waiting"}else{"held"}}})
            }
            HelperCommand::Runtime(CommandRequest::ReleaseLease(request)) => {
                assert_eq!(request.lease, LeaseId(1));
                *seen.lock().unwrap() = true;
                json!({"reply":"ack"})
            }
            _ => panic!("unexpected"),
        });
        host.context.control_socket = Some(server.path.clone());
        assert!(execute(&host, &invocation).await.is_err());
        assert!(*released.lock().unwrap());
        server.close().await;
    }
}

#[tokio::test]
async fn stream_delivers_lines_before_exit_feeds_stdin_and_follows_a_log() {
    let scratch = Scratch::new();
    let (host,invocation)=scratch.host(r#"from sluice_fn import run,stream,ShError
import sys

def main(inp,ctx):
    seen=[]
    marker=ctx.run_dir/'seen'
    def on_line(line,source):
        seen.append([source,line])
        marker.touch()
    command=['sh','-c',f'echo first; echo err >&2; while [ ! -e "{marker}" ]; do sleep .01; done; cat; exit 4']
    try: stream(command,on_line,input='secret prompt\n')
    except ShError as error:
        assert error.code==4 and 'secret prompt' in error.stdout and 'err' in error.stderr
    file=ctx.run_dir/'tool.log'
    file.write_text('old line\n')
    stream([sys.executable,'-c',f'from pathlib import Path;Path({str(file)!r}).write_text("new\\nlast")'],on_line,follow=file)
    return {'seen':seen}
run(main)
"#,json!({"seen":"string[][]"}));
    let out = value(&execute(&host, &invocation).await.unwrap());
    let seen = out["seen"].as_array().unwrap();
    assert!(seen.contains(&json!(["stdout", "first"])));
    assert!(seen.contains(&json!(["stdout", "secret prompt"])));
    assert!(seen.contains(&json!(["stderr", "err"])));
    assert!(seen.contains(&json!(["follow", "new"])));
    assert!(seen.contains(&json!(["follow", "last"])));
    assert!(
        !std::fs::read_to_string(host.context.run_dir.join("stderr.log"))
            .unwrap()
            .contains("secret prompt")
    );
}
