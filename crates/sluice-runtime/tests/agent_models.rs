//! An agent step's `model` must be a JSON object. A string (the retired form) is refused by
//! the edit that adds the step or changes its model, with the launch's message, while a plan
//! that already holds one still loads and takes every edit that leaves that model alone.
//! Driven through the coordinator socket.
#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    error::PublicError,
    plan::FnSignature,
    rpc::{FnInvocation, JsonMap, decode_json},
    types::Type,
};
use sluice_process::guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence};
use sluice_runtime::{
    client::CoordinatorClient,
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

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

fn ty(form: Value) -> Type {
    Type::parse(&form).unwrap()
}
/// The fixture catalog with `agent.codex` and a user agent block shaped like lash's
/// `lash.worker` (open, its `model` typed `model_type`, passed on to `agent.run`).
fn catalog(model_type: Value) -> Catalog {
    let mut catalog = Catalog::fixtures();
    catalog.0.insert(
        "agent.codex".into(),
        FnSignature {
            inputs: [
                ("cwd".into(), ty(json!("string"))),
                ("spec".into(), ty(json!("string"))),
                ("model".into(), ty(json!("Any?"))),
            ]
            .into(),
            outputs: [("final".into(), ty(json!("string")))].into(),
            open: true,
            ..Default::default()
        },
    );
    catalog.0.insert(
        "lash.worker".into(),
        FnSignature {
            inputs: [
                (
                    "engine".into(),
                    ty(json!({"type":"enum","symbols":["devin","opus","codex"]})),
                ),
                ("cwd".into(), ty(json!("string"))),
                ("spec".into(), ty(json!("string"))),
                ("model".into(), ty(model_type)),
                ("ticket".into(), ty(json!("string?"))),
            ]
            .into(),
            outputs: [("final".into(), ty(json!("string")))].into(),
            open: true,
            ..Default::default()
        },
    );
    catalog
}

struct Served {
    client: CoordinatorClient,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
async fn serve(home: &home::ScratchHome, catalog: Catalog) -> Served {
    let broker = Coordinator::open(home.path().into(), catalog, Fake)
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let token = stop.clone();
    let server = tokio::spawn(async move { broker.serve(token).await });
    let client = CoordinatorClient::new(home.path());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while std::os::unix::net::UnixStream::connect(&client.path).is_err() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Served {
        client,
        stop,
        server,
    }
}
impl Served {
    async fn call(&self, name: &str, mut args: Value) -> Result<CommandReply, PublicError> {
        args["project"] = json!({"kind":"name","value":"p"});
        self.client.command(request(name, args)).await
    }
    async fn plan(&self) -> Value {
        let CommandReply::Data(v) = self.call("plan_get", json!({})).await.unwrap() else {
            panic!("plan")
        };
        v.into_value()
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}
fn request(name: &str, args: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap()
}
fn edit(reason: &str) -> Value {
    json!({"dry_run":false,"reason":reason})
}
async fn project(served: &Served) {
    served
        .client
        .command(request(
            "project_create",
            json!({"name":"p","description":"","resources":{}}),
        ))
        .await
        .unwrap();
}
/// The refusal's message, asserting it is `invalid`.
fn refused(reply: Result<CommandReply, PublicError>) -> (String, Vec<String>) {
    match reply {
        Err(PublicError::Invalid { message, errors }) => (message, errors),
        other => panic!("expected invalid, got {other:?}"),
    }
}
fn worker(model: Value) -> Value {
    json!({"run":"lash.worker","in":{
        "engine":{"default":"codex"},"cwd":{"default":"/tmp"},"spec":{"default":"s"},
        "model":model}})
}
const SOL: &str = r#"model must be a JSON object, not "sol"; use {"type":"normal","model":"sol","effort":"high"}"#;

/// A recipe shaped like lash's `study`: its `model` param is an enum of strings, bound to the
/// worker's model; the real object comes from unit_add's `inputs` override.
fn study(home: &home::ScratchHome) {
    let dir = home.path().join("recipes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("study.json"),
        serde_json::to_vec(&json!({
            "name":"study",
            "params":{
                "spec":"string",
                "model":["null",{"type":"enum","symbols":["sol","astra"]}],
                "engine":{"type":"enum","symbols":["devin","codex","opus"]}
            },
            "steps":{
                "{unit}-work":{"run":"lash.worker","tags":["study"],"in":{
                    "engine":{"default":"{engine}"},
                    "model":{"default":"{model}"},
                    "cwd":{"default":"/tmp"},
                    "spec":{"default":"{spec}"}}}
            }
        }))
        .unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn unit_add_refuses_a_string_model_and_takes_an_object_from_inputs() {
    let home = home::ScratchHome::new().unwrap();
    study(&home);
    let served = serve(&home, catalog(json!("Any?"))).await;
    project(&served).await;
    // params.model "sol" binds the worker's model to the string: refused at the edit.
    let (message, errors) = refused(
        served
            .call(
                "unit_add",
                json!({"recipe":"study","unit":"s1","params":{"spec":"x","engine":"codex","model":"sol"},
                       "after":{},"edit":edit("add")}),
            )
            .await,
    );
    assert_eq!(message, SOL);
    assert_eq!(errors, [format!("steps.s1-work.in.model: {SOL}")]);
    assert!(
        served.plan().await["plan"]["steps"]
            .get("s1-work")
            .is_none()
    );
    // A dry run is refused the same way.
    refused(
        served
            .call(
                "unit_add",
                json!({"recipe":"study","unit":"s1","params":{"spec":"x","engine":"codex","model":"sol"},
                       "after":{},"edit":{"dry_run":true,"reason":"add"}}),
            )
            .await,
    );
    // The recipe's string param is overridden by the object from inputs: the resolved
    // binding is what is checked, so this one is taken.
    let object = json!({"type":"normal","model":"astra","effort":"high"});
    served
        .call(
            "unit_add",
            json!({"recipe":"study","unit":"s2","params":{"spec":"x","engine":"codex","model":"astra"},
                   "inputs":{"work":{"model":object}},"after":{},"edit":edit("add")}),
        )
        .await
        .unwrap();
    assert_eq!(
        served.plan().await["plan"]["steps"]["s2-work"]["in"]["model"],
        json!({"default":object})
    );
    // No model at all (the null param) runs the engine default.
    served
        .call(
            "unit_add",
            json!({"recipe":"study","unit":"s3","params":{"spec":"x","engine":"devin","model":null},
                   "after":{},"edit":edit("add")}),
        )
        .await
        .unwrap();
    served.close().await;
}

#[tokio::test]
async fn step_add_step_set_input_and_plan_patch_refuse_a_string_model() {
    let home = home::ScratchHome::new().unwrap();
    let served = serve(&home, catalog(json!("Any?"))).await;
    project(&served).await;
    let (message, _) = refused(
        served
            .call(
                "step_add",
                json!({"step":"w","spec":worker(json!({"default":"sol"})),"edit":edit("add")}),
            )
            .await,
    );
    assert_eq!(message, SOL);
    // The built-in agents are agent fns too.
    let (message, _) = refused(
        served
            .call(
                "step_add",
                json!({"step":"c","spec":{"run":"agent.codex","in":{"cwd":{"default":"/tmp"},
                       "spec":{"default":"s"},"model":{"default":"sol"}}},"edit":edit("add")}),
            )
            .await,
    );
    assert_eq!(message, SOL);
    let rev = served.plan().await["rev"].clone();
    let (message, _) = refused(
        served
            .call(
                "plan_patch",
                json!({"rev":rev,"ops":[{"op":"add","path":"/steps/w","value":worker(json!({"default":"sol"}))}],
                       "dry_run":false,"reason":"patch"}),
            )
            .await,
    );
    assert_eq!(message, SOL);
    served
        .call(
            "step_add",
            json!({"step":"w","spec":worker(json!({"default":{"type":"normal","model":"sol"}})),"edit":edit("add")}),
        )
        .await
        .unwrap();
    let (message, _) = refused(
        served
            .call(
                "step_set_input",
                json!({"selection":{"steps":["w"],"tags":null},"inputs":{"model":"sol"},"edit":edit("set")}),
            )
            .await,
    );
    assert_eq!(message, SOL);
    // A model read from a plan input is checked with the input's value, both when the
    // step is added and when the input is set.
    let rev = served.plan().await["rev"].clone();
    served
        .call(
            "plan_patch",
            json!({"rev":rev,"ops":[{"op":"add","path":"/inputs","value":{"model":"Any"}}],
                   "dry_run":false,"reason":"input"}),
        )
        .await
        .unwrap();
    served
        .call(
            "plan_set_input",
            json!({"name":"model","value":"sol","edit":edit("set")}),
        )
        .await
        .unwrap();
    let (message, _) = refused(
        served
            .call(
                "step_add",
                json!({"step":"v","spec":worker(json!({"source":"model"})),"edit":edit("add")}),
            )
            .await,
    );
    assert_eq!(message, SOL);
    served
        .call(
            "plan_set_input",
            json!({"name":"model","value":{"type":"normal","model":"sol"},"edit":edit("set")}),
        )
        .await
        .unwrap();
    served
        .call(
            "step_add",
            json!({"step":"v","spec":worker(json!({"source":"model"})),"edit":edit("add")}),
        )
        .await
        .unwrap();
    let (message, errors) = refused(
        served
            .call(
                "plan_set_input",
                json!({"name":"model","value":"sol","edit":edit("set")}),
            )
            .await,
    );
    assert_eq!(message, SOL);
    assert_eq!(errors, [format!("steps.v.in.model: {SOL}")]);
    served.close().await;
}

/// A plan written before the check (here: while the worker's model was typed `string`) keeps
/// its string model: it loads, and every edit that leaves that model alone is taken.
#[tokio::test]
async fn a_plan_with_an_old_string_model_takes_edits_that_leave_it_alone() {
    let home = home::ScratchHome::new().unwrap();
    let served = serve(&home, catalog(json!("string?"))).await;
    project(&served).await;
    served
        .call(
            "step_add",
            json!({"step":"old","spec":worker(json!({"default":"sol"})),"start":false,"edit":edit("old")}),
        )
        .await
        .unwrap();
    served.close().await;

    let served = serve(&home, catalog(json!("Any?"))).await;
    assert_eq!(
        served.plan().await["plan"]["steps"]["old"]["in"]["model"],
        json!({"default":"sol"})
    );
    // Unrelated edits: another step, its edges, the old step's other inputs, tags and pause.
    served
        .call(
            "step_add",
            json!({"step":"other","spec":{"run":"fixture.echo","in":{"value":{"default":1}}},"edit":edit("add")}),
        )
        .await
        .unwrap();
    served
        .call(
            "edge_add",
            json!({"step":"other","after":["old"],"edit":edit("edge")}),
        )
        .await
        .unwrap();
    served
        .call(
            "step_update",
            json!({"step":"old","changes":{"doc":"still sol"},"edit":edit("doc")}),
        )
        .await
        .unwrap();
    served
        .call(
            "step_set_input",
            json!({"selection":{"steps":["old"],"tags":null},"inputs":{"ticket":"FIG-1"},"edit":edit("ticket")}),
        )
        .await
        .unwrap();
    served
        .call(
            "step_pause",
            json!({"selection":{"steps":["old"],"tags":null},"paused":true,"subtree":false,"edit":edit("pause")}),
        )
        .await
        .unwrap();
    // Changing the old step's model to another string is refused; to an object, taken.
    let mut spec = worker(json!({"default":"astra"}));
    let (message, _) = refused(
        served
            .call(
                "step_update",
                json!({"step":"old","changes":{"in":spec["in"].take()},"edit":edit("model")}),
            )
            .await,
    );
    assert!(
        message.starts_with(r#"model must be a JSON object, not "astra""#),
        "{message}"
    );
    let object = json!({"type":"normal","model":"sol","effort":"high"});
    let spec = worker(json!({"default":object}));
    served
        .call(
            "step_update",
            json!({"step":"old","changes":{"in":spec["in"]},"edit":edit("model")}),
        )
        .await
        .unwrap();
    assert_eq!(
        served.plan().await["plan"]["steps"]["old"]["in"]["model"],
        json!({"default":object})
    );
    served.close().await;
}
