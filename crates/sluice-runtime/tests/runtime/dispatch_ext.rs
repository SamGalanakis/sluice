use super::home;
use serde_json::{Value, json};
use sluice_model::{
    RuntimeApi,
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
use sluice_store::{RetrySafety, records};
use std::{collections::BTreeSet, time::Duration};
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
    broker: Coordinator<Fake>,
    client: CoordinatorClient,
    project: ProjectId,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<Result<(), PublicError>>,
}
impl Fixture {
    async fn next_project(&mut self) {
        let CommandReply::Project(p) = self
            .client
            .command(request(
                "project_create",
                json!({"name":format!("case-{}",ProjectId::new()),"description":"","resources":{}}),
            ))
            .await
            .unwrap()
        else {
            panic!("project")
        };
        self.project = p.project_id;
        self.patch(json!([
            {"op":"input.put","name":"value","declaration":"int"},
            {"op":"input.put","name":"enabled","declaration":"boolean"},
            {"op":"step.add","step":"pre","spec":{"run":"core.external","outputs":{"ok":"boolean"},"tags":["unit:pre","exit"]}},
            {"op":"step.add","step":"work","spec":{"run":"fixture.submit","in":{"value":{"source":"value"}},"after":["enabled"],"outputs":{"ready":"boolean"}}}
        ])).await;
    }

    async fn new() -> Self {
        let home = home::ScratchHome::new().unwrap();
        assert!(home.root().exists());
        home::ScratchHome::validate(home.path()).unwrap();
        let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
            .await
            .unwrap();
        let stop = CancellationToken::new();
        let cloned = broker.clone();
        let token = stop.clone();
        let server = tokio::spawn(async move { cloned.serve(token).await });
        let client = CoordinatorClient::new(home.path());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !client.path.exists() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::task::yield_now().await;
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
        let f = Self {
            home,
            broker,
            client,
            project: p.project_id,
            stop,
            server,
        };
        f.patch(json!([
            {"op":"input.put","name":"value","declaration":"int"},
            {"op":"input.put","name":"enabled","declaration":"boolean"},
            {"op":"step.add","step":"pre","spec":{"run":"core.external","outputs":{"ok":"boolean"},"tags":["unit:pre","exit"]}},
            {"op":"step.add","step":"work","spec":{"run":"fixture.submit","in":{"value":{"source":"value"}},"after":["enabled"],"outputs":{"ready":"boolean"}}}
        ])).await;
        f
    }
    fn selector(&self) -> Value {
        json!({"kind":"id","value":self.project})
    }
    /// A tool call in the project; a message neither the owner's nor a run's is the session
    /// `@orch`'s.
    async fn call(&self, name: &str, mut args: Value) -> Result<CommandReply, PublicError> {
        if name != "query" {
            args["project"] = self.selector();
        }
        if matches!(name, "ask" | "say" | "reply")
            && args.get("owner").is_none()
            && args.get("run").is_none()
            && args.get("me").is_none()
        {
            args["me"] = json!("@orch");
        }
        self.client.command(request(name, args)).await
    }
    async fn rev(&self) -> u64 {
        data(self.call("plan_get", json!({})).await.unwrap())["rev"]
            .as_u64()
            .unwrap()
    }
    async fn patch(&self, ops: Value) {
        self.call(
            "plan_edit",
            json!({"rev":self.rev().await,"ops":ops,"reason":"fixture"}),
        )
        .await
        .unwrap();
    }
    async fn input(&self, name: &str, value: Value, dry_run: bool) -> CommandReply {
        self.call(
            "plan_set_input",
            json!({"name":name,"value":value,"edit":options(dry_run)}),
        )
        .await
        .unwrap()
    }
    fn recipe(&self, scope: &str, name: &str, value: Value) {
        let root = if scope == "project" {
            self.home
                .path()
                .join("projects")
                .join(self.project.to_string())
        } else {
            self.home.path().to_owned()
        };
        let dir = root.join("recipes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.json")),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}
fn request(name: &str, args: Value) -> CommandRequest {
    decode_json(&serde_json::to_vec(&json!({"command":name,"args":args})).unwrap()).unwrap()
}
/// A reply's object, whatever its variant (`{"reply": …, "data": …}` on the wire).
fn data(reply: CommandReply) -> Value {
    serde_json::to_value(&reply).unwrap()["data"].take()
}
fn options(dry_run: bool) -> Value {
    json!({"dry_run":dry_run,"reason":"test"})
}

fn recipe(doc: &str) -> Value {
    json!({"name":"lane","doc":doc,"params":{"base":{"type":"int","doc":"number"}},"steps":{
        "{unit}-work":{"run":"fixture.echo","in":{"value":{"default":"{base}"}},"tags":["worker"]},
        "{unit}-land":{"run":"fixture.echo","in":{"value":{"source":"{unit}-work/value"}},"after":["{unit}-work"],"tags":["exit","{unit}-landed"]},
        "{unit}-rm":{"run":"core.external","outputs":{"done":"boolean"},"after":["{unit}-land?"]}
    }})
}

