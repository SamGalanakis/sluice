//! MCP and tool adapters use the same typed commands as the CLI and Unix broker.
use crate::tool_args;
use futures_util::future::BoxFuture;
use rmcp::{
    ErrorData, RoleServer, ServerHandler, ServiceExt,
    model::*,
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Map, Value, json};
use sluice_model::{
    RuntimeApi,
    commands::{CommandReply, CommandRequest},
    error::PublicError,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const WAIT_CAP: u64 = 3600;
pub trait CommandService: Send + Sync {
    fn command(&self, request: CommandRequest) -> BoxFuture<'_, Result<CommandReply, PublicError>>;
}
impl<T: RuntimeApi> CommandService for T {
    fn command(&self, request: CommandRequest) -> BoxFuture<'_, Result<CommandReply, PublicError>> {
        Box::pin(RuntimeApi::command(self, request))
    }
}
#[derive(Clone)]
pub struct McpServer {
    commands: Arc<dyn CommandService>,
}
impl McpServer {
    pub fn new(commands: Arc<dyn CommandService>) -> Self {
        Self { commands }
    }
    pub async fn call(
        &self,
        name: &str,
        args: Map<String, Value>,
        client: Option<&str>,
    ) -> CallToolResult {
        let result = async {
            let request = request_for_tool(self.commands.as_ref(), name, args, client).await?;
            if let CommandRequest::Docs { topic } = request {
                return docs(topic.as_deref());
            }
            let wait = match &request {
                CommandRequest::FnCall(r) => r.wait_seconds.unwrap_or(0),
                CommandRequest::LogWait(r) => r.timeout_seconds,
                CommandRequest::StepWait(r) => r.timeout_seconds,
                CommandRequest::Next(r) => r.timeout_seconds.saturating_add(r.settle_max_seconds),
                _ => 0,
            };
            let reply = tokio::time::timeout(
                Duration::from_secs(wait.saturating_add(30)),
                self.commands.command(request),
            )
            .await
            .map_err(|_| PublicError::Busy {
                message: "command deadline exceeded; inspect status before repeating a write"
                    .into(),
                retryable: false,
            })??;
            reply_value(reply)
        }
        .await;
        match result {
            Ok(Value::String(text)) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Ok(value) => {
                let structured = if value.is_object() {
                    value.clone()
                } else {
                    json!({"result":value})
                };
                let mut result =
                    CallToolResult::success(vec![ContentBlock::text(value.to_string())]);
                result.structured_content = Some(structured);
                result
            }
            Err(error) => {
                CallToolResult::structured_error(serde_json::to_value(error).expect("public error"))
            }
        }
    }
}

/// The HTTP tool endpoint uses the same command decoder and reply shaping as MCP.
pub async fn call_http(
    server: &McpServer,
    name: &str,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::{
        body::to_bytes,
        http::{StatusCode, header},
        response::IntoResponse,
    };
    let status_error = |status, message: &str| (status, axum::Json(bad(message))).into_response();
    if !request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|s| {
            s.split(';')
                .next()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return status_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "expected application/json",
        );
    }
    let bytes = match to_bytes(request.into_body(), crate::http::MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return status_error(StatusCode::PAYLOAD_TOO_LARGE, "request body exceeds 1 MiB"),
    };
    let args: sluice_model::rpc::JsonMap = match sluice_model::rpc::decode_json(&bytes) {
        Ok(args) => args,
        Err(error) => return crate::http::error_response(error),
    };
    let value = serde_json::to_value(args)
        .expect("arguments")
        .as_object()
        .expect("argument map")
        .clone();
    let result = server.call(name, value, Some("http")).await;
    if result.is_error == Some(true) {
        let error: PublicError =
            serde_json::from_value(result.structured_content.expect("structured public error"))
                .expect("public error");
        crate::http::error_response(error)
    } else {
        let text = result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|c| c.text.clone())
            .unwrap_or_default();
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => axum::Json(value).into_response(),
            Err(_) => text.into_response(),
        }
    }
}

/// Public tool arguments are flat; nested `edit`, `selection`, and `read` are internal only.
pub fn decode_tool(
    name: &str,
    mut args: Map<String, Value>,
    client: Option<&str>,
) -> Result<CommandRequest, PublicError> {
    if !tools().iter().any(|tool| tool.name == name) {
        return Err(tool_args::unknown_tool(
            name,
            tools().iter().map(|tool| tool.name.as_ref()),
            "",
        ));
    }
    let schema = tool_schema(name).expect("an offered tool's schema");
    let properties = schema["properties"].as_object().expect("tool properties");
    if let Some(key) = args.keys().find(|key| !properties.contains_key(*key)) {
        return Err(tool_args::unknown_argument(
            name,
            key,
            properties.keys().map(String::as_str),
        ));
    }
    tool_args::coerce_integers(schema, &mut args)?;
    for (key, schema) in properties {
        if !args.contains_key(key)
            && let Some(default) = schema.get("default")
        {
            args.insert(key.clone(), default.clone());
        }
    }
    if properties.contains_key("author") {
        let given = args.get("author").and_then(Value::as_str);
        args.insert("author".into(), json!(author_of(given, client, "mcp")));
    }
    if name == "log_wait"
        && let Some(wake) = args.get_mut("wake")
    {
        *wake = match wake.as_str() {
            Some("any") => json!(false),
            Some("questions") => json!(true),
            _ => return Err(bad("wake must be any or questions")),
        };
    }
    for field in ["steps", "tags", "projects", "state", "units", "status"] {
        if let Some(value) = args.get_mut(field)
            && value.is_string()
        {
            *value = json!([value.clone()]);
        }
    }
    for (public, internal) in renames(name) {
        if let Some(value) = args.remove(*public) {
            args.insert((*internal).into(), value);
        }
    }
    if name != "step_submit"
        && let Some(project) = args.get_mut("project")
        && !project.is_null()
    {
        let selector = project
            .as_str()
            .ok_or_else(|| bad("project must be a current name or id:<uuid>"))?
            .parse::<sluice_model::ids::ProjectSelector>()
            .map_err(|e| bad(e.to_string()))?;
        *project = serde_json::to_value(selector).expect("project selector");
    }
    if let Some(projects) = args.get_mut("projects")
        && !projects.is_null()
    {
        let projects = projects
            .as_array_mut()
            .ok_or_else(|| bad("projects must be an array"))?;
        for project in projects {
            let selector = project
                .as_str()
                .ok_or_else(|| bad("project must be a current name or id:<uuid>"))?
                .parse::<sluice_model::ids::ProjectSelector>()
                .map_err(|e| bad(e.to_string()))?;
            *project = serde_json::to_value(selector).expect("project selector");
        }
    }
    let root = command_schema();
    let raw = argument_schema(root, name);
    for field in ["edit", "selection", "read"] {
        if raw["properties"].get(field).is_some() {
            let nested = resolve(root, &raw["properties"][field]);
            let mut values = Map::new();
            for key in nested["properties"]
                .as_object()
                .expect("nested properties")
                .keys()
            {
                if let Some(value) = args.remove(key) {
                    values.insert(key.clone(), value);
                }
            }
            args.insert(field.into(), Value::Object(values));
        }
    }
    for field in [
        "wait_seconds",
        "timeout_seconds",
        "settle_seconds",
        "settle_max_seconds",
    ] {
        if let Some(value) = args.get_mut(field)
            && !value.is_null()
        {
            let number = value
                .as_u64()
                .ok_or_else(|| bad(format!("{field} must be a nonnegative integer")))?;
            *value = json!(number.min(WAIT_CAP));
        }
    }
    let value = if name == "projects_list" {
        json!({"command":name})
    } else {
        json!({"command":name,"args":args})
    };
    let request: CommandRequest =
        sluice_model::rpc::decode_json(&serde_json::to_vec(&value).expect("tool command"))?;
    request.check_plan_arguments()?;
    Ok(request)
}

