use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest},
    error::PublicError,
    ids::{ProjectId, ProjectSelector},
};
use sluice_store::{
    ReadPool, RetrySafety, Writer,
    projects::{self, CreateProject, EmptyPlanInitializer, NoResourceSettings},
};
use sluice_web::{
    http,
    mcp::CommandService,
    views::{DashboardState, EmptyCatalog, PageState},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

struct Commands;
impl CommandService for Commands {
    fn command(&self, request: CommandRequest) -> BoxFuture<'_, Result<CommandReply, PublicError>> {
        Box::pin(async move {
            match request {
                CommandRequest::ProjectsList => Ok(CommandReply::Projects(vec![])),
                CommandRequest::PlanGet { .. } => Err(PublicError::NotFound {
                    message: "no such project".into(),
                }),
                _ => Ok(CommandReply::Ack),
            }
        })
    }
}
struct Fixture {
    _home: tempfile::TempDir,
    writer: Writer,
    app: Router,
    id: ProjectId,
    stop: CancellationToken,
}
impl Fixture {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let writer = Writer::open(home.path()).unwrap();
        let project = writer
            .write(RetrySafety::NonIdempotent, |tx| {
                projects::project_create(
                    tx,
                    CreateProject {
                        name: "web".parse().unwrap(),
                        description: "scratch HTTP project".into(),
                        icon: None,
                        resources: None,
                        author: "test".into(),
                    },
                    &EmptyPlanInitializer,
                    &NoResourceSettings,
                )
            })
            .await
            .unwrap();
        let pages = PageState::new(DashboardState::new(
            ReadPool::open(home.path(), 2).unwrap(),
            Arc::new(EmptyCatalog),
        ));
        let stop = CancellationToken::new();
        let app = http::router(pages, Arc::new(Commands), stop.clone());
        Self {
            _home: home,
            writer,
            app,
            id: project.project_id,
            stop,
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        host: &str,
        origin: Option<&str>,
        content_type: Option<&str>,
        body: Body,
    ) -> axum::response::Response {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, host);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin)
        }
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type)
        }
        self.app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap()
    }
}
async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &to_bytes(response.into_body(), http::MAX_BODY * 2)
            .await
            .unwrap(),
    )
    .unwrap()
}
#[tokio::test]
async fn methods_content_types_and_shared_errors() {
    let fixture = Fixture::new().await;
    let response = fixture
        .request(
            "GET",
            "/api/tools/projects_list",
            "localhost",
            None,
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(json_body(response).await["error"], "bad_request");
    let response = fixture
        .request(
            "POST",
            "/api/tools/projects_list",
            "localhost",
            None,
            Some("text/plain"),
            Body::from("{}"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(json_body(response).await["error"], "bad_request");
    let response = fixture
        .request(
            "POST",
            "/api/tools/plan_get",
            "localhost",
            None,
            Some("application/json; charset=utf-8"),
            Body::from(r#"{"project":"missing"}"#),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await,
        json!({"error":"not_found","message":"no such project"})
    );
    for body in [
        r#"{"project":"a","project":"b"}"#,
        r#"{"project":"a","bogus":true}"#,
        r#"[1]"#,
        r#"{"project": "#,
    ] {
        let response = fixture
            .request(
                "POST",
                "/api/tools/plan_get",
                "localhost",
                None,
                Some("application/json"),
                Body::from(body),
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["error"], "bad_request");
    }
    let response = fixture
        .request(
            "POST",
            "/api/tools/projects_list",
            "localhost",
            None,
            Some("application/json"),
            Body::from("{}"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await, json!([]));
    fixture.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn host_and_origin_checks_cover_pages_writes_and_mcp() {
    let fixture = Fixture::new().await;
    for host in [
        "evil.example",
        "localhost.evil",
        "127.0.0.1.evil",
        "0.0.0.0",
        "localhost:invalid",
    ] {
        let response = fixture
            .request("GET", "/", host, None, None, Body::empty())
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{host}");
        assert_eq!(json_body(response).await["error"], "bad_request");
    }
    for origin in [
        "http://evil.example",
        "null",
        "https://localhost",
        "http://localhost:9999",
        "http://localhost/path",
        "http://owner@localhost",
        "http://localhost?x",
    ] {
        let response = fixture
            .request(
                "POST",
                "/api/tools/projects_list",
                "localhost",
                Some(origin),
                Some("application/json"),
                Body::from("{}"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origin}");
    }
    for host in [
        "localhost",
        "127.0.0.1",
        "[::1]",
        "localhost:1234",
        "[::1]:1234",
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse().unwrap());
        assert!(http::valid_origin(&headers), "{host}");
        headers.insert(header::ORIGIN, format!("http://{host}").parse().unwrap());
        assert!(http::valid_origin(&headers), "{host}");
        headers.append(header::ORIGIN, "http://localhost".parse().unwrap());
        assert!(!http::valid_origin(&headers));
    }
    let response = fixture
        .request(
            "GET",
            "/mcp",
            "localhost",
            Some("http://evil.example"),
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    fixture.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn streamed_bodies_are_bounded_and_requests_have_server_ids() {
    let fixture = Fixture::new().await;
    // No Content-Length: enforcing only that header would let this body through.
    for path in ["/api/tools/projects_list", "/mcp"] {
        let body = if path == "/mcp" {
            Body::from(vec![b'a'; http::MAX_BODY + 1])
        } else {
            Body::from_stream(futures_util::stream::iter([
                Ok::<_, std::io::Error>(vec![b'a'; http::MAX_BODY]),
                Ok(vec![b'b']),
            ]))
        };
        let response = fixture
            .request(
                "POST",
                path,
                "localhost",
                None,
                Some("application/json"),
                body,
            )
            .await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(response.headers().get("x-request-id").is_some());
        assert_eq!(json_body(response).await["error"], "bad_request");
    }
    let request = Request::builder()
        .uri("/")
        .header(header::HOST, "localhost")
        .header("x-request-id", "caller-id")
        .body(Body::empty())
        .unwrap();
    let response = fixture.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_ne!(response.headers()["x-request-id"], "caller-id");
    assert_eq!(
        response.headers()[header::X_CONTENT_TYPE_OPTIONS],
        "nosniff"
    );
    fixture.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn canonical_urls_survive_rename_and_stale_names_do_not_redirect() {
    let fixture = Fixture::new().await;
    let response = fixture
        .request(
            "GET",
            "/projects/web/log?kind=message",
            "localhost",
            None,
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers()[header::LOCATION],
        format!("/projects/id/{}/log?kind=message", fixture.id)
    );
    let id = fixture.id;
    fixture
        .writer
        .write(RetrySafety::NonIdempotent, move |tx| {
            projects::project_update(
                tx,
                &ProjectSelector::Id(id),
                projects::UpdateProject {
                    new_name: Some("renamed".parse().unwrap()),
                    author: "test".into(),
                    ..Default::default()
                },
                &NoResourceSettings,
            )
        })
        .await
        .unwrap();
    let response = fixture
        .request(
            "GET",
            "/projects/web",
            "localhost",
            None,
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = fixture
        .request(
            "GET",
            "/projects/renamed/settings",
            "localhost",
            None,
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(
        response.headers()[header::LOCATION],
        format!("/projects/id/{}/settings", fixture.id)
    );
    let response = fixture
        .request(
            "GET",
            &format!("/projects/id/{}", fixture.id),
            "localhost",
            None,
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(response.into_body(), http::MAX_BODY)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("renamed"));
    fixture.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn embedded_assets_and_unknown_routes() {
    let fixture = Fixture::new().await;
    let response = fixture
        .request(
            "GET",
            "/static/dashboard.css",
            "localhost",
            None,
            None,
            Body::empty(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/css")
    );
    for path in ["/missing", "/static/missing.js", "/projects/missing"] {
        let response = fixture
            .request("GET", path, "localhost", None, None, Body::empty())
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(json_body(response).await["error"], "not_found");
    }
    fixture.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn shutdown_ends_open_page_streams() {
    let fixture = Fixture::new().await;
    let response = fixture
        .request("GET", "/stream", "localhost", None, None, Body::empty())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    fixture.stop.cancel();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        to_bytes(response.into_body(), http::MAX_BODY),
    )
    .await
    .unwrap()
    .unwrap();
    fixture.writer.shutdown().await.unwrap();
}

async fn broker_fixture(
    home: &std::path::Path,
    count: usize,
    handler: impl Fn(CommandRequest) -> CommandReply + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::UnixListener::bind(home.join("coordinator.sock")).unwrap();
    tokio::spawn(async move {
        for _ in 0..count {
            let (mut stream, _) = listener.accept().await.unwrap();
            let length = stream.read_u32().await.unwrap() as usize;
            assert!(length <= sluice_model::rpc::MAX_FRAME_BYTES);
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes).await.unwrap();
            let request: sluice_model::rpc::RpcRequest =
                sluice_model::rpc::decode_json(&bytes).unwrap();
            let reply = sluice_model::rpc::RpcReply {
                protocol: request.protocol,
                request_id: request.request_id,
                result: sluice_model::rpc::RpcResult::Ok(Box::new(handler(request.command))),
            };
            stream
                .write_all(&sluice_model::rpc::encode_frame(&reply).unwrap())
                .await
                .unwrap();
        }
    })
}
#[tokio::test]
async fn catalog_is_loaded_only_from_the_broker_and_keeps_exact_port_types() {
    use sluice_web::views::{CatalogSource, board::RegistrySource};
    let fixture = Fixture::new().await;
    let id = fixture.id;
    let broker = broker_fixture(fixture._home.path(), 3, move |request| match request {
        CommandRequest::ProjectsList => CommandReply::Projects(vec![sluice_model::commands::ProjectIdentity {project_id:id,name:"web".parse().unwrap()}]),
        CommandRequest::FnList { .. } => CommandReply::Data(json!([
            {"name":"custom.work","scope":"project","doc":"Work","inputs":{"prompt":{"type":"string","doc":"Prompt"}},"outputs":{"ready":"boolean"},"submits":{"result":{"type":"string","doc":"Result"}}},
            {"name":"broken.fn","scope":"project","error":"Invalid manifest"}
        ]).try_into().unwrap()),
        _ => panic!("unexpected broker request"),
    }).await;
    let catalog = http::SocketCatalog::default();
    catalog
        .refresh(&sluice_runtime::client::CoordinatorClient::new(
            fixture._home.path(),
        ))
        .await
        .unwrap();
    broker.await.unwrap();
    let view = catalog.catalog(Some(id)).unwrap();
    assert_eq!(view.entries[0].inputs[0].ty, "string");
    assert_eq!(view.entries[1].error, "Invalid manifest");
    assert_eq!(view.version.len(), 64);
    let exact = catalog.signatures(id).unwrap();
    assert_eq!(
        exact.functions[0].1.submits["result"].doc.as_deref(),
        Some("Result")
    );
    assert_eq!(
        exact.functions[0].1.outputs["ready"],
        sluice_model::types::Type::Boolean
    );
    fixture.writer.shutdown().await.unwrap();
}
#[tokio::test]
async fn owner_actions_send_the_displayed_revision_and_author_through_the_socket() {
    use sluice_web::views::step::{Action, CommandService, OwnerCommand};
    let home = tempfile::tempdir().unwrap();
    let id = ProjectId::new();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = requests.clone();
    let broker = broker_fixture(home.path(), 3, move |request| {
        captured.lock().unwrap().push(request);
        CommandReply::Ack
    })
    .await;
    let commands =
        http::SocketOwnerCommands(sluice_runtime::client::CoordinatorClient::new(home.path()));
    for action in [Action::Retry, Action::Cancel, Action::PauseProject] {
        commands
            .execute(OwnerCommand {
                project: id,
                step: if action == Action::PauseProject {
                    None
                } else {
                    Some("work".parse().unwrap())
                },
                action,
                revision: 7,
                message: "continue".into(),
                author: "owner",
            })
            .await
            .unwrap();
    }
    broker.await.unwrap();
    let requests = requests.lock().unwrap();
    let CommandRequest::StepRetry(retry) = &requests[0] else {
        panic!()
    };
    assert_eq!(retry.expected_rev, Some(sluice_model::ids::Revision(7)));
    assert_eq!(retry.message.as_deref(), Some("continue"));
    assert_eq!(retry.author.as_deref(), Some("owner"));
    let CommandRequest::StepCancel(cancel) = &requests[1] else {
        panic!()
    };
    assert_eq!(cancel.expected_rev, Some(sluice_model::ids::Revision(7)));
    let CommandRequest::ProjectUpdate(update) = &requests[2] else {
        panic!()
    };
    assert_eq!(
        update.expected_settings_rev,
        Some(sluice_model::ids::Revision(7))
    );
    assert_eq!(update.paused, Some(true));
}
