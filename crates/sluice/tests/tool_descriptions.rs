//! Every MCP tool description that says what the tool returns ("Returns {a, b?, ...}" or
//! "Returns [{...}]") names keys a real reply has: each tool below is called through the MCP
//! server on a scratch home with a live coordinator, and every non-optional key its
//! description lists must be in the reply (in every element, for a list).
#[allow(dead_code)]
#[path = "../../../tests/support/mod.rs"]
mod support;

use serde_json::{Value, json};
use sluice_model::{error::PublicError, rpc::FnInvocation, rpc::JsonMap};
use sluice_process::guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence};
use sluice_runtime::{
    client::CoordinatorClient,
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use sluice_web::mcp::{McpServer, tools};
use std::{sync::Arc, time::Duration};
use support::home::ScratchHome;
use tokio_util::sync::CancellationToken;

/// Publishes the real fn registry and accepts every launch without ever completing it, so a
/// started step stays running.
#[derive(Clone)]
struct Fake;
impl FnHost for Fake {
    async fn invoke(&self, _: FnInvocation) -> Result<JsonMap, PublicError> {
        Ok(JsonMap::default())
    }
}
impl AdoptionHost for Fake {
    async fn reconcile(&self, _: &AdoptionAttempt) -> std::io::Result<GuardianPresence> {
        Ok(GuardianPresence::Ambiguous("fixture".into()))
    }
}
impl ExecutionHost for Fake {
    fn composition_enabled(&self) -> bool {
        true
    }
    async fn launch(&self, _: Launch) -> Result<LaunchOutcome, PublicError> {
        Ok(LaunchOutcome::Accepted)
    }
    async fn cleanup_valid(
        &self,
        _: &sluice_process::journal::CompletionJournal,
    ) -> Result<bool, PublicError> {
        Ok(true)
    }
}

/// The keys of the first `{...}` (or `[{...}]`) after "Returns" in a description: the
/// top-level names, each with whether it is optional (`name?`). `...` is skipped.
fn described(description: &str) -> Option<(bool, Vec<(String, bool)>)> {
    let at = description.find("Returns")?;
    let rest = &description[at..];
    let open = rest.find('{')?;
    if rest[..open].contains(". ") {
        return None;
    }
    let list = rest[..open].trim_end().ends_with('[');
    let mut depth = 0;
    let mut items = vec![String::new()];
    for c in rest[open..].chars() {
        match c {
            '{' | '[' => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            ',' if depth == 1 => {
                items.push(String::new());
                continue;
            }
            _ => {}
        }
        if depth >= 1 {
            items.last_mut().unwrap().push(c);
        }
    }
    let keys = items
        .iter()
        .filter_map(|item| {
            let item = item.trim();
            let name: String = item
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then(|| (name.clone(), item[name.len()..].starts_with('?')))
        })
        .collect();
    Some((list, keys))
}

struct Fixture {
    home: ScratchHome,
    mcp: McpServer,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
impl Fixture {
    async fn new() -> Self {
        let home = ScratchHome::new().unwrap();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
            .await
            .unwrap();
        broker.acquire_scheduler("test".into()).await.unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let server = tokio::spawn(async move { broker.serve(token).await });
        let client = CoordinatorClient::new(home.path());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !client.path.exists() {
            assert!(tokio::time::Instant::now() < deadline, "socket never bound");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Self {
            home,
            mcp: McpServer::new(Arc::new(client)),
            stop,
            server,
        }
    }
    /// The tool's MCP result as JSON (a string result, such as plan_view's, as a string).
    async fn call(&self, name: &str, args: Value) -> Value {
        let Value::Object(args) = args else {
            panic!("args")
        };
        let result = serde_json::to_value(self.mcp.call(name, args, Some("test")).await).unwrap();
        assert_ne!(result["isError"], true, "{name}: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap_or_else(|_| json!(text))
    }
    /// Calls the tool and checks the keys its description says it returns.
    async fn check(&self, name: &str, args: Value) -> Value {
        let tool = tools()
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("no tool {name}"));
        let description = tool.description.as_deref().unwrap_or_default();
        let (list, keys) =
            described(description).unwrap_or_else(|| panic!("{name} does not say what it returns"));
        let reply = self.call(name, args).await;
        let objects: Vec<&Value> = if list {
            let items = reply
                .as_array()
                .unwrap_or_else(|| panic!("{name}: {reply}"));
            assert!(!items.is_empty(), "{name}: an empty list proves nothing");
            items.iter().collect()
        } else {
            vec![&reply]
        };
        for object in objects {
            for (key, optional) in &keys {
                assert!(
                    *optional || object.get(key).is_some(),
                    "{name} describes `{key}`, which its reply lacks: {object}"
                );
            }
        }
        reply
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}

#[test]
fn described_keys_are_parsed() {
    assert_eq!(
        described("x. Returns the edit result {project, rev?, preview: {a, b}}: more"),
        Some((
            false,
            vec![
                ("project".into(), false),
                ("rev".into(), true),
                ("preview".into(), false)
            ]
        ))
    );
    assert_eq!(
        described("Returns [{seq, at, ...}]"),
        Some((true, vec![("seq".into(), false), ("at".into(), false)]))
    );
    assert_eq!(described("Returns nothing. {a}"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_descriptions_name_the_keys_their_replies_have() {
    let f = Fixture::new().await;
    f.call(
        "project_create",
        json!({"name":"p","description":"","resources":{}}),
    )
    .await;
    std::fs::create_dir_all(f.home.path().join("recipes")).unwrap();
    std::fs::write(
        f.home.path().join("recipes/lane.json"),
        json!({"name":"lane","doc":"one lane","params":{"x":"string"},
               "steps":{"{unit}-a":{"run":"fixture.echo","in":{"value":{"default":"{x}"}}}}})
        .to_string(),
    )
    .unwrap();
    // A directory of no project, so verify has something to report.
    std::fs::create_dir_all(f.home.path().join("projects/stray")).unwrap();
    f.check(
        "fn_save",
        json!({"fn":{"name":"custom.noop","doc":"does nothing","inputs":{},"outputs":{}},
               "main_py":"from sluice_fn import run\nrun(lambda inp, ctx: {})\n","project":"p"}),
    )
    .await;
    f.check("fn_list", json!({"project":"p"})).await;
    f.check("fn_get", json!({"name":"custom.noop","project":"p"}))
        .await;
    f.check("recipe_list", json!({"project":"p"})).await;
    let plan = f
        .check(
            "plan_patch",
            json!({"project":"p","rev":1,"reason":"the fixture plan","ops":[{"op":"replace","path":"","value":{
                "inputs":{"x":"int"},"outputs":{},
                "steps":{
                    "a":{"run":"fixture.echo","paused":true,"in":{"value":{"default":1}}},
                    "b":{"run":"fixture.echo","paused":true,"after":["a"],"in":{"value":{"default":2}}},
                    "ext":{"run":"core.external","outputs":{"done":"boolean"}},
                    "work":{"run":"fixture.submit","in":{"value":{"default":3}},
                            "outputs":{"note":"string"}}}}}]}),
        )
        .await;
    assert_eq!(plan["rev"], 2);
    f.check("plan_get", json!({"project":"p"})).await;
    f.check(
        "step_add",
        json!({"project":"p","step":"c","start":false,
               "spec":{"run":"fixture.echo","in":{"value":{"default":4}}}}),
    )
    .await;
    f.check("edge_add", json!({"project":"p","step":"c","after":["a"]}))
        .await;
    f.check(
        "edge_remove",
        json!({"project":"p","step":"c","after":["a"]}),
    )
    .await;
    f.check(
        "step_update",
        json!({"project":"p","step":"c","changes":{"doc":"the third"}}),
    )
    .await;
    f.check("step_remove", json!({"project":"p","steps":"c"}))
        .await;
    f.check(
        "step_set_output",
        json!({"project":"p","step":"a","outputs":{"value":1},"force":true,"reason":"by hand"}),
    )
    .await;
    f.check("step_retry", json!({"project":"p","steps":"a"}))
        .await;
    f.check(
        "plan_set_input",
        json!({"project":"p","name":"x","value":1}),
    )
    .await;
    f.check(
        "step_cancel",
        json!({"project":"p","steps":"ext","reason":"done elsewhere"}),
    )
    .await;
    // The open step was launched (and never completes): submit to its run.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let run = loop {
        let status = f
            .call("status", json!({"project":"p","steps":"work"}))
            .await;
        if status["steps"]["work"]["status"] == "running" {
            break status["steps"]["work"]["run_ids"][0].clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "{status}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    f.check(
        "step_submit",
        json!({"project":"p","step":"work","run":run,"outputs":{"note":"half done"}}),
    )
    .await;
    // The message verbs return their receipt; the run speaks as its step.
    let asked = f
        .check(
            "ask",
            json!({"project":"p","to":"orchestrator","body":"which?","run":run}),
        )
        .await;
    assert_eq!(asked["thread"], "step-work");
    f.check("say", json!({"project":"p","to":"work","body":"fyi"}))
        .await;
    f.check(
        "reply",
        json!({"project":"p","to_message":asked["id"],"body":"this one"}),
    )
    .await;
    f.check(
        "messages",
        json!({"project":"p","view":"thread","thread":"step-work"}),
    )
    .await;
    f.check("plan_history", json!({"project":"p"})).await;
    f.check("log_read", json!({"project":"p","limit":5})).await;
    f.check("log_wait", json!({"project":"p","since_seq":0,"timeout":1}))
        .await;
    f.check(
        "next",
        json!({"projects":"p","since_seq":0,"timeout":1,"settle":0}),
    )
    .await;
    let call = f
        .check(
            "fn_call",
            json!({"name":"fixture.echo","inputs":{"value":1},"project":"p"}),
        )
        .await;
    f.check("call_status", json!({"call":call["call"],"project":"p"}))
        .await;
    f.check("query", json!({"sql":"SELECT 1 AS n"})).await;
    f.check("verify", json!({})).await;
    f.check("drain", json!({"projects":"p"})).await;
    f.check("release", json!({})).await;
    f.close().await;
}