#[tokio::test]
async fn recipe_expansion_precedence_tags_edges_inputs_and_atomic_failure_over_socket() {
    let f = Fixture::new().await;
    f.recipe("global", "lane", recipe("global"));
    f.recipe("project", "lane", recipe("project"));
    f.recipe(
        "global",
        "broken",
        json!({"name":"broken","steps":{"x":{"run":"core.external","outputs":{"ok":"boolean"}}}}),
    );
    f.recipe(
        "project",
        "broken",
        json!({"name":"broken","steps":{"x":{"when":"enabled"}}}),
    );
    let recipes = data(f.call("recipe_list", json!({})).await.unwrap());
    assert_eq!(recipes[0]["name"], "broken");
    assert!(recipes[0]["error"].as_str().unwrap().contains("when"));
    assert_eq!(recipes[1]["scope"], "project");
    assert_eq!(recipes[1]["doc"], "project");
    assert_eq!(
        recipes[1]["params"],
        json!({"unit":"string","base":{"type":"int","doc":"number"}})
    );
    let recipe_root = f.home.path().join("recipes");
    for (name, bytes) in [
        (
            "duplicate",
            br#"{"name":"duplicate","name":"duplicate","steps":{}}"#.as_slice(),
        ),
        ("array", b"[]".as_slice()),
        (
            "overflow",
            br#"{"name":"overflow","steps":{"a":{"n":9223372036854775808}}}"#.as_slice(),
        ),
        ("invalid", b"{".as_slice()),
    ] {
        std::fs::write(recipe_root.join(format!("{name}.json")), bytes).unwrap();
    }
    let catalog = data(f.call("recipe_list", json!({})).await.unwrap());
    for name in ["duplicate", "array", "overflow", "invalid"] {
        assert!(
            catalog
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["name"] == name)
                .unwrap()["error"]
                .is_string()
        );
    }
    assert!(
        catalog
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "lane" && entry["doc"] == "project")
    );
    let before_errors = data(f.call("plan_get", json!({})).await.unwrap());
    for (unit, params, expected) in [
        ("bad", json!({}), "base"),
        ("bad", json!({"base":"no"}), "base"),
        ("bad", json!({"base":1,"extra":2}), "extra"),
    ] {
        let error = f.call("unit_add", json!({"recipe":"lane","unit":unit,"params":params,"after":{},"edit":options(false)})).await.unwrap_err();
        assert!(matches!(error, PublicError::Invalid { .. }), "{error:?}");
        let PublicError::Invalid { errors, .. } = &error else {
            unreachable!()
        };
        assert!(
            errors.iter().any(|path| path.contains(expected)),
            "{error:?}"
        );
        assert_eq!(
            data(f.call("plan_get", json!({})).await.unwrap()),
            before_errors
        );
    }
    let args = json!({"recipe":"lane","unit":"lane","params":{"base":7},"start":false,"after":{"*":["unit:pre"],"land":["pre/ok"]},"tags":["delivery"],"inputs":{"land":{"value":9}},"edit":options(false)});
    let before = f.rev().await;
    let result = f.call("unit_add", args).await.unwrap();
    assert!(matches!(result, CommandReply::Edit(_)));
    let plan = data(f.call("plan_get", json!({})).await.unwrap());
    assert_eq!(plan["rev"], before + 1);
    assert_eq!(
        plan["plan"]["steps"]["lane-work"],
        json!({"run":"fixture.echo","in":{"value":{"default":7}},"tags":["unit:lane","worker","delivery"],"paused":true,"after":["unit:pre"]})
    );
    assert_eq!(
        plan["plan"]["steps"]["lane-land"],
        json!({"run":"fixture.echo","in":{"value":{"default":9}},"after":["lane-work","pre/ok"],"tags":["unit:lane","exit","lane-landed","delivery"],"paused":true})
    );
    assert_eq!(
        plan["plan"]["steps"]["lane-rm"]["after"],
        json!(["lane-land?"])
    );
    let error = f
        .call(
            "unit_add",
            json!({"recipe":"broken","unit":"bad","params":{},"after":{},"edit":options(false)}),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, PublicError::Invalid { .. }));
    assert_eq!(f.rev().await, before + 1);
    let mut global = recipe("global-only");
    global["name"] = json!("globalonly");
    f.recipe("global", "globalonly", global);
    let global_result = f.call("unit_add",json!({"recipe":"globalonly","unit":"globalunit","params":{"base":2},"start":false,"after":{},"edit":options(true)})).await.unwrap();
    assert!(matches!(global_result, CommandReply::Preview(_)));
    f.close().await;
}

async fn input_preview_validates_without_changing_inputs_history_revisions_or_versions(
    f: &Fixture,
) {
    f.input("value", json!(3), false).await;
    let before = data(
        f.call(
            "query",
            json!({"sql":"SELECT count(*) FROM records","params":[],"limit":200}),
        )
        .await
        .unwrap(),
    );
    let versions = f.broker.project_versions().await.unwrap();
    let rev = f.rev().await;
    let CommandReply::Preview(preview) = f.input("enabled", json!(true), true).await else {
        panic!("preview")
    };
    assert_eq!(preview.would_start, vec![StepId::new("work").unwrap()]);
    assert!(preview.errors.is_empty());
    let after = data(
        f.call(
            "query",
            json!({"sql":"SELECT count(*) FROM records","params":[],"limit":200}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(before, after);
    assert_eq!(versions, f.broker.project_versions().await.unwrap());
    assert_eq!(rev, f.rev().await);
    let status = data(
        f.call(
            "status",
            json!({"selection":{"steps":null,"tags":null},"all":true}),
        )
        .await
        .unwrap(),
    );
    assert!(status["inputs"]["enabled"].is_null());
    assert!(matches!(
        f.call(
            "plan_set_input",
            json!({"name":"enabled","value":1,"edit":options(true)})
        )
        .await,
        Err(PublicError::Invalid { .. })
    ));
    assert!(matches!(f.call("plan_set_input",json!({"name":"enabled","value":true,"edit":{"expected":rev+1,"dry_run":true,"reason":"stale"}})).await,Err(PublicError::Conflict{..})));
}

async fn history_survives_feed_retention_and_filters_revisions(f: &Fixture) {
    f.input("value", json!(4), false).await;
    let entries = |reply: CommandReply| -> Vec<Value> {
        let CommandReply::History(page) = reply else {
            panic!("a history page: {reply:?}")
        };
        serde_json::to_value(page).unwrap()["entries"]
            .as_array()
            .unwrap()
            .clone()
    };
    let edits = |entries: &[Value]| -> Vec<Value> {
        entries
            .iter()
            .filter(|e| e["kind"] == "plan.edit")
            .cloned()
            .collect()
    };
    let all = entries(f.call("plan_history", json!({})).await.unwrap());
    assert_eq!(all.len(), 3);
    assert!(all.iter().any(|e| e["kind"] == "plan.input"));
    let project = f.project;
    f.broker
        .writer()
        .write(RetrySafety::Idempotent, move |tx| {
            records::trim_to(tx, Some(project), 1, 1)
        })
        .await
        .unwrap();
    // The plan's edits are never trimmed, and the feed kept its last record (the input's).
    let after = entries(f.call("plan_history", json!({})).await.unwrap());
    assert_eq!(edits(&after), edits(&all));
    assert_eq!(after, all);
    let filtered = entries(
        f.call("plan_history", json!({"since_rev":1}))
            .await
            .unwrap(),
    );
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0]["kind"], "plan.edit");
}

#[tokio::test]
async fn deletion_checks_archive_confirmation_revision_and_active_work() {
    let f = Fixture::new().await;
    let args = |rev, name| json!({"confirm_name":name,"expected_settings_rev":rev});
    assert!(f.call("project_delete", args(1, "p")).await.is_err());
    f.call(
        "project_update",
        json!({"archived":true,"expected_settings_rev":1}),
    )
    .await
    .unwrap();
    assert!(matches!(
        f.call("project_delete", args(1, "p")).await,
        Err(PublicError::Conflict { .. })
    ));
    assert!(f.call("project_delete", args(2, "wrong")).await.is_err());
    let p = f.project;
    f.broker
        .writer()
        .write(RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='running' WHERE project_id=?1 AND step_id='work'",
                [p.to_string()],
            )?;
            tx.changed(Some(p), "status");
            Ok(())
        })
        .await
        .unwrap();
    assert!(f.call("project_delete", args(2, "p")).await.is_err());
    f.broker
        .writer()
        .write(RetrySafety::Idempotent, move |tx| {
            tx.sql().execute(
                "UPDATE steps SET status='pending' WHERE project_id=?1 AND step_id='work'",
                [p.to_string()],
            )?;
            tx.changed(Some(p), "status");
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        f.call("project_delete", args(2, "p")).await.unwrap(),
        CommandReply::Deleted { deleted: true, .. }
    ));
    assert!(matches!(
        f.call("plan_get", json!({})).await,
        Err(PublicError::NotFound { .. })
    ));
    f.close().await;
}

