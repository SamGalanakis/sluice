#[path = "../../../tests/support/home.rs"]
mod home;
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
        let f = Self {
            home,
            broker,
            client,
            project: p.project_id,
            stop,
            server,
        };
        f.patch(json!([
            {"op":"add","path":"/inputs","value":{"value":"int","enabled":"boolean"}},
            {"op":"add","path":"/steps/pre","value":{"run":"core.external","outputs":{"ok":"boolean"},"tags":["unit:pre","exit"]}},
            {"op":"add","path":"/steps/work","value":{"run":"fixture.submit","in":{"value":{"source":"value"}},"after":["enabled"],"outputs":{"ready":"boolean"}}}
        ])).await;
        f
    }
    fn selector(&self) -> Value {
        json!({"kind":"id","value":self.project})
    }
    async fn call(&self, name: &str, mut args: Value) -> Result<CommandReply, PublicError> {
        if name != "query" {
            args["project"] = self.selector();
        }
        self.client.command(request(name, args)).await
    }
    async fn rev(&self) -> u64 {
        data(self.call("plan_get", json!({})).await.unwrap())["rev"]
            .as_u64()
            .unwrap()
    }
    async fn patch(&self, ops: Value) {
        self.call("plan_patch",json!({"rev":self.rev().await,"ops":ops,"dry_run":false,"reason":"fixture","start":true})).await.unwrap();
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
fn data(reply: CommandReply) -> Value {
    let CommandReply::Data(v) = reply else {
        panic!("expected data, got {reply:?}")
    };
    v.into_value()
}
fn options(dry_run: bool) -> Value {
    json!({"dry_run":dry_run,"reason":"test"})
}
fn selection() -> Value {
    json!({"steps":["work"],"tags":null})
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

#[tokio::test]
async fn input_preview_validates_without_changing_inputs_history_revisions_or_versions() {
    let f = Fixture::new().await;
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
    f.close().await;
}

#[tokio::test]
async fn history_survives_feed_retention_and_filters_revisions() {
    let f = Fixture::new().await;
    f.input("value", json!(4), false).await;
    let all = data(f.call("plan_history", json!({})).await.unwrap());
    assert_eq!(all.as_array().unwrap().len(), 3);
    f.broker
        .writer()
        .write(RetrySafety::Idempotent, move |tx| {
            records::trim_to(tx, Some(f.project), 1, 1)
        })
        .await
        .unwrap();
    let after = data(f.call("plan_history", json!({})).await.unwrap());
    assert_eq!(after, all);
    let filtered = data(
        f.call("plan_history", json!({"since_rev":1}))
            .await
            .unwrap(),
    );
    assert_eq!(filtered.as_array().unwrap().len(), 2);
    assert_eq!(filtered[0]["kind"], "plan.edit");
    f.close().await;
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
    f.patch(json!([{"op":"add","path":"/steps/work/after","value":["pre","enabled"]},{"op":"add","path":"/steps/work/doc","value":"<script>alert('x')</script>"}] )).await;
    f.call("message_post",json!({"thread":"step-work","body":"question","from":"work","to":"orchestrator","needs_reply":true})).await.unwrap();
    let ctx = data(
        f.call("step_context", json!({"step":"work"}))
            .await
            .unwrap(),
    );
    assert_eq!(ctx["inputs"]["value"], 11);
    assert_eq!(ctx["upstream"][0]["step"], "pre");
    assert_eq!(ctx["messages"].as_array().unwrap().len(), 1);
    assert_eq!(ctx["submit"]["outputs"][0]["name"], "ready");
    assert!(ctx["ask"].as_str().unwrap().contains("message.post"));
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
    assert_eq!(docs.as_object().unwrap().len(), 8);
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

async fn post(f: &Fixture, body: &str, question: bool) -> MessageId {
    let CommandReply::Posted { id } = f
        .call(
            "message_post",
            json!({"thread":"t","body":body,"from":"worker","to":"owner","needs_reply":question}),
        )
        .await
        .unwrap()
    else {
        panic!("post")
    };
    id
}
fn wait_request(f: &Fixture, since: i64, limit: u32, timeout: u64) -> CommandRequest {
    request(
        "log_wait",
        json!({"read":{"project":f.selector(),"since_seq":since,"kinds":["message"],"threads":["t"],"limit":limit},"timeout_seconds":timeout,"questions_only":true}),
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
async fn log_wait_holds_notes_wakes_on_question_times_out_and_expires_without_blocking_writer() {
    log_wait_cases(false).await;
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
    let wait = tokio::spawn(async move {
        if socket {
            client.command(request).await
        } else {
            broker.command(request).await
        }
    });
    let note = post(&f, "note", false).await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(!wait.is_finished());
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
    tokio::select! {
        biased;
        _ = &mut wait => panic!("nothing to return yet"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {}
    }
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
async fn bounded_notes_page_never_skips_the_question_and_mark_read_is_thread_scoped() {
    bounded_notes_cases(false).await;
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
            json!({"project":f.project,"identity":"cli","thread":"t","through":c}),
        ))
        .await
        .unwrap();
    let page = f.call("messages", json!({"view":"inbox"})).await.unwrap();
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
    assert!(preview.ops.is_empty());
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
    let path = f.home.root().join("backup.db");
    let backup = data(
        f.client
            .command(request("backup", json!({"destination":path})))
            .await
            .unwrap(),
    );
    assert!(backup["bytes"].as_u64().unwrap() > 0);
    assert!(path.exists());
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
fn declared_variants() -> BTreeSet<String> {
    let source = include_str!("../../sluice-model/src/commands.rs");
    let body = source
        .split("pub enum CommandRequest {")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    body.lines()
        .filter_map(|line| {
            let line = line.strip_prefix("    ")?;
            if !line.starts_with(|c: char| c.is_ascii_uppercase()) {
                return None;
            }
            Some(
                line.split(['(', '{', ','])
                    .next()
                    .unwrap()
                    .trim()
                    .to_owned(),
            )
        })
        .collect()
}
#[tokio::test]
async fn every_command_variant_dispatches_through_a_real_socket() {
    let f = Fixture::new().await;
    f.recipe("global", "lane", recipe("global"));
    let sel = selection();
    let edit = options(true);
    let run = RunId::new();
    let log = json!({"project":f.selector(),"since_seq":0,"limit":1});
    let cases = vec![
        (
            "ProjectUpdate",
            "project_update",
            json!({"description":"updated"}),
        ),
        (
            "ProjectDelete",
            "project_delete",
            json!({"confirm_name":"p","expected_settings_rev":1}),
        ),
        (
            "PlanPatch",
            "plan_patch",
            json!({"rev":f.rev().await,"ops":[],"dry_run":true,"reason":"test"}),
        ),
        (
            "StepAdd",
            "step_add",
            json!({"step":"new","spec":{"run":"core.external","outputs":{"ok":"boolean"}},"edit":edit}),
        ),
        (
            "UnitAdd",
            "unit_add",
            json!({"recipe":"lane","unit":"lane","params":{"base":1},"after":{},"edit":edit}),
        ),
        (
            "StepUpdate",
            "step_update",
            json!({"step":"work","changes":{"doc":"doc"},"edit":edit}),
        ),
        (
            "StepRemove",
            "step_remove",
            json!({"selection":sel,"edit":edit}),
        ),
        (
            "StepPause",
            "step_pause",
            json!({"selection":sel,"paused":true,"edit":edit}),
        ),
        (
            "UnitTag",
            "unit_tag",
            json!({"unit":"pre","add":["tag"],"remove":[],"edit":edit}),
        ),
        (
            "PlanPrune",
            "plan_prune",
            json!({"older_than_seconds":0,"edit":edit}),
        ),
        (
            "PlanSetInput",
            "plan_set_input",
            json!({"name":"enabled","value":true,"edit":edit}),
        ),
        (
            "StepSetInput",
            "step_set_input",
            json!({"selection":sel,"inputs":{"value":1},"edit":edit}),
        ),
        (
            "StepSetOutput",
            "step_set_output",
            json!({"step":"pre","outputs":{"ok":true},"force":true,"reason":"test"}),
        ),
        ("StepRetry", "step_retry", json!({"selection":sel})),
        (
            "StepCancel",
            "step_cancel",
            json!({"selection":sel,"reason":"test"}),
        ),
        (
            "StepSubmit",
            "step_submit",
            json!({"project":f.project,"step":"work","run":run,"outputs":{"ready":true}}),
        ),
        ("Messages", "messages", json!({"view":"questions"})),
        (
            "MarkRead",
            "mark_read",
            json!({"project":f.project,"identity":"cli","thread":"t","through":0}),
        ),
        (
            "FnCall",
            "fn_call",
            json!({"name":"fixture.echo","inputs":{"value":1},"wait_seconds":0,"direct":false}),
        ),
        ("LogRead", "log_read", json!({"limit":1})),
        (
            "LogWait",
            "log_wait",
            json!({"read":log,"timeout_seconds":0,"questions_only":false}),
        ),
        (
            "Next",
            "next",
            json!({"projects":[f.selector()],"since_seq":0,"me":"test","timeout_seconds":0,"all":false,"settle_seconds":0,"settle_max_seconds":0,"settles":"none"}),
        ),
        // Message-record round trips belong to the ignored record-at cases below.
        (
            "MessagePost",
            "message_post",
            json!({"body":"note","needs_reply":false}),
        ),
        (
            "Query",
            "query",
            json!({"sql":"SELECT 1","params":[],"limit":1}),
        ),
        (
            "AcquireLease",
            "acquire_lease",
            json!({"run":run,"resource":"lane","amount":1,"priority":0,"request_id":"test"}),
        ),
        (
            "ReleaseLease",
            "release_lease",
            json!({"lease":1,"run":run}),
        ),
        (
            "RegisterCompletionAction",
            "register_completion_action",
            json!({"project":f.project,"run":run,"target":{"step":"work","generation":1,"work":1,"result":ResultId::new()},"message":"feedback","author":"test"}),
        ),
        (
            "EdgeAdd",
            "edge_add",
            json!({"step":"work","after":["pre"],"edit":edit}),
        ),
        (
            "EdgeRemove",
            "edge_remove",
            json!({"step":"work","after":["enabled"],"edit":edit}),
        ),
        ("ProjectsList", "projects_list", Value::Null),
        (
            "ProjectCreate",
            "project_create",
            json!({"name":"second","description":"","resources":{}}),
        ),
        ("PlanGet", "plan_get", json!({})),
        ("PlanHistory", "plan_history", json!({})),
        ("RecipeList", "recipe_list", json!({})),
        ("FnList", "fn_list", json!({})),
        ("FnGet", "fn_get", json!({"name":"fixture.echo"})),
        (
            "FnSave",
            "fn_save",
            json!({"manifest":{"name":"custom.fn","inputs":{},"outputs":{}},"main_py":"pass"}),
        ),
        ("CallStatus", "call_status", json!({"call":run})),
        ("StepContext", "step_context", json!({"step":"work"})),
        ("Status", "status", json!({"selection":sel})),
        ("PlanView", "plan_view", json!({"format":"mermaid"})),
        ("Verify", "verify", json!({})),
        ("Drain", "drain", json!({"author":"test"})),
        ("Release", "release", json!({"author":"test"})),
        (
            "Backup",
            "backup",
            json!({"destination":f.home.root().join("all-commands.db")}),
        ),
        ("Docs", "docs", json!({"topic":"instructions"})),
        (
            "Builtin",
            "builtin",
            json!({"invocation":{"project":f.project,"run":run,"attempt":AttemptId::new(),"invocation":InvocationId::new(),"name":"agent.run","inputs":{}}}),
        ),
        ("Submission", "submission", json!({"run":run})),
    ];
    assert_eq!(
        declared_variants(),
        cases
            .iter()
            .map(|(variant, _, _)| variant.to_string())
            .collect(),
        "a CommandRequest variant has no socket test instance"
    );
    for (variant, name, mut args) in cases {
        let scoped = matches!(
            variant,
            "ProjectUpdate"
                | "ProjectDelete"
                | "PlanPatch"
                | "StepAdd"
                | "UnitAdd"
                | "StepUpdate"
                | "StepRemove"
                | "StepPause"
                | "UnitTag"
                | "PlanPrune"
                | "PlanSetInput"
                | "StepSetInput"
                | "StepSetOutput"
                | "StepRetry"
                | "StepCancel"
                | "MessagePost"
                | "Messages"
                | "FnCall"
                | "LogRead"
                | "EdgeAdd"
                | "EdgeRemove"
                | "PlanGet"
                | "PlanHistory"
                | "RecipeList"
                | "FnList"
                | "FnGet"
                | "FnSave"
                | "CallStatus"
                | "StepContext"
                | "Status"
                | "PlanView"
                | "Verify"
        );
        if scoped {
            args["project"] = f.selector();
        }
        let raw = if variant == "ProjectsList" {
            json!({"command":name})
        } else {
            json!({"command":name,"args":args})
        };
        let command: CommandRequest = decode_json(&serde_json::to_vec(&raw).unwrap())
            .unwrap_or_else(|e| panic!("{variant} fixture invalid: {e:?}"));
        let result = tokio::time::timeout(Duration::from_secs(3), f.client.command(command))
            .await
            .unwrap_or_else(|_| panic!("{variant} timed out"));
        if let Err(ref error) = result {
            let encoded = serde_json::to_string(error).unwrap();
            assert!(
                !encoded.contains("not implemented") && !encoded.contains("not_implemented"),
                "{variant}: {error:?}"
            );
            assert!(
                !matches!(error, PublicError::Storage { .. }),
                "{variant} did not return a valid semantic response: {error:?}"
            );
        }
    }
    f.close().await;
}

#[tokio::test]
async fn input_preview_reports_cached_resource_queue_and_stale_work() {
    let f = Fixture::new().await;
    f.input("value", json!(3), false).await;
    f.call("project_update", json!({"resources":{"cpu":1}}))
        .await
        .unwrap();
    f.patch(json!([{"op":"add","path":"/steps/work/needs","value":{"cpu":1}}]))
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
    f.close().await;
}

#[tokio::test]
async fn message_records_in_log_read_and_next_over_socket() {
    let f = Fixture::new().await;
    let id = post(&f, "question", true).await;
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
    let CommandReply::Next(next)=f.client.command(request("next",json!({"projects":[f.selector()],"since_seq":id.0-1,"me":"owner","timeout_seconds":0,"all":true,"settle_seconds":0,"settle_max_seconds":0,"settles":"none"}))).await.unwrap() else {panic!("next")};
    assert_eq!(next.records[0].seq.0, id.0);
    f.close().await;
}
