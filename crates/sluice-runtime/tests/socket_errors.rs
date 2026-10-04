//! The coordinator answers a request it cannot decode with an error reply on the same
//! connection instead of closing it unanswered (a panic in the work is contained the same
//! way; `contain`'s unit test covers that).
#[allow(dead_code)]
#[path = "../../../tests/support/home.rs"]
mod home;
use serde_json::{Value, json};
use sluice_model::{error::PublicError, rpc::JsonMap};
use sluice_process::{
    guardian::{AdoptionAttempt, AdoptionHost, FnHost, GuardianPresence},
    socket,
};
use sluice_runtime::{
    coordinator::Coordinator,
    dispatch::Catalog,
    execution::{ExecutionHost, Launch, LaunchOutcome},
};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Fake;
impl FnHost for Fake {
    async fn invoke(&self, _: sluice_model::rpc::FnInvocation) -> Result<JsonMap, PublicError> {
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

async fn ask(home: &std::path::Path, request: Value) -> Value {
    let mut stream = UnixStream::connect(home.join("coordinator.sock"))
        .await
        .unwrap();
    socket::write_frame(&mut stream, &request).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), socket::read_frame(&mut stream))
        .await
        .expect("a reply, not a hang")
        .expect("a reply, not a closed connection")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undecodable_requests_get_error_replies() {
    let home = home::ScratchHome::new().unwrap();
    let broker = Coordinator::open(home.path().into(), Catalog::fixtures(), Fake)
        .await
        .unwrap();
    let stop = CancellationToken::new();
    let server = tokio::spawn({
        let (broker, stop) = (broker.clone(), stop.clone());
        async move { broker.serve(stop).await }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(home.path().join("coordinator.sock"))
        .await
        .is_err()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "socket never served"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // A public command with a field it does not know.
    let reply = ask(
        home.path(),
        json!({"protocol":1,"request_id":"bad","run_capability":null,
               "command":{"command":"status","args":{"project":{"kind":"name","value":"p"},
               "selection":{"steps":null,"tags":null},"bogus":true}}}),
    )
    .await;
    assert_eq!(reply["request_id"], "bad");
    assert_eq!(reply["result"]["status"], "error", "{reply}");
    assert_eq!(reply["result"]["value"]["error"], "bad_request", "{reply}");
    // A runtime command that does not decode gets the runtime envelope.
    let reply = ask(
        home.path(),
        json!({"protocol":1,"request_id":"rt","run_capability":null,
               "command":{"runtime":"no_such","args":{}}}),
    )
    .await;
    assert_eq!(reply["request_id"], "rt");
    assert!(reply["result"]["Err"].is_object(), "{reply}");
    // An unsupported protocol is refused, not dropped.
    let reply = ask(
        home.path(),
        json!({"protocol":99,"request_id":"old","run_capability":null,
               "command":{"command":"projects_list"}}),
    )
    .await;
    assert_eq!(reply["result"]["value"]["error"], "conflict", "{reply}");
    // The coordinator keeps serving after refusing.
    let reply = ask(
        home.path(),
        json!({"protocol":1,"request_id":"after","run_capability":null,
               "command":{"command":"projects_list"}}),
    )
    .await;
    assert_eq!(reply["result"]["status"], "ok", "{reply}");
    stop.cancel();
    server.await.unwrap().unwrap();
}

/// A coordinator that accepts and never answers (still starting, adopting runs, or
/// wedged) is a clear retryable error for every client request, never an endless wait.
#[tokio::test]
async fn a_silent_coordinator_is_a_bounded_error_not_a_hang() {
    use sluice_model::{
        RuntimeApi, commands::CommandRequest, events::ChangeCursor, ids::RecordSeq,
    };
    use sluice_runtime::client::CoordinatorClient;
    let home = home::ScratchHome::new().unwrap();
    let listener = tokio::net::UnixListener::bind(home.path().join("coordinator.sock")).unwrap();
    let held = tokio::spawn(async move {
        let mut open = Vec::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            open.push(stream);
        }
    });
    let mut client = CoordinatorClient::new(home.path());
    client.reply_timeout = Duration::from_millis(200);
    let silent = |error: PublicError| {
        assert!(
            matches!(&error, PublicError::Busy { message, retryable: true } if message.contains("did not answer within")),
            "{error:?}"
        );
    };
    let began = std::time::Instant::now();
    silent(
        client
            .command(CommandRequest::ProjectsList)
            .await
            .unwrap_err(),
    );
    silent(
        client
            .changes(ChangeCursor {
                after: RecordSeq(0),
                projects: vec![],
            })
            .await
            .unwrap_err(),
    );
    silent(client.acquire_scheduler().await.unwrap_err());
    assert!(began.elapsed() < Duration::from_secs(5));
    held.abort();
}

#[test]
fn long_polls_keep_their_own_wait_on_top_of_the_reply_limit() {
    use sluice_runtime::client::reply_limit;
    let base = Duration::from_secs(120);
    let command = |value: Value| serde_json::from_value(value).unwrap();
    assert_eq!(
        reply_limit(base, &command(json!({"command":"projects_list"}))),
        base
    );
    assert_eq!(
        reply_limit(
            base,
            &command(
                json!({"command":"log_wait","args":{"read":{"project":null,"since_seq":null,"kinds":null,"threads":null,"limit":10},"timeout_seconds":300,"questions_only":false}})
            )
        ),
        base + Duration::from_secs(300)
    );
    assert_eq!(
        reply_limit(
            base,
            &command(
                json!({"command":"fn_call","args":{"name":"core.echo","inputs":{},"project":null,"wait_seconds":null,"direct":true,"author":null}})
            )
        ),
        base + Duration::from_secs(sluice_runtime::calls::MAX_WAIT_SECONDS)
    );
    assert_eq!(
        reply_limit(
            base,
            &command(
                json!({"command":"fn_call","args":{"name":"core.echo","inputs":{},"project":null,"wait_seconds":7,"direct":false,"author":null}})
            )
        ),
        base + Duration::from_secs(7)
    );
}
