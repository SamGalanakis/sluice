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
    commands::{CommandReply, CommandRequest, IconUpload, StepStatus},
    error::PublicError,
    ids::{MessageId, RecordSeq, Revision},
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
        "board_set",
        "board_slot_set",
        "board_get",
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
        "step_settle",
        "plan_history",
        "plan_set_input",
        "step_set_input",
        "step_set_output",
        "step_retry",
        "step_submit",
        "step_progress",
        "log_read",
        "log_wait",
        "step_wait",
        "next",
        "drain",
        "release",
        "step_context",
        "query",
        "verify",
        "plan_view",
        "status",
        "plan_prune",
        "ask",
        "say",
        "reply",
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
    for (args, subtree) in [
        (
            json!({"project":"p","steps":"one","subtree":true,"reason":"hold"}),
            true,
        ),
        (json!({"project":"p","steps":"one"}), false),
    ] {
        let CommandRequest::StepPause(request) = decode("step_pause", args).unwrap() else {
            panic!()
        };
        assert_eq!(request.subtree, subtree);
        assert!(request.paused);
    }
    let CommandRequest::ProjectCreate { icon, .. } = decode(
        "project_create",
        json!({"name":"p","icon":"/srv/icons/p.svg"}),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(icon, Some(IconUpload::Text("/srv/icons/p.svg".into())));
    // step_progress takes the project as any tool does, and no author: it writes no record.
    let run = "019a2b3c-4d5e-7f01-8234-56789abcdef0";
    let CommandRequest::StepProgress(request) = decode(
        "step_progress",
        json!({"project":"p","step":"tests-main","run":run,"outputs":{"red":3}}),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(request.project, "p".parse().unwrap());
    assert_eq!(
        (request.step.as_str(), request.run.to_string()),
        ("tests-main", run.into())
    );
    assert!(
        decode(
            "step_progress",
            json!({"project":"p","step":"s","run":run,"outputs":{},"author":"x"})
        )
        .is_err()
    );
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
        (
            "step_wait",
            json!({"project":"p","steps":"a","until":"settled","timeout":3601}),
        ),
        ("next", json!({"timeout":3601})),
    ] {
        let request = decode(tool, args).unwrap();
        match request {
            CommandRequest::FnCall(r) => assert_eq!(r.wait_seconds, Some(3600)),
            CommandRequest::LogWait(r) => assert_eq!(r.timeout_seconds, 3600),
            CommandRequest::StepWait(r) => assert_eq!(r.timeout_seconds, 3600),
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
    let CommandRequest::LogWait(wait) = decode(
        "log_wait",
        json!({"project":"p","kinds":["step.status","message"],"statuses":["failed","stale"],"recipients":["orchestrator"]}),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        wait.read.statuses,
        Some(vec![StepStatus::Failed, StepStatus::Stale])
    );
    assert_eq!(wait.read.recipients, Some(vec!["orchestrator".to_owned()]));
    assert!(decode("log_read", json!({"statuses":["finished"]})).is_err());
    // step_wait: flat steps (one or a list) or tags, a default timeout, until passed as given.
    let CommandRequest::StepWait(wait) = decode(
        "step_wait",
        json!({"project":"p","steps":"a","until":{"any_of":["failed"]}}),
    )
    .unwrap() else {
        panic!()
    };
    assert_eq!(wait.timeout_seconds, 300);
    assert_eq!(wait.selection.steps, Some(vec!["a".parse().unwrap()]));
    assert_eq!(wait.until.as_value(), &json!({"any_of":["failed"]}));
    assert!(decode("step_wait", json!({"project":"p","tags":"unit:a"})).is_err());
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
    assert_eq!(tools.len(), 49);
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
    eprintln!("stdio fixture PID {pid} reaped; 47 tools");
}

/// ask, say and reply speak as the orchestrator over MCP (or the run they name): no
/// `owner` argument; ask and say require `to`, and a call without one decodes so the
/// coordinator refuses it as `invalid`. Reading may be done as the owner.
#[test]
fn message_verbs_take_a_required_recipient_and_no_owner_over_mcp() {
    let schema = |name: &str| {
        mcp::tools()
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("no tool {name}"))
            .input_schema
            .clone()
    };
    for name in ["ask", "say", "reply"] {
        let s = schema(name);
        assert!(s["properties"].get("owner").is_none(), "{name}");
        assert!(s["properties"].get("thread").is_none(), "{name}");
        assert!(s["properties"].get("from").is_none(), "{name}");
        assert!(s["properties"].get("run").is_some(), "{name}");
        assert!(
            matches!(
                mcp::decode_tool(
                    name,
                    serde_json::from_value(json!({"project":"p","owner":true,"body":"x"})).unwrap(),
                    None
                ),
                Err(PublicError::BadRequest { .. })
            ),
            "{name}"
        );
    }
    for name in ["ask", "say"] {
        let s = schema(name);
        assert!(
            s["required"].as_array().unwrap().contains(&json!("to")),
            "{name}"
        );
        assert!(s["properties"]["to"].get("default").is_none(), "{name}");
        let to = match mcp::decode_tool(
            name,
            serde_json::from_value(json!({"project":"p","body":"x"})).unwrap(),
            None,
        )
        .unwrap()
        {
            CommandRequest::Say(say) => say.to,
            CommandRequest::Ask(ask) => ask.to,
            other => panic!("{name}: {other:?}"),
        };
        assert_eq!(to, "", "{name}: left for the coordinator to refuse");
    }
    assert!(schema("messages")["properties"].get("owner").is_some());
    assert!(mcp::tools().iter().all(|t| t.name != "message_post"));
    assert!(matches!(
        mcp::decode_tool("message_post", serde_json::Map::new(), None),
        Err(PublicError::BadRequest { .. })
    ));
}

/// A string of decimal digits is the integer wherever the command's schema wants one (message
/// ids, seqs, limits, waits, revisions); a string field keeps its digits; anything else is
/// refused naming the field.
#[test]
fn decimal_strings_are_taken_for_integer_arguments() {
    let Ok(CommandRequest::Reply(reply)) = decode(
        "reply",
        json!({"project":"p","to_message":"24771","body":"x"}),
    ) else {
        panic!("reply")
    };
    assert_eq!(reply.to_message, MessageId(24771));
    let Ok(CommandRequest::LogRead(read)) =
        decode("log_read", json!({"since_seq":"5","limit":"10"}))
    else {
        panic!("log_read")
    };
    assert_eq!((read.since_seq, read.limit), (Some(RecordSeq(5)), 10));
    let Ok(CommandRequest::FnCall(call)) = decode("fn_call", json!({"name":"x","wait":"60"}))
    else {
        panic!("fn_call")
    };
    assert_eq!(call.wait_seconds, Some(60));
    let Ok(CommandRequest::Next(next)) = decode("next", json!({"since_seq":"7","timeout":"5000"}))
    else {
        panic!("next")
    };
    assert_eq!(
        (next.since_seq, next.timeout_seconds),
        (RecordSeq(7), mcp::WAIT_CAP)
    );
    let Ok(CommandRequest::StepAdd(add)) = decode(
        "step_add",
        json!({"project":"p","step":"s","spec":{},"rev":"3"}),
    ) else {
        panic!("step_add")
    };
    assert_eq!(add.edit.expected, Some(Revision(3)));
    let Ok(CommandRequest::StepWait(wait)) = decode(
        "step_wait",
        json!({"project":"p","steps":"a","until":"settled","timeout":"30"}),
    ) else {
        panic!("step_wait")
    };
    assert_eq!(wait.timeout_seconds, 30);
    let Ok(CommandRequest::LogWait(wait)) = decode(
        "log_wait",
        json!({"since_seq":"9","statuses":["failed"],"recipients":["orchestrator"],"timeout":"1"}),
    ) else {
        panic!("log_wait")
    };
    assert_eq!(
        (wait.read.since_seq, wait.timeout_seconds),
        (Some(RecordSeq(9)), 1)
    );
    let Ok(CommandRequest::Say(say)) = decode("say", json!({"project":"p","to":"12","body":"34"}))
    else {
        panic!("say")
    };
    assert_eq!((say.to.as_str(), say.body.as_str()), ("12", "34"));
    for (name, args, field) in [
        (
            "reply",
            json!({"project":"p","to_message":"abc"}),
            "to_message",
        ),
        ("log_read", json!({"limit":"-1"}), "limit"),
        ("fn_call", json!({"name":"x","wait":"1m"}), "wait"),
    ] {
        let Err(PublicError::BadRequest { message }) = decode(name, args) else {
            panic!("{name}")
        };
        assert!(message.starts_with(field), "{name}: {message}");
    }
}

/// An unknown tool or argument is named with the nearest valid names, from the schema.
#[test]
fn unknown_tools_and_arguments_suggest_the_nearest_names() {
    let message = |name: &str, args: Value| match decode(name, args) {
        Err(PublicError::BadRequest { message }) => message,
        other => panic!("{name}: {other:?}"),
    };
    assert_eq!(
        message("step_contxt", json!({})),
        "unknown tool step_contxt; did you mean step_context?"
    );
    assert_eq!(
        message("step_submit", json!({"output":{}})),
        "step_submit takes no argument 'output'; did you mean outputs?"
    );
    assert_eq!(
        message("reply", json!({"project":"p","message":1})),
        "reply takes no argument 'message'; did you mean to_message?"
    );
    assert!(
        message("status", json!({"project":"p","zzzzzz":1}))
            .starts_with("status takes no argument 'zzzzzz'; its arguments are project, ")
    );
    assert_eq!(
        message("step_wiat", json!({})),
        "unknown tool step_wiat; did you mean step_wait?"
    );
    assert_eq!(
        message(
            "step_wait",
            json!({"project":"p","steps":"a","untill":"settled"})
        ),
        "step_wait takes no argument 'untill'; did you mean until?"
    );
    assert_eq!(
        message("log_read", json!({"status":["failed"]})),
        "log_read takes no argument 'status'; did you mean statuses?"
    );
    assert_eq!(
        message("nothing_like_it", json!({})),
        "unknown tool nothing_like_it"
    );
}