/// Resolve the mutable project name once before constructing an id-scoped submission.
pub async fn request_for_tool(
    commands: &dyn CommandService,
    name: &str,
    mut args: Map<String, Value>,
    client: Option<&str>,
) -> Result<CommandRequest, PublicError> {
    if name == "step_submit"
        && let Some(project) = args.get("project").and_then(Value::as_str)
    {
        let project = project.to_owned();
        if let Some(id) = project.strip_prefix("id:") {
            args.insert("project".into(), json!(id));
        } else if project.parse::<sluice_model::ids::ProjectId>().is_err() {
            let reply = tokio::time::timeout(
                Duration::from_secs(30),
                commands.command(CommandRequest::ProjectsList),
            )
            .await
            .map_err(|_| bad("project resolution timed out"))??;
            let projects = reply_value(reply)?;
            let id = projects
                .as_array()
                .and_then(|projects| projects.iter().find(|p| p["name"] == project))
                .and_then(|p| p.get("project_id"))
                .cloned()
                .ok_or_else(|| PublicError::NotFound {
                    message: format!("project {project} not found"),
                })?;
            args.insert("project".into(), id);
        }
    }
    decode_tool(name, args, client)
}

pub fn author_of(given: Option<&str>, client: Option<&str>, fallback: &str) -> String {
    let env = std::env::var("SLUICE_AUTHOR").ok();
    let step = std::env::var("SLUICE_STEP")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("step:{}", s.trim()));
    [
        given,
        env.as_deref(),
        step.as_deref(),
        client,
        Some(fallback),
    ]
    .into_iter()
    .flatten()
    .find(|s| !s.trim().is_empty())
    .unwrap_or(fallback)
    .trim()
    .into()
}
pub fn reply_value(reply: CommandReply) -> Result<Value, PublicError> {
    sluice_runtime::compose::reply_value(reply)
}
fn bad(message: impl Into<String>) -> PublicError {
    PublicError::BadRequest {
        message: message.into(),
    }
}
/// The wire schema of every command (`CommandRequest`), its `$defs` included.
pub fn command_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::to_value(schemars::schema_for!(CommandRequest)).expect("command schema")
    })
}
fn resolve<'a>(root: &'a Value, value: &'a Value) -> &'a Value {
    if let Some(reference) = value.get("$ref").and_then(Value::as_str) {
        &root["$defs"][reference
            .strip_prefix("#/$defs/")
            .expect("local schema reference")]
    } else {
        value
    }
}
fn argument_schema<'a>(root: &'a Value, name: &str) -> &'a Value {
    let variant = root["oneOf"]
        .as_array()
        .expect("command variants")
        .iter()
        .find(|v| v["properties"]["command"]["const"] == name)
        .expect("registered command");
    resolve(root, &variant["properties"]["args"])
}
/// The public names MCP gives some wire fields (`rev` for an edit's `expected`, `wait` for
/// `wait_seconds`, ...), as (public, wire) pairs.
pub fn renames(name: &str) -> &'static [(&'static str, &'static str)] {
    match name {
        "fn_call" => &[("wait", "wait_seconds")],
        "fn_save" => &[("fn", "manifest")],
        "log_wait" => &[("timeout", "timeout_seconds"), ("wake", "questions_only")],
        "step_wait" => &[("timeout", "timeout_seconds")],
        "next" => &[
            ("timeout", "timeout_seconds"),
            ("settle", "settle_seconds"),
            ("settle_max", "settle_max_seconds"),
        ],
        "plan_prune" => &[("older_than", "older_than_seconds"), ("rev", "expected")],
        "step_add" | "unit_add" | "step_update" | "step_remove" | "step_pause" | "unit_tag"
        | "edge_add" | "edge_remove" | "step_set_input" | "plan_set_input" => {
            &[("rev", "expected")]
        }
        _ => &[],
    }
}
fn default_value(name: &str, key: &str) -> Option<Value> {
    if matches!(name, "plan_edit" | "unit_update" | "unit_remove") && key == "reason" {
        return None;
    }
    match key {
        "reason" | "description" => Some(json!("")),
        "dry_run" | "force" | "direct" | "all" | "brief" => Some(json!(false)),
        "view" if name == "status" => Some(json!("steps")),
        "start" | "paused" if name == "step_pause" || key == "start" => Some(json!(true)),
        "params" if name == "query" => Some(json!([])),
        "questions_only" => Some(json!("any")),
        "params" | "resources" | "after" | "inputs"
            if matches!(name, "unit_add" | "project_create" | "fn_call") =>
        {
            Some(json!({}))
        }
        "add" | "remove" | "tags" if name == "unit_tag" || name == "unit_add" => Some(json!([])),
        "preview_scope" => Some(json!("impact")),
        "compact" if name == "plan_read" => Some(json!(true)),
        "compact" if matches!(name, "step_get" | "unit_get") => Some(json!(false)),
        "limit" => Some(json!(200)),
        "timeout_seconds" => Some(json!(300)),
        "older_than_seconds" | "since_seq" if name == "plan_prune" || name == "next" => {
            Some(json!(0))
        }
        "settle_seconds" => Some(json!(20)),
        "settle_max_seconds" => Some(json!(120)),
        "settles" => Some(json!("short")),
        "me" => Some(json!("orchestrator")),
        "projects" if name == "next" => Some(json!([])),
        "format" => Some(json!("mermaid")),
        _ => None,
    }
}
/// Definitions are generated from the canonical commands, including p1c's folded fields.
pub fn tools() -> &'static [Tool] {
    static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        DESCRIPTIONS
            .iter()
            .map(|(name, description)| tool_definition(name, description))
            .collect()
    })
}
fn tool_definition(name: &str, description: &str) -> Tool {
    let schema = tool_schema(name).expect("registered command");
    Tool::new(
        name.to_owned(),
        description.to_owned(),
        schema.as_object().expect("object schema").clone(),
    )
}
/// The flat public argument schema of any command, by its tool name: the input schema MCP
/// lists for the tools it offers, and the same shape for the commands it does not (which
/// `sluice tool` still runs). `properties`, `required`, `additionalProperties: false` and the
/// `$defs` they reference.
pub fn tool_schema(name: &str) -> Option<&'static Value> {
    static SCHEMAS: OnceLock<BTreeMap<String, Value>> = OnceLock::new();
    SCHEMAS
        .get_or_init(|| {
            command_schema()["oneOf"]
                .as_array()
                .expect("command variants")
                .iter()
                .filter_map(|v| v["properties"]["command"]["const"].as_str())
                .map(|name| (name.to_owned(), public_schema(name)))
                .collect()
        })
        .get(name)
}
/// Every field of a command's wire arguments by name, the nested `edit`, `selection` and
/// `read` objects flattened, each with its schema (references into `command_schema`'s
/// `$defs`).
pub fn wire_fields(name: &str) -> Map<String, Value> {
    fn flatten(root: &Value, schema: &Value, out: &mut Map<String, Value>) {
        let Some(fields) = schema["properties"].as_object() else {
            return;
        };
        for (key, value) in fields {
            if matches!(key.as_str(), "edit" | "selection" | "read") {
                flatten(root, resolve(root, value), out);
            } else {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    let root = command_schema();
    let mut out = Map::new();
    if root["oneOf"].as_array().is_some_and(|vs| {
        vs.iter()
            .any(|v| v["properties"]["command"]["const"] == name)
    }) {
        flatten(root, argument_schema(root, name), &mut out);
    }
    out
}
fn public_schema(name: &str) -> Value {
    let root = command_schema();
    let raw = argument_schema(root, name);
    let mut properties = Map::new();
    let mut required = Vec::new();
    schema_fields(root, name, raw, &mut properties, &mut required);
    let mut definitions = root["$defs"].clone();
    let mut id = root["$defs"]["ProjectId"].clone();
    id.as_object_mut().expect("id schema").remove("format");
    if let Some(pattern) = id["pattern"].as_str() {
        id["pattern"] = json!(format!("^id:{}", pattern.trim_start_matches('^')));
    }
    definitions["ProjectSelector"] = json!({"anyOf":[root["$defs"]["ProjectName"].clone(),id],"description":"Current project name or id:<uuid>. Stale names return not_found."});
    if name == "step_submit" {
        properties.insert("project".into(), definitions["ProjectSelector"].clone());
    }
    let needed = reachable_definitions(&Value::Object(properties.clone()), &definitions);
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false,"$defs":needed})
}
fn schema_fields(
    root: &Value,
    name: &str,
    schema: &Value,
    properties: &mut Map<String, Value>,
    required: &mut Vec<Value>,
) {
    let Some(fields) = schema["properties"].as_object() else {
        return;
    };
    for (key, value) in fields {
        // Only the dashboard speaks as the owner; MCP callers read as it at most.
        if key == "owner" && matches!(name, "ask" | "say" | "reply") {
            continue;
        }
        if matches!(key.as_str(), "edit" | "selection" | "read") {
            schema_fields(root, name, resolve(root, value), properties, required);
            continue;
        }
        let mut value = value.clone();
        let public = renames(name)
            .iter()
            .find(|(_, internal)| *internal == key)
            .map_or(key.as_str(), |(public, _)| *public);
        // ask and say name their recipient: required here, though a call without
        // one decodes, so the store refuses it as `invalid` like an unknown one.
        if key == "to" && matches!(name, "ask" | "say") {
            value.as_object_mut().expect("to schema").remove("default");
            required.push(json!(key));
            properties.insert(key.clone(), value);
            continue;
        }
        let default = default_value(name, key).or_else(|| value.get("default").cloned());
        if let Some(default) = default {
            value["default"] = default;
        } else if schema["required"]
            .as_array()
            .is_some_and(|r| r.contains(&json!(key)))
        {
            required.push(json!(public));
        }
        if key == "questions_only" {
            value = json!({"type":"string","enum":["any","questions"],"default":"any"});
        }
        if matches!(
            key.as_str(),
            "steps" | "tags" | "projects" | "state" | "units" | "status"
        ) {
            let default = value.get("default").cloned();
            value = json!({"anyOf":[value,{"type":"string"}]});
            if let Some(default) = default {
                value["default"] = default;
            }
        }
        properties.insert(public.into(), value);
    }
}

fn reachable_definitions(value: &Value, definitions: &Value) -> Map<String, Value> {
    fn visit(value: &Value, definitions: &Value, found: &mut Map<String, Value>) {
        match value {
            Value::Object(map) => {
                if let Some(name) = map
                    .get("$ref")
                    .and_then(Value::as_str)
                    .and_then(|reference| reference.strip_prefix("#/$defs/"))
                    && !found.contains_key(name)
                {
                    let definition = definitions[name].clone();
                    found.insert(name.into(), definition.clone());
                    visit(&definition, definitions, found);
                }
                for value in map.values() {
                    visit(value, definitions, found);
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, definitions, found);
                }
            }
            _ => {}
        }
    }
    let mut found = Map::new();
    visit(value, definitions, &mut found);
    found
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("sluice", env!("CARGO_PKG_VERSION")))
        .with_instructions(doc_page("instructions").unwrap_or("sluice runs plans"))
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools().to_vec(),
            ttl_ms: Some(0),
            cache_scope: Some(rmcp::model::CacheScope::Private),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let peer = context.client_info();
        let client = peer.as_ref().map(|info| info.name.as_str());
        Ok(self
            .call(&request.name, request.arguments.unwrap_or_default(), client)
            .await
            .into())
    }
    async fn list_resources(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: DOCS
                .iter()
                .map(|(topic, page)| {
                    Resource::new(format!("sluice://docs/{topic}"), *topic)
                        .with_description(title(page))
                        .with_mime_type("text/markdown")
                })
                .collect(),
            ..Default::default()
        })
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let page = request
            .uri
            .strip_prefix("sluice://docs/")
            .and_then(doc_page)
            .ok_or_else(|| ErrorData::resource_not_found("no such docs resource", None))?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(page, request.uri).with_mime_type("text/markdown"),
        ])
        .into())
    }
}
pub fn http_service(
    server: McpServer,
    stop: CancellationToken,
) -> StreamableHttpService<McpServer, LocalSessionManager> {
    let mut config = StreamableHttpServerConfig::default();
    config.cancellation_token = stop;
    config.max_request_body_bytes = crate::http::MAX_BODY;
    config.stateless_protocol_metadata_required = false;
    config.json_response = true;
    StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    )
}
pub async fn serve_stdio(server: McpServer) -> Result<(), PublicError> {
    let service = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| bad(e.to_string()))?;
    service.waiting().await.map_err(|e| bad(e.to_string()))?;
    Ok(())
}
fn title(page: &str) -> &str {
    page.lines()
        .find(|s| !s.trim().is_empty())
        .unwrap_or("")
        .trim_start_matches('#')
        .trim()
}
fn doc_page(topic: &str) -> Option<&'static str> {
    DOCS.iter()
        .find(|(name, _)| *name == topic)
        .map(|(_, page)| *page)
}
pub fn docs(topic: Option<&str>) -> Result<Value, PublicError> {
    match topic {
        Some(topic) => doc_page(topic)
            .map(|s| json!(s))
            .ok_or_else(|| PublicError::NotFound {
                message: format!("no docs topic {topic}"),
            }),
        None => Ok(Value::Object(
            DOCS.iter()
                .map(|(topic, page)| ((*topic).into(), json!(title(page))))
                .collect(),
        )),
    }
}