#[tokio::test]
async fn context_docs_query_and_views_use_the_coordinator_snapshot() {
    let f = Fixture::new().await;
    f.input("value", json!(11), false).await;
    f.call(
        "step_set_output",
        json!({"step":"pre","outputs":{"ok":true},"force":true,"reason":"fixture"}),
    )
    .await
    .unwrap();
    f.patch(json!([{"op":"step.update","step":"work","changes":{"after":["pre","enabled"],"doc":"<script>alert('x')</script>"}}])).await;
    f.call("ask", json!({"to":"work","body":"question","owner":true}))
        .await
        .unwrap();
    let ctx = data(
        f.call("step_context", json!({"step":"work"}))
            .await
            .unwrap(),
    );
    assert_eq!(ctx["inputs"]["value"], 11);
    assert_eq!(ctx["upstream"][0]["step"], "pre");
    assert_eq!(ctx["messages"].as_array().unwrap().len(), 1);
    assert_eq!(ctx["submit"]["outputs"][0]["name"], "ready");
    assert!(ctx["ask"].as_str().unwrap().starts_with("sluice tool ask "));
    // The step's run asks with no recipient: its unit's owner answers.
    assert!(!ctx["ask"].as_str().unwrap().contains("\"to\""));
    assert!(
        ctx["submit"]["command"]
            .as_str()
            .unwrap()
            .contains("<boolean>")
    );
    let mermaid = data(
        f.call("plan_view", json!({"format":"mermaid"}))
            .await
            .unwrap(),
    );
    assert!(mermaid.as_str().unwrap().starts_with("flowchart TD"));
    assert!(
        mermaid
            .as_str()
            .unwrap()
            .contains("work / fixture.submit / pending")
    );
    let all = data(
        f.call("plan_view", json!({"format":"mermaid","all":true}))
            .await
            .unwrap(),
    );
    assert!(
        all.as_str()
            .unwrap()
            .contains("pre / core.external / succeeded")
    );
    assert!(!mermaid.as_str().unwrap().contains("pre / core.external"));
    let html = data(f.call("plan_view", json!({"format":"html"})).await.unwrap());
    assert!(html.as_str().unwrap().contains("<svg"));
    assert!(!html.as_str().unwrap().contains("<script>"));
    let docs = data(f.client.command(request("docs", json!({}))).await.unwrap());
    assert_eq!(docs.as_object().unwrap().len(), 9);
    for topic in docs.as_object().unwrap().keys() {
        let page = data(
            f.client
                .command(request("docs", json!({"topic":topic})))
                .await
                .unwrap(),
        );
        assert!(!page.as_str().unwrap().is_empty());
    }
    assert!(matches!(
        f.client
            .command(request("docs", json!({"topic":"../secrets"})))
            .await,
        Err(PublicError::NotFound { .. })
    ));
    let result=data(f.client.command(request("query",json!({"sql":"SELECT ? AS text, ? AS flag, ? AS n","params":["x'; DROP TABLE projects; --",true,null],"limit":1}))).await.unwrap());
    assert_eq!(
        result["rows"][0],
        json!(["x'; DROP TABLE projects; --", 1, null])
    );
    assert!(matches!(
        f.client
            .command(request(
                "query",
                json!({"sql":"DELETE FROM projects","params":[],"limit":200})
            ))
            .await,
        Err(PublicError::BadRequest { .. })
    ));
    f.close().await;
}

/// The session `@orch` tells or asks the owner, on the session's thread `@orch`.
async fn post(f: &Fixture, body: &str, question: bool) -> MessageId {
    let CommandReply::Receipt(receipt) = f
        .call(
            if question { "ask" } else { "say" },
            json!({"to":"owner","body":body,"me":"@orch"}),
        )
        .await
        .unwrap()
    else {
        panic!("post")
    };
    assert_eq!(receipt.thread, "@orch");
    receipt.id
}
fn wait_request(f: &Fixture, since: i64, limit: u32, timeout: u64) -> CommandRequest {
    request(
        "log_wait",
        json!({"read":{"project":f.selector(),"since_seq":since,"kinds":["message"],"threads":["@orch"],"limit":limit},"timeout_seconds":timeout,"questions_only":true}),
    )
}
async fn send_wait(
    f: &Fixture,
    socket: bool,
    request: CommandRequest,
) -> Result<CommandReply, PublicError> {
    if socket {
        f.client.command(request).await
    } else {
        f.broker.command(request).await
    }
}

