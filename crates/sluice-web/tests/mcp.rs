use futures_util::future::BoxFuture;
use rmcp::{
    ServiceExt,
    model::{
        CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
        ReadResourceRequestParams,
    },
    transport::StreamableHttpClientTransport,
};
use serde_json::{Value, json};
use sluice_model::{
    commands::{CommandReply, CommandRequest, IconUpload},
    error::PublicError,
    ids::Revision,
};
use sluice_web::mcp::{self, CommandService, McpServer};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Commands(Mutex<Vec<CommandRequest>>);
impl CommandService for Commands {
    fn command(&self, request: CommandRequest) -> BoxFuture<'_, Result<CommandReply, PublicError>> {
        self.0.lock().unwrap().push(request.clone());
        Box::pin(async move {
            match request {
                CommandRequest::ProjectsList => Ok(CommandReply::Projects(vec![])),
                CommandRequest::PlanGet { .. } => Err(PublicError::NotFound {
                    message: "missing project".into(),
                }),
                CommandRequest::PlanPatch(_) => Err(PublicError::Conflict {
                    message: "plan is at rev 7".into(),
                    current_rev: Some(Revision(7)),
                }),
                CommandRequest::StepSetInput(_) => Err(PublicError::Invalid {
                    message: "invalid inputs".into(),
                    errors: vec!["steps.a.in.n: expected int".into()],
                }),
                _ => Ok(CommandReply::Ack),
            }
        })
    }
}
fn decode(name: &str, value: Value) -> Result<CommandRequest, PublicError> {
    mcp::decode_tool(
        name,
        value.as_object().unwrap().clone(),
        Some("fixture-client"),
    )
}
#[test]
fn every_v2_tool_has_a_shared_strict_schema() {
    let tools = mcp::tools();
    let expected = [
        "docs",
        "projects_list",
        "project_create",
        "project_update",
        "project_delete",
        "fn_list",
        "fn_get",
        "fn_save",
        "fn_call",
        "call_status",
        "plan_get",
        "plan_patch",
        "step_add",
        "recipe_list",
        "unit_add",
        "unit_tag",
        "edge_add",
        "edge_remove",
        "step_update",
        "step_remove",
        "step_pause",
        "step_cancel",
        "plan_history",
        "plan_set_input",
        "step_set_input",
        "step_set_output",
        "step_retry",
        "step_submit",
        "log_read",
        "log_wait",
        "next",
        "drain",
        "release",
        "step_context",
        "query",
        "verify",
        "plan_view",
        "status",
        "plan_prune",
        "message_post",
        "messages",
    ];
    assert_eq!(
        tools.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>(),
        expected
    );
    for tool in tools {
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert!(tool.description.as_ref().is_some_and(|d| !d.is_empty()));
        assert!(
            decode(&tool.name, json!({"bogus":true})).is_err(),
            "{}",
            tool.name
        );
        let props = &tool.input_schema["properties"];
        assert!(
            props.get("edit").is_none()
                && props.get("selection").is_none()
                && props.get("read").is_none()
        );
    }
    let schema = |name: &str| &tools.iter().find(|t| t.name == name).unwrap().input_schema;
    assert_eq!(
        schema("step_set_input")["properties"]["inputs"]["$ref"],
        "#/$defs/JsonMap"
    );
    assert!(
        schema("step_set_input")["properties"]
            .get("input")
            .is_none()
    );
    assert!(schema("step_retry")["properties"].get("message").is_some());
    assert!(
        schema("project_update")["properties"]
            .get("new_name")
            .is_some()
    );
    assert!(
        schema("project_delete")["required"]
            .as_array()
            .unwrap()
            .contains(&json!("confirm_name"))
    );
    assert!(
        schema("plan_patch")["required"]
            .as_array()
            .unwrap()
            .contains(&json!("rev"))
    );
    assert!(schema("unit_add")["properties"].get("tags").is_some());
    assert!(schema("unit_add")["properties"].get("inputs").is_some());
    assert!(
        mcp::tools()
            .iter()
            .find(|t| t.name == "plan_patch")
            .unwrap()
            .description
            .as_ref()
            .unwrap()
            .contains("rev: the revision you read")
    );
}
#[test]
fn flat_edits_decode_to_the_folded_commands() {
    let request = decode("unit_add",json!({"project":"p","recipe":"work","unit":"one","params":{},"after":{},"tags":["unit"],"inputs":{"entry":{"prompt":"hi"}},"rev":4,"dry_run":true,"reason":"preview","author":"reviewer"})).unwrap();
    let CommandRequest::UnitAdd(request) = request else {
        panic!()
    };
    assert_eq!(request.tags, ["unit"]);
    assert!(request.inputs.contains_key("entry"));
    assert!(request.start && request.edit.dry_run);
    assert_eq!(request.edit.expected, Some(Revision(4)));
    assert_eq!(request.edit.author.as_deref(), Some("reviewer"));
    let CommandRequest::StepSetInput(request) = decode(
        "step_set_input",
        json!({"project":"p","steps":"one","inputs":{"prompt":"hi"},"dry_run":true}),
    )
    .unwrap() else {
        panic!()
    };
    assert!(request.edit.dry_run);
    assert_eq!(request.selection.steps.unwrap()[0].as_str(), "one");
    let CommandRequest::StepRetry(request) = decode(
        "step_retry",
        json!({"project":"p","tags":["unit"],"message":"continue","author":"owner","expected_rev":4}),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(request.expected_rev, Some(Revision(4)));
    assert_eq!(request.message.as_deref(), Some("continue"));
    assert_eq!(request.author.as_deref(), Some("owner"));
    assert!(
        decode(
            "step_set_input",
            json!({"project":"p","input":"prompt","value":"hi"})
        )
        .is_err()
    );
    assert!(decode("plan_patch", json!({"project":"p"})).is_err());
}
#[test]
fn waits_are_capped_and_negative_waits_are_refused() {
    let CommandRequest::FnCall(call) = decode(
        "fn_call",
        json!({"name":"core.echo","inputs":{},"wait":3600}),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(call.wait_seconds, Some(3600));
    for (tool, args) in [
        (
            "fn_call",
            json!({"name":"core.echo","inputs":{},"wait":3601}),
        ),
        ("log_wait", json!({"timeout":3601})),
        ("next", json!({"timeout":3601})),
    ] {
        let request = decode(tool, args).unwrap();
        match request {
            CommandRequest::FnCall(r) => assert_eq!(r.wait_seconds, Some(3600)),
            CommandRequest::LogWait(r) => assert_eq!(r.timeout_seconds, 3600),
            CommandRequest::Next(r) => assert_eq!(r.timeout_seconds, 3600),
            _ => panic!(),
        };
    }
    assert!(decode("fn_call", json!({"name":"core.echo","wait":-1})).is_err());
    let CommandRequest::LogWait(wait) = decode(
        "log_wait",
        json!({"timeout":3,"project":"p","limit":10,"wake":"questions"}),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(wait.timeout_seconds, 3);
    assert_eq!(wait.read.limit, 10);
    assert!(wait.questions_only);
}
#[tokio::test]
async fn errors_keep_the_common_envelope_and_authorship() {
    let commands = Arc::new(Commands::default());
    let server = McpServer::new(commands.clone());
    for (name, args, code) in [
        ("plan_get", json!({"project":"missing"}), "not_found"),
        (
            "plan_patch",
            json!({"project":"p","rev":1,"ops":[],"reason":"stale","author":"editor"}),
            "conflict",
        ),
        (
            "step_set_input",
            json!({"project":"p","steps":["a"],"inputs":{"n":"x"}}),
            "invalid",
        ),
        ("projects_list", json!({"extra":true}), "bad_request"),
    ] {
        let result = server
            .call(name, args.as_object().unwrap().clone(), Some("client"))
            .await;
        assert_eq!(result.is_error, Some(true));
        let error = result.structured_content.unwrap();
        assert_eq!(error["error"], code);
        assert_eq!(
            serde_json::from_str::<Value>(&result.content[0].as_text().unwrap().text).unwrap(),
            error
        );
        if code == "conflict" {
            assert_eq!(error["current_rev"], 7)
        }
        if code == "invalid" {
            assert_eq!(error["errors"], json!(["steps.a.in.n: expected int"]))
        }
    }
    let recorded = commands.0.lock().unwrap();
    let CommandRequest::PlanPatch(request) = &recorded[1] else {
        panic!()
    };
    assert_eq!(request.author.as_deref(), Some("editor"));
    assert_eq!(request.reason, "stale");
}
#[test]
fn docs_are_embedded_and_topic_names_cannot_read_files() {
    assert_eq!(mcp::docs(None).unwrap()["types"], "Types");
    assert!(
        mcp::docs(Some("types"))
            .unwrap()
            .as_str()
            .unwrap()
            .starts_with("# Types\n")
    );
    assert!(matches!(
        mcp::docs(Some("../../../etc/passwd")),
        Err(PublicError::NotFound { .. })
    ));
}
#[test]
fn icons_round_trip_with_store_validation() {
    use sluice_store::projects::{ICON_MAX, Icon};
    let upload: IconUpload = serde_json::from_value(json!("🌊")).unwrap();
    assert_eq!(Icon::try_from(upload).unwrap(), Icon::text("🌊").unwrap());
    let icon = Icon::image(b"GIF89a".to_vec()).unwrap();
    assert_eq!(
        Icon::try_from(IconUpload::from(icon.clone())).unwrap(),
        icon
    );
    for value in [
        json!({"media_type":"image/png","bytes_base64":"R0lGODlh"}),
        json!({"media_type":"image/gif","bytes_base64":"??"}),
        json!({"media_type":"image/gif","bytes_base64":"A".repeat(ICON_MAX.div_ceil(3)*4+1)}),
    ] {
        assert!(Icon::try_from(serde_json::from_value::<IconUpload>(value).unwrap()).is_err());
    }
}

fn client_config() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("fixture-client", "1"),
    )
    .with_protocol_version(ProtocolVersion::V_2025_03_26)
}
async fn exercise(client: &rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>) {
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 41);
    let result = client
        .call_tool(CallToolRequestParams::new("projects_list"))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true));
    assert_eq!(result.structured_content, Some(json!({"result":[]})));
    let resources = client.list_all_resources().await.unwrap();
    assert!(resources.iter().any(|r| r.uri == "sluice://docs/types"));
    let page = client
        .read_resource(ReadResourceRequestParams::new("sluice://docs/types"))
        .await
        .unwrap();
    assert!(
        serde_json::to_value(page).unwrap()["contents"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("# Types")
    );
}
#[tokio::test]
async fn rmcp_client_initializes_lists_and_reads_over_http() {
    let stop = CancellationToken::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().nest_service(
        "/mcp",
        mcp::http_service(McpServer::new(Arc::new(Commands::default())), stop.clone()),
    );
    let shutdown = stop.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap()
    });
    let client = client_config()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "http://{address}/mcp"
        )))
        .await
        .unwrap();
    exercise(&client).await;
    client.cancel().await.unwrap();
    stop.cancel();
    task.await.unwrap();
}
/// This test executable is the isolated stdio child, launched by the test below.
#[test]
#[ignore = "stdio child entry point"]
fn stdio_fixture() {
    if std::env::var_os("SLUICE_STDIO_FIXTURE").is_none() {
        return;
    }
    println!("\nSLUICE_STDIO_READY");
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(mcp::serve_stdio(McpServer::new(Arc::new(
            Commands::default(),
        ))))
        .unwrap();
    std::process::exit(0);
}
#[tokio::test]
async fn rmcp_client_initializes_lists_and_reads_over_process_stdio() {
    let home = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "stdio_fixture", "--ignored", "--nocapture"])
        .env("SLUICE_STDIO_FIXTURE", "1")
        .env("SLUICE_HOME", home.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    // Consume the Rust test harness preamble before handing the real pipes to rmcp.
    loop {
        let mut line = String::new();
        assert_ne!(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                stdout.read_line(&mut line)
            )
            .await
            .unwrap()
            .unwrap(),
            0
        );
        if line.trim() == "SLUICE_STDIO_READY" {
            break;
        }
    }
    let client = client_config().serve((stdout, stdin)).await.unwrap();
    exercise(&client).await;
    client.cancel().await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    eprintln!("stdio fixture PID {pid} reaped; 41 tools");
}