const DOCS: &[(&str, &str)] = sluice_runtime::docs::PAGES;

const DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "docs",
        "Read sluice's documentation for agents.\n\nArgs:\n    topic: a page name such as \"plans\", \"types\", \"fns\" or \"examples\". Leave it out to\n        get the index: {topic: first heading} for every page.",
    ),
    (
        "projects_list",
        "List the live projects by name: [{project_id, name, description, rev, settings_rev,\ncounts, archived, paused, board_rev, resources?, icon?}]; settings_rev is what project_update\nand project_delete take as expected_settings_rev; board_rev is the board's revision (0 before\nany board; board_set takes it as expected_rev); counts maps step status -> number of steps in\nthe project's plan; resources,\nwhen it declares any, maps each to {\"capacity\": n} or {\"capacity_fn\": fn}; icon, when\nthe project has one, is {\"kind\": \"image\", \"type\": <content type>} or {\"kind\": \"text\",\n\"text\": <text>}.",
    ),
    (
        "project_create",
        "Create a project with an empty plan (rev 1). Returns {project_id, name}.\n\nArgs:\n    name: lowercase letters, digits, - and _ (starting with a letter or digit).\n    description: what the project is for; put any context an orchestrator needs here.\n    icon: the project's icon: an absolute path to an image file (SVG, PNG, WebP, JPEG\n        or GIF, at most 256 KB, copied into the project's row), or a short\n        text icon (an emoji; at most 16 characters).\n    resources: named capacities its steps' `needs` draw on (docs(\"plans\")):\n        {name: n} or {name: {\"capacity\": n}} for a fixed capacity (an integer >= 0),\n        {name: {\"capacity_fn\": \"<fn>\"}} for one a fn the project sees returns as\n        {capacity}, called about every 10 s, e.g. {\"lane\": 56, \"cpu\":\n        {\"capacity_fn\": \"cpu-free\"}}.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "project_update",
        "Update a project by current name or id:<uuid>, including new_name, description, icon, resources, paused, archived, prune_done_after or prune_keep. prune_done_after (seconds, null turns it off) retires done units automatically once their last step finished that long ago: sluice runs plan_prune itself, every 5 minutes at most, as one edit by \"sluice\", keeping referenced units; set it so finished lanes retire themselves (e.g. 21600 for 6 h). prune_keep is a list of unit-name patterns (* any run, ? one character) it never removes; [] or null clears it. Read both with query: SELECT prune_done_after, prune_keep FROM projects. icon is a short text icon, an absolute (or ~/) path to an SVG, PNG, WebP, JPEG or GIF file of at most 256 KB, or {media_type, bytes_base64}; \"\" removes it. expected_settings_rev (projects_list's settings_rev) fences stale settings. Each change records author and reason. Returns project_id and current name.",
    ),
    (
        "project_delete",
        "Delete an archived project and its owned state. confirm_name must equal the current name and expected_settings_rev must match its settings_rev in projects_list. Refused while any step, call or guardian is live. Returns project_id, name and deleted.",
    ),
    (
        "fn_list",
        "List the functions a project sees (built-in, global, then the project's own; without\na project: built-in and global). Returns [{name, doc, inputs, outputs, scope, submits?,\nicon?, open?}]. A function with a problem (bad fn.json, name collision) is left out; verify\nreports it.\n\nArgs:\n    project: the project whose functions to list.",
    ),
    (
        "fn_get",
        "Return one function's fn.json plus where it lives. Returns the fn.json with {scope, path}\nadded (path is its directory, null for a built-in).\n\nArgs:\n    name: the function name, e.g. \"git.head\".\n    project: look it up as this project sees it (project functions included).",
    ),
    (
        "fn_save",
        "Create or replace a function: writes fns/<name>/fn.json and main.py into the project\n(or, without one, the global fns dir) and republishes the functions. Returns {name, scope,\npath, generation}. Read docs(\"fns\") first.\n\nArgs:\n    fn: the fn.json: {name, doc?, inputs: {name: type}, outputs: {name: type}}. Checked\n        before anything is written; the name may not collide with a built-in or global\n        function (nor, for a global function, with any project's own).\n    main_py: the Python source of main.py (a uv script: `from sluice_fn import run`, then\n        `run(main)`).\n    project: the project that owns it; leave out for a global function.",
    ),
    (
        "fn_call",
        "Run one function outside the plan. Returns {call, project_id, status, inputs, outputs,\nerror, direct}; status is pending, running, succeeded or failed. Poll call_status(call) for\na slow one; every status change is also a `call` record in the log (log_read).\n\nArgs:\n    name: the function to run.\n    inputs: an object keyed by the function's input names; checked against its types\n        before anything runs (an `invalid` error lists every mismatch with its path).\n    project: run it in this project (with the functions it sees); leave out for none.\n    wait: how many seconds to wait for the result, a number such as 60 (default 0:\n        return at once; capped at 3600).\n    direct: run it here and now, to the end, instead of queueing it for the runner\n        (for use without a runner, e.g. from the command line); ignores `wait`.\n    author: who is calling (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "call_status",
        "Return a fn_call's state. Returns {call, project_id, status, inputs, outputs, error,\ndirect, finishing?}, as fn_call does; finishing {since, submission_seq, release} is there\nwhile its run has submitted and is only finishing.\n\nArgs:\n    call: the id fn_call returned.\n    project: the project it ran in, if any.",
    ),
    (
        "board_set",
        "Set or clear the project's board: an OpenUI Lang program the dashboard draws beside the\nplan (its own section on a phone): its layout, live components (Units, StepStatus, Output,\nCount, Metric, Query, Chart, LatestMessage) filled from the project, and Doc(), where the board's\ndocument goes (board_doc_write); see docs(\"board\"). Returns {rev, warnings}: the board's\nnew revision (unchanged when the program is the one it has), and each step the program\nnames that is not in the plan, as \"line N: StepStatus names step `x`, which is not in the\nplan\", and each query that counts status 'failed' with the owner's cancels among them\n(warnings: the program is set). Name a step by \"tag:<tag>\" to survive renames. A\nstale expected_rev is a conflict; a program that does not check is invalid, each problem as\n\"line N: ...\". The change is a project.board record (author, reason, rev; not the\nprogram).\n\nArgs:\n    project: the project (in a run, the run's own).\n    program: the board program, or null to clear the board.\n    expected_rev: the board_rev you read (status, projects_list, board_get); refused if it\n        has changed since.\n    reason: why, for the record.",
    ),
    (
        "board_get",
        "Read the project's board. Returns {project, rev, program}: the project {project_id,\nname}, the board's revision (0 before any board) and its program (null when none is set).\n\nArgs:\n    project: the project.",
    ),
    (
        "board_doc_read",
        "Read the board's document: the hand-written part of the project's board, plain prose\nfor the owner that the board's Doc() draws. Read it before you edit it. Returns {rev,\nupdated_at, author, markdown, numbered}: its revision (0 and markdown \"\" before the first\nwrite), when and by whom it was last edited, its markdown, and the same text with each line\nnumbered as `cat -n` numbers it (the numbers board_doc_edit takes). Refused (invalid) when\nthe board's program has no Doc().\n\nArgs:\n    project: the project (in a run, the run's own).",
    ),
    (
        "board_doc_write",
        "Replace the board's whole document: markdown the board's Doc() draws (headings, lists,\nlinks, bold, code, quotes). It is one human-facing document: plain prose for the owner,\nwith tickets and runs only as links. Rewrite it, or edit the lines that changed\n(board_doc_edit), whenever anything it says changes. Returns {rev, changed}; the same text\nagain changes nothing. With expected_rev, a document at another rev is a conflict (with\ncurrent_rev) and nothing changes. Over 64 KiB, or a board without Doc(), is invalid. Each\nchange is a project.update record whose fields is [\"board_doc\"]; the board's own rev\nstays.\n\nArgs:\n    project: the project (in a run, the run's own).\n    markdown: the whole document.\n    expected_rev: the rev you read (board_doc_read); refused if it has changed since.\n    reason: why, for the record.\n    author: who is writing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client's\n        name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "board_doc_edit",
        "Edit lines of the board's document: read it first (board_doc_read), then replace the\nlines that changed, keeping it plain prose for the owner. Each edit {start, end, text}\nreplaces lines start..end (1-based, inclusive, numbered as board_doc_read's numbered) with\ntext (several lines, or \"\" to delete them); end = start - 1 inserts before start, and\nstart = lines + 1 appends. Edits must not overlap; all apply or none. Returns {rev,\nchanged}. A document no longer at expected_rev is a conflict (with current_rev): read it\nagain. A bad range or an overlap is invalid, naming the edit. Each change is a\nproject.update record whose fields is [\"board_doc\"].\n\nArgs:\n    project: the project (in a run, the run's own).\n    expected_rev: the rev the line numbers come from.\n    edits: [{start, end, text}], e.g. [{\"start\": 3, \"end\": 4, \"text\": \"- Lanes: 11 red\"}].\n    reason: why, for the record.\n    author: who is writing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client's\n        name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "plan_get",
        "Export the whole plan from its rows in authored order, without compiling it. Works even while the fn catalog is broken. Returns {project, rev, plan}.\n\nArgs:\n    project: the project.",
    ),
    (
        "plan_read",
        "Read a page of selected steps in position order. Compact reads do not decode declarations. Filters combine with AND; lists match any member, an empty list matches nothing. Missing filter names give an empty page. Returns {project, rev, state_epoch, recipe_generation, steps, next_cursor}. Compact steps are {id, unit, recipe, position, run, status, paused, priority}; full steps add spec and references.\n\nArgs:\n    project: the project.\n    units: unit names, or one name.\n    steps: step ids, or one id.\n    status: stored statuses, or one status (pending, running, succeeded, failed, stale, skipped).\n    recipe: units matching this recipe now.\n    compact: true by default; false includes spec and references.\n    limit: default 200, 1 to 1000, larger capped at 1000; zero is bad_request.\n    cursor: next_cursor from the previous page, with the same project and filters. A changed bound revision, status epoch or recipe generation is cursor_expired; read again without cursor.",
    ),
    (
        "step_get",
        "Read one step. Returns {project, rev, state_epoch, step}. The step has {id, unit, recipe, position, run, status, paused, priority}; a full step adds spec and references. A missing step is not_found (no step <id>).\n\nArgs:\n    project: the project.\n    step: the exact step id.\n    compact: false by default; true omits spec and references without decoding a declaration.",
    ),
    (
        "unit_get",
        "Read one unit with its entry and exit steps, completion and settlement. Returns {project, rev, state_epoch, recipe_generation, unit}. The unit is {id, recipe, entry_steps, exit_steps, done, settled, steps}; steps are in position order. A missing unit is not_found (no unit <name>).\n\nArgs:\n    project: the project.\n    unit: the unit name.\n    compact: false by default; true omits step specs and references without decoding declarations.",
    ),
    (
        "unit_update",
        "Change listed unit members by exact id, without regenerating the recipe. Empty changes or member changes are bad_request. Unknown members are invalid. Returns {project, rev, preview, steps?, board_warnings?}. With dry_run, returns the preview alone.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.\n    unit: the unit name.\n    changes: {step id: changes}; only run, in, scatter, doc, outputs, paused, after, tags, needs and priority. Null removes a key; in replaces the whole map.\n    reason: required.",
    ),
    (
        "unit_remove",
        "Remove every member of a unit atomically. A running member or surviving reference refuses the edit. Finished outcomes stay archived. Steps in the reply are removed members in position order. Returns {project, rev, preview, steps?, board_warnings?}. With dry_run, returns the preview alone.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.\n    unit: the unit name.\n    reason: required.",
    ),
    (
        "plan_edit",
        "Apply typed operations in order to one candidate, validate once, then commit all or nothing. Unknown operations or fields are bad_request; operation refusals are invalid with ops[i] paths. Empty ops, changes, step removal lists and edge lists are bad_request. Returns {project, rev, preview, steps?, board_warnings?}. With dry_run, returns the preview alone.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.\n    ops: input.put {name, declaration}, input.remove {name}, output.put {name, source}, output.remove {name}, step.add {step, spec}, step.update {step, changes}, step.remove {steps}, edge.add or edge.remove {step, after}, unit.add {recipe, unit, params?, after?, inputs?, tags?}, unit.update {unit, changes}, unit.remove {unit}, order.set {collection, ids}. order.set requires rev and each current member exactly once.\n    start: true by default; false adds new steps paused unless they specify paused themselves.\n    reason: required.",
    ),
    (
        "step_add",
        "Add one step. It starts as soon as it is ready; start=false adds it paused unless its spec sets paused. Returns {project, rev, preview, steps?, board_warnings?}. With dry_run, returns the preview alone.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.\n    step: the new step id.\n    spec: {run, in, scatter?, doc?, outputs?, paused?, after?, tags?, needs?, priority?}.\n    start: true by default.",
    ),
    (
        "recipe_list",
        "List the recipes a project sees, by name (scope global: SLUICE_HOME/recipes/, or project:\nthe project's own recipes/, which wins on a name clash). Returns [{name, doc, params,\nscope, stages, title?, view?, title_error?, view_error?}]; a broken recipe file is listed\nas {name, scope, error}. params maps each param to its type (or {type, doc}), `unit`\n(always there) included; stages are its step ids without \"{unit}-\"; title and view say\nhow its units read on the dashboard, and a view that does not check is view_error (the\nrecipe still works). Read docs(\"plans\") on recipes and titles.\n\nArgs:\n    project: the project.",
    ),
    (
        "unit_add",
        "Add one unit of work from a recipe: its steps with `{param}` filled in, each tagged\n`unit:<unit>` (and `tags`), with the edges and input overrides given, in one plan\nedit at the current rev: one call stages a whole lane. They start as soon as they are\nready; start=false adds them paused. Refused (`bad_request`) when an id it would add\nis already in the plan; `invalid` lists every param, expansion or staging problem\n(nothing is written). Returns the edit result {project, rev, preview, steps}: steps\nare the ids it added.\n\nArgs:\n    project: the project.\n    recipe: the recipe's name (recipe_list).\n    unit: the new unit name, a valid step id.\n    params: {<param>: value, ...}, checked against the recipe's param types.\n    start: false adds the new steps paused.\n    tags: more tags for every step of the unit, e.g. [\"arc:tsvm\"] (select by them in\n        status, step_pause, step_cancel, plan_prune, ...); `unit:` ones are reserved.\n    after: {suffix: [step ids]}: ids appended to that recipe step's `after` (its own\n        kept). A suffix is the recipe step's id without the leading \"<unit>-\", e.g.\n        {\"draft\": [\"a-6-publish\"]}.\n    inputs: {suffix: {input: value}}: literals bound to that step's inputs\n        ({\"default\": value}, replacing the recipe's binding), e.g.\n        {\"work\": {\"model\": {\"type\": \"normal\", \"model\": \"sol\", \"effort\": \"xhigh\"}}};\n        an input its fn does not declare and the recipe\n        does not bind is refused.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n    reason: why, recorded in the plan's history (default \"add unit <unit> (recipe\n        <recipe>)\").\n\ndry_run: preview without committing. Every committed change records author and reason. tags adds unit tags; inputs binds inputs by entry step.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.",
    ),
    (
        "unit_tag",
        "Add and/or remove tags on every step of a unit (those tagged `unit:<unit>`), in one\nplan edit at the current rev, e.g. to put units into an arc after the fact. `unit:`\ntags are reserved; an unknown unit is `not_found`. Nothing to change: no edit, the\ncurrent rev. Returns the edit result {project, rev, preview, steps}: steps are the\nunit's steps.\n\nArgs:\n    project: the project.\n    unit: the unit's name.\n    add: tags to add, e.g. [\"arc:tsvm\"].\n    remove: tags to remove.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every committed change records author and reason.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.",
    ),
    (
        "edge_add",
        "Make a step run after others: append gate entries to its `after` (deduplicated, its\nexisting entries kept) in one plan edit at the current rev, with no rev to read and no\nway to drop an entry someone else added. `step` may be `unit:<name>`: every entry step of\nthat unit. An unknown step or unit is `invalid`, and so is a cycle. Returns the edit result {project, rev, preview}: the project {project_id, name}, the new\nrev and what the edit set going (its resolved changes and the steps it starts, queues, skips or stales).\n\nArgs:\n    project: the project.\n    step: the step that waits, or unit:<name>.\n    after: the gate entries it waits for (docs(\"plans\")): a step id, `<id>?` (a skip\n        is fine too), `unit:<name>`, or a boolean output `<id>/<output>` (`!` negates).\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing (the result is then the preview {scope, changes, would_start,\nwould_queue, would_skip, would_stale, errors}). Every committed change records author and reason.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.",
    ),
    (
        "edge_remove",
        "Remove gate entries from a step's `after` in one plan edit at the current rev (the others\nkept). `step` may be `unit:<name>`: every entry step of that unit. An unknown step or unit\nis `invalid`. Returns the edit result {project, rev, preview}: the project {project_id, name}, the new\nrev and what the edit set going (its resolved changes and the steps it starts, queues, skips or stales).\n\nArgs:\n    project: the project.\n    step: the step that waits, or unit:<name>.\n    after: the entries it should no longer wait for.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing (the result is then the preview {scope, changes, would_start,\nwould_queue, would_skip, would_stale, errors}). Every committed change records author and reason.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.",
    ),
    (
        "step_update",
        "Replace each supplied step field; null removes it, in replaces the whole input map. Only run, in, scatter, doc, outputs, paused, after, tags, needs and priority are accepted. Unknown keys and empty changes are bad_request. A running step takes only paused and tags. Returns {project, rev, preview, steps?, board_warnings?}. With dry_run, returns the preview alone.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.\n    step: the step id.\n    changes: the fields to replace or remove.",
    ),
    (
        "step_remove",
        "Remove steps from a plan in one edit, selected by ids and/or tags. Refused\n(`invalid`) while a step left or a plan output still reads one, or while one runs.\nEach one that finished keeps its outcome (the `outcomes` table, see query).\nReturns the edit result {project, rev, preview, board_warnings?}: the project {project_id,\nname}, the new rev and what the edit set going (its resolved changes and the steps it starts, queues,\nskips or stales); board_warnings names each step the project's board still names that the\nedit removed (the edit is made).\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: every step carrying any of these tags.\n    reason: why, recorded in the plan's history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing (the result is then the preview {scope, changes, would_start,\nwould_queue, would_skip, would_stale, errors}). Every committed change records author and reason.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.",
    ),
    (
        "step_pause",
        "Pause or unpause steps in one edit. A paused step does not start, however ready\nits inputs, until unpaused; a running one finishes (pausing never stops it: see\nstep_cancel). Select by ids and/or tags; with subtree, also everything downstream\n(steps that read from or run after them, transitively), including those that become\nready later. Pausing what is paused already, or unpausing what is not, is no edit.\nReturns the edit result {project, rev, preview, steps}: the steps selected.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: select every step carrying any of these tags.\n    subtree: include everything downstream of the selected steps.\n    paused: true to pause, false to let them start.\n    reason: why; kept on each paused step (status shows it) and in the history.\n    author: who is editing (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).\n\ndry_run: preview without committing. Every committed change records author and reason.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.",
    ),
    (
        "step_cancel",
        "Stop running steps, selected by ids and/or tags: the runner kills their processes\nand fails each with `cancelled: <reason>`; step_retry runs them again. A pending\ncore.external step (work done outside sluice) fails the same way at once. Refused\nunless every selected step is running or a pending core.external one. Returns {ok: true}.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: every step carrying any of these tags.\n    reason: why, in their errors and `step.cancel` log records.\n    expected_rev: refuse (`conflict`) unless the plan is at this revision.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_settle",
        "Settle a finishing step on its submission: its run has stored a valid step_submit but\nis still running (a run pinned to a release from before the done signal waits out its\nagent's idle grace, refusing messages, a second submit and step_set_output meanwhile).\nThe runner stops its agent as step_cancel does, and the step succeeds with the outputs\nthe done signal would have given: the submission plus the agent fn's own (session, final,\nmodel, git of its cwd). Only a step running an agent fn itself (agent.run, agent.codex,\nagent.devin, agent.claude, agent.review, decide.llm); a fn that composes an agent returns\nits own outputs, so it is refused (`invalid`) with the way to settle it by hand:\nstep_cancel, then step_set_output. Refused (`invalid`) unless the step is running, not\nscattered, and its run has submitted. Returns {project, step, run, outputs}.\n\nArgs:\n    project: the project.\n    step: the finishing step.\n    reason: why, in the `step.settle` log record.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "plan_history",
        "Read persistent plan edits and retained plan.input, step.output and step.retry records, merged by seq oldest first. Edits contain resolved changes and reach revision 1 even after log trimming. Returns {project, entries, next_after_seq}.\n\nArgs:\n    project: the project.\n    since_rev: keep records with a greater rev.\n    after_seq: keep records with a greater seq; both filters apply together.\n    limit: default 200, 1 to 1000, larger capped at 1000; zero is bad_request.",
    ),
    (
        "plan_set_input",
        "Set a declared plan input; steps reading it can then start. Changing it later makes\nthe succeeded steps that read it (and their dependents) stale. Returns {ok: true}; with\ndry_run, the preview {scope, changes, would_start, would_queue, would_skip, would_stale, errors}.\n\nArgs:\n    project: the project.\n    name: the plan input's name.\n    value: its value, checked against the input's type.\n    rev: refuse (`conflict`) unless the plan is at this revision.\n    reason: why, recorded in the plan's history.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_set_input",
        "Bind literal inputs ({name: value}, each as {\"default\": value}) on the steps selected by steps and/or tags, in one edit at rev if supplied. Only steps whose fn has the inputs change; running steps are left alone; a succeeded step whose binding changes turns stale. Returns the edit result {project, rev, preview} with changed, running (selected but running) and unsupported ([{step, inputs}]: selected but lacking those inputs); an edit that changes no step is bad_request. dry_run returns the preview. reason and author record why and who.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.",
    ),
    (
        "step_set_output",
        "Mark a step succeeded with outputs you supply (manual: true); it is not run unless\nretried. Refused (`invalid`, naming them) while a step it reads has not succeeded or a\nplan input it reads has no value, unless force. Returns {ok: true}.\n\nArgs:\n    project: the project.\n    step: a step that is not running.\n    outputs: every output of its function, type-checked (arrays for a scattered step).\n    reason: why, recorded in the plan's history.\n    force: set it anyway although what it reads is not ready (e.g. a broken\n        upstream); the step then turns stale once those values are all there.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_retry",
        "Retry succeeded, failed or stale steps and re-arm the region they block. Returns {project,\nsteps, rearmed, stopped_at}: the steps retried, the failed, stale or pending dependents\nre-armed, and the succeeded, running or skipped steps the re-arm stopped at.\n\nArgs:\n    project: the project.\n    steps: step ids (one id is fine too).\n    tags: every step carrying any of these tags.\n    message: posted to each retried step's thread in the same edit, so its next run\n        (a resumed agent session, say) is given it.\n    reason: why, recorded in the log.\n    expected_rev: refuse (`conflict`) unless the plan is at this revision.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_submit",
        "Submit the outputs a running step declares (its `outputs`), from the agent doing\nthe step's work. Checked against the declared outputs: every required one present,\ntypes fitting, no others; `invalid` lists every mismatch with its path, so fix them\nand submit again. A valid submission is the agent's done signal: it ends its session,\nand the run submits once (a second submission is a conflict). When the step's fn exits,\nthese join its outputs; a required one never submitted fails the step. Returns\n{ok: true}.\n\nArgs:\n    project: the project.\n    step: the running step.\n    outputs: an object keyed by declared output name.\n    run: the run id (SLUICE_RUN_ID).\n    author: who is submitting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_progress",
        "Publish a running step's latest values without finishing it, from its current run: a\nstep that never ends (a rolling test run, say) shows its latest result this way. Each\nfield is one of the step's outputs (its function's or declared) and must fit its type;\nthe fields merge over the run's earlier progress. Progress is never final: it feeds no\ninput, handoff or gate, never finishes the step and is not a submission. It writes no\nlog record, so it wakes no next or log_wait. It stays, with its time, until the step's\nnext run starts; the dashboard shows it (live while the step runs) and the query tool\nreads it as steps.progress and steps.progress_at. Refused (conflict) unless the step is\nrunning and run is its current run; invalid for an unknown field, a value that does\nnot fit or a scattered step. Returns {project, step, run, progress, at}: the merged\nprogress and when it was set.\n\nArgs:\n    project: the project.\n    step: the running step.\n    run: the run id (SLUICE_RUN_ID).\n    outputs: an object keyed by output name: the fields to set now.",
    ),
    (
        "log_read",
        "Read the log. Returns {records, last_seq}: records are {seq, at, kind, ...}, oldest first\n(seqs increase across the whole home, so one log's have gaps). Kinds: plan.edit,\nplan.input, step.output, step.retry, step.cancel, step.submit, step.settle, step.status, step.lease,\nstep.queued, call, message, project.pause, project.archive, project.update,\nproject.board, project.rename, project.delete, project.capacity, project.notify, run.adopt, run.orphan,\nrun.completion_action, run.completion_action.register, unit.settled.\n\nArgs:\n    project: the project's log; leave out for the home log (calls without a project).\n    since_seq: only records after this seq (pass the last_seq you got to continue).\n        Without it: the last `limit` matching records.\n    kinds: only these kinds; \"plan\", \"step\", \"project\", \"run\" or \"unit\" match every\n        kind under them.\n    threads: only messages on these threads (and, without kinds, only messages).\n    statuses: only step.status records whose `to` is one of these step statuses; records\n        of other kinds pass (narrow them with kinds).\n    recipients: only messages whose `to` is one of these: step ids, \"orchestrator\",\n        \"owner\"; records of other kinds pass (narrow them with kinds).\n    limit: at most this many records (default 200).",
    ),
    (
        "log_wait",
        "Wait for log records after since_seq. Returns {records, last_seq} as soon as at least\none matching record exists, or with no records once `timeout` seconds pass. Call it\nagain with the last_seq it returned to keep watching.\n\nArgs:\n    since_seq: wait for records after this seq (0 for any; last_seq from log_read).\n    project: the project's log; leave out for the home log.\n    kinds: only these kinds (as in log_read).\n    threads: only messages on these threads (as in log_read).\n    statuses: only step.status records to these statuses (as in log_read).\n    recipients: only messages to these recipients (as in log_read). One standing wait\n        that wakes on failures and on messages to you: kinds [\"step.status\",\n        \"message\"], statuses [\"failed\", \"stale\"], recipients [\"orchestrator\"].\n    timeout: seconds to wait at most (default 300; capped at 3600).\n    limit: at most this many records (default 200).\n    wake: \"any\" (default) or \"questions\": a note (a message posted with needs_reply\n        false) does not end the wait; it comes back with the next record that does,\n        or once `timeout` passes.",
    ),
    (
        "step_wait",
        "Wait until steps reach a status, instead of polling status. Returns {met, steps, seq}: met\nis whether every selected step meets `until`, steps maps each selected step id to its\nstatus (plan order), and seq is the project log's last seq at that reading (pass it to\nlog_read or log_wait as since_seq to see what happened next). It returns at once when the\ncondition already holds, else as soon as a status change makes it hold, else with met\nfalse once `timeout` seconds pass. An unknown step, a tag no step carries, both or neither\nof steps and tags, or a bad `until` is `invalid`, naming each.\n\nArgs:\n    project: the project.\n    steps: these steps (ids); give steps or tags, not both.\n    tags: every step carrying any of these tags (`unit:<name>` selects a unit).\n    until: \"succeeded\" (every step succeeded), \"settled\" (every step needs an edit\n        or a retry before anything more happens to it: succeeded, failed, stale or skipped,\n        or pending and held: paused, a core.external step waiting for its outputs, or\n        waiting only on settled steps or unset plan inputs), or {\"any_of\": [statuses]}\n        (every step in one of these statuses).\n    timeout: seconds to wait at most (default 300; capped at 3600; 0 checks once).",
    ),
    (
        "next",
        "Wait for the records an orchestrator should act on across the projects. Returns\n{records, notes, last_seq, timed_out}. It wakes on a `unit.settled` record (a unit, the\nsteps tagged with the same unit:<name>, else a step on its own, settles once per work\ngeneration: none of its steps running, or pending and startable); a step that failed,\nwent stale or was skipped; a message needing a reply, not from you, addressed to `me` or\nto nobody; an answering reply not from you; a project paused or archived by someone else.\nA unit.settled record is {unit, work, steps: [{id, status, held, outputs, omitted}]},\nthe succeeded steps' outputs cut as `settles` says. After the first waking record it\nkeeps collecting until `settle` seconds pass with no new one, or `settle_max` seconds\nafter the first. Messages come first in `records`, whole. `notes` are the notes\n(messages with needs_reply false, not from you) held on the way; read them before the\nrecords. Pass `last_seq` back as `since_seq` to continue; nothing is missed or repeated.\nThe command-line form is `sluice next`.\n\nArgs:\n    projects: the projects to watch (one or several; leave out for every live project).\n    since_seq: records after this seq (the last_seq you last got).\n    me: your name; your own messages never wake it (default \"orchestrator\").\n    timeout: seconds to wait at most for the first waking record (default 300;\n        capped at 3600); on a timeout records is empty and timed_out is true.\n    all: every record wakes it (default false).\n    settle: seconds with no new waking record that end the batch (default 20; 0\n        returns at the first).\n    settle_max: seconds after the first waking record that end the batch at the\n        latest (default 120; capped at 3600).\n    settles: how much of a settled unit's outputs each step carries: \"short\"\n        (default) only booleans, numbers, strings of at most 80 characters and a\n        `summary`'s first line (cut to 200), the rest named in `omitted` (read them\n        with status or query); \"full\" every output whole; \"none\" no outputs.",
    ),
    (
        "drain",
        "Pause the projects (default: every project not archived) that are not already\npaused, recording which ones and the author as the drain's owner, so `release` lets\nexactly those go again; new plan work and user calls are refused home-wide until then.\nReturns {paused, status: {mode, owner, paused, blockers, pending_calls, open_questions,\ndrained}}: the projects it paused now, and what is still to finish (the CLI's\n`sluice drain` waits until drained).\n\nArgs:\n    projects: the projects to drain; leave out for every project not archived.\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "release",
        "Unpause exactly the projects the drain recorded (what `sluice drain --release` does) and\nreturn the home to normal. Projects paused otherwise stay paused. Returns {released}.\n\nArgs:\n    author: who is acting (default: SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP\n        client's name, else \"mcp\"; \"cli\" from `sluice tool`).",
    ),
    (
        "step_context",
        "Where a step stands, for the agent doing it — the same as `sluice me` inside the\nstep: its fn, doc, status and running time, inputs, the status and short outputs of\nevery step it reads or runs after, the unanswered messages on its thread\n(step-<id>), the outputs it must submit with the exact step_submit command, the\nthread with the command to ask a question, finishing {since, submission_seq, release}\nonce its run has submitted, and attempt {number, previous?, worktree?, note}: which\nattempt this is, how the one before it ended (outcome, error, who cancelled it, what it\nsubmitted or output, its git head) and the git status of its cwd now; note is the same as\nthe text heading its agent's task (\"first attempt\" when there was none).\n\nArgs:\n    project: the project.\n    step: the step id.",
    ),
    (
        "query",
        "Run one read-only SQL statement against the home database. Returns {columns, rows,\ntruncated}. params binds values; limit defaults to 200 and is at most 1000 rows. Writes,\nATTACH, extensions and unsafe functions are refused; it has a two-second deadline and a\n1 MiB response bound.\n\nArgs:\n    sql: one statement.\n    params: values bound to its placeholders.\n    limit: at most this many rows (default 200).",
    ),
    (
        "verify",
        "Check functions (fn.json shape and types, name collisions), .env syntax, each project's\nplan and its state, and directories under projects/ of no project; changes nothing.\nReturns [{where, message}], empty when all is well.\n\nArgs:\n    project: check this project (and the built-in and global functions it sees);\n        leave out to check everything.",
    ),
    (
        "plan_view",
        "Draw selected steps as Mermaid text or self-contained HTML. Every edge touching the selection remains; its outside endpoint is a boundary node labelled <id> · outside. A comment counts boundary nodes. Done units are left out by default and counted. Returns the graph text.\n\nArgs:\n    project: the project.\n    format: mermaid by default, or html.\n    all: true keeps done units.\n    units: unit names, or one name.\n    steps: step ids, or one id.\n    status: stored statuses, or one status.\n    recipe: units matching this recipe now.\n\nFilters combine with AND, lists match any member and empty lists match nothing.",
    ),
    (
        "status",
        "A project's state at a glance: {project, rev, board_rev, paused, inputs, outputs, resources,\nsteps: {id: {status, title, stage?, outputs, error, run_ids, done, total, instances, manual,\npaused?, queued?, waiting?, finishing?}}, done_units?}. title is how the dashboard names\nthe step (its doc's first line, its spec's heading, its recipe's title, else its id);\nstage its id without \"<unit>-\" in a unit of several steps. finishing {since, submission_seq,\nrelease} marks a running step whose run has submitted: its agent's work is done and the\nrun is only finishing (step_settle settles one that lingers). paused is true or the pause's reason; waiting,\non a pending step that is not about to start, lists why: its pause, the project's, each\nhandoff or gate not ready, the resource shortfall (queued names the resources) or the\nexternal wait. Without steps or tags, the done units (every step\nsucceeded or skipped) are left out and counted in done_units {units, steps}, unless all\nis true.\n\nArgs:\n    project: the project.\n    steps: only these steps (ids).\n    tags: only steps carrying any of these tags (with steps: either).\n    brief: cut every string over 200 characters in inputs and outputs to its first 200\n        and `… [n more characters]`.\n    all: include the done units too (steps or tags always return what they select).\n    view: \"steps\" (default) as above, or \"units\": one compact row per unit (the steps\n        sharing a `unit:<name>` tag, else a step on its own) instead: {project, rev,\n        board_rev, paused, resources?, units: [{unit, title, recipe, state, age, engine, steps,\n        blocked, last, line, finishing?}], done_units?}, oldest first; recipe names the recipe that made it\n        (empty when none matches); finishing lists its finishing steps\n        [{step, since, submission_seq, release}]. state is failed (a step failed or stale,\n        checked first), running, settled (every step succeeded or skipped), blocked (nothing\n        running or startable, something held), queued (a step waits for resources) or pending; age\n        the seconds its running step has run, else since its last change; engine\n        engine·model·effort of its agent step; steps each step's mark, the dashboard's\n        (✗ failed, ~ stale, □ stopping, ▷ finishing, ▶ running, ↗ outside, ‖ paused, ⊖ blocked\n        behind a failure, ∅ held on a plan input, ≡ queued, · pending, ✓ succeeded, – skipped); blocked\n        why a blocked unit's first held step is held, or a queued unit's first queued\n        step's reason; last its steps' threads' last message (\"Q: \" for a question);\n        line all of it in at most 80 characters.\n    state: with view \"units\", only units in this state (one or a list).",
    ),
    (
        "plan_prune",
        "Remove done units (every step succeeded or skipped), all or those named in units or tagged with tags, whose last step finished at least older_than seconds ago, in one edit. A unit a surviving step or plan output references is kept, and so is one whose name matches a keep pattern (keep: [\"ta-*\"], * any run, ? one character). Returns the edit result {project, rev, preview, steps} (steps: the removed steps) with units (the removed units) and kept: [{unit, step}], [{unit, output}] or [{unit, keep}], each kept unit with the step, plan output or pattern holding it. Nothing to remove: no edit, the current rev. dry_run previews without committing. rev is the revision you read, reason and author record the edit.\n\nArgs:\n    project: the project.\n    rev: the revision read earlier; a stale one is conflict with current_rev.\n    dry_run: preview without committing.\n    preview_scope: impact by default; all requires dry_run=true.\n    reason: why, recorded in history.\n    author: explicit, else SLUICE_AUTHOR, step:<SLUICE_STEP>, the MCP client name, else mcp; cli from sluice tool.\n\nA no-op keeps the revision and writes no history. Busy with retryable=true means send the same edit again. The preview is {scope, changes, would_start, would_queue, would_skip, would_stale, errors}; impact describes affected work. Changes are resolved rows, never request operations.",
    ),
    (
        "ask",
        "Ask a question that needs a reply. Returns its receipt {id, to, thread, delivery,\nrun?}. The thread and sender are derived: a message to or from a step lives on\n`step-<step>`, the orchestrator and owner talk on `owner`; you are the orchestrator,\nor the run's step with `run`. delivery is `delivered` (a live run of the step that\nlistens was handed it, or the orchestrator's or owner's inbox), `queued` (the step\nwill run and its next run is assigned it) or `no_live_run` (the step is done, failed\nor paused: a later run, e.g. a retry, gets it). The first reply answers it.\n\nArgs:\n    project: the project.\n    to: a step of the project's current plan, \"orchestrator\" or \"owner\"; anything\n        else is `invalid` and nothing is stored.\n    body: the question, markdown.\n    title: a short title for the inbox.\n    ui: an OpenUI form the owner answers with.\n    input: a plan input the answer sets.\n    data: any JSON for the recipient.\n    run: the asking run (SLUICE_RUN_ID): its step is the sender.",
    ),
    (
        "say",
        "Tell a step, the orchestrator or the owner something; no reply is expected.\nReturns its receipt {id, to, thread, delivery, run?}, as ask does.\n\nArgs:\n    project: the project.\n    to: a step of the project's current plan, \"orchestrator\" or \"owner\".\n    body: the note, markdown.\n    data: any JSON for the recipient.\n    run: the speaking run (SLUICE_RUN_ID): its step is the sender.",
    ),
    (
        "reply",
        "Reply to a message by to_message, or answer the single open question from to addressed to you. Give exactly one selector. No match or several matches is a conflict; several matches list their ids and first lines. Returns its receipt {id, to,\nthread, delivery, run?}. A reply to an open question answers it (with `input`, it\nsets that plan input to `answer.values.value`, `answer.params.value` or the body);\nanswer {\"action\": \"close\"} closes it without setting anything. A reply to a\nquestion already answered or closed is just a message, but one with an `answer` is\nrefused with `conflict`.\n\nArgs:\n    project: the project.\n    to_message: the id of the message replied to; omit when using to.\n    to: the sender whose single open question addressed to you is answered.\n    body: the reply, markdown (may be empty with an answer).\n    answer: {action, params?, values?}, the answer to a question's ui form.\n    run: the replying run (SLUICE_RUN_ID): its step is the sender.",
    ),
    (
        "messages",
        "Read a project's messages. Returns {project, messages, last_id}. Each message is {id,\nverb (ask, say or reply), from, to, thread, body, title?, ui?, input?, data?, run?, at,\nto_message?, answer?}; a question also carries state: open, answered (with\nanswered_by, the answering message's id) or closed.\n\nArgs:\n    project: the project.\n    view: inbox (default; your open questions and unread notes and replies), questions (every\n        open question), history (every conversation you took part in) or thread.\n    thread: the thread, for the thread view (or to narrow another).\n    since: only messages after this id.\n    owner: read as the owner, whose inbox the dashboard shows (default: the\n        orchestrator).",
    ),
];
