//! Edit replies, no-op edits, project listing and icons and status wait reasons, each
//! driven through the coordinator socket.
#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Value, json};
use sluice_model::{
    commands::*,
    error::PublicError,
    ids::*,
    rpc::{FnInvocation, JsonMap, decode_json},
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

struct Fixture {
    home: home::ScratchHome,
    client: CoordinatorClient,
    project: ProjectId,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
impl Fixture {
    async fn new() -> Self {
        let home = home::ScratchHome::new().unwrap();
        home::ScratchHome::validate(home.path()).unwrap();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let token = stop.clone();
        let server = tokio::spawn(async move { broker.serve(token).await });
        let client = CoordinatorClient::new(home.path());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !client.path.exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let CommandReply::Project(p) = client
            .command(request(
                "project_create",
                json!({"name":"p","description":"","resources":{}}),
            ))
            .await
            .unwrap()
        else {
            panic!("project")
        };
        Self {
            home,
            client,
            project: p.project_id,
            stop,
            server,
        }
    }
    async fn call(&self, name: &str, mut args: Value) -> Result<CommandReply, PublicError> {
        args["project"] = json!({"kind":"id","value":self.project});
        self.client.command(request(name, args)).await
    }
    async fn plan(&self) -> Value {
        data(self.call("plan_get", json!({})).await.unwrap())
    }
    async fn rev(&self) -> u64 {
        self.plan().await["rev"].as_u64().unwrap()
    }
    async fn patch(&self, ops: Value) {
        let rev = self.rev().await;
        self.call(
            "plan_patch",
            json!({"rev":rev,"ops":ops,"dry_run":false,"reason":"fixture","start":true}),
        )
        .await
        .unwrap();
    }
    async fn history_len(&self) -> usize {
        data(self.call("plan_history", json!({})).await.unwrap())
            .as_array()
            .unwrap()
            .len()
    }
    async fn status(&self) -> Value {
        data(
            self.call(
                "status",
                json!({"selection":{"steps":null,"tags":null},"brief":false,"all":true,"view":"steps"}),
            )
            .await
            .unwrap(),
        )
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}
fn request(name: &str, args: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap()
}
fn data(reply: CommandReply) -> Value {
    let CommandReply::Data(v) = reply else {
        panic!("expected data, got {reply:?}")
    };
    v.into_value()
}
fn edit(reason: &str) -> Value {
    json!({"dry_run":false,"reason":reason})
}
fn steps(ids: &[&str]) -> Value {
    json!({"steps":ids,"tags":null})
}
fn edited(reply: CommandReply) -> EditResult {
    match reply {
        CommandReply::Edit(result) => result,
        other => panic!("expected an edit result, got {other:?}"),
    }
}
fn ids(values: &[StepId]) -> Vec<&str> {
    values.iter().map(StepId::as_str).collect()
}

const CHAIN: &str = r#"[
  {"op":"add","path":"/inputs","value":{"go":"boolean"}},
  {"op":"add","path":"/steps/a","value":{"run":"core.external","outputs":{"ok":"boolean"}}},
  {"op":"add","path":"/steps/b","value":{"run":"fixture.echo","in":{"value":{"source":"a/ok"}}}},
  {"op":"add","path":"/steps/c","value":{"run":"fixture.echo","in":{"value":{"default":1}},"after":["b"]}},
  {"op":"add","path":"/steps/d","value":{"run":"fixture.echo","in":{"value":{"default":2}},"after":["go"],"tags":["unit:lone"]}}
]"#;

#[tokio::test]
async fn edits_that_change_nothing_commit_no_revision_or_history() {
    let f = Fixture::new().await;
    f.patch(serde_json::from_str(CHAIN).unwrap()).await;
    let rev = f.rev().await;
    let history = f.history_len().await;
    let unchanged = |reply: CommandReply| {
        let result = edited(reply);
        assert_eq!(result.rev, Revision(rev));
        assert!(result.preview.ops.is_empty(), "{result:?}");
    };
    // An edge already there.
    unchanged(
        f.call(
            "edge_add",
            json!({"step":"c","after":["b"],"edit":edit("again")}),
        )
        .await
        .unwrap(),
    );
    // An edge not there.
    unchanged(
        f.call(
            "edge_remove",
            json!({"step":"c","after":["a"],"edit":edit("none")}),
        )
        .await
        .unwrap(),
    );
    // Tags already on the unit, and one not on it to remove.
    unchanged(
        f.call(
            "unit_tag",
            json!({"unit":"lone","add":[],"remove":["absent"],"edit":edit("same")}),
        )
        .await
        .unwrap(),
    );
    // Unpausing steps that are not paused.
    unchanged(
        f.call(
            "step_pause",
            json!({"selection":steps(&["a","b"]),"paused":false,"edit":edit("noop")}),
        )
        .await
        .unwrap(),
    );
    // A patch whose result is the plan as it is.
    unchanged(
        f.call("plan_patch", json!({"rev":rev,"ops":[{"op":"replace","path":"/steps/c/after","value":["b"]}],"dry_run":false,"reason":"same","start":true}))
            .await
            .unwrap(),
    );
    // A prune with nothing done.
    let CommandReply::Pruned(pruned) = f
        .call(
            "plan_prune",
            json!({"older_than_seconds":0,"edit":edit("nothing")}),
        )
        .await
        .unwrap()
    else {
        panic!("prune reply")
    };
    assert_eq!(pruned.edit.rev, Revision(rev));
    assert!(pruned.units.is_empty() && pruned.edit.preview.ops.is_empty());
    // step_set_input still refuses an edit that changes nothing.
    let refused = f
        .call(
            "step_set_input",
            json!({"selection":steps(&["c"]),"inputs":{"value":1},"edit":edit("same")}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, PublicError::BadRequest { .. }),
        "{refused:?}"
    );
    assert_eq!(f.rev().await, rev);
    assert_eq!(f.history_len().await, history);
    // A real change still commits.
    let changed = edited(
        f.call(
            "edge_add",
            json!({"step":"d","after":["a"],"edit":edit("real")}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(changed.rev, Revision(rev + 1));
    assert_eq!(f.history_len().await, history + 1);
    f.close().await;
}

#[tokio::test]
async fn step_pause_keeps_its_reason_and_covers_the_subtree() {
    let f = Fixture::new().await;
    f.patch(serde_json::from_str(CHAIN).unwrap()).await;
    let rev = f.rev().await;
    let result = edited(
        f.call(
            "step_pause",
            json!({"selection":steps(&["a"]),"subtree":true,"paused":true,"edit":edit("waiting on review")}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(result.rev, Revision(rev + 1));
    assert_eq!(ids(result.steps.as_deref().unwrap()), ["a", "b", "c"]);
    let plan = f.plan().await;
    for id in ["a", "b", "c"] {
        assert_eq!(plan["plan"]["steps"][id]["paused"], "waiting on review");
    }
    assert!(plan["plan"]["steps"]["d"].get("paused").is_none());
    let status = f.status().await;
    assert_eq!(status["steps"]["b"]["paused"], "waiting on review");
    assert_eq!(
        status["steps"]["b"]["waiting"][0],
        "paused: waiting on review"
    );
    assert!(status["steps"]["d"].get("paused").is_none());
    // Pausing again without a reason keeps each step's own: no edit.
    let again = edited(
        f.call(
            "step_pause",
            json!({"selection":steps(&["b"]),"subtree":true,"paused":true,"edit":edit("")}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(again.rev, Revision(rev + 1));
    assert_eq!(ids(again.steps.as_deref().unwrap()), ["b", "c"]);
    // Without a reason a step that was not paused gets true.
    f.call(
        "step_pause",
        json!({"selection":steps(&["d"]),"paused":true,"edit":edit("")}),
    )
    .await
    .unwrap();
    assert_eq!(f.plan().await["plan"]["steps"]["d"]["paused"], true);
    assert_eq!(f.status().await["steps"]["d"]["paused"], true);
    // Unpausing the subtree removes every pause in it.
    let released = edited(
        f.call(
            "step_pause",
            json!({"selection":steps(&["b"]),"subtree":true,"paused":false,"edit":edit("go")}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(ids(released.steps.as_deref().unwrap()), ["b", "c"]);
    let plan = f.plan().await;
    assert_eq!(plan["plan"]["steps"]["a"]["paused"], "waiting on review");
    assert!(plan["plan"]["steps"]["b"].get("paused").is_none());
    assert!(plan["plan"]["steps"]["c"].get("paused").is_none());
    f.close().await;
}

#[tokio::test]
async fn step_set_input_reports_changed_and_unsupported_steps() {
    let f = Fixture::new().await;
    f.patch(serde_json::from_str(CHAIN).unwrap()).await;
    let rev = f.rev().await;
    let CommandReply::Inputs(result) = f
        .call(
            "step_set_input",
            json!({"selection":steps(&["a","c","d"]),"inputs":{"value":5},"edit":edit("bump")}),
        )
        .await
        .unwrap()
    else {
        panic!("inputs reply")
    };
    assert_eq!(result.edit.rev, Revision(rev + 1));
    assert_eq!(ids(&result.changed), ["c", "d"]);
    assert!(result.running.is_empty());
    assert_eq!(result.unsupported.len(), 1);
    assert_eq!(result.unsupported[0].step.as_str(), "a");
    assert_eq!(result.unsupported[0].inputs, ["value"]);
    // On the wire the edit result's fields sit beside the report.
    let wire = serde_json::to_value(CommandReply::Inputs(result)).unwrap();
    assert_eq!(wire["data"]["rev"], rev + 1);
    assert_eq!(wire["data"]["changed"], json!(["c", "d"]));
    f.close().await;
}

#[tokio::test]
async fn prune_reports_removed_units_and_each_kept_unit_with_its_holder() {
    let f = Fixture::new().await;
    f.patch(json!([
        {"op":"add","path":"/steps/old","value":{"run":"core.external","outputs":{"ok":"boolean"}}},
        {"op":"add","path":"/steps/held","value":{"run":"core.external","outputs":{"ok":"boolean"}}},
        {"op":"add","path":"/steps/out","value":{"run":"core.external","outputs":{"ok":"boolean"}}},
        {"op":"add","path":"/steps/reader","value":{"run":"fixture.echo","in":{"value":{"source":"held/ok"}}}},
        {"op":"add","path":"/outputs","value":{"final":{"source":"out/ok"}}}
    ]))
    .await;
    for step in ["old", "held", "out"] {
        f.call(
            "step_set_output",
            json!({"step":step,"outputs":{"ok":true},"force":true,"reason":"done"}),
        )
        .await
        .unwrap();
    }
    let rev = f.rev().await;
    let CommandReply::Pruned(result) = f
        .call(
            "plan_prune",
            json!({"older_than_seconds":0,"edit":edit("tidy")}),
        )
        .await
        .unwrap()
    else {
        panic!("prune reply")
    };
    assert_eq!(result.edit.rev, Revision(rev + 1));
    assert_eq!(
        result
            .units
            .iter()
            .map(UnitName::as_str)
            .collect::<Vec<_>>(),
        ["old"]
    );
    assert_eq!(ids(result.edit.steps.as_deref().unwrap()), ["old"]);
    assert_eq!(
        serde_json::to_value(&result.kept).unwrap(),
        json!([{"unit":"held","step":"reader"},{"unit":"out","output":"final"}])
    );
    f.close().await;
}

#[tokio::test]
async fn unit_add_and_unit_tag_report_the_units_steps() {
    let f = Fixture::new().await;
    let dir = f.home.path().join("recipes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("lane.json"),
        serde_json::to_vec(&json!({"name":"lane","steps":{
            "{unit}-work":{"run":"fixture.echo","in":{"value":{"default":1}}},
            "{unit}-land":{"run":"fixture.echo","in":{"value":{"source":"{unit}-work/value"}}}
        }}))
        .unwrap(),
    )
    .unwrap();
    let added = edited(
        f.call(
            "unit_add",
            json!({"recipe":"lane","unit":"x","params":{},"after":{},"start":false,"edit":edit("add")}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(ids(added.steps.as_deref().unwrap()), ["x-work", "x-land"]);
    let tagged = edited(
        f.call(
            "unit_tag",
            json!({"unit":"x","add":["arc:a"],"remove":[],"edit":edit("arc")}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(tagged.rev, Revision(added.rev.0 + 1));
    assert_eq!(ids(tagged.steps.as_deref().unwrap()), ["x-work", "x-land"]);
    let again = edited(
        f.call(
            "unit_tag",
            json!({"unit":"x","add":["arc:a"],"remove":[],"edit":edit("arc")}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(again.rev, tagged.rev);
    assert_eq!(ids(again.steps.as_deref().unwrap()), ["x-work", "x-land"]);
    f.close().await;
}

/// The smallest PNG signature the icon sniffer accepts.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

#[tokio::test]
async fn projects_list_reports_settings_and_icons_from_a_path() {
    let f = Fixture::new().await;
    let icon = f.home.root().join("icon.png");
    std::fs::write(&icon, PNG).unwrap();
    f.patch(json!([
        {"op":"add","path":"/steps/a","value":{"run":"core.external","outputs":{"ok":"boolean"}}},
        {"op":"add","path":"/steps/b","value":{"run":"core.external","outputs":{"ok":"boolean"}}}
    ]))
    .await;
    f.call(
        "step_set_output",
        json!({"step":"a","outputs":{"ok":true},"force":true,"reason":"done"}),
    )
    .await
    .unwrap();
    f.client
        .command(request(
            "project_create",
            json!({"name":"pictured","description":"has an image","icon":icon,"resources":{"lane":2}}),
        ))
        .await
        .unwrap();
    f.call(
        "project_update",
        json!({"icon":"🧪","description":"text icon"}),
    )
    .await
    .unwrap();
    let missing = f
        .client
        .command(request(
            "project_create",
            json!({"name":"nopicture","description":"","icon":"/nonexistent/icon.png","resources":{}}),
        ))
        .await
        .unwrap_err();
    assert!(
        missing.to_string().contains("no readable file"),
        "{missing:?}"
    );
    let CommandReply::Projects(list) = f
        .client
        .command(CommandRequest::ProjectsList)
        .await
        .unwrap()
    else {
        panic!("projects")
    };
    let list = serde_json::to_value(&list).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2);
    let p = &list[0];
    assert_eq!(p["name"], "p");
    assert_eq!(p["description"], "text icon");
    assert_eq!(p["icon"], json!({"kind":"text","text":"🧪"}));
    assert_eq!(p["counts"], json!({"pending":1,"succeeded":1}));
    assert_eq!(p["rev"], f.rev().await);
    assert_eq!(p["paused"], false);
    assert_eq!(p["archived"], false);
    assert!(p.get("resources").is_none());
    let pictured = &list[1];
    assert_eq!(pictured["icon"], json!({"kind":"image","type":"image/png"}));
    assert_eq!(pictured["resources"], json!({"lane":{"capacity":2}}));
    // An image path on update too, and the listed settings_rev deletes the project.
    let id = pictured["project_id"].as_str().unwrap().to_owned();
    let selector = json!({"kind":"id","value":id});
    f.client
        .command(request(
            "project_update",
            json!({"project":selector,"icon":icon,"archived":true}),
        ))
        .await
        .unwrap();
    let CommandReply::Projects(list) = f
        .client
        .command(CommandRequest::ProjectsList)
        .await
        .unwrap()
    else {
        panic!("projects")
    };
    let pictured = list.iter().find(|p| p.name.as_str() == "pictured").unwrap();
    assert!(pictured.archived);
    let stale = f
        .client
        .command(request(
            "project_delete",
            json!({"project":selector,"confirm_name":"pictured","expected_settings_rev":pictured.settings_rev.0 - 1}),
        ))
        .await
        .unwrap_err();
    assert!(matches!(stale, PublicError::Conflict { .. }), "{stale:?}");
    let deleted = f
        .client
        .command(request(
            "project_delete",
            json!({"project":selector,"confirm_name":"pictured","expected_settings_rev":pictured.settings_rev}),
        ))
        .await
        .unwrap();
    assert!(matches!(
        deleted,
        CommandReply::Deleted { deleted: true, .. }
    ));
    f.close().await;
}

#[tokio::test]
async fn status_says_why_every_pending_step_waits() {
    let f = Fixture::new().await;
    f.patch(json!([
        {"op":"add","path":"/inputs","value":{"go":"boolean","n":"int"}},
        {"op":"add","path":"/steps/ext","value":{"run":"core.external","outputs":{"ok":"boolean"}}},
        {"op":"add","path":"/steps/reads","value":{"run":"fixture.echo","in":{"value":{"source":"ext/ok"}}}},
        {"op":"add","path":"/steps/gated","value":{"run":"fixture.echo","in":{"value":{"source":"n"}},"after":["go"]}},
        {"op":"add","path":"/steps/held","value":{"run":"fixture.echo","in":{"value":{"default":1}},"paused":"by hand"}},
        {"op":"add","path":"/steps/free","value":{"run":"fixture.echo","in":{"value":{"default":1}}}}
    ]))
    .await;
    let status = f.status().await;
    let waiting = |step: &str| status["steps"][step]["waiting"].clone();
    assert_eq!(
        waiting("ext"),
        json!(["external: set its outputs with step_set_output"])
    );
    assert_eq!(waiting("reads"), json!(["step ext is pending"]));
    assert_eq!(
        waiting("gated"),
        json!([
            "plan input n has no value",
            "after go (plan input go has no value)"
        ])
    );
    assert_eq!(waiting("held"), json!(["paused: by hand"]));
    assert_eq!(status["steps"]["held"]["paused"], "by hand");
    assert!(status["steps"]["free"].get("waiting").is_none());
    f.call(
        "project_update",
        json!({"paused":true,"reason":"maintenance"}),
    )
    .await
    .unwrap();
    let status = f.status().await;
    assert_eq!(
        status["steps"]["free"]["waiting"],
        json!(["project paused"])
    );
    assert_eq!(
        status["steps"]["held"]["waiting"],
        json!(["paused: by hand", "project paused"])
    );
    f.close().await;
}
