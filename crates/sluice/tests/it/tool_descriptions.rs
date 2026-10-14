//! Every MCP tool description that says what the tool returns ("Returns {a, b?, ...}" or
//! "Returns [{...}]") names keys a real reply has: each tool below is called through the MCP
//! server on a scratch home with a live coordinator, and every non-optional key its
//! description lists must be in the reply (in every element, for a list).
use crate::support;

use serde_json::{Value, json};
use sluice_model::{error::PublicError, rpc::FnInvocation, rpc::JsonMap};
use sluice_process::guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence};
use sluice_runtime::{
    client::CoordinatorClient,
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use sluice_web::mcp::McpServer;
use std::{sync::Arc, time::Duration};
use support::home::ScratchHome;
use tokio_util::sync::CancellationToken;

// Publishes the real fn registry and accepts every launch without ever completing it, so a
// started step stays running.
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

struct Fixture {
    _home: ScratchHome,
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
            _home: home,
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
    /// The tool's raw MCP result, an error included.
    async fn result(&self, name: &str, args: Value) -> Value {
        let Value::Object(args) = args else {
            panic!("args")
        };
        serde_json::to_value(self.mcp.call(name, args, Some("test")).await).unwrap()
    }
    async fn close(self) {
        self.stop.cancel();
        self.server.await.unwrap().unwrap();
    }
}

// The board's document through MCP on a live coordinator: refused until the board has a
// Doc(); written, read with numbered lines and edited by several line edits at once; a
// stale rev or an overlap refused with nothing changed; each change one project.update
// record with its author. A program that still draws a Slot is told what replaced it, and a
// plan edit that takes a step the board names says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_board_document_through_mcp() {
    let f = Fixture::new().await;
    f.call(
        "project_create",
        json!({"name":"p","description":"","resources":{}}),
    )
    .await;
    let refused = f.result("board_doc_read", json!({"project":"p"})).await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(
        refused
            .to_string()
            .contains("this board's program has no Doc()"),
        "{refused}"
    );
    let slot = f
        .result(
            "board_set",
            json!({"project":"p","program":"root = Stack([a])\na = Slot(\"phase\")"}),
        )
        .await;
    assert!(
        slot.to_string().contains("line 2: Slot was replaced by Doc: put the slots' text in the board's document (board_doc_write) and a Doc() where they were"),
        "{slot}"
    );
    f.call(
        "plan_edit",
        json!({"project":"p","rev":1,"reason":"plan","ops":[{"op":"step.add","step":"tests-main",
            "spec":{"run":"fixture.echo","paused":true,"in":{"value":{"default":1}}}}]}),
    )
    .await;
    let set = f
        .call(
            "board_set",
            json!({"project":"p","program":"root = Stack([doc, red])\ndoc = Doc()\nred = Metric(\"Red\", \"SELECT count(*) FROM steps WHERE project_id = ? AND step_id = 'tests-main'\")"}),
        )
        .await;
    assert_eq!(set, json!({"rev": 1, "warnings": []}));
    let empty = f.call("board_doc_read", json!({"project":"p"})).await;
    assert_eq!(
        empty,
        json!({"rev":0,"updated_at":null,"author":null,"markdown":"","numbered":""})
    );
    let written = f
        .call(
            "board_doc_write",
            json!({"project":"p","markdown":"## Phase\nGreen soon.\n## Asks\n- one\n- two\n","reason":"first words","author":"orchestrator"}),
        )
        .await;
    assert_eq!(written, json!({"rev": 1, "changed": true}));
    let read = f.call("board_doc_read", json!({"project":"p"})).await;
    assert_eq!(
        read["numbered"],
        "     1\t## Phase\n     2\tGreen soon.\n     3\t## Asks\n     4\t- one\n     5\t- two\n"
    );
    assert_eq!(read["author"], "orchestrator");
    assert!(read["updated_at"].is_string(), "{read}");
    let edited = f
        .call(
            "board_doc_edit",
            json!({"project":"p","expected_rev":1,"reason":"lanes moved","author":"lane-7","edits":[
                {"start":2,"end":2,"text":"Green: **11** red left.\nLanes cut."},
                {"start":4,"end":4,"text":""},
                {"start":1,"end":0,"text":"# Release"},
                {"start":6,"end":5,"text":"## Figments\n- none"}]}),
        )
        .await;
    assert_eq!(edited, json!({"rev": 2, "changed": true}));
    let after = "# Release\n## Phase\nGreen: **11** red left.\nLanes cut.\n## Asks\n- two\n## Figments\n- none\n";
    assert_eq!(
        f.call("board_doc_read", json!({"project":"p"})).await["markdown"],
        after
    );
    // A stale rev, an overlap and a range past the end are refused; nothing changes.
    for (edits, rev, says) in [
        (json!([{"start":1,"end":1,"text":"x"}]), 1, "\"conflict\""),
        (
            json!([{"start":1,"end":2,"text":"x"},{"start":2,"end":2,"text":"y"}]),
            2,
            "edits[1] (start 2, end 2) overlaps edits[0] (start 1, end 2)",
        ),
        (
            json!([{"start":2,"end":12,"text":"x"}]),
            2,
            "end is past the last line (8)",
        ),
    ] {
        let refused = f
            .result(
                "board_doc_edit",
                json!({"project":"p","expected_rev":rev,"edits":edits}),
            )
            .await;
        assert_eq!(refused["isError"], true, "{refused}");
        assert!(refused.to_string().contains(says), "{says}: {refused}");
    }
    let stale = f
        .result(
            "board_doc_write",
            json!({"project":"p","markdown":"x","expected_rev":1}),
        )
        .await;
    assert!(stale.to_string().contains("\"current_rev\":2"), "{stale}");
    let read = f.call("board_doc_read", json!({"project":"p"})).await;
    assert_eq!(
        (&read["rev"], &read["markdown"]),
        (&json!(2), &json!(after))
    );
    // One project.update record per change, with its author and reason.
    let log = f
        .call(
            "log_read",
            json!({"project":"p","kinds":["project.update"]}),
        )
        .await;
    let updates: Vec<(Value, Value, Value)> = log["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["fields"].clone(),
                r["author"].clone(),
                r["reason"].clone(),
            )
        })
        .collect();
    assert_eq!(
        updates,
        [
            (
                json!(["board_doc"]),
                json!("orchestrator"),
                json!("first words")
            ),
            (json!(["board_doc"]), json!("lane-7"), json!("lanes moved")),
        ]
    );
    // The board's own rev did not move: a button pressed on it stays valid.
    assert_eq!(f.call("board_get", json!({"project":"p"})).await["rev"], 1);
    // Removing the step the Metric names is made, and says so.
    let removed = f
        .call(
            "step_remove",
            json!({"project":"p","steps":"tests-main","reason":"renamed"}),
        )
        .await;
    assert_eq!(
        removed["board_warnings"],
        json!(["line 3: Metric names step `tests-main`, which is not in the plan"])
    );
    // A board without a Doc() refuses the document tools.
    f.call(
        "board_set",
        json!({"project":"p","program":"root = Units()"}),
    )
    .await;
    let refused = f
        .result("board_doc_write", json!({"project":"p","markdown":"x"}))
        .await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert!(
        refused
            .to_string()
            .contains("this board's program has no Doc()"),
        "{refused}"
    );
    f.close().await;
}
