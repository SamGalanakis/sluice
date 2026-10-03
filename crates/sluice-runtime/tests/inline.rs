use serde_json::{Value, json};
use sluice_model::{
    ids::*,
    rpc::{FnInvocation, JsonMap, decode_json},
};
use sluice_runtime::{
    inline::{inline_signature, invoke_inline},
    python::*,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = sluice_process::host::guard_scratch_home(
            &std::env::temp_dir().join(format!("sluice-test-inline-{}", RunId::new())),
        )
        .unwrap();
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn setup(
        &self,
        name: &str,
        inputs: Value,
        extra: Value,
        outputs: Value,
    ) -> (PythonHost, FnInvocation) {
        let returns = serde_json::to_value(&inline_signature(name).unwrap().outputs).unwrap();
        let release = self.0.join("helper");
        std::fs::create_dir_all(release.join("sluice_fn")).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
        std::fs::copy(
            source.join("inline_python.py"),
            release.join("inline_python.py"),
        )
        .unwrap();
        std::fs::copy(
            source.join("sluice_fn/__init__.py"),
            release.join("sluice_fn/__init__.py"),
        )
        .unwrap();
        let mut config = PythonConfig::new(release, self.0.join("sluice"));
        config
            .environment
            .remove(std::ffi::OsStr::new("PYTHONPATH"));
        config
            .environment
            .remove(std::ffi::OsStr::new("VIRTUAL_ENV"));
        let host = PythonHost {
            config,
            bundle: PinnedPythonFn {
                bundle_dir: self.0.clone(),
                sibling_helper_root: None,
            },
            context: PythonContext {
                home: self.0.clone(),
                run_dir: self.0.clone(),
                project: "p".into(),
                prev_run: None,
                extra_inputs: map(extra),
                outputs: map(outputs),
                returns: map(returns),
                control_socket: None,
                run_capability: None,
            },
            cancellation: CancellationToken::new(),
        };
        let invocation = FnInvocation {
            project: ProjectId::new(),
            step: Some("inline".parse().unwrap()),
            run: RunId::new(),
            attempt: AttemptId::new(),
            invocation: InvocationId::new(),
            name: name.into(),
            inputs: map(inputs),
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
async fn run(host: &PythonHost, invocation: &FnInvocation) -> Result<Value, PythonError> {
    tokio::time::timeout(Duration::from_secs(30), invoke_inline(host, invocation))
        .await
        .unwrap()
        .map(|o| serde_json::to_value(o).unwrap())
}

#[tokio::test]
async fn bash_exports_extra_inputs_and_captures_stdout_stderr_cwd_and_exit() {
    let scratch = Scratch::new();
    let (host,invocation)=scratch.setup("inline.bash",json!({"code":"echo \"$name-$count-$my_list\"; echo oops >&2; pwd","name":"x y","count":3,"my-list":[1,2],"cwd":scratch.0.to_str()}),json!({"name":"string","count":"int","my-list":"int[]"}),json!({}));
    let out = run(&host, &invocation).await.unwrap();
    assert_eq!(
        out,
        json!({"stdout":format!("x y-3-[1,2]\n{}\n",scratch.0.display()),"stderr":"oops\n","code":0})
    );
}
#[tokio::test]
async fn bash_null_unsets_host_variable_pipefail_stops_and_check_false_keeps_exit() {
    let scratch = Scratch::new();
    let (mut host, invocation) = scratch.setup(
        "inline.bash",
        json!({"code":"echo seen${maybe+x}end","maybe":null}),
        json!({"maybe":"Any?"}),
        json!({}),
    );
    host.config
        .environment
        .insert("maybe".into(), "ambient".into());
    assert_eq!(
        run(&host, &invocation).await.unwrap()["stdout"],
        "seenend\n"
    );
    let (host, invocation) = scratch.setup(
        "inline.bash",
        json!({"code":"echo hi | false; echo unreachable"}),
        json!({}),
        json!({}),
    );
    assert!(matches!(
        run(&host, &invocation).await,
        Err(PythonError::Exit(_))
    ));
    let (host, invocation) = scratch.setup(
        "inline.bash",
        json!({"code":"exit 7","check":false}),
        json!({}),
        json!({}),
    );
    assert_eq!(run(&host, &invocation).await.unwrap()["code"], 7);
}
#[tokio::test]
async fn bash_refuses_variable_collisions_and_reserved_out() {
    let scratch = Scratch::new();
    for extra in [
        json!({"a-b":"int","a_b":"int"}),
        json!({"OUT":"string"}),
        json!({"1a":"int"}),
    ] {
        let (host, invocation) =
            scratch.setup("inline.bash", json!({"code":"true"}), extra, json!({}));
        assert!(matches!(
            run(&host, &invocation).await,
            Err(PythonError::Protocol(_))
        ));
    }
}
#[tokio::test]
async fn bash_out_file_declared_fields_are_typed_and_stale_file_is_removed() {
    let scratch = Scratch::new();
    let (host, invocation) = scratch.setup(
        "inline.bash",
        json!({"code":"printf '{\"sha\":\"abc\",\"n\":2,\"other\":1}' > \"$OUT\""}),
        json!({}),
        json!({"sha":"string","n":"int"}),
    );
    let out = run(&host, &invocation).await.unwrap();
    assert_eq!(out["sha"], "abc");
    assert_eq!(out["n"], 2);
    assert!(out.get("other").is_none());
    for code in [
        "true",
        "echo '{\"sha\":5,\"n\":2}' > \"$OUT\"",
        "echo '[1]' > \"$OUT\"",
    ] {
        let (host, invocation) = scratch.setup(
            "inline.bash",
            json!({"code":code}),
            json!({}),
            json!({"sha":"string","n":"int"}),
        );
        assert!(run(&host, &invocation).await.is_err());
    }
}
#[tokio::test]
async fn python_sees_variables_inp_ctx_and_out_and_captures_prints() {
    let scratch = Scratch::new();
    let (host,invocation)=scratch.setup("inline.python",json!({"code":"import os\nprint('cwd',os.getcwd())\nout = [n*2 for n in nums]","nums":[1,2,3],"cwd":scratch.0.to_str()}),json!({"nums":"int[]"}),json!({}));
    assert_eq!(
        run(&host, &invocation).await.unwrap(),
        json!({"value":[2,4,6],"stdout":format!("cwd {}\n",scratch.0.display())})
    );
    let log = std::fs::read_to_string(scratch.0.join("stderr.log")).unwrap();
    assert!(log.contains("cwd"));
    assert!(!log.contains("\"ok\""));
    let (host,invocation)=scratch.setup("inline.python",json!({"code":"out={'total':sum(inp['nums']),'label':who+'!','run':ctx.run_id}","nums":[4,5],"who":"sam"}),json!({"nums":"int[]","who":"string"}),json!({"total":"int","label":"string"}));
    let out = run(&host, &invocation).await.unwrap();
    assert_eq!(out["total"], 9);
    assert_eq!(out["label"], "sam!");
    assert_eq!(out["value"]["run"], invocation.run.to_string());
}
#[tokio::test]
async fn python_missing_ill_typed_outputs_and_scope_collisions_fail() {
    let scratch = Scratch::new();
    for code in ["out=3", "out={'total':'bad'}", "raise ValueError('nope')"] {
        let (host, invocation) = scratch.setup(
            "inline.python",
            json!({"code":code}),
            json!({}),
            json!({"total":"int"}),
        );
        assert!(run(&host, &invocation).await.is_err());
    }
    for extra in [
        json!({"inp":"Any"}),
        json!({"out":"Any"}),
        json!({"a-b":"int","a_b":"int"}),
    ] {
        let (host, invocation) =
            scratch.setup("inline.python", json!({"code":"out=1"}), extra, json!({}));
        assert!(run(&host, &invocation).await.is_err());
    }
}
#[tokio::test]
async fn inline_tools_use_the_host_interpreter_and_clean_environment() {
    let scratch = Scratch::new();
    let (host,invocation)=scratch.setup("inline.python",json!({"code":"import subprocess,os\nout={'prefix':subprocess.run(['python3','-c','import sys;print(sys.prefix)'],capture_output=True,text=True).stdout.strip(), 'venv':os.environ.get('VIRTUAL_ENV'),'pp':os.environ.get('PYTHONPATH')}"}),json!({}),json!({}));
    let out = run(&host, &invocation).await.unwrap();
    assert_eq!(out["value"]["venv"], Value::Null);
    assert_eq!(out["value"]["pp"], Value::Null);
    assert!(
        !out["value"]["prefix"]
            .as_str()
            .unwrap()
            .contains("environments-v2")
    );
    let (host, invocation) = scratch.setup(
        "inline.bash",
        json!({"code":"python3 -c 'import sys;print(sys.prefix)'"}),
        json!({}),
        json!({}),
    );
    let bash = run(&host, &invocation).await.unwrap();
    assert_eq!(
        bash["stdout"].as_str().unwrap().trim(),
        out["value"]["prefix"]
    );
}
#[tokio::test]
async fn bash_stderr_result_is_complete_even_when_artifact_is_clamped() {
    let scratch = Scratch::new();
    let (host, invocation) = scratch.setup(
        "inline.bash",
        json!({"code":"python3 -c 'import sys;sys.stderr.write(\"x\"*1200000)'"}),
        json!({}),
        json!({}),
    );
    let result = run(&host, &invocation).await.unwrap();
    assert_eq!(result["stderr"].as_str().unwrap().len(), 1200000);
    assert_eq!(
        std::fs::metadata(scratch.0.join("stderr.log"))
            .unwrap()
            .len(),
        STDERR_ARTIFACT_BYTES as u64
    );
}