#[tokio::test]
async fn log_wait_message_records_over_socket() {
    log_wait_cases(true).await;
}
async fn log_wait_cases(socket: bool) {
    let f = Fixture::new().await;
    let log = f.call("log_read", json!({"limit":200})).await.unwrap();
    let CommandReply::Records(log) = log else {
        panic!("log")
    };
    let broker = f.broker.clone();
    let client = f.client.clone();
    let request = wait_request(&f, log.last_seq.0, 200, 2);
    let mut wait = tokio::spawn(async move {
        if socket {
            client.command(request).await
        } else {
            broker.command(request).await
        }
    });
    let note = post(&f, "note", false).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(60), &mut wait)
            .await
            .is_err()
    );
    let question = post(&f, "question", true).await;
    let CommandReply::Records(page) = tokio::time::timeout(Duration::from_secs(1), wait)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
    else {
        panic!("records")
    };
    assert_eq!(
        page.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
        vec![note.0, question.0]
    );
    let start = tokio::time::Instant::now();
    let reply = send_wait(&f, socket, wait_request(&f, page.last_seq.0, 200, 0))
        .await
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    let CommandReply::Records(empty) = reply else {
        panic!("page")
    };
    assert!(empty.records.is_empty());
    let last = post(&f, "held at timeout", false).await;
    let start = tokio::time::Instant::now();
    let CommandReply::Records(page) = send_wait(&f, socket, wait_request(&f, question.0, 200, 1))
        .await
        .unwrap()
    else {
        panic!("page")
    };
    assert!(start.elapsed() >= Duration::from_millis(900));
    assert_eq!(page.records[0].seq.0, last.0);
    let p = f.project;
    f.broker
        .writer()
        .write(RetrySafety::Idempotent, move |tx| {
            records::trim_to(tx, Some(p), 1, 1)
        })
        .await
        .unwrap();
    assert!(matches!(
        send_wait(&f, socket, wait_request(&f, 0, 200, 0)).await,
        Err(PublicError::CursorExpired { .. })
    ));
    f.close().await;
}
#[tokio::test]
async fn log_wait_reads_only_when_its_own_log_commits_and_expires_a_foreign_cursor() {
    // No server: its scheduler would read on every commit and muddy the count.
    let home = home::ScratchHome::new().unwrap();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
        .await
        .unwrap();
    let mut ids = vec![];
    for name in ["p", "q"] {
        let CommandReply::Project(p) = broker
            .command(request(
                "project_create",
                json!({"name":name,"description":"","resources":{}}),
            ))
            .await
            .unwrap()
        else {
            panic!("project")
        };
        ids.push(p.project_id);
    }
    let (p, q) = (ids[0], ids[1]);
    let selector = json!({"kind":"id","value":p});
    let CommandReply::Records(log) = broker
        .command(request("log_read", json!({"project":selector,"limit":200})))
        .await
        .unwrap()
    else {
        panic!("log")
    };
    let update = || sluice_model::events::Event::ProjectUpdate {
        fields: vec!["description".into()],
        reason: None,
        author: "test".into(),
    };
    let wait = broker.command(request(
        "log_wait",
        json!({"read":{"project":selector,"since_seq":log.last_seq,"limit":200},"timeout_seconds":30,"questions_only":false}),
    ));
    tokio::pin!(wait);
    // Let the wait read its page and park.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut wait)
            .await
            .is_err(),
        "nothing to return yet"
    );
    let before = broker.reads().snapshots();
    // Another project's records and this project's other views commit: the
    // wait must neither wake nor read for them.
    for _ in 0..50 {
        tokio::select! {
            biased;
            _ = &mut wait => panic!("woke for another log"),
            written = broker.writer().write(RetrySafety::NonIdempotent, move |tx| {
                tx.append_record(Some(q), update())?;
                tx.changed(Some(p), "status");
                Ok(())
            }) => written.unwrap(),
        }
    }
    assert_eq!(broker.reads().snapshots(), before);
    let own = broker
        .writer()
        .write(RetrySafety::NonIdempotent, move |tx| {
            tx.append_record(Some(p), update())
        })
        .await
        .unwrap();
    let CommandReply::Records(page) = tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("records")
    };
    assert_eq!(
        page.records.iter().map(|r| r.seq).collect::<Vec<_>>(),
        vec![own.seq]
    );
    assert_eq!(page.last_seq, own.seq);
    // One durable version check, then one page read.
    assert_eq!(broker.reads().snapshots(), before + 2);
    // A cursor no seq of this home ever reached (another home's, or one from
    // before an import renumbered the log) fails at once with the real bounds.
    let stale = own.seq.0 + 33_000;
    let started = tokio::time::Instant::now();
    let expired = broker
        .command(request(
            "log_wait",
            json!({"read":{"project":selector,"since_seq":stale,"limit":200},"timeout_seconds":30,"questions_only":false}),
        ))
        .await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        matches!(&expired, Err(PublicError::CursorExpired { message }) if message.contains(&format!("latest={}", own.seq.0))),
        "{expired:?}"
    );
    broker.writer().shutdown().await.unwrap();
}