#[tokio::test]
#[ignore = "built binary acceptance on an explicitly supplied scratch URL"]
async fn real_boot_acceptance() {
    let Some(url) = std::env::var("SLUICE_ACCEPTANCE_URL").ok() else {
        return;
    };
    let base = url.strip_suffix("/mcp").unwrap();
    assert!(!base.ends_with(":3065"));
    let client = client_config()
        .serve(StreamableHttpClientTransport::from_uri(url.clone()))
        .await
        .unwrap();
    exercise(&client).await;
    eprintln!(
        "Real boot MCP tool count: {}",
        client.list_all_tools().await.unwrap().len()
    );
    let args = |value: Value| value.as_object().unwrap().clone();
    let created = client
        .call_tool(
            CallToolRequestParams::new("project_create").with_arguments(args(
                json!({"name":"web","description":"HTTP acceptance","author":"acceptance"}),
            )),
        )
        .await
        .unwrap();
    assert_ne!(created.is_error, Some(true), "{created:?}");
    let identity = created.structured_content.unwrap();
    let id = identity["project_id"].as_str().unwrap();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for path in [
        "/".to_owned(),
        "/fns".into(),
        "/log".into(),
        "/inbox".into(),
        format!("/projects/id/{id}"),
        format!("/projects/id/{id}/settings"),
        format!("/projects/id/{id}/log"),
        format!("/projects/id/{id}/inbox"),
        "/static/dashboard.css".into(),
    ] {
        let response = http.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
    }
    let response = http
        .get(format!("{base}/projects/web/settings"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response.headers()["location"],
        format!("/projects/id/{id}/settings")
    );
    let settings = http
        .get(format!("{base}/projects/id/{id}/settings"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let revision = settings
        .split("data-revision=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let changed = http
        .post(format!("{base}/projects/id/{id}/settings"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "field=description&value=Saved+through+the+socket&expected_settings_rev={revision}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), reqwest::StatusCode::OK);
    let html = changed.text().await.unwrap();
    assert!(html.contains("Saved through the socket"));
    let next_revision = html
        .split("data-revision=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let uploaded = http
        .post(format!(
            "{base}/projects/id/{id}/settings/icon?expected_settings_rev={next_revision}"
        ))
        .body(b"GIF89a".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), reqwest::StatusCode::OK);
    let icon = http
        .get(format!("{base}/projects/id/{id}/icon?generation=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(icon.status(), reqwest::StatusCode::OK);
    assert_eq!(icon.bytes().await.unwrap().as_ref(), b"GIF89a");
    let stale = http
        .post(format!("{base}/projects/id/{id}/settings"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!(
            "field=description&value=Stale&expected_settings_rev={revision}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let renamed = client
        .call_tool(
            CallToolRequestParams::new("project_update").with_arguments(args(
                json!({"project":"web","new_name":"renamed","author":"acceptance"}),
            )),
        )
        .await
        .unwrap();
    assert_ne!(renamed.is_error, Some(true), "{renamed:?}");
    assert_eq!(
        http.get(format!("{base}/projects/web"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        http.get(format!("{base}/projects/id/{id}"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    let response = http
        .post(format!("{base}/api/tools/projects_list"))
        .header("origin", "http://evil.example")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    client.cancel().await.unwrap();
}