#[tokio::test]
async fn bounded_notes_page_over_socket() {
    bounded_notes_cases(true).await;
}
async fn bounded_notes_cases(socket: bool) {
    let f = Fixture::new().await;
    let a = post(&f, "a", false).await;
    let b = post(&f, "b", false).await;
    let c = post(&f, "c", false).await;
    let q = post(&f, "q", true).await;
    let CommandReply::Records(first) = send_wait(&f, socket, wait_request(&f, a.0 - 1, 2, 1))
        .await
        .unwrap()
    else {
        panic!("records")
    };
    assert_eq!(first.last_seq.0, b.0);
    assert_eq!(first.records.len(), 2);
    let CommandReply::Records(second) =
        send_wait(&f, socket, wait_request(&f, first.last_seq.0, 2, 1))
            .await
            .unwrap()
    else {
        panic!("records")
    };
    assert_eq!(
        second.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
        vec![c.0, q.0]
    );
    f.client
        .command(request(
            "mark_read",
            json!({"project":f.project,"identity":"owner","thread":"@orch","through":c}),
        ))
        .await
        .unwrap();
    let page = f
        .call("messages", json!({"view":"inbox","owner":true}))
        .await
        .unwrap();
    let CommandReply::Messages(page) = page else {
        panic!("messages")
    };
    assert_eq!(
        page.messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![q]
    );
    f.close().await;
}
#[tokio::test]
async fn prune_uses_age_evidence_and_backup_is_online_and_refuses_overwrite() {
    let f = Fixture::new().await;
    f.call(
        "step_set_output",
        json!({"step":"pre","outputs":{"ok":true},"force":true,"reason":"fixture"}),
    )
    .await
    .unwrap();
    let preview = f
        .call(
            "plan_prune",
            json!({"older_than_seconds":3600,"edit":options(true)}),
        )
        .await
        .unwrap();
    let CommandReply::Preview(preview) = preview else {
        panic!("preview")
    };
    assert!(preview.changes.is_empty());
    let before = f.rev().await;
    f.call(
        "plan_prune",
        json!({"older_than_seconds":0,"edit":options(false)}),
    )
    .await
    .unwrap();
    assert_eq!(f.rev().await, before + 1);
    let plan = data(f.call("plan_get", json!({})).await.unwrap());
    assert!(plan["plan"]["steps"].get("pre").is_none());
    let names = [
        "ta-harness-work",
        "ta-",
        "beta-x",
        "fig-1",
        "fig-12",
        "fig-4200-landed",
        "axxbyyc",
        "axxbyy",
        "y",
    ];
    f.patch(json!(names.iter().map(|name| json!({"op":"step.add","step":name,"spec":{"run":"core.external","outputs":{"done":"boolean"},"tags":[format!("unit:{name}")]}})).collect::<Vec<_>>())).await;
    for name in names {
        f.call(
            "step_set_output",
            json!({"step":name,"outputs":{"done":true},"force":true,"reason":"fixture"}),
        )
        .await
        .unwrap();
    }
    for (patterns, name, expected) in [
        (vec!["ta-*"], "ta-harness-work", Some("ta-*")),
        (vec!["ta-*"], "ta-", Some("ta-*")),
        (vec!["ta-*"], "beta-x", None),
        (vec!["fig-?"], "fig-1", Some("fig-?")),
        (vec!["fig-?"], "fig-12", None),
        (vec!["*-landed"], "fig-4200-landed", Some("*-landed")),
        (vec!["a*b*c"], "axxbyyc", Some("a*b*c")),
        (vec!["a*b*c"], "axxbyy", None),
        (vec!["x", "*"], "y", Some("*")),
        (vec![], "y", None),
    ] {
        let before = f.rev().await;
        let CommandReply::Pruned(preview) = f
            .call(
                "plan_prune",
                json!({"units":[name],"keep":patterns,"older_than_seconds":0,"edit":options(false)}),
            )
            .await
            .unwrap()
        else {
            panic!("prune preview")
        };
        assert_eq!(
            preview
                .kept
                .iter()
                .find(|kept| kept.unit.as_str() == name)
                .and_then(|kept| kept.keep.as_deref()),
            expected,
            "{name}: {preview:?}"
        );
        assert_eq!(
            preview.units.iter().any(|unit| unit.as_str() == name),
            expected.is_none()
        );
        assert_eq!(f.rev().await, before + u64::from(expected.is_none()));
    }
    let path = f.home.root().join("backup.db");
    let backup = data(
        f.client
            .command(request("backup", json!({"destination":path})))
            .await
            .unwrap(),
    );
    assert!(backup["bytes"].as_u64().unwrap() > 0);
    assert!(path.exists());
    let original = std::fs::read(&path).unwrap();
    let source = f.home.path().join("sluice.db");
    for target in [&path, &source, &f.home.path().to_owned()] {
        let error = f
            .client
            .command(request("backup", json!({"destination":target})))
            .await
            .unwrap_err();
        assert!(matches!(error, PublicError::BadRequest { .. }), "{error:?}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(
        data(f.call("plan_get", json!({})).await.unwrap())["plan"]["steps"]
            .get("work")
            .is_some()
    );
    let reserved = f.home.root().join("sidecar.db");
    let sidecar = f.home.root().join("sidecar.db-wal");
    std::fs::write(&sidecar, b"another owner's sidecar").unwrap();
    assert!(
        f.client
            .command(request("backup", json!({"destination":reserved})))
            .await
            .is_err()
    );
    assert!(!reserved.exists());
    assert_eq!(std::fs::read(sidecar).unwrap(), b"another owner's sidecar");
    assert!(
        f.client
            .command(request("backup", json!({"destination":path})))
            .await
            .is_err()
    );
    f.close().await;
}

// CommandRequest is non_exhaustive across crates. Check its actual source, rather
// than a stale schema snapshot, so any newly declared variant fails this test.

async fn input_preview_reports_cached_resource_queue_and_stale_work(f: &Fixture) {
    f.input("value", json!(3), false).await;
    f.call("project_update", json!({"resources":{"cpu":1}}))
        .await
        .unwrap();
    f.patch(json!([{"op":"step.update","step":"work","changes":{"needs":{"cpu":1}}}]))
        .await;
    f.call("project_update", json!({"resources":{"cpu":0}}))
        .await
        .unwrap();
    let CommandReply::Preview(preview) = f.input("enabled", json!(true), true).await else {
        panic!("preview")
    };
    assert_eq!(preview.would_queue, vec![StepId::new("work").unwrap()]);
    f.input("enabled", json!(true), false).await;
    f.call(
        "step_set_output",
        json!({"step":"work","outputs":{"value":3,"ready":true},"force":true,"reason":"fixture"}),
    )
    .await
    .unwrap();
    let CommandReply::Preview(preview) = f.input("value", json!(4), true).await else {
        panic!("preview")
    };
    assert_eq!(preview.would_stale, vec![StepId::new("work").unwrap()]);
    let state = data(
        f.call(
            "status",
            json!({"selection":{"steps":null,"tags":null},"all":true}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(state["steps"]["work"]["status"], "succeeded");
    assert_eq!(state["inputs"]["value"], 3);
}

async fn message_records_in_log_read_and_next_over_socket(f: &Fixture) {
    let id = post(f, "question", true).await;
    let CommandReply::Records(page) = f
        .call(
            "log_read",
            json!({"since_seq":id.0-1,"kinds":["message"],"limit":1}),
        )
        .await
        .unwrap()
    else {
        panic!("records")
    };
    assert_eq!(page.records[0].seq.0, id.0);
    let CommandReply::Next(next)=f.client.command(request("next",json!({"projects":[f.selector()],"since_seq":id.0-1,"me":"@lead","scope":"all","timeout_seconds":0,"all":true,"settle_seconds":0,"settle_max_seconds":0,"settles":"none"}))).await.unwrap() else {panic!("next")};
    assert_eq!(next.records[0].seq.0, id.0);
}

fn waited(reply: CommandReply) -> StepWaitResult {
    let CommandReply::StepWait(result) = reply else {
        panic!("expected a step_wait result, got {reply:?}")
    };
    result
}
fn statuses(result: &StepWaitResult) -> Vec<(String, StepStatus)> {
    result
        .steps
        .iter()
        .map(|(id, status)| (id.to_string(), status.clone()))
        .collect()
}
/// A `step_wait` or `log_wait` over the socket, answered on its own task.
fn spawn_call(
    f: &Fixture,
    name: &'static str,
    mut args: Value,
) -> tokio::task::JoinHandle<Result<CommandReply, PublicError>> {
    if name == "log_wait" {
        args["read"]["project"] = f.selector();
    } else {
        args["project"] = f.selector();
    }
    let client = f.client.clone();
    tokio::spawn(async move { client.command(request(name, args)).await })
}
/// The call is parked: it has not answered by now.
async fn parked(call: &mut tokio::task::JoinHandle<Result<CommandReply, PublicError>>) {
    assert!(
        tokio::time::timeout(Duration::from_millis(150), call)
            .await
            .is_err(),
        "answered before anything it waits for"
    );
}
async fn answered(
    call: tokio::task::JoinHandle<Result<CommandReply, PublicError>>,
) -> CommandReply {
    tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("the wait did not wake")
        .unwrap()
        .unwrap()
}
async fn latest(f: &Fixture) -> RecordPage {
    let CommandReply::Records(page) = f.call("log_read", json!({"limit":1})).await.unwrap() else {
        panic!("records")
    };
    page
}
fn wait_args(selection: Value, until: Value, timeout: u64) -> Value {
    json!({"selection":selection,"until":until,"timeout_seconds":timeout})
}
fn steps(ids: &[&str]) -> Value {
    json!({"steps":ids,"tags":null})
}

async fn step_wait_answers_at_once_when_the_condition_already_holds(f: &Fixture) {
    let seq = latest(f).await.last_seq;
    // pre is a ready core.external step and work waits on unset plan inputs: both held.
    let started = tokio::time::Instant::now();
    let result = waited(
        f.call(
            "step_wait",
            wait_args(steps(&["work", "pre"]), json!("settled"), 30),
        )
        .await
        .unwrap(),
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(result.met);
    assert_eq!(
        statuses(&result),
        vec![
            ("pre".into(), StepStatus::Pending),
            ("work".into(), StepStatus::Pending)
        ]
    );
    assert_eq!(result.seq, seq);
    let result = waited(
        f.call(
            "step_wait",
            wait_args(steps(&["pre"]), json!({"any_of":["pending","running"]}), 30),
        )
        .await
        .unwrap(),
    );
    assert!(result.met);
    // Not met, and a zero timeout reads once.
    let result = waited(
        f.call(
            "step_wait",
            wait_args(steps(&["pre"]), json!("succeeded"), 0),
        )
        .await
        .unwrap(),
    );
    assert!(!result.met);
    assert_eq!(statuses(&result), vec![("pre".into(), StepStatus::Pending)]);
}

async fn step_wait_wakes_when_another_caller_changes_the_status(f: &Fixture) {
    let mut wait = spawn_call(
        f,
        "step_wait",
        wait_args(steps(&["pre"]), json!("succeeded"), 30),
    );
    parked(&mut wait).await;
    f.call(
        "step_set_output",
        json!({"step":"pre","outputs":{"ok":true},"force":true,"reason":"elsewhere"}),
    )
    .await
    .unwrap();
    let result = waited(answered(wait).await);
    assert!(result.met);
    assert_eq!(
        statuses(&result),
        vec![("pre".into(), StepStatus::Succeeded)]
    );
    // seq covers the status change: a log_read from before it finds the record.
    let CommandReply::Records(page) = f
        .call(
            "log_read",
            json!({"kinds":["step.status"],"statuses":["succeeded"],"limit":1}),
        )
        .await
        .unwrap()
    else {
        panic!("records")
    };
    assert!(result.seq >= page.records[0].seq);
}

#[tokio::test]
async fn step_wait_times_out_with_met_false() {
    let mut f = Fixture::new().await;
    f.client.reply_timeout = Duration::from_millis(100);
    let started = tokio::time::Instant::now();
    let result = waited(
        f.call(
            "step_wait",
            wait_args(steps(&["pre"]), json!("succeeded"), 1),
        )
        .await
        .unwrap(),
    );
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert!(!result.met);
    assert_eq!(statuses(&result), vec![("pre".into(), StepStatus::Pending)]);
    f.close().await;
}

async fn step_wait_any_of_wakes_on_one_of_the_listed_statuses(f: &Fixture) {
    let mut wait = spawn_call(
        f,
        "step_wait",
        wait_args(steps(&["pre"]), json!({"any_of":["failed","stale"]}), 30),
    );
    parked(&mut wait).await;
    // Cancelling a pending core.external step fails it at once.
    f.call(
        "step_cancel",
        json!({"selection":steps(&["pre"]),"reason":"no longer needed"}),
    )
    .await
    .unwrap();
    let result = waited(answered(wait).await);
    assert!(result.met);
    assert_eq!(statuses(&result), vec![("pre".into(), StepStatus::Failed)]);
}

async fn step_wait_selects_by_tag(f: &Fixture) {
    let mut wait = spawn_call(
        f,
        "step_wait",
        wait_args(
            json!({"steps":null,"tags":["unit:pre"]}),
            json!("succeeded"),
            30,
        ),
    );
    parked(&mut wait).await;
    f.call(
        "step_set_output",
        json!({"step":"pre","outputs":{"ok":true},"force":true,"reason":"by hand"}),
    )
    .await
    .unwrap();
    let result = waited(answered(wait).await);
    assert!(result.met);
    assert_eq!(
        statuses(&result),
        vec![("pre".into(), StepStatus::Succeeded)]
    );
}

async fn step_wait_settled_counts_held_steps_but_not_work_that_can_still_move(f: &Fixture) {
    f.broker.acquire_scheduler("test".into()).await.unwrap();
    // later waits on pre, which waits for its outputs by hand: both are held.
    f.patch(json!([{"op":"step.add","step":"later","spec":{"run":"fixture.echo","in":{"value":{"default":1}},"after":["pre"]}}])).await;
    let settled = |ids: &'static [&'static str]| wait_args(steps(ids), json!("settled"), 0);
    assert!(waited(f.call("step_wait", settled(&["later"])).await.unwrap()).met);
    // work starts once its inputs are set, and a step waiting on running work is not settled.
    f.patch(json!([{"op":"step.add","step":"after-work","spec":{"run":"fixture.echo","in":{"value":{"default":1}},"after":["work"]}}])).await;
    let running = spawn_call(
        f,
        "step_wait",
        wait_args(steps(&["work"]), json!({"any_of":["running"]}), 30),
    );
    f.input("value", json!(1), false).await;
    f.input("enabled", json!(true), false).await;
    assert!(waited(answered(running).await).met);
    let result = waited(f.call("step_wait", settled(&["after-work"])).await.unwrap());
    assert!(!result.met);
    assert_eq!(
        statuses(&result),
        vec![("after-work".into(), StepStatus::Pending)]
    );
    // A paused step is held, whatever it waits on.
    f.call(
        "step_pause",
        json!({"selection":steps(&["after-work"]),"paused":true,"edit":options(false)}),
    )
    .await
    .unwrap();
    assert!(waited(f.call("step_wait", settled(&["after-work"])).await.unwrap()).met);
}

async fn step_wait_refuses_bad_selections_and_conditions_as_invalid_naming_each(f: &Fixture) {
    let cases = [
        (
            wait_args(steps(&["nope", "pre", "ghost"]), json!("succeeded"), 0),
            vec!["steps: no step nope", "steps: no step ghost"],
        ),
        (
            wait_args(
                json!({"steps":null,"tags":["missing","unit:pre"]}),
                json!("succeeded"),
                0,
            ),
            vec!["tags: no step is tagged missing"],
        ),
        (
            wait_args(
                json!({"steps":["pre"],"tags":["unit:pre"]}),
                json!("succeeded"),
                0,
            ),
            vec!["tags: leave out"],
        ),
        (
            wait_args(json!({"steps":null,"tags":null}), json!("succeeded"), 0),
            vec!["steps: name at least one"],
        ),
        (
            wait_args(json!({"steps":[],"tags":[]}), json!("succeeded"), 0),
            vec!["steps: name at least one"],
        ),
        (
            wait_args(steps(&["pre"]), json!("done"), 0),
            vec![r#"not "done""#],
        ),
        (
            wait_args(steps(&["pre"]), json!({"any_of":["bogus","failed",3]}), 0),
            vec!["until.any_of[0]: \"bogus\"", "until.any_of[2]: 3"],
        ),
        (
            wait_args(steps(&["pre"]), json!({"any_of":[]}), 0),
            vec!["until.any_of: name at least one status"],
        ),
        (
            wait_args(steps(&["pre"]), json!({"all_of":["failed"]}), 0),
            vec!["until.all_of: unknown field", "until.any_of: missing"],
        ),
    ];
    for (args, expected) in cases {
        let result = f.call("step_wait", args.clone()).await;
        let Err(PublicError::Invalid { errors, .. }) = &result else {
            panic!("{args}: {result:?}")
        };
        assert_eq!(errors.len(), expected.len(), "{args}: {errors:?}");
        for (error, expected) in errors.iter().zip(expected) {
            assert!(error.contains(expected), "{args}: {error} lacks {expected}");
        }
    }
}

fn page(reply: CommandReply) -> RecordPage {
    let CommandReply::Records(page) = reply else {
        panic!("records")
    };
    page
}
fn kinds(page: &RecordPage) -> BTreeSet<String> {
    page.records
        .iter()
        .map(|r| {
            serde_json::to_value(r).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}
fn field(record: &sluice_model::events::Record, name: &str) -> Value {
    serde_json::to_value(record).unwrap()[name].clone()
}

async fn log_read_filters_step_status_by_status_and_messages_by_recipient(f: &Fixture) {
    f.call("say", json!({"to":"pre","body":"for pre"}))
        .await
        .unwrap();
    let note = post(f, "for the owner", false).await;
    f.input("value", json!(1), false).await;
    f.call(
        "step_set_output",
        json!({"step":"pre","outputs":{"ok":true},"force":true,"reason":"by hand"}),
    )
    .await
    .unwrap();
    f.patch(json!([{"op":"step.add","step":"extra","spec":{"run":"core.external","outputs":{"ok":"boolean"}}}])).await;
    f.call(
        "step_cancel",
        json!({"selection":steps(&["extra"]),"reason":"not needed"}),
    )
    .await
    .unwrap();
    let read = |args: Value| async {
        let mut args = args;
        args["since_seq"] = json!(0);
        args["limit"] = json!(200);
        page(f.call("log_read", args).await.unwrap())
    };
    // Alone, statuses narrows step.status records and lets every other kind through.
    let all = read(json!({})).await;
    let only = read(json!({"statuses":["succeeded"]})).await;
    let step_status = |page: &RecordPage| -> Vec<Value> {
        page.records
            .iter()
            .filter(|r| field(r, "kind") == "step.status")
            .map(|r| field(r, "to"))
            .collect()
    };
    assert!(step_status(&all).len() > step_status(&only).len());
    assert_eq!(step_status(&only), vec![json!("succeeded")]);
    assert_eq!(
        only.records.len(),
        all.records.len() - step_status(&all).len() + 1
    );
    assert!(kinds(&only).contains("message") && kinds(&only).contains("plan.input"));
    // With kinds, only the matching step.status records.
    let only = read(json!({"kinds":["step.status"],"statuses":["succeeded"]})).await;
    assert_eq!(kinds(&only), BTreeSet::from(["step.status".to_owned()]));
    assert_eq!(step_status(&only), vec![json!("succeeded")]);
    // recipients narrows messages the same way.
    let only = read(json!({"recipients":["owner"]})).await;
    let messages: Vec<_> = only
        .records
        .iter()
        .filter(|r| field(r, "kind") == "message")
        .map(|r| r.seq.0)
        .collect();
    assert_eq!(messages, vec![note.0]);
    assert!(kinds(&only).contains("step.status"));
    let only = read(json!({"kinds":["message"],"recipients":["owner"]})).await;
    assert_eq!(
        only.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
        vec![note.0]
    );
    // Both, with kinds: the succeeded status and the message to the owner, in order.
    let only = read(
        json!({"kinds":["step.status","message"],"statuses":["succeeded"],"recipients":["owner"]}),
    )
    .await;
    assert_eq!(only.records.len(), 2);
    assert_eq!(only.records[0].seq.0, note.0);
    assert_eq!(field(&only.records[1], "to"), "succeeded");
    // threads and recipients both apply to a message.
    assert!(
        read(json!({"threads":["@orch"],"recipients":["pre"]}))
            .await
            .records
            .is_empty()
    );
    assert_eq!(
        read(json!({"threads":["@orch"],"recipients":["owner"]}))
            .await
            .records
            .iter()
            .map(|r| r.seq.0)
            .collect::<Vec<_>>(),
        vec![note.0]
    );
}

async fn standing_log_wait_sleeps_through_unrelated_records_and_wakes_on_the_first_match(
    f: &Fixture,
) {
    let standing = |since: RecordSeq| json!({"read":{"since_seq":since,"kinds":["step.status","message"],"statuses":["failed","stale"],"recipients":["@orch"],"limit":200},"timeout_seconds":30,"questions_only":false});
    let mut wait = spawn_call(f, "log_wait", standing(latest(f).await.last_seq));
    parked(&mut wait).await;
    // A message to a step, a note to the owner, plan inputs, an edit and a running step.
    f.call("say", json!({"to":"pre","body":"for pre"}))
        .await
        .unwrap();
    f.call("say", json!({"to":"owner","body":"for the owner"}))
        .await
        .unwrap();
    f.input("value", json!(1), false).await;
    f.patch(json!([{"op":"step.add","step":"extra","spec":{"run":"core.external","outputs":{"ok":"boolean"}}}])).await;
    f.call(
        "step_set_output",
        json!({"step":"extra","outputs":{"ok":true},"force":true,"reason":"by hand"}),
    )
    .await
    .unwrap();
    parked(&mut wait).await;
    f.call(
        "step_cancel",
        json!({"selection":steps(&["pre"]),"reason":"no longer needed"}),
    )
    .await
    .unwrap();
    let woke = page(answered(wait).await);
    assert_eq!(woke.records.len(), 1, "{:?}", woke.records);
    assert_eq!(field(&woke.records[0], "kind"), "step.status");
    assert_eq!(field(&woke.records[0], "step"), "pre");
    assert_eq!(field(&woke.records[0], "to"), "failed");
    // From there, the next standing wait wakes on the message to the session.
    let mut wait = spawn_call(f, "log_wait", standing(woke.last_seq));
    parked(&mut wait).await;
    f.call("say", json!({"to":"work","body":"for work"}))
        .await
        .unwrap();
    parked(&mut wait).await;
    let CommandReply::Receipt(note) = f
        .call(
            "say",
            json!({"to":"@orch","body":"for the session","owner":true}),
        )
        .await
        .unwrap()
    else {
        panic!("say")
    };
    let note = note.id;
    let woke = page(answered(wait).await);
    assert_eq!(
        woke.records.iter().map(|r| r.seq.0).collect::<Vec<_>>(),
        vec![note.0]
    );
}

#[tokio::test]
async fn status_names_steps_and_units_and_recipe_list_reports_titles_and_views_over_socket() {
    let f = Fixture::new().await;
    let spec = f.home.path().join("fig-1.md");
    std::fs::write(
        &spec,
        "Notes first\n\n# Fix the cron driver on Postgres\n\nBody.",
    )
    .unwrap();
    f.recipe(
        "project",
        "named",
        json!({"name":"named","params":{"ticket":"string","spec":"string","quiet":"string?"},
            "title":"{ticket}: {spec}",
            "view":"root = Stack([Param(\"ticket\"), Output(\"work\", \"ok\")], \"row\")",
            "steps":{
                "{unit}-work":{"run":"core.external","in":{"ticket":{"default":"{ticket}"},"spec":{"file":"{spec}"}},"outputs":{"ok":"boolean"}},
                "{unit}-land":{"run":"core.external","after":["{unit}-work"],"outputs":{"ok":"boolean"}}}}),
    );
    // a broken title and view are reported, and the recipe still adds units
    f.recipe(
        "project",
        "rough",
        json!({"name":"rough","params":{"quiet":"string?"},"title":"{quiet} lane",
            "view":"root = Stack([Output(\"nope\", \"ok\"), Chart(\"bar\", \"SELECT 1\")])",
            "steps":{"{unit}-only":{"run":"core.external","outputs":{"ok":"boolean"}}}}),
    );
    let recipes = data(f.call("recipe_list", json!({})).await.unwrap());
    let named = recipes
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "named")
        .unwrap();
    assert_eq!(named["title"], "{ticket}: {spec}");
    assert_eq!(named["stages"], json!(["work", "land"]));
    assert!(
        named.get("view_error").is_none() && named.get("title_error").is_none(),
        "{named}"
    );
    let rough = recipes
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "rough")
        .unwrap();
    let title_error = rough["title_error"].as_str().unwrap();
    assert!(
        title_error.contains("{quiet} is a param no step uses"),
        "{title_error}"
    );
    let view_error = rough["view_error"].as_str().unwrap();
    assert!(
        view_error.contains("line 1: Chart is not a unit view component"),
        "{view_error}"
    );
    assert!(view_error.contains("not \"nope\""), "{view_error}");
    for (recipe, unit, params) in [
        (
            "named",
            "fig-1",
            json!({"ticket":"FIG-1","spec":spec.to_str().unwrap()}),
        ),
        ("rough", "r-1", json!({})),
    ] {
        f.call(
            "unit_add",
            json!({"recipe":recipe,"unit":unit,"params":params,"after":{},"edit":options(false)}),
        )
        .await
        .unwrap();
    }
    f.patch(json!([{"op":"step.add","step":"watch","spec":{"run":"core.external","doc":"Watches main for red\nand says so","outputs":{"ok":"boolean"}}}])).await;
    let status = data(
        f.call("status", json!({"selection":{"steps":null,"tags":null}}))
            .await
            .unwrap(),
    );
    let steps = &status["steps"];
    assert_eq!(
        steps["fig-1-work"]["title"],
        "FIG-1: Fix the cron driver on Postgres"
    );
    assert_eq!(steps["fig-1-work"]["stage"], "work");
    assert_eq!(
        steps["fig-1-land"]["title"],
        "FIG-1: Fix the cron driver on Postgres"
    );
    assert_eq!(steps["fig-1-land"]["stage"], "land");
    assert_eq!(steps["watch"]["title"], "Watches main for red");
    assert!(steps["watch"].get("stage").is_none());
    // no title of its own and none to borrow: its id
    assert_eq!(steps["work"]["title"], "work");
    assert_eq!(steps["r-1-only"]["title"], "r-1-only");
    let units = data(
        f.call(
            "status",
            json!({"selection":{"steps":null,"tags":null},"view":"units"}),
        )
        .await
        .unwrap(),
    );
    let row = units["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["unit"] == "fig-1")
        .unwrap();
    assert_eq!(row["title"], "FIG-1: Fix the cron driver on Postgres");
    assert_eq!(row["recipe"], "named");
    f.close().await;
}

#[tokio::test]
async fn edit_previews_and_durable_history() {
    let mut f = Fixture::new().await;
    input_preview_validates_without_changing_inputs_history_revisions_or_versions(&f).await;
    f.next_project().await;
    input_preview_reports_cached_resource_queue_and_stale_work(&f).await;
    f.next_project().await;
    history_survives_feed_retention_and_filters_revisions(&f).await;
    f.close().await;
}

#[tokio::test]
async fn step_wait_selections_conditions_and_live_status() {
    let mut f = Fixture::new().await;
    step_wait_answers_at_once_when_the_condition_already_holds(&f).await;
    f.next_project().await;
    step_wait_wakes_when_another_caller_changes_the_status(&f).await;
    f.next_project().await;
    step_wait_any_of_wakes_on_one_of_the_listed_statuses(&f).await;
    f.next_project().await;
    step_wait_selects_by_tag(&f).await;
    f.next_project().await;
    step_wait_refuses_bad_selections_and_conditions_as_invalid_naming_each(&f).await;
    f.next_project().await;
    step_wait_settled_counts_held_steps_but_not_work_that_can_still_move(&f).await;
    f.close().await;
}

#[tokio::test]
async fn message_records_and_filtered_waits() {
    let mut f = Fixture::new().await;
    message_records_in_log_read_and_next_over_socket(&f).await;
    f.next_project().await;
    log_read_filters_step_status_by_status_and_messages_by_recipient(&f).await;
    f.next_project().await;
    standing_log_wait_sleeps_through_unrelated_records_and_wakes_on_the_first_match(&f).await;
    f.close().await;
}
